//! Structured errors with a stable kind, a stable machine-readable code, and, when the source
//! provides one, a span.
//!
//! [`ErrorKind`] groups errors into broad pipeline categories (scanner, parser, limits, Serde,
//! emitter, and so on). [`ErrorCode`] identifies the specific failure within those categories
//! using a stable `yaml.<kebab-name>` string (returned by [`ErrorCode::as_str`]). Match on
//! [`Error::code`] or [`Error::kind`] rather than message text; in `libyaml-compat` mode, a few
//! failures remap [`Error::kind`] to match libyaml (for example, depth limits report
//! [`ErrorKind::Serde`]) while keeping their specific [`ErrorCode`] (such as
//! [`ErrorCode::DepthLimit`]).
//!
//! # Masking sensitive documents
//!
//! Normal [`Display`](fmt::Display) and [`Debug`](fmt::Debug) output on [`Error`] (as well as
//! [`Error::message`] and [`std::error::Error::source`]) may include text from the YAML document,
//! such as scalar values, mapping keys and typed error paths, tags, directives, anchor or alias
//! names, or underlying I/O error text. When logging or exposing errors from untrusted or
//! sensitive inputs, format [`Error::masked`] ([`MaskedError`]) instead: its `Display` and
//! `Debug` forms report only the kind, code, and source location (plus byte span and numeric
//! resource-limit metadata in `Debug`) and never include document text or source error strings.

use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;

use crate::limits::LimitKind;
use crate::span::{Location, Span};

/// The longest path a message holds. A longer one keeps its deepest steps, after [`ELIDED`].
const MAX_PATH_BYTES: usize = 1024;
/// The most characters of one key a path shows.
const MAX_KEY_CHARS: usize = 64;
/// What starts a path that lost its outer steps.
const ELIDED: &str = "…";

/// Stable coarse error categories.
///
/// Use [`ErrorKind`] when handling failures by pipeline stage, and [`ErrorCode`] (via
/// [`Error::code`]) when matching a specific failure reason. Multiple [`ErrorCode`] variants can
/// share one [`ErrorKind`], and in `libyaml-compat` mode a few failures remap [`ErrorKind`] to
/// match libyaml while preserving their specific [`ErrorCode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Input is not valid UTF-8 or uses an unsupported encoding.
    InvalidEncoding,
    /// The scanner could not produce a token.
    Scanner,
    /// The parser could not build an event.
    Parser,
    /// An alias is unknown, cyclic, or over budget.
    Alias,
    /// A mapping contains a duplicate semantic key.
    DuplicateKey,
    /// A tag is malformed or unsupported.
    Tag,
    /// A scalar could not be resolved under the active profile.
    Resolution,
    /// A configured resource limit was exceeded.
    Limit,
    /// Composition or value-model construction failed.
    Composition,
    /// Serde deserialization or serialization failed.
    Serde,
    /// The emitter failed.
    Emitter,
    /// I/O failed.
    Io,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::InvalidEncoding => "invalid encoding",
            Self::Scanner => "scanner error",
            Self::Parser => "parser error",
            Self::Alias => "alias error",
            Self::DuplicateKey => "duplicate key",
            Self::Tag => "tag error",
            Self::Resolution => "scalar resolution error",
            Self::Limit => "resource limit",
            Self::Composition => "composition error",
            Self::Serde => "serde error",
            Self::Emitter => "emitter error",
            Self::Io => "I/O error",
        };
        formatter.write_str(name)
    }
}

/// Stable, machine-readable failure codes in `yaml.<kebab-name>` form.
///
/// Every error returned by `ayaml` carries an [`ErrorCode`] accessible through [`Error::code`],
/// alongside its broader [`ErrorKind`] from [`Error::kind`]. Match on [`ErrorCode`] rather than
/// message text. See `docs/error-codes.md` for the full reference table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorCode {
    // -- Encoding --
    /// Input bytes are not valid UTF-8 (`yaml.invalid-utf8`).
    InvalidUtf8,

    // -- Scanner --
    /// Disallowed non-printable character or invalid character boundary (`yaml.non-printable-char`).
    NonPrintableChar,
    /// Byte order mark in a position where YAML forbids one (`yaml.invalid-bom`).
    InvalidBom,
    /// Reserved indicator (`%`, `@`, or `` ` ``) starting a plain scalar
    /// (`yaml.reserved-indicator`).
    ReservedIndicator,
    /// Tab used where YAML forbids tabs as indentation or structural separation
    /// (`yaml.invalid-tab-indentation`).
    InvalidTabIndentation,
    /// Single- or double-quoted scalar was not closed (`yaml.unterminated-quoted-scalar`).
    UnterminatedQuotedScalar,
    /// Quoted scalar continuation line violates indentation or contains a document marker
    /// (`yaml.invalid-quoted-continuation`).
    InvalidQuotedContinuation,
    /// Malformed or unknown escape sequence in a double-quoted scalar (`yaml.invalid-escape`).
    InvalidEscape,
    /// Malformed block scalar header (`yaml.invalid-block-scalar-header`).
    InvalidBlockScalarHeader,
    /// Block scalar indentation violates its parent or leading-empty-line rules
    /// (`yaml.invalid-block-scalar-indentation`).
    InvalidBlockScalarIndentation,
    /// Flow collection continuation is not indented past its enclosing block collection
    /// (`yaml.invalid-flow-indentation`).
    InvalidFlowIndentation,
    /// Document marker (`---` or `...`) inside a flow collection (`yaml.document-marker-in-flow`).
    DocumentMarkerInFlow,
    /// Unseparated `?` before a flow indicator (`yaml.invalid-flow-key`).
    InvalidFlowKey,
    /// `#` comment not separated by whitespace from the preceding token
    /// (`yaml.missing-comment-whitespace`).
    MissingCommentWhitespace,
    /// Malformed `%YAML`, `%TAG`, or unknown directive syntax (`yaml.invalid-directive`).
    InvalidDirective,
    /// Duplicate `%YAML` directive or duplicate `%TAG` handle in one document
    /// (`yaml.duplicate-directive`).
    DuplicateDirective,
    /// Empty or improperly terminated anchor or alias name (`yaml.invalid-anchor`).
    InvalidAnchor,
    /// Malformed tag syntax, verbatim URI, or percent-escape (`yaml.invalid-tag`).
    InvalidTag,
    /// Tag uses a handle not declared by `%TAG` (`yaml.undefined-tag-handle`).
    UndefinedTagHandle,

    // -- Parser --
    /// Directive not followed by a document start marker (`yaml.directive-without-document`).
    DirectiveWithoutDocument,
    /// Invalid node or block collection on a `---` document start line
    /// (`yaml.invalid-document-start`).
    InvalidDocumentStart,
    /// Unexpected non-comment tokens after a `...` document end marker
    /// (`yaml.content-after-document-end`).
    ContentAfterDocumentEnd,
    /// Unexpected content after the document root (`yaml.trailing-content`).
    TrailingContent,
    /// Block sequence or mapping entry is misindented relative to its parent
    /// (`yaml.invalid-block-indentation`).
    InvalidBlockIndentation,
    /// Mapping entry line is missing a `:` separator (`yaml.missing-mapping-colon`).
    MissingMappingColon,
    /// Single-document reader encountered another document (`yaml.trailing-documents`).
    TrailingDocuments,
    /// Unexpected token in block or flow grammar (`yaml.unexpected-token`).
    UnexpectedToken,
    /// Unexpected token or mapping value in a mapping value position
    /// (`yaml.unexpected-mapping-value`).
    UnexpectedMappingValue,
    /// Flow sequence or flow mapping reached end of input without a closing bracket
    /// (`yaml.unterminated-flow-collection`).
    UnterminatedFlowCollection,
    /// Missing separator, unexpected bracket, or empty entry in a flow collection
    /// (`yaml.invalid-flow-collection`).
    InvalidFlowCollection,
    /// Node carries more than one anchor or more than one tag (`yaml.duplicate-node-properties`).
    DuplicateNodeProperties,
    /// Node properties placed before a compact block sequence or explicit key
    /// (`yaml.misplaced-node-properties`).
    MisplacedNodeProperties,
    /// Implicit mapping key spans multiple lines or exceeds 1,024 characters
    /// (`yaml.invalid-implicit-key`).
    InvalidImplicitKey,
    /// Alias node carries a tag or anchor (`yaml.alias-with-properties`).
    AliasWithProperties,

    // -- Composition, aliases, and duplicate keys --
    /// Alias refers to an anchor not defined earlier in the document (`yaml.unknown-anchor`).
    UnknownAnchor,
    /// Alias refers to an enclosing collection that is still open (`yaml.cyclic-alias`).
    CyclicAlias,
    /// Mapping contains two semantically equal keys (`yaml.duplicate-key`).
    DuplicateKey,
    /// Merge key (`<<`) operand is neither a mapping nor a sequence of mappings
    /// (`yaml.invalid-merge-operand`).
    InvalidMergeOperand,
    /// Structural mismatch in an event stream during composition (`yaml.malformed-event-stream`).
    MalformedEventStream,

    // -- Scalar resolution --
    /// Scalar does not match the YAML 1.2 Core Schema `!!int` rule (`yaml.invalid-int-scalar`).
    InvalidIntScalar,
    /// Scalar does not match the YAML 1.2 Core Schema `!!float` rule (`yaml.invalid-float-scalar`).
    InvalidFloatScalar,
    /// Scalar does not match the YAML 1.2 Core Schema `!!bool` rule (`yaml.invalid-bool-scalar`).
    InvalidBoolScalar,
    /// Scalar does not match the YAML 1.2 Core Schema `!!null` rule (`yaml.invalid-null-scalar`).
    InvalidNullScalar,
    /// Generic scalar resolution failure (`yaml.invalid-scalar`).
    InvalidScalar,

    // -- Resource limits --
    /// Generic resource limit exceeded (`yaml.limit-exceeded`).
    LimitExceeded,
    /// Invalid [`Limits`](crate::Limits) configuration (`yaml.invalid-limits`).
    InvalidLimits,
    /// Input byte length exceeded `max_input_bytes` (`yaml.input-size-limit`).
    InputSizeLimit,
    /// Nesting depth exceeded `max_depth` (`yaml.depth-limit`).
    DepthLimit,
    /// Document count exceeded `max_documents` (`yaml.document-count-limit`).
    DocumentCountLimit,
    /// Scalar size exceeded `max_scalar_bytes` (`yaml.scalar-size-limit`).
    ScalarSizeLimit,
    /// Collection entry count exceeded `max_collection_entries`
    /// (`yaml.collection-entries-limit`).
    CollectionEntriesLimit,
    /// Event or token count exceeded `max_events` (`yaml.event-count-limit`).
    EventCountLimit,
    /// Anchor count exceeded `max_anchors` (`yaml.anchor-count-limit`).
    AnchorCountLimit,
    /// Alias dereference count exceeded `max_alias_dereferences`
    /// (`yaml.alias-dereference-limit`).
    AliasDereferenceLimit,
    /// Alias expansion budget exceeded `max_alias_expansion_bytes` (`yaml.alias-expansion-limit`).
    AliasExpansionLimit,
    /// Emitted byte count exceeded `max_output_bytes` (`yaml.output-size-limit`).
    OutputSizeLimit,
    /// Expanded `%TAG` bytes exceeded `max_tag_expansion_bytes` (`yaml.tag-expansion-limit`).
    TagExpansionLimit,

    // -- Serde --
    /// Serde type mismatch (`yaml.invalid-type`).
    InvalidType,
    /// Serde value mismatch (`yaml.invalid-value`).
    InvalidValue,
    /// Serde sequence or tuple length mismatch (`yaml.invalid-length`).
    InvalidLength,
    /// Unknown enum variant name (`yaml.unknown-variant`).
    UnknownVariant,
    /// Unknown struct field name (`yaml.unknown-field`).
    UnknownField,
    /// Required struct field missing (`yaml.missing-field`).
    MissingField,
    /// Duplicate struct field (`yaml.duplicate-field`).
    DuplicateField,
    /// YAML node cannot be decoded as the requested enum representation
    /// (`yaml.invalid-enum-representation`).
    InvalidEnumRepresentation,
    /// Serde map visitor or serializer handled a value before its key (`yaml.map-value-before-key`).
    MapValueBeforeKey,
    /// Custom Serde deserialization or serialization error (`yaml.serde-custom`).
    SerdeCustom,

    // -- Emitter and I/O --
    /// Invalid emitter option such as out-of-range indentation (`yaml.invalid-emitter-options`).
    InvalidEmitterOptions,
    /// Tag cannot be emitted or does not match its scalar's core-schema rule
    /// (`yaml.unsupported-tag`).
    UnsupportedTag,
    /// Event stream handed to an event writer is malformed or has a dangling alias or invalid
    /// directive (`yaml.invalid-emit-event`).
    InvalidEmitEvent,
    /// Underlying `std::io::Write` returned an I/O error (`yaml.io`).
    Io,
}

impl ErrorCode {
    /// Stable `yaml.<kebab-name>` identifier for this error code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidUtf8 => "yaml.invalid-utf8",
            Self::NonPrintableChar => "yaml.non-printable-char",
            Self::InvalidBom => "yaml.invalid-bom",
            Self::ReservedIndicator => "yaml.reserved-indicator",
            Self::InvalidTabIndentation => "yaml.invalid-tab-indentation",
            Self::UnterminatedQuotedScalar => "yaml.unterminated-quoted-scalar",
            Self::InvalidQuotedContinuation => "yaml.invalid-quoted-continuation",
            Self::InvalidEscape => "yaml.invalid-escape",
            Self::InvalidBlockScalarHeader => "yaml.invalid-block-scalar-header",
            Self::InvalidBlockScalarIndentation => "yaml.invalid-block-scalar-indentation",
            Self::InvalidFlowIndentation => "yaml.invalid-flow-indentation",
            Self::DocumentMarkerInFlow => "yaml.document-marker-in-flow",
            Self::InvalidFlowKey => "yaml.invalid-flow-key",
            Self::MissingCommentWhitespace => "yaml.missing-comment-whitespace",
            Self::InvalidDirective => "yaml.invalid-directive",
            Self::DuplicateDirective => "yaml.duplicate-directive",
            Self::InvalidAnchor => "yaml.invalid-anchor",
            Self::InvalidTag => "yaml.invalid-tag",
            Self::UndefinedTagHandle => "yaml.undefined-tag-handle",
            Self::DirectiveWithoutDocument => "yaml.directive-without-document",
            Self::InvalidDocumentStart => "yaml.invalid-document-start",
            Self::ContentAfterDocumentEnd => "yaml.content-after-document-end",
            Self::TrailingContent => "yaml.trailing-content",
            Self::InvalidBlockIndentation => "yaml.invalid-block-indentation",
            Self::MissingMappingColon => "yaml.missing-mapping-colon",
            Self::TrailingDocuments => "yaml.trailing-documents",
            Self::UnexpectedToken => "yaml.unexpected-token",
            Self::UnexpectedMappingValue => "yaml.unexpected-mapping-value",
            Self::UnterminatedFlowCollection => "yaml.unterminated-flow-collection",
            Self::InvalidFlowCollection => "yaml.invalid-flow-collection",
            Self::DuplicateNodeProperties => "yaml.duplicate-node-properties",
            Self::MisplacedNodeProperties => "yaml.misplaced-node-properties",
            Self::InvalidImplicitKey => "yaml.invalid-implicit-key",
            Self::AliasWithProperties => "yaml.alias-with-properties",
            Self::UnknownAnchor => "yaml.unknown-anchor",
            Self::CyclicAlias => "yaml.cyclic-alias",
            Self::DuplicateKey => "yaml.duplicate-key",
            Self::InvalidMergeOperand => "yaml.invalid-merge-operand",
            Self::MalformedEventStream => "yaml.malformed-event-stream",
            Self::InvalidIntScalar => "yaml.invalid-int-scalar",
            Self::InvalidFloatScalar => "yaml.invalid-float-scalar",
            Self::InvalidBoolScalar => "yaml.invalid-bool-scalar",
            Self::InvalidNullScalar => "yaml.invalid-null-scalar",
            Self::InvalidScalar => "yaml.invalid-scalar",
            Self::LimitExceeded => "yaml.limit-exceeded",
            Self::InvalidLimits => "yaml.invalid-limits",
            Self::InputSizeLimit => "yaml.input-size-limit",
            Self::DepthLimit => "yaml.depth-limit",
            Self::DocumentCountLimit => "yaml.document-count-limit",
            Self::ScalarSizeLimit => "yaml.scalar-size-limit",
            Self::CollectionEntriesLimit => "yaml.collection-entries-limit",
            Self::EventCountLimit => "yaml.event-count-limit",
            Self::AnchorCountLimit => "yaml.anchor-count-limit",
            Self::AliasDereferenceLimit => "yaml.alias-dereference-limit",
            Self::AliasExpansionLimit => "yaml.alias-expansion-limit",
            Self::OutputSizeLimit => "yaml.output-size-limit",
            Self::TagExpansionLimit => "yaml.tag-expansion-limit",
            Self::InvalidType => "yaml.invalid-type",
            Self::InvalidValue => "yaml.invalid-value",
            Self::InvalidLength => "yaml.invalid-length",
            Self::UnknownVariant => "yaml.unknown-variant",
            Self::UnknownField => "yaml.unknown-field",
            Self::MissingField => "yaml.missing-field",
            Self::DuplicateField => "yaml.duplicate-field",
            Self::InvalidEnumRepresentation => "yaml.invalid-enum-representation",
            Self::MapValueBeforeKey => "yaml.map-value-before-key",
            Self::SerdeCustom => "yaml.serde-custom",
            Self::InvalidEmitterOptions => "yaml.invalid-emitter-options",
            Self::UnsupportedTag => "yaml.unsupported-tag",
            Self::InvalidEmitEvent => "yaml.invalid-emit-event",
            Self::Io => "yaml.io",
        }
    }

    /// Default code used when [`Error::new`] constructs an error with only an [`ErrorKind`].
    #[must_use]
    pub(crate) const fn default_for_kind(kind: ErrorKind) -> Self {
        match kind {
            ErrorKind::InvalidEncoding => Self::InvalidUtf8,
            ErrorKind::Scanner | ErrorKind::Parser => Self::UnexpectedToken,
            ErrorKind::Alias => Self::UnknownAnchor,
            ErrorKind::DuplicateKey => Self::DuplicateKey,
            ErrorKind::Tag => Self::InvalidTag,
            ErrorKind::Resolution => Self::InvalidScalar,
            ErrorKind::Limit => Self::LimitExceeded,
            ErrorKind::Composition => Self::MalformedEventStream,
            ErrorKind::Serde => Self::SerdeCustom,
            ErrorKind::Emitter => Self::InvalidEmitEvent,
            ErrorKind::Io => Self::Io,
        }
    }

    /// Code corresponding to a structured [`LimitKind`].
    #[must_use]
    pub(crate) const fn from_limit_kind(kind: LimitKind) -> Self {
        match kind {
            LimitKind::InputBytes => Self::InputSizeLimit,
            LimitKind::Depth => Self::DepthLimit,
            LimitKind::Documents => Self::DocumentCountLimit,
            LimitKind::ScalarBytes => Self::ScalarSizeLimit,
            LimitKind::CollectionEntries => Self::CollectionEntriesLimit,
            LimitKind::Events => Self::EventCountLimit,
            LimitKind::Anchors => Self::AnchorCountLimit,
            LimitKind::AliasDereferences => Self::AliasDereferenceLimit,
            LimitKind::AliasExpansion => Self::AliasExpansionLimit,
            LimitKind::OutputBytes => Self::OutputSizeLimit,
            LimitKind::TagExpansion => Self::TagExpansionLimit,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for ErrorCode {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// An ayaml error with a stable category, a stable failure code, optional source span, and, for
/// resource-limit failures, the structured limit that was exceeded.
///
/// Normal [`Display`](fmt::Display) and [`Debug`](fmt::Debug) formatting may include text from
/// the YAML document (such as scalar values, mapping keys, tags, directives, or anchor names).
/// Use [`Error::masked`] when logging or returning errors from sensitive or untrusted documents.
#[derive(Clone, Debug)]
pub struct Error {
    kind: ErrorKind,
    code: ErrorCode,
    /// Whether a `ReservedIndicator` followed a byte-order mark at line start, for compat wording.
    reserved_after_bom: bool,
    /// How much of `message` is the path to what a typed reader rejected, with the `: ` after it.
    /// A path is at most `MAX_PATH_BYTES` and one step long.
    path_len: u16,
    /// How much of the end of `message` is context that the error's location goes before.
    context_len: u16,
    span: Option<Span>,
    location: Option<Location>,
    message: String,
    limit: Option<Box<crate::limits::LimitExceeded>>,
    source: Option<Arc<dyn StdError + Send + Sync + 'static>>,
}

impl PartialEq for Error {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.code == other.code
            && self.path_len == other.path_len
            && self.context_len == other.context_len
            && self.span == other.span
            && self.location == other.location
            && self.message == other.message
            && self.limit == other.limit
            && sources_eq(self.source.as_ref(), other.source.as_ref())
    }
}

impl Eq for Error {}

fn sources_eq(
    left: Option<&Arc<dyn StdError + Send + Sync + 'static>>,
    right: Option<&Arc<dyn StdError + Send + Sync + 'static>>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a.to_string() == b.to_string(),
        _ => false,
    }
}

impl Error {
    /// Create an error with no source location and the default [`ErrorCode`] for `kind`.
    #[must_use]
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self::coded(kind, ErrorCode::default_for_kind(kind), message)
    }

    /// Create an error with an explicit [`ErrorCode`].
    #[must_use]
    pub(crate) fn coded(kind: ErrorKind, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            kind,
            code,
            reserved_after_bom: false,
            path_len: 0,
            context_len: 0,
            span: None,
            location: None,
            message: message.into(),
            limit: None,
            source: None,
        }
    }

    /// Override the stable failure code on this error.
    #[must_use]
    pub const fn with_code(mut self, code: ErrorCode) -> Self {
        self.code = code;
        self
    }

    /// The stable machine-readable failure code.
    ///
    /// Prefer matching on [`ErrorCode`] (or [`ErrorCode::as_str`]) over inspecting
    /// [`Error::message`].
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    /// Mark that a reserved indicator appeared immediately after a byte order mark.
    #[must_use]
    pub(crate) const fn with_reserved_after_bom(mut self) -> Self {
        self.reserved_after_bom = true;
        self
    }

    /// Whether this error is a reserved indicator immediately after a byte order mark.
    #[cfg(feature = "libyaml-compat")]
    #[must_use]
    pub(crate) const fn is_reserved_after_bom(&self) -> bool {
        self.reserved_after_bom
    }

    /// Attach a source span.
    ///
    /// A span carries byte offsets. [`location`](Self::location) is filled in when the error
    /// crosses a reader, which has the input's line index; a manually built
    /// error has no location until [`with_location`](Self::with_location) supplies one. This
    /// clears a location that was set before, which was the earlier span's, so set the location
    /// after the span.
    #[must_use]
    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self.location = None;
        self
    }

    /// Attach the one-based line and column for this error's span.
    #[must_use]
    pub fn with_location(mut self, location: Location) -> Self {
        self.location = Some(location);
        self
    }

    /// Fill in the line and column from this error's byte span, using `input`'s line index.
    ///
    /// This is the boundary step for an error that was built from a byte span deeper in a
    /// reader. It leaves an error that already has a location, or has no span, unchanged.
    #[must_use]
    pub(crate) fn with_input_location(self, input: &str, compat: bool) -> Self {
        if self.location.is_some() || self.span.is_none() {
            return self;
        }
        let index = if compat {
            crate::position::LineStarts::counting_line_boms(input)
        } else {
            crate::position::LineStarts::new(input)
        };
        self.with_location_from(&index, input)
    }

    /// Fill in the line and column from an index the reader already holds.
    #[must_use]
    pub(crate) fn with_location_from(
        mut self,
        index: &crate::position::LineStarts,
        input: &str,
    ) -> Self {
        if self.location.is_some() {
            return self;
        }
        let Some(span) = self.span else {
            return self;
        };
        if let Ok(position) = index.location(input, span.start(), crate::ColumnEncoding::Scalar) {
            self.location = Some(position);
        }
        self
    }

    /// End the message with `context`, which follows the location when the error is displayed.
    #[cold]
    pub(crate) fn with_context(mut self, context: &str) -> Self {
        self.message.push_str(context);
        // The parser's contexts are short. A longer one would stay before the location.
        self.context_len = u16::try_from(context.len()).unwrap_or(0);
        self
    }

    /// Reword the message body, keeping any path prefix and trailing context intact.
    #[cold]
    pub(crate) fn map_message(mut self, update: impl FnOnce(&str) -> String) -> Self {
        let start = self.path_len as usize;
        let end = self.message.len().saturating_sub(self.context_len as usize);
        if start <= end {
            let replacement = update(&self.message[start..end]);
            self.message.replace_range(start..end, &replacement);
        }
        self
    }

    /// Attach a source error.
    #[must_use]
    pub fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.source = Some(Arc::new(source));
        self
    }

    /// Attach the structured limit that was exceeded.
    ///
    /// When the error still holds the generic [`ErrorCode::LimitExceeded`] code (or the code from
    /// an earlier limit attached by this method), this updates the code to match `limit.kind()`.
    #[must_use]
    pub fn with_limit(mut self, limit: crate::limits::LimitExceeded) -> Self {
        if self.code == ErrorCode::LimitExceeded
            || self
                .limit
                .as_deref()
                .is_some_and(|prev| self.code == ErrorCode::from_limit_kind(prev.kind()))
        {
            self.code = ErrorCode::from_limit_kind(limit.kind());
        }
        self.limit = Some(Box::new(limit));
        self
    }

    /// The structured resource-limit failure, when this is a limit error.
    #[must_use]
    pub fn limit(&self) -> Option<crate::limits::LimitExceeded> {
        self.limit.as_deref().copied()
    }

    /// The stable category.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The source span, if any.
    #[must_use]
    pub const fn span(&self) -> Option<Span> {
        self.span
    }

    /// The one-based line/column, if the error has a location.
    #[must_use]
    pub fn location(&self) -> Option<Location> {
        self.location
    }

    /// Return a wrapper whose [`Display`](fmt::Display) and [`Debug`](fmt::Debug) output hold no
    /// text from the YAML document.
    ///
    /// Normal [`Display`](fmt::Display) and [`Debug`](fmt::Debug) output on [`Error`] may include
    /// scalar values, mapping keys, typed error paths, tags, directives, anchor or alias names,
    /// or underlying source error text. Use `masked()` when logging, recording telemetry, or
    /// returning diagnostics from sensitive or untrusted documents.
    ///
    /// ```
    /// use std::collections::HashMap;
    ///
    /// let error = ayaml::from_str::<HashMap<String, u32>>("secret_token: hunter2\n").unwrap_err();
    /// assert!(error.to_string().contains("secret_token"));
    /// assert!(error.to_string().contains("hunter2"));
    ///
    /// let masked = error.masked();
    /// assert!(!masked.to_string().contains("secret_token"));
    /// assert!(!masked.to_string().contains("hunter2"));
    /// assert_eq!(
    ///     masked.to_string(),
    ///     "serde error (yaml.invalid-type) at line 1 column 15"
    /// );
    /// ```
    #[must_use]
    pub const fn masked(&self) -> MaskedError<'_> {
        MaskedError(self)
    }

    /// The human-readable message without location decoration.
    ///
    /// This message may include text from the YAML document. When logging or displaying errors
    /// from sensitive or untrusted inputs, use [`Error::masked`] instead.
    ///
    /// A flow collection's error ends with where the collection opened, as in
    /// `unterminated flow sequence, opened at line 1 column 1`. That is part of the message, and
    /// the error's own location goes before it when the error is displayed.
    ///
    /// When a typed reader rejects something below the document's root, the message starts with
    /// the path to it, as in `tasks[0].limits.cpu: invalid type: string "x", expected u32`. A
    /// field is its name, an element is its index in brackets, and a key that isn't a short name
    /// made of ASCII letters, digits, `_` and `-` is quoted in brackets, as in `ports["80"]`. A
    /// key that is a collection is `[?]`. An enum's content adds the variant's name. A long key,
    /// or a long path, is cut short with `…`.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Say that this error came from inside element `index` of a sequence.
    #[cold]
    pub(crate) fn inside_index(self, index: usize) -> Self {
        self.inside(&format!("[{index}]"))
    }

    /// Say that this error came from inside the value of the mapping key with this text.
    #[cold]
    pub(crate) fn inside_key(self, key: &str) -> Self {
        if is_short_name(key) {
            self.inside(key)
        } else {
            self.inside(&quoted_key(key))
        }
    }

    /// Say that this error came from inside the value of a key that is a collection.
    #[cold]
    pub(crate) fn inside_collection_key(self) -> Self {
        self.inside("[?]")
    }

    /// Put one step in front of the path. Errors travel outwards, so the path grows from its
    /// deepest step, and only the step that reads a value adds to it.
    fn inside(mut self, step: &str) -> Self {
        let path_len = self.path_len as usize;
        if path_len == 0 {
            self.message.insert_str(0, ": ");
            self.message.insert_str(0, step);
            self.path_len = path_bytes(step.len().saturating_add(2));
            return self;
        }
        if self.message.starts_with(ELIDED) {
            return self;
        }
        let (chosen, separator, next_len) =
            resolve_path_step(path_len, self.message.starts_with('['), step);
        self.message.insert_str(0, separator);
        self.message.insert_str(0, chosen);
        self.path_len = next_len;
        self
    }
}

/// Decide the step text, separator, and updated saturating path byte length for one outer path step.
fn resolve_path_step(
    path_len: usize,
    starts_with_bracket: bool,
    step: &str,
) -> (&str, &'static str, u16) {
    let separator = if starts_with_bracket { "" } else { "." };
    let candidate = path_len
        .saturating_add(step.len())
        .saturating_add(separator.len());
    let chosen = if candidate > MAX_PATH_BYTES {
        ELIDED
    } else {
        step
    };
    let next_len = path_bytes(
        path_len
            .saturating_add(chosen.len())
            .saturating_add(separator.len()),
    );
    (chosen, separator, next_len)
}

/// A path's length as kept. One past `MAX_PATH_BYTES` has the same effect as any more: the next
/// step is elided.
fn path_bytes(len: usize) -> u16 {
    u16::try_from(len).unwrap_or(u16::MAX)
}

/// Whether a key can stand in a path as it is, with no doubt about where it ends.
fn is_short_name(key: &str) -> bool {
    let mut chars = key.chars();
    key.len() <= MAX_KEY_CHARS
        && chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|next| next.is_ascii_alphanumeric() || next == '_' || next == '-')
}

/// Format `\u{<hex>}` for a Unicode scalar value into a fixed stack buffer (max 10 ASCII bytes).
const fn format_unicode_escape(ch: char) -> ([u8; 10], usize) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut buf = [0_u8; 10];
    buf[0] = b'\\';
    buf[1] = b'u';
    buf[2] = b'{';
    let mut len = 3;
    let code = ch as u32;
    if code == 0 {
        buf[len] = b'0';
        len += 1;
    } else {
        let mut started = false;
        let mut shift = 20_u32;
        loop {
            let nibble = ((code >> shift) & 0xf) as usize;
            if nibble != 0 || started {
                started = true;
                buf[len] = HEX[nibble];
                len += 1;
            }
            if shift == 0 {
                break;
            }
            shift -= 4;
        }
    }
    buf[len] = b'}';
    len += 1;
    (buf, len)
}

/// Append `\u{<hex>}` for a Unicode scalar value without allocating an intermediate `String`.
fn push_unicode_escape(out: &mut String, ch: char) {
    let (buf, len) = format_unicode_escape(ch);
    for &byte in &buf[..len] {
        out.push(char::from(byte));
    }
}

/// A key in brackets and quotes, with anything that could change how a terminal shows it escaped.
fn quoted_key(key: &str) -> String {
    let mut quoted = String::with_capacity(key.len().min(MAX_KEY_CHARS * 4) + 8);
    push_quoted_key(&mut quoted, key, MAX_KEY_CHARS);
    quoted
}

/// Append `key` in brackets and quotes to `quoted`, keeping at most `max_chars` Unicode scalars.
fn push_quoted_key(quoted: &mut String, key: &str, max_chars: usize) {
    quoted.push_str("[\"");
    let mut chars = key.chars();
    for next in chars.by_ref().take(max_chars) {
        match next {
            '"' | '\\' => {
                quoted.push('\\');
                quoted.push(next);
            }
            _ if hides_text(next) => push_unicode_escape(quoted, next),
            _ => quoted.push(next),
        }
    }
    if chars.next().is_some() {
        quoted.push_str(ELIDED);
    }
    quoted.push_str("\"]");
}

/// Control characters, line separators, the byte order mark, and the marks that reorder text.
fn hides_text(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{61c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
                | '\u{feff}'
        )
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (Some(_span), Some(location)) = (self.span, self.location) else {
            return formatter.write_str(&self.message);
        };
        let (problem, context) = self
            .message
            .split_at(self.message.len() - self.context_len as usize);
        write!(
            formatter,
            "{problem} at line {} column {}{context}",
            location.line(),
            location.column()
        )
    }
}

impl StdError for Error {
    #[allow(
        trivial_casts,
        reason = "Arc<dyn Error + Send + Sync> is widened to the plain error trait object"
    )]
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}

/// A view of an [`Error`] whose [`Display`](fmt::Display) and [`Debug`](fmt::Debug) forms exclude
/// all document text.
///
/// Returned by [`Error::masked`]. Normal [`Error`] formatting may include values, mapping keys,
/// paths, tags, directives, anchor or alias names, or underlying source error messages from the
/// document. `MaskedError` reports only the coarse [`ErrorKind`], stable [`ErrorCode`], and
/// one-based [`Location`] (plus byte [`Span`] and numeric [`LimitExceeded`](crate::LimitExceeded)
/// metadata in `Debug`), and its [`std::error::Error::source`] always returns `None` so an
/// unmasked cause cannot leak through error-chain formatters.
///
/// Use `MaskedError` whenever errors from untrusted or secret-bearing YAML documents are written
/// to logs, metrics, crash reports, or user-visible responses.
#[derive(Clone, Copy)]
pub struct MaskedError<'a>(&'a Error);

impl MaskedError<'_> {
    /// The stable coarse error category.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        self.0.kind
    }

    /// The stable machine-readable failure code.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.0.code
    }

    /// The byte span of the failure, if any.
    #[must_use]
    pub const fn span(&self) -> Option<Span> {
        self.0.span
    }

    /// The one-based line and column of the failure, if any.
    #[must_use]
    pub const fn location(&self) -> Option<Location> {
        self.0.location
    }

    /// The structured resource-limit metadata, when this is a limit error.
    #[must_use]
    pub fn limit(&self) -> Option<crate::limits::LimitExceeded> {
        self.0.limit()
    }
}

impl PartialEq for MaskedError<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.kind == other.0.kind
            && self.0.code == other.0.code
            && self.0.span == other.0.span
            && self.0.location == other.0.location
            && self.0.limit == other.0.limit
    }
}

impl Eq for MaskedError<'_> {}

impl fmt::Display for MaskedError<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.location {
            Some(location) => write!(
                formatter,
                "{} ({}) at line {} column {}",
                self.0.kind,
                self.0.code,
                location.line(),
                location.column()
            ),
            None => write!(formatter, "{} ({})", self.0.kind, self.0.code),
        }
    }
}

impl fmt::Debug for MaskedError<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MaskedError")
            .field("kind", &self.0.kind)
            .field("code", &self.0.code)
            .field("span", &self.0.span)
            .field("location", &self.0.location)
            .field("limit", &self.0.limit)
            .finish()
    }
}

impl StdError for MaskedError<'_> {}

/// Minimal `serde::de::Error` adapter that formats Serde's default error messages verbatim so
/// `Error` can assign specific [`ErrorCode`] variants without altering wording.
#[derive(Debug)]
struct SerdeMessage(String);

impl fmt::Display for SerdeMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl StdError for SerdeMessage {}

impl serde::de::Error for SerdeMessage {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self(message.to_string())
    }
}

impl serde::de::Error for Error {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self::coded(
            ErrorKind::Serde,
            ErrorCode::SerdeCustom,
            message.to_string(),
        )
    }

    fn invalid_type(unexp: serde::de::Unexpected<'_>, exp: &dyn serde::de::Expected) -> Self {
        Self::coded(
            ErrorKind::Serde,
            ErrorCode::InvalidType,
            SerdeMessage::invalid_type(unexp, exp).0,
        )
    }

    fn invalid_value(unexp: serde::de::Unexpected<'_>, exp: &dyn serde::de::Expected) -> Self {
        Self::coded(
            ErrorKind::Serde,
            ErrorCode::InvalidValue,
            SerdeMessage::invalid_value(unexp, exp).0,
        )
    }

    fn invalid_length(len: usize, exp: &dyn serde::de::Expected) -> Self {
        Self::coded(
            ErrorKind::Serde,
            ErrorCode::InvalidLength,
            SerdeMessage::invalid_length(len, exp).0,
        )
    }

    fn unknown_variant(variant: &str, expected: &'static [&'static str]) -> Self {
        Self::coded(
            ErrorKind::Serde,
            ErrorCode::UnknownVariant,
            SerdeMessage::unknown_variant(variant, expected).0,
        )
    }

    fn unknown_field(field: &str, expected: &'static [&'static str]) -> Self {
        Self::coded(
            ErrorKind::Serde,
            ErrorCode::UnknownField,
            SerdeMessage::unknown_field(field, expected).0,
        )
    }

    fn missing_field(field: &'static str) -> Self {
        Self::coded(
            ErrorKind::Serde,
            ErrorCode::MissingField,
            SerdeMessage::missing_field(field).0,
        )
    }

    fn duplicate_field(field: &'static str) -> Self {
        Self::coded(
            ErrorKind::Serde,
            ErrorCode::DuplicateField,
            SerdeMessage::duplicate_field(field).0,
        )
    }
}

impl serde::ser::Error for Error {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self::coded(
            ErrorKind::Serde,
            ErrorCode::SerdeCustom,
            message.to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, ErrorCode, ErrorKind};
    use crate::limits::{LimitExceeded, LimitKind};
    use crate::span::{Location, Span};
    use std::collections::HashSet;
    use std::error::Error as _;

    const ALL_CODES: &[ErrorCode] = &[
        ErrorCode::InvalidUtf8,
        ErrorCode::NonPrintableChar,
        ErrorCode::InvalidBom,
        ErrorCode::ReservedIndicator,
        ErrorCode::InvalidTabIndentation,
        ErrorCode::UnterminatedQuotedScalar,
        ErrorCode::InvalidQuotedContinuation,
        ErrorCode::InvalidEscape,
        ErrorCode::InvalidBlockScalarHeader,
        ErrorCode::InvalidBlockScalarIndentation,
        ErrorCode::InvalidFlowIndentation,
        ErrorCode::DocumentMarkerInFlow,
        ErrorCode::InvalidFlowKey,
        ErrorCode::MissingCommentWhitespace,
        ErrorCode::InvalidDirective,
        ErrorCode::DuplicateDirective,
        ErrorCode::InvalidAnchor,
        ErrorCode::InvalidTag,
        ErrorCode::UndefinedTagHandle,
        ErrorCode::DirectiveWithoutDocument,
        ErrorCode::InvalidDocumentStart,
        ErrorCode::ContentAfterDocumentEnd,
        ErrorCode::TrailingContent,
        ErrorCode::InvalidBlockIndentation,
        ErrorCode::MissingMappingColon,
        ErrorCode::TrailingDocuments,
        ErrorCode::UnexpectedToken,
        ErrorCode::UnexpectedMappingValue,
        ErrorCode::UnterminatedFlowCollection,
        ErrorCode::InvalidFlowCollection,
        ErrorCode::DuplicateNodeProperties,
        ErrorCode::MisplacedNodeProperties,
        ErrorCode::InvalidImplicitKey,
        ErrorCode::AliasWithProperties,
        ErrorCode::UnknownAnchor,
        ErrorCode::CyclicAlias,
        ErrorCode::DuplicateKey,
        ErrorCode::InvalidMergeOperand,
        ErrorCode::MalformedEventStream,
        ErrorCode::InvalidIntScalar,
        ErrorCode::InvalidFloatScalar,
        ErrorCode::InvalidBoolScalar,
        ErrorCode::InvalidNullScalar,
        ErrorCode::InvalidScalar,
        ErrorCode::LimitExceeded,
        ErrorCode::InvalidLimits,
        ErrorCode::InputSizeLimit,
        ErrorCode::DepthLimit,
        ErrorCode::DocumentCountLimit,
        ErrorCode::ScalarSizeLimit,
        ErrorCode::CollectionEntriesLimit,
        ErrorCode::EventCountLimit,
        ErrorCode::AnchorCountLimit,
        ErrorCode::AliasDereferenceLimit,
        ErrorCode::AliasExpansionLimit,
        ErrorCode::OutputSizeLimit,
        ErrorCode::TagExpansionLimit,
        ErrorCode::InvalidType,
        ErrorCode::InvalidValue,
        ErrorCode::InvalidLength,
        ErrorCode::UnknownVariant,
        ErrorCode::UnknownField,
        ErrorCode::MissingField,
        ErrorCode::DuplicateField,
        ErrorCode::InvalidEnumRepresentation,
        ErrorCode::MapValueBeforeKey,
        ErrorCode::SerdeCustom,
        ErrorCode::InvalidEmitterOptions,
        ErrorCode::UnsupportedTag,
        ErrorCode::InvalidEmitEvent,
        ErrorCode::Io,
    ];

    /// Exhaustive match so adding an `ErrorCode` variant without adding it to `ALL_CODES` fails
    /// to compile.
    fn is_listed(code: ErrorCode) -> bool {
        match code {
            ErrorCode::InvalidUtf8
            | ErrorCode::NonPrintableChar
            | ErrorCode::InvalidBom
            | ErrorCode::ReservedIndicator
            | ErrorCode::InvalidTabIndentation
            | ErrorCode::UnterminatedQuotedScalar
            | ErrorCode::InvalidQuotedContinuation
            | ErrorCode::InvalidEscape
            | ErrorCode::InvalidBlockScalarHeader
            | ErrorCode::InvalidBlockScalarIndentation
            | ErrorCode::InvalidFlowIndentation
            | ErrorCode::DocumentMarkerInFlow
            | ErrorCode::InvalidFlowKey
            | ErrorCode::MissingCommentWhitespace
            | ErrorCode::InvalidDirective
            | ErrorCode::DuplicateDirective
            | ErrorCode::InvalidAnchor
            | ErrorCode::InvalidTag
            | ErrorCode::UndefinedTagHandle
            | ErrorCode::DirectiveWithoutDocument
            | ErrorCode::InvalidDocumentStart
            | ErrorCode::ContentAfterDocumentEnd
            | ErrorCode::TrailingContent
            | ErrorCode::InvalidBlockIndentation
            | ErrorCode::MissingMappingColon
            | ErrorCode::TrailingDocuments
            | ErrorCode::UnexpectedToken
            | ErrorCode::UnexpectedMappingValue
            | ErrorCode::UnterminatedFlowCollection
            | ErrorCode::InvalidFlowCollection
            | ErrorCode::DuplicateNodeProperties
            | ErrorCode::MisplacedNodeProperties
            | ErrorCode::InvalidImplicitKey
            | ErrorCode::AliasWithProperties
            | ErrorCode::UnknownAnchor
            | ErrorCode::CyclicAlias
            | ErrorCode::DuplicateKey
            | ErrorCode::InvalidMergeOperand
            | ErrorCode::MalformedEventStream
            | ErrorCode::InvalidIntScalar
            | ErrorCode::InvalidFloatScalar
            | ErrorCode::InvalidBoolScalar
            | ErrorCode::InvalidNullScalar
            | ErrorCode::InvalidScalar
            | ErrorCode::LimitExceeded
            | ErrorCode::InvalidLimits
            | ErrorCode::InputSizeLimit
            | ErrorCode::DepthLimit
            | ErrorCode::DocumentCountLimit
            | ErrorCode::ScalarSizeLimit
            | ErrorCode::CollectionEntriesLimit
            | ErrorCode::EventCountLimit
            | ErrorCode::AnchorCountLimit
            | ErrorCode::AliasDereferenceLimit
            | ErrorCode::AliasExpansionLimit
            | ErrorCode::OutputSizeLimit
            | ErrorCode::TagExpansionLimit
            | ErrorCode::InvalidType
            | ErrorCode::InvalidValue
            | ErrorCode::InvalidLength
            | ErrorCode::UnknownVariant
            | ErrorCode::UnknownField
            | ErrorCode::MissingField
            | ErrorCode::DuplicateField
            | ErrorCode::InvalidEnumRepresentation
            | ErrorCode::MapValueBeforeKey
            | ErrorCode::SerdeCustom
            | ErrorCode::InvalidEmitterOptions
            | ErrorCode::UnsupportedTag
            | ErrorCode::InvalidEmitEvent
            | ErrorCode::Io => ALL_CODES.contains(&code),
        }
    }

    #[test]
    fn every_error_code_has_a_unique_yaml_kebab_name() {
        let mut seen = HashSet::new();
        for &code in ALL_CODES {
            assert!(is_listed(code));
            let text = code.as_str();
            assert_eq!(code.to_string(), text);
            assert_eq!(code.as_ref(), text);
            let rest = text
                .strip_prefix("yaml.")
                .unwrap_or_else(|| panic!("missing yaml. prefix: {text}"));
            assert!(!rest.is_empty(), "empty suffix: {text}");
            assert!(
                !rest.starts_with('-') && !rest.ends_with('-') && !rest.contains("--"),
                "{text}"
            );
            assert!(
                rest.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "not kebab-case: {text}"
            );
            assert!(seen.insert(text), "duplicate error code string: {text}");
        }
    }

    #[test]
    fn every_error_code_is_documented_in_reference_page() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = manifest.join("../..");
        let path = root.join("docs/error-codes.md");
        if !path.is_file() {
            assert!(
                manifest.join("Cargo.toml.orig").is_file(),
                "{} is neither in the repository nor an unpacked package",
                manifest.display()
            );
            return;
        }
        let doc = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        let mut documented = HashSet::new();
        for line in doc.lines() {
            let trimmed = line.trim();
            if !trimmed.starts_with('|') {
                continue;
            }
            let cells: Vec<&str> = trimmed.split('|').map(str::trim).collect();
            if cells.len() >= 4
                && let Some(variant) = cells[1].strip_prefix('`').and_then(|s| s.strip_suffix('`'))
                && let Some(code_str) = cells[2].strip_prefix('`').and_then(|s| s.strip_suffix('`'))
                && code_str.starts_with("yaml.")
            {
                assert!(
                    documented.insert((variant.to_owned(), code_str.to_owned())),
                    "duplicate code in docs/error-codes.md: ({variant}, {code_str})"
                );
            }
        }
        let expected: HashSet<(String, String)> = ALL_CODES
            .iter()
            .map(|code| (format!("{code:?}"), code.as_str().to_owned()))
            .collect();
        let missing: Vec<_> = expected.difference(&documented).collect();
        let extra: Vec<_> = documented.difference(&expected).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "docs/error-codes.md drift: missing={missing:?}, extra={extra:?}"
        );
    }

    #[test]
    fn error_supports_clone_and_partial_eq() {
        let a = Error::new(ErrorKind::Parser, "bad")
            .with_code(ErrorCode::TrailingDocuments)
            .with_span(Span::new(1, 3))
            .with_location(Location::new(1, 2));
        let b = a.clone();
        assert_eq!(a, b);
        assert_ne!(a, a.clone().with_code(ErrorCode::UnexpectedToken));

        let with_limit = Error::new(ErrorKind::Limit, "too deep").with_limit(LimitExceeded::new(
            LimitKind::Depth,
            4,
            5,
        ));
        assert_eq!(with_limit.code(), ErrorCode::DepthLimit);
        assert_eq!(with_limit, with_limit.clone());
        let replaced_limit = with_limit.with_limit(LimitExceeded::new(LimitKind::InputBytes, 8, 9));
        assert_eq!(replaced_limit.code(), ErrorCode::InputSizeLimit);
        let explicit_code = Error::new(ErrorKind::Limit, "custom")
            .with_code(ErrorCode::InvalidLimits)
            .with_limit(LimitExceeded::new(LimitKind::Depth, 4, 5));
        assert_eq!(explicit_code.code(), ErrorCode::InvalidLimits);

        let s1 = Error::new(ErrorKind::Io, "write failed")
            .with_source(std::io::Error::other("broken pipe"));
        let s2 = Error::new(ErrorKind::Io, "write failed")
            .with_source(std::io::Error::other("broken pipe"));
        let s3 = Error::new(ErrorKind::Io, "write failed")
            .with_source(std::io::Error::other("disk full"));
        assert_eq!(s1, s1.clone());
        assert_eq!(s1, s2);
        assert_ne!(s1, s3);
        assert_ne!(s1, Error::new(ErrorKind::Io, "write failed"));
    }

    #[test]
    fn default_codes_for_every_error_kind_are_pinned() {
        let defaults = [
            (ErrorKind::InvalidEncoding, ErrorCode::InvalidUtf8),
            (ErrorKind::Scanner, ErrorCode::UnexpectedToken),
            (ErrorKind::Parser, ErrorCode::UnexpectedToken),
            (ErrorKind::Alias, ErrorCode::UnknownAnchor),
            (ErrorKind::DuplicateKey, ErrorCode::DuplicateKey),
            (ErrorKind::Tag, ErrorCode::InvalidTag),
            (ErrorKind::Resolution, ErrorCode::InvalidScalar),
            (ErrorKind::Limit, ErrorCode::LimitExceeded),
            (ErrorKind::Composition, ErrorCode::MalformedEventStream),
            (ErrorKind::Serde, ErrorCode::SerdeCustom),
            (ErrorKind::Emitter, ErrorCode::InvalidEmitEvent),
            (ErrorKind::Io, ErrorCode::Io),
        ];
        for (kind, expected) in defaults {
            assert_eq!(Error::new(kind, "msg").code(), expected, "{kind:?}");
        }
    }

    #[test]
    fn kind_and_message_are_exposed() {
        let error = Error::new(ErrorKind::Parser, "unexpected token");
        assert_eq!(error.kind(), ErrorKind::Parser);
        assert_eq!(error.code(), ErrorCode::UnexpectedToken);
        assert_eq!(error.message(), "unexpected token");
        assert_eq!(error.to_string(), "unexpected token");
        assert!(error.span().is_none());
        assert!(error.location().is_none());
    }

    #[test]
    fn span_and_location_are_one_based() {
        let span = Span::new(4, 5);
        let error = Error::new(ErrorKind::Scanner, "bad token")
            .with_span(span)
            .with_location(Location::new(2, 3));
        assert_eq!(error.span(), Some(span));
        let location = error.location().expect("location");
        assert_eq!((location.line(), location.column()), (2, 3));
    }

    #[test]
    fn source_is_chained() {
        let inner = std::io::Error::other("inner");
        let error = Error::new(ErrorKind::Io, "outer").with_source(inner);
        assert!(error.source().is_some());
    }

    #[test]
    fn a_path_joins_its_steps_and_stands_before_the_message() {
        let error = Error::new(ErrorKind::Serde, "bad")
            .inside_key("c")
            .inside_index(2)
            .inside_key("b")
            .inside_key("a b")
            .inside_index(0);
        assert_eq!(error.message(), "[0][\"a b\"].b[2].c: bad");
        assert_eq!(error.kind(), ErrorKind::Serde);
        let error = Error::new(ErrorKind::Serde, "bad").inside_collection_key();
        assert_eq!(error.message(), "[?]: bad");
    }

    #[test]
    fn a_key_is_bare_only_if_it_is_a_short_name() {
        for bare in ["a", "_a", "runs-on", "a_1", &"k".repeat(64)] {
            let error = Error::new(ErrorKind::Serde, "bad").inside_key(bare);
            assert_eq!(error.message(), format!("{bare}: bad"));
        }
        for (key, shown) in [
            ("", "[\"\"]"),
            ("80", "[\"80\"]"),
            ("-a", "[\"-a\"]"),
            ("a.b", "[\"a.b\"]"),
            ("名前", "[\"名前\"]"),
            ("a\"b\\c", "[\"a\\\"b\\\\c\"]"),
            ("a\nb\u{1b}", "[\"a\\u{a}b\\u{1b}\"]"),
            ("\u{85}\u{2028}\u{2029}", "[\"\\u{85}\\u{2028}\\u{2029}\"]"),
            ("\u{200e}\u{200f}", "[\"\\u{200e}\\u{200f}\"]"),
            ("a\u{61c}b", "[\"a\\u{61c}b\"]"),
            (
                "\u{202e}\u{2066}\u{feff}",
                "[\"\\u{202e}\\u{2066}\\u{feff}\"]",
            ),
        ] {
            let error = Error::new(ErrorKind::Serde, "bad").inside_key(key);
            assert_eq!(error.message(), format!("{shown}: bad"));
        }
    }

    #[test]
    fn a_long_key_and_a_long_path_are_cut_short() {
        let error = Error::new(ErrorKind::Serde, "bad").inside_key(&"é".repeat(65));
        assert_eq!(error.message(), format!("[\"{}…\"]: bad", "é".repeat(64)));

        let mut error = Error::new(ErrorKind::Serde, "bad");
        let name = "k".repeat(64);
        for _ in 0..100 {
            error = error.inside_key(&name);
        }
        let (path, rest) = error.message().split_once(": ").expect("a path");
        assert_eq!(rest, "bad");
        assert!(path.starts_with("….k"), "{path}");
        assert!(path.len() <= 1024 + 4, "{}", path.len());
        let before = error.message().to_owned();
        let error = error.inside_key("outer").inside_index(1);
        assert_eq!(error.message(), before);

        let mut error = Error::new(ErrorKind::Serde, "bad").inside_index(7);
        for _ in 0..200 {
            error = error.inside_index(1_234_567);
        }
        assert!(error.message().starts_with("…["), "{}", error.message());
    }

    /// Text that reads like a location, in a key or in the message, never stands for the error's.
    #[test]
    fn a_location_in_the_text_does_not_stand_for_the_error_s() {
        let span = Span::new(4, 5);
        let error = Error::new(ErrorKind::Serde, "bad")
            .with_span(span)
            .with_location(Location::new(2, 3))
            .inside_key("at line 9");
        assert_eq!(error.to_string(), "[\"at line 9\"]: bad at line 2 column 3");
        let error = Error::new(ErrorKind::Serde, "invalid type: string \"at line 9\"")
            .with_span(span)
            .with_location(Location::new(2, 3))
            .inside_key("a");
        assert_eq!(
            error.to_string(),
            "a: invalid type: string \"at line 9\" at line 2 column 3"
        );
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn an_error_stays_small() {
        assert_eq!(size_of::<Error>(), 104);
        assert_eq!(size_of::<Result<crate::Value, Error>>(), 104);
    }

    /// A path's steps are as long as `inside` lets them be, and a context too long to keep its
    /// length still shows.
    #[test]
    fn a_long_path_or_context_keeps_its_text() {
        let long = "k".repeat(70_000);
        let error = Error::new(ErrorKind::Serde, "bad").inside_key(&long);
        assert!(
            error.message().ends_with("…\"]: bad"),
            "{}",
            error.message()
        );
        let error = (0..2_000).fold(Error::new(ErrorKind::Serde, "bad"), |error, index| {
            error.inside_index(index)
        });
        let start: String = error.message().chars().take(40).collect();
        assert!(error.message().starts_with('…'), "{start}");
        assert!(error.message().len() < 2_048);
        let span = Span::new(4, 5);
        let context = format!(", {}", "c".repeat(70_000));
        let error = Error::new(ErrorKind::Parser, "bad")
            .with_context(&context)
            .with_span(span)
            .with_location(Location::new(2, 3));
        assert_eq!(
            error.to_string(),
            format!("bad{context} at line 2 column 3")
        );
    }

    #[test]
    fn the_location_goes_before_the_context() {
        let span = Span::new(4, 5);
        let error = Error::new(ErrorKind::Parser, "unterminated flow sequence")
            .with_context(", opened at line 1 column 1")
            .with_span(span)
            .with_location(Location::new(2, 3));
        assert_eq!(
            error.message(),
            "unterminated flow sequence, opened at line 1 column 1"
        );
        assert_eq!(
            error.to_string(),
            "unterminated flow sequence at line 2 column 3, opened at line 1 column 1"
        );
        let error = error.inside_index(0).inside_key("k");
        assert_eq!(
            error.to_string(),
            "k[0]: unterminated flow sequence at line 2 column 3, opened at line 1 column 1"
        );
        // With no location, the context follows the problem.
        let error = Error::new(ErrorKind::Parser, "unterminated flow sequence")
            .with_context(", opened at line 1 column 1");
        assert_eq!(
            error.to_string(),
            "unterminated flow sequence, opened at line 1 column 1"
        );
    }

    #[test]
    fn categories_display_stably() {
        assert_eq!(ErrorKind::DuplicateKey.to_string(), "duplicate key");
        assert_eq!(ErrorKind::Limit.to_string(), "resource limit");
    }

    #[test]
    fn masked_error_shows_kind_code_and_location_without_message_or_source() {
        fn assert_masked_traits<
            T: Copy
                + Clone
                + PartialEq
                + Eq
                + Send
                + Sync
                + std::fmt::Display
                + std::fmt::Debug
                + std::error::Error,
        >(
            _: T,
        ) {
        }

        let secret = "TOP_SECRET_VALUE_991";
        let error = Error::new(
            ErrorKind::Serde,
            format!("invalid type: string \"{secret}\""),
        )
        .with_code(ErrorCode::InvalidType)
        .with_span(Span::new(10, 20))
        .with_location(Location::new(3, 5))
        .with_context(", in flow mapping")
        .inside_key("secret_key_991")
        .with_source(std::io::Error::other(format!("inner {secret}")));

        assert!(error.to_string().contains(secret));
        assert!(error.to_string().contains("secret_key_991"));

        let masked = error.masked();
        let display = masked.to_string();
        let debug = format!("{masked:?}");
        let pretty_debug = format!("{masked:#?}");

        assert_eq!(
            display,
            "serde error (yaml.invalid-type) at line 3 column 5"
        );
        assert!(!display.contains(secret), "leaked in display: {display}");
        assert!(
            !display.contains("secret_key_991"),
            "leaked key in display: {display}"
        );
        assert!(!debug.contains(secret), "leaked in debug: {debug}");
        assert!(
            !debug.contains("secret_key_991"),
            "leaked key in debug: {debug}"
        );
        assert!(
            !pretty_debug.contains(secret) && !pretty_debug.contains("secret_key_991"),
            "leaked in pretty debug: {pretty_debug}"
        );
        assert!(debug.contains("Serde") && debug.contains("InvalidType"));
        assert!(masked.source().is_none());
        assert_eq!(masked.kind(), ErrorKind::Serde);
        assert_eq!(masked.code(), ErrorCode::InvalidType);
        assert_eq!(masked.span(), Some(Span::new(10, 20)));
        assert_eq!(masked.location(), Some(Location::new(3, 5)));
        assert_eq!(masked.limit(), None);

        let no_loc = Error::new(ErrorKind::Io, format!("write {secret}"))
            .with_source(std::io::Error::other(secret));
        assert_eq!(no_loc.masked().to_string(), "I/O error (yaml.io)");
        assert!(!format!("{:?}", no_loc.masked()).contains(secret));
        assert!(no_loc.masked().source().is_none());

        let with_limit = Error::new(ErrorKind::Limit, format!("too deep {secret}"))
            .with_limit(LimitExceeded::new(LimitKind::Depth, 4, 5));
        assert_eq!(
            with_limit.masked().to_string(),
            "resource limit (yaml.depth-limit)"
        );
        assert_eq!(
            with_limit.masked().limit(),
            Some(LimitExceeded::new(LimitKind::Depth, 4, 5))
        );
        assert!(!format!("{:?}", with_limit.masked()).contains(secret));

        let loc_without_span = Error::new(ErrorKind::Parser, format!("bad {secret}"))
            .with_location(Location::new(2, 3));
        assert_eq!(
            loc_without_span.masked().to_string(),
            "parser error (yaml.unexpected-token) at line 2 column 3"
        );
        assert_eq!(loc_without_span.masked().span(), None);
        assert_eq!(
            loc_without_span.masked().location(),
            Some(Location::new(2, 3))
        );

        let span_without_loc =
            Error::new(ErrorKind::Scanner, format!("bad {secret}")).with_span(Span::new(1, 5));
        assert_eq!(
            span_without_loc.masked().to_string(),
            "scanner error (yaml.unexpected-token)"
        );
        assert_eq!(span_without_loc.masked().span(), Some(Span::new(1, 5)));
        assert_eq!(span_without_loc.masked().location(), None);
        assert!(!format!("{:?}", span_without_loc.masked()).contains(secret));
        assert!(!format!("{:#?}", span_without_loc.masked()).contains(secret));

        assert_masked_traits(masked);
    }

    #[test]
    fn masked_error_equality_ignores_redacted_message_and_source() {
        let a = Error::new(ErrorKind::Serde, "secret A")
            .with_code(ErrorCode::InvalidType)
            .with_span(Span::new(0, 4))
            .with_location(Location::new(1, 1))
            .with_source(std::io::Error::other("cause A"));
        let b = Error::new(ErrorKind::Serde, "secret B")
            .with_code(ErrorCode::InvalidType)
            .with_span(Span::new(0, 4))
            .with_location(Location::new(1, 1))
            .with_source(std::io::Error::other("cause B"));
        let c = Error::new(ErrorKind::Serde, "secret A")
            .with_code(ErrorCode::InvalidValue)
            .with_span(Span::new(0, 4))
            .with_location(Location::new(1, 1));

        assert_ne!(a, b);
        assert_eq!(a.masked(), b.masked());
        assert_ne!(a.masked(), c.masked());
    }

    #[test]
    fn path_truncation_and_key_quoting_preserve_utf8_and_bounds() {
        assert_eq!(super::path_bytes(0), 0);
        assert_eq!(super::path_bytes(usize::MAX), u16::MAX);

        // Long 4-byte emoji key truncates at MAX_KEY_CHARS without splitting UTF-8.
        let emoji_key = "🚀".repeat(super::MAX_KEY_CHARS + 10);
        let quoted = super::quoted_key(&emoji_key);
        assert!(quoted.starts_with("[\""));
        assert!(
            quoted.ends_with(&format!("{}]", super::ELIDED))
                || quoted.ends_with(&format!("{}\"]", super::ELIDED))
        );
        assert!(quoted.ends_with(&format!("{}\"]", super::ELIDED)));
        assert_eq!(
            quoted.chars().filter(|&c| c == '🚀').count(),
            super::MAX_KEY_CHARS
        );

        // Control chars and BiDi override characters are always escaped in quoted keys.
        let hostile = "a\u{0000}\u{202e}\u{feff}\"\\z";
        let quoted_hostile = super::quoted_key(hostile);
        assert!(quoted_hostile.chars().all(|c| !super::hides_text(c)));
        assert!(quoted_hostile.contains("\\u{0}"));
        assert!(quoted_hostile.contains("\\u{202e}"));
        assert!(quoted_hostile.contains("\\u{feff}"));
        assert!(quoted_hostile.contains("\\\""));
        assert!(quoted_hostile.contains("\\\\"));

        // Repeated `inside_key` calls cap path length and become idempotent once elided.
        let mut err = Error::new(ErrorKind::Serde, "bad value");
        for i in 0..40 {
            err = err.inside_key(&format!("segment_with_long_name_{i:02}"));
        }
        assert!(err.message().starts_with(super::ELIDED));
        let once_elided = err.message().to_owned();
        let after_more = err.inside_key("outer_ignored");
        assert_eq!(after_more.message(), once_elided);
    }
}

#[cfg(kani)]
mod kani_proofs {
    use super::{
        ELIDED, Error, ErrorKind, MAX_KEY_CHARS, MAX_PATH_BYTES, format_unicode_escape, hides_text,
        is_short_name, path_bytes, push_quoted_key, resolve_path_step,
    };

    #[kani::proof]
    fn verify_path_bytes_saturates() {
        let len: usize = kani::any();
        let kept = path_bytes(len);
        if len <= usize::from(u16::MAX) {
            assert_eq!(usize::from(kept), len);
        } else {
            assert_eq!(kept, u16::MAX);
        }
    }

    #[kani::proof]
    #[kani::unwind(6)]
    fn verify_is_short_name_ascii_sound() {
        let bytes: [u8; 4] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= bytes.len());
        // Restrict to ASCII bytes to keep UTF-8 validation symbolic-free; non-ASCII is also
        // rejected immediately by `is_short_name`.
        let mut i = 0;
        while i < len {
            kani::assume(bytes[i] < 0x80);
            i += 1;
        }
        let text = std::str::from_utf8(&bytes[..len]).unwrap();
        if is_short_name(text) {
            assert!(!text.is_empty());
            assert!(text.len() <= MAX_KEY_CHARS);
            let first = bytes[0];
            assert!(first.is_ascii_alphabetic() || first == b'_');
            let mut j = 1;
            while j < len {
                let b = bytes[j];
                assert!(b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
                j += 1;
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(12)]
    fn verify_quoted_key_escapes_control_and_valid_utf8() {
        // Exhaustively verify `hides_text` and `format_unicode_escape` over ALL Unicode scalar
        // values U+0000..=U+10FFFF: escaped output is pure printable ASCII without hidden chars.
        let ch: char = kani::any();
        if ch.is_control()
            || matches!(
                ch,
                '\u{61c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
                    | '\u{feff}'
            )
        {
            assert!(hides_text(ch));
            let (buf, len) = format_unicode_escape(ch);
            assert!(len >= 5 && len <= 10);
            assert!(buf[0] == b'\\' && buf[1] == b'u' && buf[2] == b'{');
            assert!(buf[len - 1] == b'}');
            let mut i = 3;
            while i + 1 < len {
                let b = buf[i];
                assert!(b.is_ascii_hexdigit());
                assert!(!hides_text(char::from(b)));
                i += 1;
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(6)]
    fn verify_quoted_key_truncation_keeps_utf8_boundaries() {
        // Symbolically verify across EVERY Unicode scalar U+0000..=U+10FFFF that taking at most
        // `cap` scalars from a multi-byte UTF-8 key always stops on a valid char boundary.
        let mut buf = [0_u8; 5];
        let ch: char = kani::any();
        let first_len = ch.encode_utf8(&mut buf).len();
        buf[first_len] = b'z';
        let text = std::str::from_utf8(&buf[..first_len + 1]).unwrap();
        let cap: usize = kani::any();
        kani::assume(cap <= 2);
        let mut kept_bytes = 0_usize;
        let mut kept_chars = 0_usize;
        for c in text.chars().take(cap) {
            kept_bytes += c.len_utf8();
            kept_chars += 1;
        }
        assert!(kept_bytes <= text.len());
        assert!(text.is_char_boundary(kept_bytes));
        assert_eq!(kept_chars, cap);
        if cap < 2 {
            assert!(kept_bytes < text.len());
        } else {
            assert_eq!(kept_bytes, text.len());
        }
    }

    #[kani::proof]
    #[kani::unwind(24)]
    fn verify_push_quoted_key_escapes_and_truncates() {
        // Verify real `push_quoted_key` across 1-, 2-, 3-, and 4-byte UTF-8 scalars, quotes,
        // backslashes, control chars, BiDi overrides, and BOM across concrete truncation caps.
        for (sample, cap, expect_elided) in [
            ("é\u{202e}🚀", 2_usize, true),
            ("é\u{202e}🚀", 3_usize, false),
            ("a\"\\\0\u{feff}", 4_usize, true),
        ] {
            let mut quoted = String::with_capacity(64);
            push_quoted_key(&mut quoted, sample, cap);
            let qbytes = quoted.as_bytes();
            assert!(qbytes.len() >= 4);
            assert_eq!(qbytes[0], b'[');
            assert_eq!(qbytes[1], b'"');
            assert_eq!(qbytes[qbytes.len() - 2], b'"');
            assert_eq!(qbytes[qbytes.len() - 1], b']');
            if expect_elided {
                let tail = &qbytes[qbytes.len() - 2 - ELIDED.len()..qbytes.len() - 2];
                assert_eq!(tail, ELIDED.as_bytes());
            }
            for out_ch in quoted.chars() {
                assert!(!hides_text(out_ch));
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(8)]
    fn verify_inside_path_truncation_bounds_and_idempotence() {
        // 1. Exhaustively verify `resolve_path_step` over all path lengths (0..=usize::MAX),
        //    bracket/non-bracket separators, and representative step lengths: zero overflow,
        //    exact transition to `ELIDED` past `MAX_PATH_BYTES`, and saturating `u16` length.
        let path_len: usize = kani::any();
        let starts_with_bracket: bool = kani::any();
        let step_choice: u8 = kani::any();
        let step = match step_choice % 4 {
            0 => "",
            1 => "a",
            2 => "[0]",
            _ => "segment_step",
        };
        let (chosen, sep, next_len) = resolve_path_step(path_len, starts_with_bracket, step);
        if starts_with_bracket {
            assert_eq!(sep.len(), 0);
        } else {
            assert_eq!(sep.as_bytes(), b".");
        }
        let candidate = path_len
            .saturating_add(step.len())
            .saturating_add(sep.len());
        if candidate > MAX_PATH_BYTES {
            assert_eq!(chosen.as_ptr(), ELIDED.as_ptr());
            assert_eq!(chosen.len(), ELIDED.len());
        } else {
            assert_eq!(chosen.as_ptr(), step.as_ptr());
            assert_eq!(chosen.len(), step.len());
            assert!(usize::from(next_len) <= MAX_PATH_BYTES);
        }
        let expected_len = path_bytes(
            path_len
                .saturating_add(chosen.len())
                .saturating_add(sep.len()),
        );
        assert_eq!(next_len, expected_len);

        // 2. Verify `Error::inside` directly at concrete lengths straddling `MAX_PATH_BYTES`,
        //    using pre-sized small buffers so `String::insert_str` never hits symbolic heap
        //    reallocation: covers initial attachment, exact elision transition, and idempotence
        //    of subsequent `inside` calls once elided.
        let mut fresh_msg = String::with_capacity(16);
        fresh_msg.push_str("bad");
        let fresh = Error::new(ErrorKind::Serde, fresh_msg).inside("k");
        assert_eq!(fresh.path_len, 3);
        assert_eq!(fresh.message.as_bytes(), b"k: bad");

        for cap in (MAX_PATH_BYTES - 4)..=MAX_PATH_BYTES {
            let mut msg = String::with_capacity(48);
            msg.push_str("a: b");
            let mut err = Error::new(ErrorKind::Serde, msg);
            err.path_len = cap as u16;
            // Step "xy" + "." = 3 bytes: fits when cap <= MAX_PATH_BYTES - 3, elides above.
            let stepped = err.inside("xy");
            if cap + 3 > MAX_PATH_BYTES {
                assert!(stepped.message.starts_with(ELIDED));
                let elided_len = stepped.message.len();
                let elided_path_len = stepped.path_len;
                let again = stepped.inside("outer_ignored");
                assert_eq!(again.message.len(), elided_len);
                assert_eq!(again.path_len, elided_path_len);
                assert!(again.message.starts_with(ELIDED));
            } else {
                assert!(stepped.message.starts_with("xy."));
                assert_eq!(usize::from(stepped.path_len), cap + 3);
                // One more 10-byte step pushes every remaining cap over MAX_PATH_BYTES and elides.
                let over = stepped.inside("over_limit");
                assert!(over.message.starts_with(ELIDED));
                let elided_len = over.message.len();
                let elided_path_len = over.path_len;
                let again = over.inside("outer_ignored");
                assert_eq!(again.message.len(), elided_len);
                assert_eq!(again.path_len, elided_path_len);
            }
        }
    }
}
