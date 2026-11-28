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
    /// Underlying `std::io::Read` or `std::io::Write` returned an I/O error (`yaml.io`).
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

#[derive(Clone)]
struct ErrorInner {
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
    limit: Option<crate::limits::LimitExceeded>,
    source: Option<Arc<dyn StdError + Send + Sync + 'static>>,
}

/// An ayaml error with a stable category, a stable failure code, optional source span, and, for
/// resource-limit failures, the structured limit that was exceeded.
///
/// Normal [`Display`](fmt::Display) and [`Debug`](fmt::Debug) formatting may include text from
/// the YAML document (such as scalar values, mapping keys, tags, directives, or anchor names).
/// Use [`Error::masked`] when logging or returning errors from sensitive or untrusted documents.
#[derive(Clone)]
pub struct Error {
    inner: Box<ErrorInner>,
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Error")
            .field("kind", &self.inner.kind)
            .field("code", &self.inner.code)
            .field("reserved_after_bom", &self.inner.reserved_after_bom)
            .field("path_len", &self.inner.path_len)
            .field("context_len", &self.inner.context_len)
            .field("span", &self.inner.span)
            .field("location", &self.inner.location)
            .field("message", &self.inner.message)
            .field("limit", &self.inner.limit)
            .field("source", &self.inner.source)
            .finish()
    }
}

impl PartialEq for Error {
    fn eq(&self, other: &Self) -> bool {
        self.inner.kind == other.inner.kind
            && self.inner.code == other.inner.code
            && self.inner.path_len == other.inner.path_len
            && self.inner.context_len == other.inner.context_len
            && self.inner.span == other.inner.span
            && self.inner.location == other.inner.location
            && self.inner.message == other.inner.message
            && self.inner.limit == other.inner.limit
            && sources_eq(self.inner.source.as_ref(), other.inner.source.as_ref())
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
    #[cold]
    #[must_use]
    pub(crate) fn coded(kind: ErrorKind, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            inner: Box::new(ErrorInner {
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
            }),
        }
    }

    /// Override the stable failure code on this error.
    #[must_use]
    pub fn with_code(mut self, code: ErrorCode) -> Self {
        self.inner.code = code;
        self
    }

    /// The stable machine-readable failure code.
    ///
    /// Prefer matching on [`ErrorCode`] (or [`ErrorCode::as_str`]) over inspecting
    /// [`Error::message`].
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        self.inner.code
    }

    /// Mark that a reserved indicator appeared immediately after a byte order mark.
    #[must_use]
    pub(crate) fn with_reserved_after_bom(mut self) -> Self {
        self.inner.reserved_after_bom = true;
        self
    }

    /// Whether this error is a reserved indicator immediately after a byte order mark.
    #[cfg(feature = "libyaml-compat")]
    #[must_use]
    pub(crate) fn is_reserved_after_bom(&self) -> bool {
        self.inner.reserved_after_bom
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
        self.inner.span = Some(span);
        self.inner.location = None;
        self
    }

    /// Attach the one-based line and column for this error.
    #[must_use]
    pub fn with_location(mut self, location: Location) -> Self {
        self.inner.location = Some(location);
        self
    }

    /// Fill in the line and column from this error's byte span, using `input` directly without
    /// allocating a line index.
    ///
    /// This is the boundary step for an error that was built from a byte span deeper in a
    /// reader. It leaves an error that already has a location, or has no span, unchanged.
    #[cold]
    #[must_use]
    pub(crate) fn with_input_location(mut self, input: &str, compat: bool) -> Self {
        if self.inner.location.is_some() {
            return self;
        }
        let Some(span) = self.inner.span else {
            return self;
        };
        if let Some(position) = crate::position::location_at(input, span.start(), compat) {
            self.inner.location = Some(position);
        }
        self
    }

    /// End the message with `context`, which follows the location when the error is displayed.
    #[cold]
    pub(crate) fn with_context(mut self, context: &str) -> Self {
        self.inner.message.push_str(context);
        // The parser's contexts are short. A longer one would stay before the location.
        self.inner.context_len = u16::try_from(context.len()).unwrap_or(0);
        self
    }

    /// Reword the message body, keeping any path prefix and trailing context intact.
    #[cold]
    pub(crate) fn map_message(mut self, update: impl FnOnce(&str) -> String) -> Self {
        let start = self.inner.path_len as usize;
        let end = self
            .inner
            .message
            .len()
            .saturating_sub(self.inner.context_len as usize);
        if start <= end {
            let replacement = update(&self.inner.message[start..end]);
            self.inner.message.replace_range(start..end, &replacement);
        }
        self
    }

    /// Attach a source error.
    #[must_use]
    pub fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.inner.source = Some(Arc::new(source));
        self
    }

    /// Attach the structured limit that was exceeded.
    ///
    /// When the error still holds the generic [`ErrorCode::LimitExceeded`] code (or the code from
    /// an earlier limit attached by this method), this updates the code to match `limit.kind()`.
    #[must_use]
    pub fn with_limit(mut self, limit: crate::limits::LimitExceeded) -> Self {
        if self.inner.code == ErrorCode::LimitExceeded
            || self
                .inner
                .limit
                .is_some_and(|prev| self.inner.code == ErrorCode::from_limit_kind(prev.kind()))
        {
            self.inner.code = ErrorCode::from_limit_kind(limit.kind());
        }
        self.inner.limit = Some(limit);
        self
    }

    /// The structured resource-limit failure, when this is a limit error.
    #[must_use]
    pub fn limit(&self) -> Option<crate::limits::LimitExceeded> {
        self.inner.limit
    }

    /// The stable category.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.inner.kind
    }

    /// The source span, if any.
    #[must_use]
    pub fn span(&self) -> Option<Span> {
        self.inner.span
    }

    /// The one-based line/column, if the error has a location.
    #[must_use]
    pub fn location(&self) -> Option<Location> {
        self.inner.location
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
        &self.inner.message
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
        prepend_path_step(&mut self.inner.message, &mut self.inner.path_len, step);
        self
    }
}

/// Prepend one path step onto `message` and update `path_len`, eliding once past `MAX_PATH_BYTES`.
fn prepend_path_step(message: &mut String, path_len: &mut u16, step: &str) {
    let current_len = *path_len as usize;
    if current_len == 0 {
        message.insert_str(0, ": ");
        message.insert_str(0, step);
        *path_len = path_bytes(step.len().saturating_add(2));
        return;
    }
    if message.starts_with(ELIDED) {
        return;
    }
    let (chosen, separator, next_len) =
        resolve_path_step(current_len, message.starts_with('['), step);
    message.insert_str(0, separator);
    message.insert_str(0, chosen);
    *path_len = next_len;
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
fn write_unicode_escape(out: &mut impl fmt::Write, ch: char) {
    let (buf, len) = format_unicode_escape(ch);
    for &byte in &buf[..len] {
        let _ = out.write_char(char::from(byte));
    }
}

/// A key in brackets and quotes, with anything that could change how a terminal shows it escaped.
fn quoted_key(key: &str) -> String {
    let mut quoted = String::with_capacity(key.len().min(MAX_KEY_CHARS * 4) + 8);
    write_quoted_key(&mut quoted, key, MAX_KEY_CHARS);
    quoted
}

/// Append a single character of a quoted error-path key, escaping quotes, backslashes, and hidden
/// characters.
fn write_quoted_key_char(quoted: &mut impl fmt::Write, next: char) {
    match next {
        '"' | '\\' => {
            let _ = quoted.write_char('\\');
            let _ = quoted.write_char(next);
        }
        _ if hides_text(next) => write_unicode_escape(quoted, next),
        _ => {
            let _ = quoted.write_char(next);
        }
    }
}

/// Append `key` in brackets and quotes to `quoted`, keeping at most `max_chars` Unicode scalars.
fn write_quoted_key(quoted: &mut impl fmt::Write, key: &str, max_chars: usize) {
    let _ = quoted.write_str("[\"");
    let mut chars = key.chars();
    for next in chars.by_ref().take(max_chars) {
        write_quoted_key_char(quoted, next);
    }
    if chars.next().is_some() {
        let _ = quoted.write_str(ELIDED);
    }
    let _ = quoted.write_str("\"]");
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
        let Some(location) = self.inner.location else {
            return formatter.write_str(&self.inner.message);
        };
        let (problem, context) = self.inner.message.split_at(
            self.inner
                .message
                .len()
                .saturating_sub(self.inner.context_len as usize),
        );
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
        self.inner
            .source
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
    pub fn kind(&self) -> ErrorKind {
        self.0.inner.kind
    }

    /// The stable machine-readable failure code.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        self.0.inner.code
    }

    /// The byte span of the failure, if any.
    #[must_use]
    pub fn span(&self) -> Option<Span> {
        self.0.inner.span
    }

    /// The one-based line and column of the failure, if any.
    #[must_use]
    pub fn location(&self) -> Option<Location> {
        self.0.inner.location
    }

    /// The structured resource-limit metadata, when this is a limit error.
    #[must_use]
    pub fn limit(&self) -> Option<crate::limits::LimitExceeded> {
        self.0.limit()
    }
}

impl PartialEq for MaskedError<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.inner.kind == other.0.inner.kind
            && self.0.inner.code == other.0.inner.code
            && self.0.inner.span == other.0.inner.span
            && self.0.inner.location == other.0.inner.location
            && self.0.inner.limit == other.0.inner.limit
    }
}

impl Eq for MaskedError<'_> {}

impl fmt::Display for MaskedError<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.inner.location {
            Some(location) => write!(
                formatter,
                "{} ({}) at line {} column {}",
                self.0.inner.kind,
                self.0.inner.code,
                location.line(),
                location.column()
            ),
            None => write!(formatter, "{} ({})", self.0.inner.kind, self.0.inner.code),
        }
    }
}

impl fmt::Debug for MaskedError<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MaskedError")
            .field("kind", &self.0.inner.kind)
            .field("code", &self.0.inner.code)
            .field("span", &self.0.inner.span)
            .field("location", &self.0.inner.location)
            .field("limit", &self.0.inner.limit)
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
mod tests;

#[cfg(kani)]
mod kani_proofs {
    use super::{
        ELIDED, MAX_KEY_CHARS, MAX_PATH_BYTES, format_unicode_escape, hides_text, is_short_name,
        path_bytes, prepend_path_step, resolve_path_step, write_quoted_key, write_quoted_key_char,
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

    struct StackBuf {
        bytes: [u8; 64],
        len: usize,
    }
    impl StackBuf {
        const fn new() -> Self {
            Self {
                bytes: [0; 64],
                len: 0,
            }
        }
        fn push_byte(&mut self, b: u8) {
            assert!(self.len < self.bytes.len());
            self.bytes[self.len] = b;
            self.len += 1;
        }
        fn as_slice(&self) -> &[u8] {
            &self.bytes[..self.len]
        }
    }
    impl core::fmt::Write for StackBuf {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for &b in s.as_bytes() {
                self.push_byte(b);
            }
            Ok(())
        }
        fn write_char(&mut self, c: char) -> core::fmt::Result {
            let cp = c as u32;
            if cp < 0x80 {
                self.push_byte(cp as u8);
            } else if cp < 0x800 {
                self.push_byte((0xC0 | (cp >> 6)) as u8);
                self.push_byte((0x80 | (cp & 0x3F)) as u8);
            } else if cp < 0x1_0000 {
                self.push_byte((0xE0 | (cp >> 12)) as u8);
                self.push_byte((0x80 | ((cp >> 6) & 0x3F)) as u8);
                self.push_byte((0x80 | (cp & 0x3F)) as u8);
            } else {
                self.push_byte((0xF0 | (cp >> 18)) as u8);
                self.push_byte((0x80 | ((cp >> 12) & 0x3F)) as u8);
                self.push_byte((0x80 | ((cp >> 6) & 0x3F)) as u8);
                self.push_byte((0x80 | (cp & 0x3F)) as u8);
            }
            Ok(())
        }
    }

    #[kani::proof]
    #[kani::unwind(12)]
    fn verify_write_quoted_key_char_all_unicode() {
        // Exhaustive symbolic proof over ALL 1,112,064 Unicode scalars through `write_quoted_key_char`.
        let ch: char = kani::any();
        let mut single = StackBuf::new();
        write_quoted_key_char(&mut single, ch);
        let sbytes = single.as_slice();
        assert!(!sbytes.is_empty() && sbytes.len() <= 10);
        if matches!(ch, '"' | '\\') {
            assert_eq!(sbytes, &[b'\\', ch as u8]);
        } else if hides_text(ch) {
            assert!(sbytes.len() >= 5 && sbytes.starts_with(b"\\u{") && sbytes.ends_with(b"}"));
            for &b in sbytes {
                assert!((0x20..0x7f).contains(&b));
            }
        } else {
            assert_eq!(sbytes.len(), ch.len_utf8());
        }
    }

    #[kani::proof]
    #[kani::unwind(36)]
    fn verify_push_quoted_key_escapes_and_truncates() {
        // Concrete table across empty, ASCII, quotes/backslashes, controls, BiDi, BOM, and
        // 2-/3-/4-byte UTF-8 at boundary caps `0..=4` keeps write indices concrete (paired with
        // the exhaustive symbolic `verify_write_quoted_key_char_all_unicode` proof above).
        for (sample, cap, expect_elided) in [
            ("", 0_usize, false),
            ("a\"\\\0", 0, true),
            ("a\"\\\0", 2, true),
            ("a\"\\\0", 4, false),
            ("é\u{202e}🚀", 2, true),
            ("é\u{202e}🚀", 3, false),
            ("\u{feff}€\n\x7f", 3, true),
            ("\u{feff}€\n\x7f", 4, false),
        ] {
            let mut out = StackBuf::new();
            write_quoted_key(&mut out, sample, cap);
            let qbytes = out.as_slice();
            assert!(qbytes.len() >= 4 && qbytes.starts_with(b"[\"") && qbytes.ends_with(b"\"]"));
            if expect_elided {
                assert_eq!(
                    &qbytes[qbytes.len() - 2 - ELIDED.len()..qbytes.len() - 2],
                    ELIDED.as_bytes()
                );
            } else {
                assert!(!qbytes[..qbytes.len() - 2].ends_with(ELIDED.as_bytes()));
            }
            for &b in qbytes {
                assert!(b >= 0x20 && b != 0x7f);
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

        // 2. Verify `prepend_path_step` (used directly by `Error::inside`) at concrete lengths
        //    straddling `MAX_PATH_BYTES`, using pre-sized small buffers without boxing `ErrorInner`
        //    on Kani's symbolic heap: covers initial attachment, exact elision transition, and
        //    idempotence of subsequent steps once elided.
        let mut fresh_msg = String::with_capacity(16);
        fresh_msg.push_str("bad");
        let mut fresh_path_len = 0_u16;
        prepend_path_step(&mut fresh_msg, &mut fresh_path_len, "k");
        assert_eq!(fresh_path_len, 3);
        assert_eq!(fresh_msg.as_bytes(), b"k: bad");

        for cap in (MAX_PATH_BYTES - 4)..=MAX_PATH_BYTES {
            let mut msg = String::with_capacity(48);
            msg.push_str("a: b");
            let mut cur_path_len = cap as u16;
            // Step "xy" + "." = 3 bytes: fits when cap <= MAX_PATH_BYTES - 3, elides above.
            prepend_path_step(&mut msg, &mut cur_path_len, "xy");
            if cap + 3 > MAX_PATH_BYTES {
                assert!(msg.starts_with(ELIDED));
                let elided_len = msg.len();
                let elided_path_len = cur_path_len;
                prepend_path_step(&mut msg, &mut cur_path_len, "outer_ignored");
                assert_eq!(msg.len(), elided_len);
                assert_eq!(cur_path_len, elided_path_len);
                assert!(msg.starts_with(ELIDED));
            } else {
                assert!(msg.starts_with("xy."));
                assert_eq!(usize::from(cur_path_len), cap + 3);
                // One more 10-byte step pushes every remaining cap over MAX_PATH_BYTES and elides.
                prepend_path_step(&mut msg, &mut cur_path_len, "over_limit");
                assert!(msg.starts_with(ELIDED));
                let elided_len = msg.len();
                let elided_path_len = cur_path_len;
                prepend_path_step(&mut msg, &mut cur_path_len, "outer_ignored");
                assert_eq!(msg.len(), elided_len);
                assert_eq!(cur_path_len, elided_path_len);
            }
        }
    }
}
