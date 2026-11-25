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

/// Ordering for [`TextEdit`]: ascending start offset, with insertions ordered before
/// replacements starting at the same byte offset, then ascending end offset.
fn edit_order(a: &TextEdit, b: &TextEdit) -> Ordering {
    a.span
        .start()
        .cmp(&b.span.start())
        .then_with(|| b.is_insertion().cmp(&a.is_insertion()))
        .then_with(|| a.span.end().cmp(&b.span.end()))
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct EditErrorInner {
    kind: EditErrorKind,
    message: String,
    source: Option<ayaml::Error>,
}

/// An error returned when an edit cannot be constructed or verified.
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

    /// The coarse error category.
    #[must_use]
    pub fn kind(&self) -> EditErrorKind {
        self.0.kind
    }

    /// Human-readable description of why the edit failed or was refused.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.0.message
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

/// Apply sorted, non-overlapping [`TextEdit`]s to `input`.
///
/// Two edits may share a start offset only if the earlier one is an insertion (empty span).
///
/// # Errors
///
/// Returns [`EditErrorKind::InvalidEdit`] if any edit is out of bounds, lands inside a UTF-8
/// code point, is out of order, or overlaps another edit.
pub fn apply_edits(input: &str, edits: &[TextEdit]) -> Result<String, EditError> {
    let mut prev_end = 0;
    for edit in edits {
        let start = edit.span.start();
        let end = edit.span.end();
        if start > end || end > input.len() {
            return Err(EditError::new(
                EditErrorKind::InvalidEdit,
                "text edit span is out of bounds",
            ));
        }
        if !input.is_char_boundary(start) || !input.is_char_boundary(end) {
            return Err(EditError::new(
                EditErrorKind::InvalidEdit,
                "text edit span does not lie on UTF-8 character boundaries",
            ));
        }
        if start < prev_end {
            return Err(EditError::new(
                EditErrorKind::InvalidEdit,
                "text edits must be sorted and non-overlapping",
            ));
        }
        prev_end = end;
    }
    for pair in edits.windows(2) {
        if edit_order(&pair[0], &pair[1]) == Ordering::Greater {
            return Err(EditError::new(
                EditErrorKind::InvalidEdit,
                "text edits must be sorted with insertions before replacements at the same offset",
            ));
        }
        if pair[0].span.start() == pair[1].span.start()
            && !pair[0].is_insertion()
            && !pair[1].is_insertion()
        {
            return Err(EditError::new(
                EditErrorKind::InvalidEdit,
                "overlapping text edits at the same offset",
            ));
        }
    }

    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    for edit in edits {
        output.push_str(&input[cursor..edit.span.start()]);
        output.push_str(&edit.replacement);
        cursor = edit.span.end();
    }
    output.push_str(&input[cursor..]);
    Ok(output)
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
    let edits: Vec<TextEdit> = planned.iter().map(|(edit, _)| edit.clone()).collect();
    let output = apply_edits(input, &edits)?;

    // Apply expected-value updates from highest source offset downward so removing an earlier
    // sequence item does not shift the indices of later planned operations.
    let mut expected_value = original_value;
    for (_, op) in planned.iter().rev() {
        expected_value = match op {
            EditOp::ReplaceScalar { path, value } => replace_value_at(&expected_value, path, value)
                .ok_or_else(|| {
                    EditError::new(
                        EditErrorKind::NotFound,
                        "target path does not exist in value tree",
                    )
                })?,
            EditOp::RemoveEntry { path } => {
                remove_value_at(&expected_value, path).ok_or_else(|| {
                    EditError::new(
                        EditErrorKind::Unsupported,
                        "cannot remove target entry from value tree",
                    )
                })?
            }
        };
    }

    // Post-edit verification: re-parse value and events and compare.
    let reparsed_value = parse_value_with(&output, &options.parse).map_err(|error| {
        EditError::new(
            EditErrorKind::VerificationFailed,
            format!("edited document failed to re-parse: {error}"),
        )
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
        EditError::new(
            EditErrorKind::VerificationFailed,
            format!("edited document failed event verification: {error}"),
        )
    })?;

    let ok = if ops.len() == 1 {
        match &ops[0] {
            EditOp::ReplaceScalar { .. } => one_scalar_changed(&before_events, &after_events),
            EditOp::RemoveEntry { .. } => one_run_removed(&before_events, &after_events),
        }
    } else {
        let replace_count = ops
            .iter()
            .filter(|op| matches!(op, EditOp::ReplaceScalar { .. }))
            .count();
        let remove_count = ops
            .iter()
            .filter(|op| matches!(op, EditOp::RemoveEntry { .. }))
            .count();
        batch_events_match(&before_events, &after_events, replace_count, remove_count)
    };
    if !ok {
        return Err(EditError::new(
            EditErrorKind::VerificationFailed,
            "edited document changed unexpected events outside the target span",
        ));
    }

    Ok(VerifiedEdit { output, edits })
}

/// Parse `input`'s events and refuse documents with multiple documents, anchors, aliases,
/// tags, block scalars, or opt-in merge-key expansion.
fn inspect_document(input: &str, options: &ParseOptions) -> Result<Vec<String>, EditError> {
    if options.merge_keys {
        return Err(EditError::new(
            EditErrorKind::Unsupported,
            "documents with merge-key expansion enabled are not supported",
        ));
    }
    let mut doc_count = 0usize;
    let mut canonical = Vec::new();
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
        canonical.push(canonical_event(&event));
    }
    if doc_count != 1 {
        return Err(EditError::new(
            EditErrorKind::Unsupported,
            "expected exactly one YAML document",
        ));
    }
    Ok(canonical)
}

fn collect_canonical_events(input: &str, options: &ParseOptions) -> Result<Vec<String>, EditError> {
    let stream = parse_events_with(input, options).map_err(EditError::parse)?;
    let mut out = Vec::new();
    for item in stream {
        let event = item.map_err(EditError::parse)?;
        out.push(canonical_event(&event));
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

    let formatted = format_inline_scalar(replacement, limits)?;
    Ok(TextEdit::new(span, formatted))
}

fn format_inline_scalar(value: &Value, limits: &Limits) -> Result<String, EditError> {
    // Emit inside a 1-element flow sequence so `ayaml`'s emitter uses flow-context quoting
    // (quoting strings with `,`, `[`, `]`, `{`, `}` as well as block-unsafe indicators).
    let emit_options = EmitOptions::new()
        .with_flow(true)
        .with_limits(limits.clone());
    let wrapped = Value::Sequence(vec![value.clone()]);
    let emitted = to_string_value_with(&wrapped, &emit_options).map_err(|error| {
        EditError::new(
            EditErrorKind::Unsupported,
            format!("could not format replacement scalar: {error}"),
        )
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

fn replace_value_at(value: &Value, path: &[PathSegment], replacement: &Value) -> Option<Value> {
    let Some((first, rest)) = path.split_first() else {
        return Some(replacement.clone());
    };
    let mut updated = value.clone();
    match (&mut updated, first) {
        (Value::Mapping(mapping), PathSegment::Key(key)) => {
            let inner = mapping.get_mut(key)?;
            *inner = replace_value_at(inner, rest, replacement)?;
        }
        (Value::Sequence(items), PathSegment::Index(idx)) => {
            let inner = items.get_mut(*idx)?;
            *inner = replace_value_at(inner, rest, replacement)?;
        }
        _ => return None,
    }
    Some(updated)
}

fn remove_value_at(value: &Value, path: &[PathSegment]) -> Option<Value> {
    let (first, rest) = path.split_first()?;
    let mut updated = value.clone();
    match (&mut updated, first) {
        (Value::Mapping(mapping), PathSegment::Key(key)) => {
            if rest.is_empty() {
                mapping.shift_remove(key)?;
                if mapping.is_empty() {
                    return None;
                }
            } else {
                let inner = mapping.get_mut(key)?;
                *inner = remove_value_at(inner, rest)?;
            }
        }
        (Value::Sequence(items), PathSegment::Index(idx)) => {
            if rest.is_empty() {
                if *idx >= items.len() || items.len() == 1 {
                    return None;
                }
                items.remove(*idx);
            } else {
                let inner = items.get_mut(*idx)?;
                *inner = remove_value_at(inner, rest)?;
            }
        }
        _ => return None,
    }
    Some(updated)
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
    const INF: (usize, usize) = (usize::MAX, usize::MAX);
    if after.len() > before.len() {
        return false;
    }
    if max_removals == 0 {
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
                if diffs > max_replaces {
                    return false;
                }
            }
        }
        return true;
    }
    // Dynamic alignment tracking (before_idx, after_idx, in_removed_run) -> min (replaces, runs).
    // Even for large documents, `after.len() <= before.len()` and `max_removals` is small;
    // keep the state bounded by matching common prefix and suffix first.
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
    // Bound DP work for very large multi-op batches; fall back to greedy alignment if the
    // mismatch window would exceed 1M DP cells.
    if (a.len() + 1).saturating_mul(cols) > 1_000_000 {
        return batch_events_match_greedy(b, a, max_replaces, max_removals);
    }
    // Rolling 2-row DP over (skip in 0..=max_skip, in_run in 0..=1) storing min
    // (removals_used, replaces_used) with `usize::MAX` meaning unreachable.
    let mut curr = vec![[INF; 2]; cols];
    let mut next = vec![[INF; 2]; cols];
    curr[0][0] = (0, 0);
    for j in 0..=a.len() {
        next.fill([INF; 2]);
        for skip in 0..=max_skip {
            let i = j + skip;
            let best_not_in = curr[skip][0];
            let best_in = curr[skip][1];
            // Option 1: delete `b[i]` (advance `skip` by 1, enter/stay in removed run).
            if skip < max_skip && i < b.len() {
                if best_not_in.0 < max_removals {
                    let cand = (best_not_in.0 + 1, best_not_in.1);
                    if cand < curr[skip + 1][1] {
                        curr[skip + 1][1] = cand;
                    }
                }
                if best_in != INF && best_in < curr[skip + 1][1] {
                    curr[skip + 1][1] = best_in;
                }
            }
            // Option 2: match or replace `b[i]` with `a[j]` (advance `j` by 1, exit removed run).
            if j < a.len() && i < b.len() {
                let best = best_not_in.min(best_in);
                if best != INF {
                    if b[i] == a[j] {
                        if best < next[skip][0] {
                            next[skip][0] = best;
                        }
                    } else if b[i].starts_with("scalar ")
                        && a[j].starts_with("scalar ")
                        && best.1 < max_replaces
                    {
                        let cand = (best.0, best.1 + 1);
                        if cand < next[skip][0] {
                            next[skip][0] = cand;
                        }
                    }
                }
            }
        }
        if j < a.len() {
            std::mem::swap(&mut curr, &mut next);
        }
    }
    let final_cost = curr[max_skip][0].min(curr[max_skip][1]);
    final_cost.0 <= max_removals && final_cost.1 <= max_replaces
}

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
    while i < before.len() && j < after.len() {
        if before[i] == after[j] {
            in_run = false;
            i += 1;
            j += 1;
            continue;
        }
        let rem_skip = (before.len() - i).saturating_sub(after.len() - j);
        if rem_skip > 0 && (in_run || removals < max_removals) {
            if !in_run {
                removals += 1;
                in_run = true;
            }
            i += 1;
        } else if before[i].starts_with("scalar ")
            && after[j].starts_with("scalar ")
            && replaces < max_replaces
        {
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
