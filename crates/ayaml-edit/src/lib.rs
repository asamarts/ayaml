#![forbid(unsafe_code)]
#![warn(rustdoc::all)]

//! Verified, source-preserving edits for YAML documents built on [`ayaml`]'s public API.
//!
//! Every edit returned by this crate is reparsed and checked against both the expected
//! [`Value`] tree and the [`Event`] stream before it is returned.
//! Constructs whose source-preserving edit rules are not yet proven exact — anchors, aliases,
//! tags, block scalars, multiple documents, merge-key expansion, and ambiguous comment placement
//! — are refused conservatively.

use std::cmp::Ordering;
use std::fmt;

use ayaml::{
    EmitOptions, Event, Limits, ParseOptions, PathSegment, ScalarStyle, Span, SpannedTree, Value,
    parse_events_with, parse_value_with, to_string_value_with,
};

/// A single byte-range replacement against a source string.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TextEdit {
    span: Span,
    replacement: String,
}

impl TextEdit {
    /// Create a text edit replacing `span` with `replacement`.
    #[must_use]
    pub fn new(span: Span, replacement: impl Into<String>) -> Self {
        Self {
            span,
            replacement: replacement.into(),
        }
    }

    /// The byte span in the original source text.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// The replacement text substituted for [`Self::span`].
    #[must_use]
    pub fn replacement(&self) -> &str {
        &self.replacement
    }

    /// Whether this edit inserts text at a zero-width byte offset.
    #[must_use]
    pub const fn is_insertion(&self) -> bool {
        self.span.is_empty()
    }
}

/// Pure span ordering: ascending start offset, with zero-width insertions ordered before
/// replacements starting at the same byte offset, then ascending end offset.
fn span_order(a: Span, b: Span) -> Ordering {
    a.start()
        .cmp(&b.start())
        .then_with(|| b.is_empty().cmp(&a.is_empty()))
        .then_with(|| a.end().cmp(&b.end()))
}

/// Ordering for [`TextEdit`]: delegates to [`span_order`].
fn edit_order(a: &TextEdit, b: &TextEdit) -> Ordering {
    span_order(a.span, b.span)
}

/// Validate a single edit span against `input` bounds, UTF-8 character boundaries, and `prev_end`.
fn validate_edit_span(input: &str, span: Span, prev_end: usize) -> Result<usize, &'static str> {
    let start = span.start();
    let end = span.end();
    if start > end || end > input.len() {
        return Err("text edit span is out of bounds");
    }
    if !input.is_char_boundary(start) || !input.is_char_boundary(end) {
        return Err("text edit span does not lie on UTF-8 character boundaries");
    }
    if start < prev_end {
        return Err("text edits must be sorted and non-overlapping");
    }
    Ok(end)
}

/// Validate ordering and non-overlap between two adjacent edit spans.
fn validate_adjacent_spans(prev: Span, next: Span) -> Result<(), &'static str> {
    if span_order(prev, next) == Ordering::Greater {
        return Err(
            "text edits must be sorted with insertions before replacements at the same offset",
        );
    }
    if prev.start() == next.start() && !prev.is_empty() && !next.is_empty() {
        return Err("overlapping text edits at the same offset");
    }
    Ok(())
}

/// The verified result of one or more YAML edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedEdit {
    output: String,
    edits: Vec<TextEdit>,
}

impl VerifiedEdit {
    /// The resulting YAML document text.
    #[must_use]
    pub fn output(&self) -> &str {
        &self.output
    }

    /// Consume the verified edit and return the resulting YAML document text.
    #[must_use]
    pub fn into_output(self) -> String {
        self.output
    }

    /// The sorted, non-overlapping [`TextEdit`]s that transform the original input into
    /// [`Self::output`]. When two edits share a start offset, an insertion appears first.
    #[must_use]
    pub fn edits(&self) -> &[TextEdit] {
        &self.edits
    }
}

/// Coarse category of an [`EditError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EditErrorKind {
    /// The input YAML document could not be parsed.
    Parse,
    /// The target path did not resolve unambiguously in the document.
    NotFound,
    /// The requested edit was refused conservatively (for example, anchors, aliases, tags,
    /// block scalars, multiple documents, flow entries, or ambiguous comment placement).
    Unsupported,
    /// Post-edit re-parse or event/value verification failed.
    VerificationFailed,
    /// Supplied [`TextEdit`]s were unsorted, overlapping, or out of UTF-8 bounds.
    InvalidEdit,
}

impl fmt::Display for EditErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Parse => "parse error",
            Self::NotFound => "target not found",
            Self::Unsupported => "unsupported edit",
            Self::VerificationFailed => "verification failed",
            Self::InvalidEdit => "invalid edit",
        };
        f.write_str(label)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EditErrorInner {
    kind: EditErrorKind,
    message: String,
    source: Option<ayaml::Error>,
}

/// An error returned when an edit cannot be constructed or verified.
///
/// Normal [`Display`](fmt::Display) and [`Debug`](fmt::Debug) output on [`EditError`] (and
/// [`std::error::Error::source`]) may include text from the YAML document when parsing failed.
/// Use [`EditError::masked`] ([`MaskedEditError`]) when logging or returning diagnostics from
/// sensitive or untrusted documents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditError(Box<EditErrorInner>);

impl EditError {
    fn new(kind: EditErrorKind, message: impl Into<String>) -> Self {
        Self(Box::new(EditErrorInner {
            kind,
            message: message.into(),
            source: None,
        }))
    }

    fn parse(error: ayaml::Error) -> Self {
        Self(Box::new(EditErrorInner {
            kind: EditErrorKind::Parse,
            message: error.to_string(),
            source: Some(error),
        }))
    }

    fn verification_with_source(message: impl Into<String>, source: ayaml::Error) -> Self {
        Self(Box::new(EditErrorInner {
            kind: EditErrorKind::VerificationFailed,
            message: message.into(),
            source: Some(source),
        }))
    }

    /// The coarse error category.
    #[must_use]
    pub fn kind(&self) -> EditErrorKind {
        self.0.kind
    }

    /// Human-readable description of why the edit failed or was refused.
    ///
    /// For [`EditErrorKind::Parse`], this message may include text from the YAML document;
    /// use [`Self::masked`] when redaction is required.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.0.message
    }

    /// Return a view whose [`Display`](fmt::Display) and [`Debug`](fmt::Debug) output exclude all
    /// YAML document text.
    ///
    /// Mirrors [`ayaml::Error::masked`]: reports only the [`EditErrorKind`] and, when an
    /// underlying [`ayaml::Error`] caused the failure, its [`ayaml::MaskedError`] view. Its
    /// [`std::error::Error::source`] always returns `None` so an unmasked cause cannot leak
    /// through error-chain formatters.
    #[must_use]
    pub const fn masked(&self) -> MaskedEditError<'_> {
        MaskedEditError(self)
    }
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.message)
    }
}

impl std::error::Error for EditError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source.as_ref().map(|error| {
            let err: &(dyn std::error::Error + 'static) = error;
            err
        })
    }
}

/// A view of an [`EditError`] whose [`Display`](fmt::Display) and [`Debug`](fmt::Debug) forms
/// exclude all YAML document text.
///
/// Returned by [`EditError::masked`]. Its [`std::error::Error::source`] always returns `None` so
/// error-chain formatters cannot expose an unmasked underlying [`ayaml::Error`].
#[derive(Clone, Copy)]
pub struct MaskedEditError<'a>(&'a EditError);

impl<'a> MaskedEditError<'a> {
    /// The coarse error category.
    #[must_use]
    pub fn kind(&self) -> EditErrorKind {
        self.0.kind()
    }

    /// The masked view of the underlying [`ayaml::Error`], if one caused this error.
    #[must_use]
    pub fn source_error(&self) -> Option<ayaml::MaskedError<'a>> {
        self.0.0.source.as_ref().map(ayaml::Error::masked)
    }
}

impl PartialEq for MaskedEditError<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.kind() == other.kind() && self.source_error() == other.source_error()
    }
}

impl Eq for MaskedEditError<'_> {}

impl fmt::Display for MaskedEditError<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.source_error() {
            Some(source) => write!(f, "{}: {source}", self.kind()),
            None => fmt::Display::fmt(&self.kind(), f),
        }
    }
}

impl fmt::Debug for MaskedEditError<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MaskedEditError")
            .field("kind", &self.kind())
            .field("source", &self.source_error())
            .finish()
    }
}

impl std::error::Error for MaskedEditError<'_> {}

/// Options controlling how documents are parsed and verified during editing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct EditOptions {
    parse: ParseOptions,
}

impl EditOptions {
    /// Default edit options using [`ParseOptions::new`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            parse: ParseOptions::new(),
        }
    }

    /// Set the [`ParseOptions`] used to parse and re-verify documents.
    #[must_use]
    pub const fn with_parse_options(mut self, options: ParseOptions) -> Self {
        self.parse = options;
        self
    }

    /// The active [`ParseOptions`].
    #[must_use]
    pub const fn parse_options(&self) -> &ParseOptions {
        &self.parse
    }
}

/// One edit operation for [`apply_batch`] / [`apply_batch_with`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EditOp {
    /// Replace the scalar at `path` with `value`.
    ReplaceScalar {
        /// Path to the target scalar.
        path: Vec<PathSegment>,
        /// New scalar value (must be null, bool, number, or string).
        value: Value,
    },
    /// Remove the block entry at `path`.
    RemoveEntry {
        /// Path to the target entry.
        path: Vec<PathSegment>,
    },
}

/// Pure arithmetic step for spliced output byte length (shared with Kani proofs).
const fn step_spliced_len(len: usize, span: Span, replacement_len: usize) -> Option<usize> {
    let span_len = span.end().saturating_sub(span.start());
    match len.checked_sub(span_len) {
        Some(base) => base.checked_add(replacement_len),
        None => None,
    }
}

/// Compute the exact spliced output byte length from `(span, replacement_len)` pairs without
/// allocating.
#[cfg(any(test, kani))]
fn spliced_output_len_pairs(
    input_len: usize,
    edits: &[(Span, usize)],
) -> Result<usize, &'static str> {
    let mut len = input_len;
    for &(span, replacement_len) in edits {
        len = step_spliced_len(len, span, replacement_len)
            .ok_or("edited output length overflows usize")?;
    }
    Ok(len)
}

/// Compute the exact spliced output byte length for validated non-overlapping edits.
fn spliced_output_len(input_len: usize, edits: &[TextEdit]) -> Result<usize, &'static str> {
    let mut len = input_len;
    for edit in edits {
        len = step_spliced_len(len, edit.span, edit.replacement.len())
            .ok_or("edited output length overflows usize")?;
    }
    Ok(len)
}

fn apply_edits_bounded(
    input: &str,
    edits: &[TextEdit],
    limits: Option<&Limits>,
) -> Result<String, EditError> {
    let mut prev_end = 0;
    for edit in edits {
        prev_end = validate_edit_span(input, edit.span, prev_end)
            .map_err(|msg| EditError::new(EditErrorKind::InvalidEdit, msg))?;
    }
    for pair in edits.windows(2) {
        validate_adjacent_spans(pair[0].span, pair[1].span)
            .map_err(|msg| EditError::new(EditErrorKind::InvalidEdit, msg))?;
    }

    let out_len = spliced_output_len(input.len(), edits)
        .map_err(|msg| EditError::new(EditErrorKind::InvalidEdit, msg))?;
    if let Some(limits) = limits
        && let Err(limit_err) = limits.check_input(out_len)
    {
        let source = ayaml::Error::from(limit_err);
        return Err(EditError::verification_with_source(
            format!("edited document failed to re-parse: {}", source.masked()),
            source,
        ));
    }

    let mut output = String::with_capacity(out_len);
    let mut cursor = 0;
    for edit in edits {
        output.push_str(&input[cursor..edit.span.start()]);
        output.push_str(&edit.replacement);
        cursor = edit.span.end();
    }
    output.push_str(&input[cursor..]);
    Ok(output)
}

/// Apply sorted, non-overlapping [`TextEdit`]s to `input`.
///
/// Two edits may share a start offset only if the earlier one is an insertion (empty span).
///
/// # Errors
///
/// Returns [`EditErrorKind::InvalidEdit`] if any edit is out of bounds, lands inside a UTF-8
/// code point, is out of order, or overlaps another edit.
pub fn apply_edits(input: &str, edits: &[TextEdit]) -> Result<String, EditError> {
    apply_edits_bounded(input, edits, None)
}

/// Replace the scalar at `path` with `replacement`, verifying both the resulting value tree
/// and event stream before returning.
///
/// # Errors
///
/// Returns [`EditError`] if the input fails to parse, `path` cannot be found, the document or
/// target uses an unsupported construct, or post-edit verification fails.
pub fn replace_scalar(
    input: &str,
    path: &[PathSegment],
    replacement: &Value,
) -> Result<VerifiedEdit, EditError> {
    replace_scalar_with(input, path, replacement, &EditOptions::new())
}

/// Replace the scalar at `path` with `replacement` using explicit [`EditOptions`].
///
/// # Errors
///
/// Returns [`EditError`] if the input fails to parse, `path` cannot be found, the document or
/// target uses an unsupported construct, or post-edit verification fails.
pub fn replace_scalar_with(
    input: &str,
    path: &[PathSegment],
    replacement: &Value,
    options: &EditOptions,
) -> Result<VerifiedEdit, EditError> {
    let op = EditOp::ReplaceScalar {
        path: path.to_vec(),
        value: replacement.clone(),
    };
    apply_batch_with(input, &[op], options)
}

/// Remove the block entry at `path`, verifying both the resulting value tree and event stream
/// before returning.
///
/// # Errors
///
/// Returns [`EditError`] if the input fails to parse, `path` cannot be found, the entry has no
/// exact removal span or uses an unsupported construct, or post-edit verification fails.
pub fn remove_entry(input: &str, path: &[PathSegment]) -> Result<VerifiedEdit, EditError> {
    remove_entry_with(input, path, &EditOptions::new())
}

/// Remove the block entry at `path` using explicit [`EditOptions`].
///
/// # Errors
///
/// Returns [`EditError`] if the input fails to parse, `path` cannot be found, the entry has no
/// exact removal span or uses an unsupported construct, or post-edit verification fails.
pub fn remove_entry_with(
    input: &str,
    path: &[PathSegment],
    options: &EditOptions,
) -> Result<VerifiedEdit, EditError> {
    let op = EditOp::RemoveEntry {
        path: path.to_vec(),
    };
    apply_batch_with(input, &[op], options)
}

/// Apply a batch of [`EditOp`]s atomically and verify the resulting document.
///
/// # Errors
///
/// Returns [`EditError`] if any operation is unsupported, if resulting edits overlap, or if
/// post-edit value or event-stream verification fails.
pub fn apply_batch(input: &str, ops: &[EditOp]) -> Result<VerifiedEdit, EditError> {
    apply_batch_with(input, ops, &EditOptions::new())
}

/// Apply a batch of [`EditOp`]s atomically using explicit [`EditOptions`].
///
/// # Errors
///
/// Returns [`EditError`] if any operation is unsupported, if resulting edits overlap, or if
/// post-edit value or event-stream verification fails.
pub fn apply_batch_with(
    input: &str,
    ops: &[EditOp],
    options: &EditOptions,
) -> Result<VerifiedEdit, EditError> {
    if ops.is_empty() {
        // Still validate that the input is a single supported document with no duplicate keys or
        // invalid core-schema scalars.
        let _unused = inspect_document(input, &options.parse)?;
        let _unused_val = parse_value_with(input, &options.parse).map_err(EditError::parse)?;
        return Ok(VerifiedEdit {
            output: input.to_owned(),
            edits: Vec::new(),
        });
    }

    let before_events = inspect_document(input, &options.parse)?;
    let tree = SpannedTree::parse_with(input, &options.parse).map_err(EditError::parse)?;
    if tree.documents().len() != 1 {
        return Err(EditError::new(
            EditErrorKind::Unsupported,
            "multi-document streams are not supported",
        ));
    }
    let original_value = parse_value_with(input, &options.parse).map_err(EditError::parse)?;

    let mut planned: Vec<(TextEdit, &EditOp)> = Vec::with_capacity(ops.len());
    for op in ops {
        let edit = match op {
            EditOp::ReplaceScalar { path, value } => {
                plan_replace_scalar(input, &tree, path, value, &options.parse.limits)?
            }
            EditOp::RemoveEntry { path } => plan_remove_entry(&tree, path)?,
        };
        planned.push((edit, op));
    }

    planned.sort_by(|(a, _), (b, _)| edit_order(a, b));
    let (edits, ordered_ops): (Vec<TextEdit>, Vec<&EditOp>) = planned.into_iter().unzip();
    let output = apply_edits_bounded(input, &edits, Some(&options.parse.limits))?;

    // Apply expected-value updates from highest source offset downward so removing an earlier
    // sequence item does not shift the indices of later planned operations.
    let mut expected_value = original_value;
    for op in ordered_ops.iter().rev() {
        match op {
            EditOp::ReplaceScalar { path, value } => {
                if !replace_value_at_mut(&mut expected_value, path, value) {
                    return Err(EditError::new(
                        EditErrorKind::NotFound,
                        "target path does not exist in value tree",
                    ));
                }
            }
            EditOp::RemoveEntry { path } => {
                if !remove_value_at_mut(&mut expected_value, path) {
                    return Err(EditError::new(
                        EditErrorKind::Unsupported,
                        "cannot remove target entry from value tree",
                    ));
                }
            }
        }
    }

    // Post-edit verification: re-parse value and events and compare.
    // Mask any parser error details so raw document text cannot leak through `VerificationFailed`.
    // Preserve `source` for limit errors (which carry no document text) so callers can inspect
    // `LimitExceeded` consistently across pre-allocation and re-parse limit failures.
    let reparsed_value = parse_value_with(&output, &options.parse).map_err(|error| {
        let message = format!("edited document failed to re-parse: {}", error.masked());
        if error.kind() == ayaml::ErrorKind::Limit {
            EditError::verification_with_source(message, error)
        } else {
            EditError::new(EditErrorKind::VerificationFailed, message)
        }
    })?;
    if reparsed_value != expected_value
        || format!("{reparsed_value:?}") != format!("{expected_value:?}")
    {
        return Err(EditError::new(
            EditErrorKind::VerificationFailed,
            "edited document did not match the expected value tree",
        ));
    }

    let after_events = collect_canonical_events(&output, &options.parse).map_err(|error| {
        let message = format!(
            "edited document failed event verification: {}",
            error.masked()
        );
        if error.kind() == ayaml::ErrorKind::Limit {
            EditError::verification_with_source(message, error)
        } else {
            EditError::new(EditErrorKind::VerificationFailed, message)
        }
    })?;

    let ok = if ops.len() == 1 {
        match &ops[0] {
            EditOp::ReplaceScalar { .. } => {
                one_scalar_changed(&before_events.texts, &after_events.texts)
            }
            EditOp::RemoveEntry { .. } => {
                one_run_removed(&before_events.texts, &after_events.texts)
            }
        }
    } else {
        verify_batch_events_with_spans(&before_events, &after_events, &edits, &ordered_ops)
    };
    if !ok {
        return Err(EditError::new(
            EditErrorKind::VerificationFailed,
            "edited document changed unexpected events outside the target span",
        ));
    }

    Ok(VerifiedEdit { output, edits })
}

struct CanonicalEvents {
    texts: Vec<String>,
    spans: Vec<Span>,
    is_non_empty_scalar: Vec<bool>,
}

impl CanonicalEvents {
    fn new() -> Self {
        Self {
            texts: Vec::new(),
            spans: Vec::new(),
            is_non_empty_scalar: Vec::new(),
        }
    }

    fn push(&mut self, event: &Event<'_>) {
        let span = event.span();
        let is_non_empty_scalar = matches!(event, Event::Scalar { .. }) && !span.is_empty();
        self.texts.push(canonical_event(event));
        self.spans.push(span);
        self.is_non_empty_scalar.push(is_non_empty_scalar);
    }
}

/// Parse `input`'s events and refuse documents with multiple documents, anchors, aliases,
/// tags, block scalars, or opt-in merge-key expansion.
fn inspect_document(input: &str, options: &ParseOptions) -> Result<CanonicalEvents, EditError> {
    if options.merge_keys {
        return Err(EditError::new(
            EditErrorKind::Unsupported,
            "documents with merge-key expansion enabled are not supported",
        ));
    }
    let mut doc_count = 0usize;
    let mut canonical = CanonicalEvents::new();
    let stream = parse_events_with(input, options).map_err(EditError::parse)?;
    for item in stream {
        let event = item.map_err(EditError::parse)?;
        match &event {
            Event::DocumentStart { .. } => {
                doc_count += 1;
                if doc_count > 1 {
                    return Err(EditError::new(
                        EditErrorKind::Unsupported,
                        "multi-document streams are not supported",
                    ));
                }
            }
            Event::Alias { .. } => {
                return Err(EditError::new(
                    EditErrorKind::Unsupported,
                    "documents with aliases are not supported",
                ));
            }
            Event::Scalar {
                style, tag, anchor, ..
            } => {
                if anchor.is_some() {
                    return Err(EditError::new(
                        EditErrorKind::Unsupported,
                        "documents with anchors are not supported",
                    ));
                }
                if tag.is_some() {
                    return Err(EditError::new(
                        EditErrorKind::Unsupported,
                        "documents with tags are not supported",
                    ));
                }
                if matches!(style, ScalarStyle::Literal | ScalarStyle::Folded) {
                    return Err(EditError::new(
                        EditErrorKind::Unsupported,
                        "documents with block scalars are not supported",
                    ));
                }
            }
            Event::SequenceStart { tag, anchor, .. } | Event::MappingStart { tag, anchor, .. } => {
                if anchor.is_some() {
                    return Err(EditError::new(
                        EditErrorKind::Unsupported,
                        "documents with anchors are not supported",
                    ));
                }
                if tag.is_some() {
                    return Err(EditError::new(
                        EditErrorKind::Unsupported,
                        "documents with tags are not supported",
                    ));
                }
            }
            _ => {}
        }
        canonical.push(&event);
    }
    if doc_count != 1 {
        return Err(EditError::new(
            EditErrorKind::Unsupported,
            "expected exactly one YAML document",
        ));
    }
    Ok(canonical)
}

fn collect_canonical_events(
    input: &str,
    options: &ParseOptions,
) -> Result<CanonicalEvents, ayaml::Error> {
    let stream = parse_events_with(input, options)?;
    let mut out = CanonicalEvents::new();
    for item in stream {
        let event = item?;
        out.push(&event);
    }
    Ok(out)
}

fn canonical_event(event: &Event<'_>) -> String {
    match event {
        Event::Scalar {
            value,
            style,
            tag,
            anchor,
            ..
        } => format!("scalar {value:?} {style:?} {tag:?} {anchor:?}"),
        Event::Alias { anchor, .. } => format!("alias {anchor:?}"),
        Event::SequenceStart {
            tag, anchor, style, ..
        } => format!("sequence {style:?} {tag:?} {anchor:?}"),
        Event::MappingStart {
            tag, anchor, style, ..
        } => format!("mapping {style:?} {tag:?} {anchor:?}"),
        Event::DocumentStart { explicit, .. } => format!("document {explicit}"),
        Event::DocumentEnd { explicit, .. } => format!("document end {explicit}"),
        Event::Directive { name, value, .. } => format!("directive {name} {value}"),
        other => format!("{:?}", std::mem::discriminant(other)),
    }
}

fn plan_replace_scalar(
    input: &str,
    tree: &SpannedTree<'_>,
    path: &[PathSegment],
    replacement: &Value,
    limits: &Limits,
) -> Result<TextEdit, EditError> {
    if !matches!(
        replacement,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    ) {
        return Err(EditError::new(
            EditErrorKind::Unsupported,
            "replace_scalar only accepts scalar values",
        ));
    }
    let node = tree
        .lookup(path)
        .ok_or_else(|| EditError::new(EditErrorKind::NotFound, "target path not found"))?;
    if !node.is_scalar() {
        return Err(EditError::new(
            EditErrorKind::Unsupported,
            "target node is not a scalar",
        ));
    }
    let span = node.span();
    if span.is_empty() {
        return Err(EditError::new(
            EditErrorKind::Unsupported,
            "replacing an unwritten empty scalar is not supported",
        ));
    }
    let raw = node.scalar_text().ok_or_else(|| {
        EditError::new(EditErrorKind::Unsupported, "target scalar has no raw text")
    })?;
    if raw.contains(['\n', '\r']) {
        return Err(EditError::new(
            EditErrorKind::Unsupported,
            "replacing a multiline scalar is not supported",
        ));
    }
    // Refuse ambiguous comment placement immediately after the scalar without whitespace.
    if let Some(rest) = input.get(span.end()..)
        && rest.starts_with('#')
    {
        return Err(EditError::new(
            EditErrorKind::Unsupported,
            "ambiguous comment immediately follows target scalar",
        ));
    }

    // Reject oversized replacement strings before cloning or emitting them: even in the densest
    // YAML escape form (`\u{2028}` -> `\L`, 3 bytes -> 2 bytes), the formatted scalar is at least
    // `text.len() / 2` bytes long.
    if let Value::String(text) = replacement
        && let Err(limit_err) = limits.check_input(text.len() / 2)
    {
        let source = ayaml::Error::from(limit_err);
        return Err(EditError::verification_with_source(
            format!("edited document failed to re-parse: {}", source.masked()),
            source,
        ));
    }

    let formatted = format_inline_scalar(replacement, limits)?;
    Ok(TextEdit::new(span, formatted))
}

fn format_inline_scalar(value: &Value, limits: &Limits) -> Result<String, EditError> {
    // Emit inside a 1-element flow sequence so `ayaml`'s emitter uses flow-context quoting
    // (quoting strings with `,`, `[`, `]`, `{`, `}` as well as block-unsafe indicators).
    // Bound emitter output by `max_input_bytes + 3` (`[` + scalar + `]\n`) so heavily escaped
    // replacement strings fail before allocating an oversized buffer.
    let input_cap = limits.max_input_bytes.saturating_add(3);
    let capped_by_input = input_cap < limits.max_output_bytes;
    let mut emit_limits = limits.clone();
    if capped_by_input {
        emit_limits.max_output_bytes = input_cap;
    }
    let emit_options = EmitOptions::new().with_flow(true).with_limits(emit_limits);
    let wrapped = Value::Sequence(vec![value.clone()]);
    let emitted = to_string_value_with(&wrapped, &emit_options).map_err(|error| {
        if capped_by_input && error.code() == ayaml::ErrorCode::OutputSizeLimit {
            let observed = error
                .limit()
                .map_or_else(
                    || limits.max_input_bytes.saturating_add(1),
                    |info| {
                        usize::try_from(info.observed())
                            .unwrap_or(usize::MAX)
                            .saturating_sub(3)
                    },
                )
                .max(limits.max_input_bytes.saturating_add(1));
            let source = match limits.check_input(observed) {
                Err(limit_err) => ayaml::Error::from(limit_err),
                Ok(()) => error,
            };
            EditError::verification_with_source(
                format!("edited document failed to re-parse: {}", source.masked()),
                source,
            )
        } else {
            EditError::new(
                EditErrorKind::Unsupported,
                format!("could not format replacement scalar: {}", error.masked()),
            )
        }
    })?;
    let trimmed = emitted.trim_end_matches(['\n', '\r']);
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .ok_or_else(|| {
            EditError::new(
                EditErrorKind::Unsupported,
                "replacement scalar cannot be formatted on a single line",
            )
        })?;
    if inner.is_empty() || inner.contains(['\n', '\r']) {
        return Err(EditError::new(
            EditErrorKind::Unsupported,
            "replacement scalar cannot be formatted on a single line",
        ));
    }
    Ok(inner.to_owned())
}

fn plan_remove_entry(tree: &SpannedTree<'_>, path: &[PathSegment]) -> Result<TextEdit, EditError> {
    if path.is_empty() {
        return Err(EditError::new(
            EditErrorKind::Unsupported,
            "cannot remove the document root",
        ));
    }
    let node = tree
        .lookup(path)
        .ok_or_else(|| EditError::new(EditErrorKind::NotFound, "target path not found"))?;
    let span = node.entry_span().ok_or_else(|| {
        EditError::new(
            EditErrorKind::Unsupported,
            "target entry has no exact removal span",
        )
    })?;
    Ok(TextEdit::new(span, String::new()))
}

fn replace_value_at_mut(
    mut current: &mut Value,
    path: &[PathSegment],
    replacement: &Value,
) -> bool {
    for segment in path {
        current = match (current, segment) {
            (Value::Mapping(mapping), PathSegment::Key(key)) => match mapping.get_mut(key) {
                Some(inner) => inner,
                None => return false,
            },
            (Value::Sequence(items), PathSegment::Index(idx)) => match items.get_mut(*idx) {
                Some(inner) => inner,
                None => return false,
            },
            _ => return false,
        };
    }
    *current = replacement.clone();
    true
}

fn remove_value_at_mut(mut current: &mut Value, path: &[PathSegment]) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    for segment in parents {
        current = match (current, segment) {
            (Value::Mapping(mapping), PathSegment::Key(key)) => match mapping.get_mut(key) {
                Some(inner) => inner,
                None => return false,
            },
            (Value::Sequence(items), PathSegment::Index(idx)) => match items.get_mut(*idx) {
                Some(inner) => inner,
                None => return false,
            },
            _ => return false,
        };
    }
    match (current, last) {
        (Value::Mapping(mapping), PathSegment::Key(key)) => {
            mapping.shift_remove(key).is_some() && !mapping.is_empty()
        }
        (Value::Sequence(items), PathSegment::Index(idx)) => {
            if *idx >= items.len() || items.len() == 1 {
                return false;
            }
            items.remove(*idx);
            true
        }
        _ => false,
    }
}

fn count_ops(ops: &[&EditOp]) -> (usize, usize) {
    let mut replaces = 0usize;
    let mut removals = 0usize;
    for op in ops {
        match op {
            EditOp::ReplaceScalar { .. } => replaces += 1,
            EditOp::RemoveEntry { .. } => removals += 1,
        }
    }
    (replaces, removals)
}

/// Partition multi-op event verification along every untouched non-empty scalar whose shifted byte
/// span and canonical event text match verbatim in `after`. Each bounded gap between anchors is
/// checked against only the exact ops whose spans fall inside that gap.
fn verify_batch_events_with_spans(
    before: &CanonicalEvents,
    after: &CanonicalEvents,
    edits: &[TextEdit],
    ordered_ops: &[&EditOp],
) -> bool {
    debug_assert_eq!(edits.len(), ordered_ops.len());
    debug_assert_eq!(before.texts.len(), before.spans.len());
    debug_assert_eq!(before.texts.len(), before.is_non_empty_scalar.len());
    debug_assert_eq!(after.texts.len(), after.spans.len());
    debug_assert_eq!(after.texts.len(), after.is_non_empty_scalar.len());

    let mut edit_idx = 0usize;
    let mut removed_bytes = 0usize;
    let mut added_bytes = 0usize;

    let mut gap_b_start = 0usize;
    let mut gap_a_start = 0usize;
    let mut gap_edit_start = 0usize;
    let mut after_cursor = 0usize;
    let mut min_next_start = 0usize;

    for b_idx in 0..before.texts.len() {
        if !before.is_non_empty_scalar[b_idx] {
            continue;
        }
        let span = before.spans[b_idx];
        if span.start() < min_next_start {
            continue;
        }

        while edit_idx < edits.len() && edits[edit_idx].span.end() <= span.start() {
            let edit = &edits[edit_idx];
            removed_bytes += edit.span.end() - edit.span.start();
            added_bytes += edit.replacement.len();
            edit_idx += 1;
        }
        min_next_start = span.start();

        // Only anchor on scalars strictly outside all edit spans.
        if edit_idx < edits.len() && span.end() > edits[edit_idx].span.start() {
            continue;
        }

        let Some(shifted_start) = span
            .start()
            .checked_sub(removed_bytes)
            .and_then(|base| base.checked_add(added_bytes))
        else {
            return false;
        };
        let Some(shifted_end) = span
            .end()
            .checked_sub(removed_bytes)
            .and_then(|base| base.checked_add(added_bytes))
        else {
            return false;
        };
        let shifted_span = Span::new(shifted_start, shifted_end);

        // Advance `after_cursor` monotonically to locate this exact shifted anchor.
        let mut matched_a = None;
        while after_cursor < after.texts.len() {
            if !after.is_non_empty_scalar[after_cursor] {
                after_cursor += 1;
                continue;
            }
            let a_span = after.spans[after_cursor];
            if a_span.start() < shifted_start {
                after_cursor += 1;
                continue;
            }
            if a_span == shifted_span && after.texts[after_cursor] == before.texts[b_idx] {
                matched_a = Some(after_cursor);
            }
            break;
        }

        if let Some(a_idx) = matched_a {
            let (gap_replaces, gap_removals) = count_ops(&ordered_ops[gap_edit_start..edit_idx]);
            if !batch_events_match(
                &before.texts[gap_b_start..b_idx],
                &after.texts[gap_a_start..a_idx],
                gap_replaces,
                gap_removals,
            ) {
                return false;
            }
            gap_b_start = b_idx + 1;
            gap_a_start = a_idx + 1;
            after_cursor = a_idx + 1;
            gap_edit_start = edit_idx;
            min_next_start = span.end();
        }
    }

    let (tail_replaces, tail_removals) = count_ops(&ordered_ops[gap_edit_start..]);
    batch_events_match(
        &before.texts[gap_b_start..],
        &after.texts[gap_a_start..],
        tail_replaces,
        tail_removals,
    )
}

fn one_scalar_changed(before: &[String], after: &[String]) -> bool {
    if before.len() != after.len() {
        return false;
    }
    let mut diffs = 0usize;
    for (b, a) in before.iter().zip(after) {
        if b != a {
            if !b.starts_with("scalar ") || !a.starts_with("scalar ") {
                return false;
            }
            diffs += 1;
        }
    }
    diffs <= 1
}

fn one_run_removed(before: &[String], after: &[String]) -> bool {
    let same = |pair: &(&String, &String)| pair.0 == pair.1;
    let prefix = before.iter().zip(after).take_while(same).count();
    let suffix = before[prefix..]
        .iter()
        .rev()
        .zip(after[prefix..].iter().rev())
        .take_while(same)
        .count();
    after.len() < before.len() && prefix + suffix == after.len()
}

/// Verify that `after` can be obtained from `before` by at most `max_replaces` scalar-to-scalar
/// substitutions and at most `max_removals` contiguous deleted event runs (and zero insertions or
/// structural changes elsewhere).
fn batch_events_match(
    before: &[String],
    after: &[String],
    max_replaces: usize,
    max_removals: usize,
) -> bool {
    if after.len() > before.len() {
        return false;
    }
    if max_removals == 0 {
        if before.len() != after.len() {
            return false;
        }
        if max_replaces == 0 {
            return before == after;
        }
        let mut diffs = 0usize;
        for (b, a) in before.iter().zip(after) {
            if b != a {
                if !b.starts_with("scalar ") || !a.starts_with("scalar ") {
                    return false;
                }
                diffs += 1;
                if diffs > max_replaces {
                    return false;
                }
            }
        }
        return true;
    }
    // Strip common prefix and suffix to shrink the DP window.
    let same = |pair: &(&String, &String)| pair.0 == pair.1;
    let prefix = before.iter().zip(after).take_while(same).count();
    let suffix = before[prefix..]
        .iter()
        .rev()
        .zip(after[prefix..].iter().rev())
        .take_while(same)
        .count();
    let b = &before[prefix..before.len() - suffix];
    let a = &after[prefix..after.len() - suffix];
    let max_skip = b.len() - a.len();
    let cols = max_skip + 1;
    // Preserve all Pareto-distinct `(removals_used, replaces_used)` states by indexing on the
    // smaller of `max_removals` and `max_replaces` and minimizing the other cost.
    let min_budget = max_removals.min(max_replaces);
    let stride = min_budget.saturating_add(1);
    // Bound DP work for very large multi-op batches; fall back to lookahead greedy alignment if
    // the exact state table would exceed 1M DP cells.
    if (a.len() + 1).saturating_mul(cols).saturating_mul(stride) > 1_000_000 {
        return batch_events_match_greedy(b, a, max_replaces, max_removals);
    }
    if max_removals <= max_replaces {
        batch_events_match_dp_by_removals(b, a, max_skip, max_replaces, max_removals)
    } else {
        batch_events_match_dp_by_replaces(b, a, max_skip, max_replaces, max_removals)
    }
}

/// Exact DP indexed by `(skip, removals_used)` minimizing `replaces_used`.
fn batch_events_match_dp_by_removals(
    b: &[String],
    a: &[String],
    max_skip: usize,
    max_replaces: usize,
    max_removals: usize,
) -> bool {
    const INF: usize = usize::MAX;
    let cols = max_skip + 1;
    let stride = max_removals + 1;
    let mut curr = vec![[INF; 2]; cols * stride];
    let mut next = vec![[INF; 2]; cols * stride];
    curr[0][0] = 0;

    for j in 0..=a.len() {
        next.fill([INF; 2]);
        for skip in 0..=max_skip {
            let i = j + skip;
            let row_base = skip * stride;
            let next_skip_base = (skip + 1) * stride;
            for r in 0..=max_removals {
                let [not_in, in_run] = curr[row_base + r];
                if not_in == INF && in_run == INF {
                    continue;
                }
                // Option 1: delete `b[i]` (advance `skip` by 1, enter/stay in removed run).
                if skip < max_skip && i < b.len() {
                    if not_in != INF && r < max_removals {
                        let slot = &mut curr[next_skip_base + r + 1][1];
                        if not_in < *slot {
                            *slot = not_in;
                        }
                    }
                    if in_run != INF {
                        let slot = &mut curr[next_skip_base + r][1];
                        if in_run < *slot {
                            *slot = in_run;
                        }
                    }
                }
                // Option 2: match or replace `b[i]` with `a[j]` (advance `j` by 1, exit run).
                if j < a.len() && i < b.len() {
                    let best = not_in.min(in_run);
                    if b[i] == a[j] {
                        let slot = &mut next[row_base + r][0];
                        if best < *slot {
                            *slot = best;
                        }
                    } else if b[i].starts_with("scalar ")
                        && a[j].starts_with("scalar ")
                        && best < max_replaces
                    {
                        let slot = &mut next[row_base + r][0];
                        if best + 1 < *slot {
                            *slot = best + 1;
                        }
                    }
                }
            }
        }
        if j < a.len() {
            std::mem::swap(&mut curr, &mut next);
        }
    }

    let final_base = max_skip * stride;
    (0..=max_removals).any(|r| {
        let [not_in, in_run] = curr[final_base + r];
        let best = not_in.min(in_run);
        best != INF && best <= max_replaces
    })
}

/// Exact DP indexed by `(skip, replaces_used)` minimizing `removals_used`.
fn batch_events_match_dp_by_replaces(
    b: &[String],
    a: &[String],
    max_skip: usize,
    max_replaces: usize,
    max_removals: usize,
) -> bool {
    const INF: usize = usize::MAX;
    let cols = max_skip + 1;
    let stride = max_replaces + 1;
    let mut curr = vec![[INF; 2]; cols * stride];
    let mut next = vec![[INF; 2]; cols * stride];
    curr[0][0] = 0;

    for j in 0..=a.len() {
        next.fill([INF; 2]);
        for skip in 0..=max_skip {
            let i = j + skip;
            let row_base = skip * stride;
            let next_skip_base = (skip + 1) * stride;
            for p in 0..=max_replaces {
                let [not_in, in_run] = curr[row_base + p];
                if not_in == INF && in_run == INF {
                    continue;
                }
                // Option 1: delete `b[i]` (advance `skip` by 1, enter/stay in removed run).
                if skip < max_skip && i < b.len() {
                    if not_in < max_removals {
                        let slot = &mut curr[next_skip_base + p][1];
                        if not_in + 1 < *slot {
                            *slot = not_in + 1;
                        }
                    }
                    if in_run != INF {
                        let slot = &mut curr[next_skip_base + p][1];
                        if in_run < *slot {
                            *slot = in_run;
                        }
                    }
                }
                // Option 2: match or replace `b[i]` with `a[j]` (advance `j` by 1, exit run).
                if j < a.len() && i < b.len() {
                    let best = not_in.min(in_run);
                    if b[i] == a[j] {
                        let slot = &mut next[row_base + p][0];
                        if best < *slot {
                            *slot = best;
                        }
                    } else if b[i].starts_with("scalar ")
                        && a[j].starts_with("scalar ")
                        && p < max_replaces
                    {
                        let slot = &mut next[row_base + p + 1][0];
                        if best < *slot {
                            *slot = best;
                        }
                    }
                }
            }
        }
        if j < a.len() {
            std::mem::swap(&mut curr, &mut next);
        }
    }

    let final_base = max_skip * stride;
    (0..=max_replaces).any(|p| {
        let [not_in, in_run] = curr[final_base + p];
        let best = not_in.min(in_run);
        best != INF && best <= max_removals
    })
}

/// Linear-time lookahead greedy alignment used when an extreme unanchored multi-op gap exceeds the
/// 1M-cell exact DP cap. Distinguishes scalar replacements from deleted runs using immediate
/// post-event sync and bounded skip-window resynchronization.
fn batch_events_match_greedy(
    before: &[String],
    after: &[String],
    max_replaces: usize,
    max_removals: usize,
) -> bool {
    let mut i = 0usize;
    let mut j = 0usize;
    let mut replaces = 0usize;
    let mut removals = 0usize;
    let mut in_run = false;
    let mut no_sync_j: Option<usize> = None;
    let mut remaining_probes = before.len().saturating_mul(4).max(1024);
    while i < before.len() && j < after.len() {
        if before[i] == after[j] {
            in_run = false;
            i += 1;
            j += 1;
            continue;
        }
        let rem_skip = (before.len() - i).saturating_sub(after.len() - j);
        let both_scalar = before[i].starts_with("scalar ") && after[j].starts_with("scalar ");
        let can_replace = both_scalar && replaces < max_replaces;

        // 1. When not already inside a removal run, two matching events immediately after
        //    `(i, j)` (or single matching tail with zero remaining skips) indicate an isolated
        //    scalar replacement.
        if !in_run
            && can_replace
            && before.get(i + 1) == after.get(j + 1)
            && ((j + 2 >= after.len() && rem_skip == 0)
                || (j + 2 < after.len() && before.get(i + 2) == after.get(j + 2)))
        {
            replaces += 1;
            i += 1;
            j += 1;
            continue;
        }

        // 2. When skips remain and we can continue or start a removal run, search the skip window
        //    for a resynchronization point `before[i + d] == after[j]`: either two matching events
        //    (normal entry / tail) or one matching event (e.g. a mapping key) immediately followed
        //    by a scalar replacement whose successor also syncs. Cache failed scans per `j` and
        //    bound total probes so the fallback stays strictly linear in `before.len()`.
        if rem_skip > 0 && (in_run || removals < max_removals) {
            let sync = if no_sync_j == Some(j) || remaining_probes == 0 {
                None
            } else {
                let limit = rem_skip.min(remaining_probes);
                let mut found = None;
                for d in 1..=limit {
                    remaining_probes -= 1;
                    if before[i + d] != after[j] {
                        continue;
                    }
                    let next_matches =
                        j + 1 == after.len() || before.get(i + d + 1) == after.get(j + 1);
                    let key_then_replace = replaces < max_replaces
                        && before
                            .get(i + d + 1)
                            .is_some_and(|s| s.starts_with("scalar "))
                        && after.get(j + 1).is_some_and(|s| s.starts_with("scalar "))
                        && (j + 2 >= after.len() || before.get(i + d + 2) == after.get(j + 2));
                    if next_matches || key_then_replace {
                        found = Some(d);
                        break;
                    }
                }
                if found.is_none() && limit == rem_skip {
                    no_sync_j = Some(j);
                }
                found
            };
            if let Some(d) = sync {
                if !in_run {
                    removals += 1;
                }
                in_run = false;
                i += d;
                continue;
            }
            if !in_run && can_replace {
                replaces += 1;
                i += 1;
                j += 1;
                continue;
            }
            if !in_run {
                removals += 1;
                in_run = true;
            }
            i += 1;
        } else if can_replace {
            in_run = false;
            replaces += 1;
            i += 1;
            j += 1;
        } else {
            return false;
        }
    }
    if j != after.len() {
        return false;
    }
    if i < before.len() && !in_run {
        removals += 1;
    }
    removals <= max_removals && replaces <= max_replaces
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;
    use std::error::Error as _;

    use ayaml::Span;

    use super::{
        EditError, EditErrorKind, TextEdit, batch_events_match, batch_events_match_greedy,
        span_order, spliced_output_len, spliced_output_len_pairs, step_spliced_len,
        validate_adjacent_spans, validate_edit_span,
    };

    fn scalar(text: &str) -> String {
        format!("scalar {text:?} Plain None None")
    }

    #[test]
    fn span_order_and_validation_helpers_enforce_bounds_and_ordering() {
        let ins = Span::new(2, 2);
        let rep = Span::new(2, 5);
        assert_eq!(span_order(ins, rep), Ordering::Less);
        assert_eq!(span_order(rep, ins), Ordering::Greater);
        assert_eq!(span_order(rep, rep), Ordering::Equal);

        let sample = "aé🚀\n";
        assert_eq!(validate_edit_span(sample, Span::new(0, 1), 0), Ok(1));
        assert_eq!(validate_edit_span(sample, Span::new(1, 3), 1), Ok(3));
        assert!(validate_edit_span(sample, Span::new(2, 3), 0).is_err());
        assert!(validate_edit_span(sample, Span::new(3, 1), 0).is_err());
        assert!(validate_edit_span(sample, Span::new(0, 99), 0).is_err());
        assert!(validate_edit_span(sample, Span::new(1, 3), 2).is_err());

        assert!(validate_adjacent_spans(ins, rep).is_ok());
        assert!(validate_adjacent_spans(rep, ins).is_err());
        assert!(validate_adjacent_spans(Span::new(1, 3), Span::new(1, 5)).is_err());

        let edits = [
            TextEdit::new(Span::new(0, 1), "xyz"),
            TextEdit::new(Span::new(2, 4), ""),
        ];
        assert_eq!(spliced_output_len(6, &edits), Ok(6));
        assert_eq!(
            spliced_output_len_pairs(6, &[(Span::new(0, 1), 3), (Span::new(2, 4), 0)]),
            Ok(6)
        );
        assert_eq!(step_spliced_len(usize::MAX, Span::new(0, 0), 1), None);
    }

    #[test]
    fn batch_events_match_preserves_pareto_incomparable_states() {
        // Two paths reach `(j=3, skip=2)`:
        // - `(removals=1, replaces=2)` by replacing X->B, B->C and deleting [Y, C]
        // - `(removals=2, replaces=0)` by deleting X and deleting Y
        // Followed by two scalar replacements (`old1->new1`, `old2->new2`), only `(2, 0)` can
        // succeed under `max_removals=2, max_replaces=2`.
        let before = vec![
            scalar("A"),
            scalar("X"),
            scalar("B"),
            scalar("Y"),
            scalar("C"),
            scalar("old1"),
            scalar("old2"),
        ];
        let after = vec![
            scalar("A"),
            scalar("B"),
            scalar("C"),
            scalar("new1"),
            scalar("new2"),
        ];
        assert!(batch_events_match(&before, &after, 2, 2));
        // Also verify symmetric DP branch (`max_replaces < max_removals`, 3 removals + 1 replace).
        let before_sym = vec![
            scalar("A"),
            scalar("X"),
            scalar("B"),
            scalar("Y"),
            scalar("C"),
            scalar("Z"),
            scalar("D"),
            scalar("old1"),
        ];
        let after_sym = vec![
            scalar("A"),
            scalar("B"),
            scalar("C"),
            scalar("D"),
            scalar("new1"),
        ];
        assert!(batch_events_match(&before_sym, &after_sym, 1, 3));
        assert!(!batch_events_match(&before_sym, &after_sym, 0, 3));
        assert!(!batch_events_match(&before_sym, &after_sym, 1, 2));
    }

    #[test]
    fn batch_events_match_greedy_handles_mixed_replace_and_remove_orderings() {
        // 1. Replacement preceding a later removal.
        let before = vec![
            scalar("k0"),
            scalar("old"),
            scalar("k1"),
            scalar("v1"),
            scalar("k2"),
            scalar("v2"),
            scalar("drop"),
            scalar("val"),
        ];
        let after = vec![
            scalar("k0"),
            scalar("new"),
            scalar("k1"),
            scalar("v1"),
            scalar("k2"),
            scalar("v2"),
        ];
        assert!(batch_events_match_greedy(&before, &after, 1, 1));
        assert!(!batch_events_match_greedy(&before, &after, 0, 1));

        // 2. Replacement immediately followed by a removed entry.
        let before_adj = vec![
            scalar("a"),
            scalar("1"),
            scalar("b"),
            scalar("2"),
            scalar("c"),
            scalar("3"),
        ];
        let after_adj = vec![scalar("a"), scalar("10"), scalar("c"), scalar("3")];
        assert!(batch_events_match_greedy(&before_adj, &after_adj, 1, 1));

        // 3. Removed nested collection immediately followed by a replaced scalar.
        let before_nested = vec![
            "mapping Block None None".to_owned(),
            scalar("k"),
            scalar("1"),
            "Discriminant(8)".to_owned(),
            scalar("2"),
            scalar("3"),
            scalar("4"),
        ];
        let after_nested = vec![scalar("20"), scalar("3"), scalar("4")];
        assert!(batch_events_match_greedy(
            &before_nested,
            &after_nested,
            1,
            1
        ));

        // 4. Removed mapping entry immediately followed by a mapping entry with replaced value.
        let before_rem_rep = vec![
            scalar("a"),
            scalar("1"),
            scalar("b"),
            scalar("old"),
            scalar("c"),
            scalar("3"),
        ];
        let after_rem_rep = vec![scalar("b"), scalar("new"), scalar("c"), scalar("3")];
        assert!(batch_events_match_greedy(
            &before_rem_rep,
            &after_rem_rep,
            1,
            1
        ));

        // 5. Rejects structural mismatch when no removal budget remains.
        let bad_after = vec!["mapping Block None None".to_owned(), scalar("3")];
        assert!(!batch_events_match_greedy(&before_adj, &bad_after, 1, 0));
    }

    #[test]
    fn masked_edit_error_ignores_redacted_message_in_equality() {
        let a = EditError::new(EditErrorKind::VerificationFailed, "detail one");
        let b = EditError::new(EditErrorKind::VerificationFailed, "detail two");
        let c = EditError::new(EditErrorKind::Unsupported, "detail one");
        assert_eq!(a.masked(), b.masked());
        assert_ne!(a.masked(), c.masked());
        assert_eq!(a.masked().to_string(), "verification failed");
        assert!(a.masked().source().is_none());
    }

    #[test]
    fn format_inline_scalar_apply_edits_bounded_and_span_gap_verification() {
        use super::{
            EditOp, apply_edits_bounded, collect_canonical_events, format_inline_scalar,
            verify_batch_events_with_spans,
        };
        use ayaml::{Limits, ParseOptions, PathSegment, Value};

        let limits = Limits::default();
        // Flow indicators and ambiguous strings are always quoted inline.
        for tricky in ["a,b", "[x]", "{k}", "true", "null", "42", "a: b"] {
            let formatted = format_inline_scalar(&Value::from(tricky), &limits).expect(tricky);
            assert!(
                formatted.starts_with('\'') || formatted.starts_with('"'),
                "expected quoted inline scalar for {tricky:?}, got {formatted:?}"
            );
        }
        // Oversized scalar fails against `max_output_bytes` (`Unsupported`) and `max_input_bytes`
        // (`Verification` with `InputSizeLimit` source).
        let tight_out = Limits::default().with_max_output_bytes(4);
        assert_eq!(
            format_inline_scalar(&Value::from("1234567890"), &tight_out)
                .expect_err("tight output limit")
                .kind(),
            EditErrorKind::Unsupported
        );
        let tight_in = Limits::default().with_max_input_bytes(4);
        assert_eq!(
            format_inline_scalar(&Value::from("1234567890"), &tight_in)
                .expect_err("tight input limit")
                .kind(),
            EditErrorKind::VerificationFailed
        );

        // `apply_edits_bounded` rejects inputs whose spliced output exceeds `max_input_bytes`
        // before allocating the output string.
        let edit = TextEdit {
            span: Span::new(3, 4),
            replacement: "1234567890".to_owned(),
        };
        let tiny_input = Limits::default().with_max_input_bytes(8);
        let bounded_err = apply_edits_bounded("a: 1\n", &[edit], Some(&tiny_input))
            .expect_err("exceeds max_input_bytes");
        assert_eq!(bounded_err.kind(), EditErrorKind::VerificationFailed);

        // `verify_batch_events_with_spans` anchors untouched spans across replace + remove gaps.
        let before = "a: 1\nb: 2\nc: 3\n";
        let after = "a: 10\nc: 3\n";
        let opts = ParseOptions::new();
        let before_ev = collect_canonical_events(before, &opts).expect("before");
        let after_ev = collect_canonical_events(after, &opts).expect("after");
        let edits = [
            TextEdit {
                span: Span::new(3, 4),
                replacement: "10".to_owned(),
            },
            TextEdit {
                span: Span::new(5, 10),
                replacement: String::new(),
            },
        ];
        let op0 = EditOp::ReplaceScalar {
            path: vec![PathSegment::key("a")],
            value: Value::from(10_u64),
        };
        let op1 = EditOp::RemoveEntry {
            path: vec![PathSegment::key("b")],
        };
        assert!(verify_batch_events_with_spans(
            &before_ev,
            &after_ev,
            &edits,
            &[&op0, &op1],
        ));
    }
}

#[cfg(kani)]
mod kani_proofs {
    use std::cmp::Ordering;

    use ayaml::Span;

    use super::{
        span_order, spliced_output_len_pairs, step_spliced_len, validate_adjacent_spans,
        validate_edit_span,
    };

    fn validate_edit_spans(input: &str, spans: &[Span]) -> Result<(), &'static str> {
        let mut prev_end = 0;
        for &span in spans {
            prev_end = validate_edit_span(input, span, prev_end)?;
        }
        for pair in spans.windows(2) {
            validate_adjacent_spans(pair[0], pair[1])?;
        }
        Ok(())
    }

    #[kani::proof]
    fn verify_span_order_is_strict_total_order() {
        let a = Span::new(kani::any(), kani::any());
        let b = Span::new(kani::any(), kani::any());
        let c = Span::new(kani::any(), kani::any());

        // Reflexive.
        assert_eq!(span_order(a, a), Ordering::Equal);
        // Antisymmetric.
        assert_eq!(span_order(a, b), span_order(b, a).reverse());
        // Transitive.
        let ab = span_order(a, b);
        let bc = span_order(b, c);
        let ac = span_order(a, c);
        if ab != Ordering::Greater && bc != Ordering::Greater {
            assert_ne!(ac, Ordering::Greater);
            if ab == Ordering::Less || bc == Ordering::Less {
                assert_eq!(ac, Ordering::Less);
            }
        }
        // Zero-width insertions always sort before non-empty replacements at the same start offset.
        if a.start() == b.start() && a.is_empty() && !b.is_empty() {
            assert_eq!(span_order(a, b), Ordering::Less);
            assert_eq!(span_order(b, a), Ordering::Greater);
        }
    }

    #[kani::proof]
    fn verify_step_spliced_len_never_overflows() {
        let len: usize = kani::any();
        let start: usize = kani::any();
        let end: usize = kani::any();
        let replacement_len: usize = kani::any();
        let span = Span::new(start, end);
        let result = step_spliced_len(len, span, replacement_len);
        if start <= end {
            let span_len = end - start;
            if span_len <= len {
                assert_eq!(result, (len - span_len).checked_add(replacement_len));
            } else {
                assert_eq!(result, None);
            }
        } else {
            assert_eq!(result, len.checked_add(replacement_len));
        }
    }

    #[kani::proof]
    #[kani::unwind(5)]
    fn verify_apply_edits_span_validation_guarantees_safe_slicing() {
        // Multi-byte UTF-8 input exercising 1-, 2-, 3-, and 4-byte UTF-8 sequences (11 bytes).
        let input = "aé€🚀\n";
        let mut spans = [Span::new(0, 0); 3];
        let mut repl_lens = [0usize; 3];
        for i in 0..3 {
            let start: usize = kani::any();
            let end: usize = kani::any();
            let repl_len: usize = kani::any();
            kani::assume(start <= 13 && end <= 13 && repl_len <= 64);
            spans[i] = Span::new(start, end);
            repl_lens[i] = repl_len;
        }

        if validate_edit_spans(input, &spans).is_ok() {
            // Validated spans are strictly ordered, non-overlapping, on char boundaries, and
            // guarantee that `apply_edits`'s slicing cursor cannot panic and `spliced_output_len`
            // computes the exact output byte length without heap allocation.
            let mut cursor = 0;
            let mut expected_len = 0usize;
            for i in 0..3 {
                let span = spans[i];
                assert!(cursor <= span.start());
                assert!(span.start() <= span.end());
                assert!(span.end() <= input.len());
                assert!(input.is_char_boundary(cursor));
                assert!(input.is_char_boundary(span.start()));
                assert!(input.is_char_boundary(span.end()));
                expected_len += input[cursor..span.start()].len();
                expected_len += repl_lens[i];
                cursor = span.end();
            }
            expected_len += input[cursor..].len();
            let pairs = [
                (spans[0], repl_lens[0]),
                (spans[1], repl_lens[1]),
                (spans[2], repl_lens[2]),
            ];
            assert_eq!(
                spliced_output_len_pairs(input.len(), &pairs),
                Ok(expected_len)
            );
            for pair in spans.windows(2) {
                assert_ne!(span_order(pair[0], pair[1]), Ordering::Greater);
                assert!(pair[0].end() <= pair[1].start());
                if pair[0].start() == pair[1].start() {
                    assert!(pair[0].is_empty() || pair[1].is_empty());
                }
            }
        }
    }
}
