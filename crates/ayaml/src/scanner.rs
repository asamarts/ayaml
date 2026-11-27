//! Lexical scanner.
//!
//! This is an iterative UTF-8 scanner that produces spanned tokens. The stable
//! public API exposes events, values, typed decoding, and text emission; the
//! token layer is internal, with an unstable counting hook under
//! `bench-internals`.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;

use crate::error::{Error, ErrorCode, ErrorKind};
use crate::event::ScalarStyle;
use crate::limits::{GlobalHasher, Limits};
use crate::position::{
    is_forbidden_control_byte, is_quoted_only_non_printable, line_start_of, location_at,
    starts_with_document_marker,
};
use crate::span::Span;
use crate::tag::{Tag, verbatim_reads_back};

mod line;
mod scalars;
#[cfg(test)]
mod tests;

use self::line::{
    NameRules, Owner, PercentError, compat_colon_stops_name, compat_question_stops_name,
    decode_percent_escapes, is_blank, is_line_break, is_tag_char, is_uri_char, leading_spaces,
    non_printable_error, starts_block_collection, uri_text_ok,
};

/// A lexical token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// Owned or input-sliced text carried inside internal `'static` tokens and queues.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TokenText {
    Slice(Span),
    Owned(String),
}

impl TokenText {
    pub(crate) const EMPTY: Self = Self::Slice(Span::empty(0));

    #[inline]
    pub(crate) fn from_decoded(decoded: String, slice: Span, input: &str) -> Self {
        if let Some(raw) = slice.slice(input)
            && raw == decoded
        {
            return Self::Slice(slice);
        }
        Self::Owned(decoded)
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Slice(span) => span.len(),
            Self::Owned(text) => text.len(),
        }
    }

    #[inline]
    pub(crate) fn as_str<'a>(&'a self, input: &'a str) -> &'a str {
        match self {
            Self::Slice(span) => span.slice(input).unwrap_or(""),
            Self::Owned(text) => text.as_str(),
        }
    }

    /// Raw UTF-8 bytes of this text without re-running `str::is_char_boundary` checks.
    #[inline]
    pub(crate) fn as_bytes<'a>(&'a self, input: &'a str) -> &'a [u8] {
        match self {
            Self::Slice(span) => span.bytes(input),
            Self::Owned(text) => text.as_bytes(),
        }
    }

    #[inline]
    pub(crate) fn into_cow(self, input: &str) -> Cow<'_, str> {
        match self {
            Self::Slice(span) => Cow::Borrowed(span.slice(input).unwrap_or("")),
            Self::Owned(text) => Cow::Owned(text),
        }
    }

    #[cfg(test)]
    pub(crate) fn into_owned(self, input: &str) -> String {
        match self {
            Self::Slice(span) => span.slice(input).unwrap_or("").to_owned(),
            Self::Owned(text) => text,
        }
    }
}

/// The prefix the built-in handle `!!` stands for.
const CORE_TAG_PREFIX: &str = "tag:yaml.org,2002:";
/// The first byte of a byte order mark in UTF-8. Other characters start with it too.
const BOM_FIRST_BYTE: u8 = 0xEF;

/// The error for a tab on an empty line inside a scalar, before the scalar's indentation.
const EMPTY_LINE_TAB: &str =
    "a tab cannot indent an empty line inside a scalar under a block collection";

/// Sentinel relative distance for an unmatched flow opener (`[` or `{`).
pub(crate) const UNMATCHED_FLOW: u32 = u32::MAX;

/// The token vocabulary the scanner currently produces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TokenKind {
    StreamStart,
    StreamEnd,
    DocumentStart { explicit: bool },
    DocumentEnd,
    Directive(Box<Directive>),
    BlockEntry,
    Key,
    Value,
    FlowSequenceStart { end: u32 },
    FlowSequenceEnd,
    FlowMappingStart { end: u32 },
    FlowMappingEnd,
    FlowEntry,
    LineBreak,
    PlainScalar,
    SingleQuotedScalar,
    DoubleQuotedScalar,
    Scalar(Box<ScalarToken>),
    Anchor,
    Alias,
    Tag(Tag),
}

/// A `%YAML` or `%TAG` directive's name and value spans in the source input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Directive {
    pub(crate) name: Span,
    pub(crate) value: Span,
}

/// A scalar token's decoded value and presentation, boxed because most tokens are plain
/// or unescaped single-line quoted scalars, whose value is sliced directly from their source span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScalarToken {
    pub(crate) value: TokenText,
    pub(crate) style: ScalarStyle,
}

impl TokenKind {
    /// The style of a scalar token, or `None` for any other token.
    pub(crate) fn scalar_style(&self) -> Option<ScalarStyle> {
        match self {
            Self::PlainScalar => Some(ScalarStyle::Plain),
            Self::SingleQuotedScalar => Some(ScalarStyle::SingleQuoted),
            Self::DoubleQuotedScalar => Some(ScalarStyle::DoubleQuoted),
            Self::Scalar(scalar) => Some(scalar.style),
            _ => None,
        }
    }
}

impl fmt::Display for TokenKind {
    #[allow(
        clippy::struct_excessive_bools,
        reason = "the flags are independent scanner or emitter options, not a state enum"
    )]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StreamStart => formatter.write_str("stream start"),
            Self::StreamEnd => formatter.write_str("stream end"),
            Self::DocumentStart { explicit } => {
                write!(formatter, "document start ({explicit})")
            }
            Self::DocumentEnd => formatter.write_str("document end"),
            Self::Directive(_) => formatter.write_str("directive"),
            Self::BlockEntry => formatter.write_str("block entry"),
            Self::Key => formatter.write_str("key indicator"),
            Self::Value => formatter.write_str("value indicator"),
            Self::FlowSequenceStart { .. } => formatter.write_str("["),
            Self::FlowSequenceEnd => formatter.write_str("]"),
            Self::FlowMappingStart { .. } => formatter.write_str("{"),
            Self::FlowMappingEnd => formatter.write_str("}"),
            Self::FlowEntry => formatter.write_str(","),
            Self::LineBreak => formatter.write_str("line break"),
            Self::PlainScalar
            | Self::SingleQuotedScalar
            | Self::DoubleQuotedScalar
            | Self::Scalar(_) => formatter.write_str("scalar"),
            Self::Anchor => formatter.write_str("anchor"),
            Self::Alias => formatter.write_str("alias"),
            Self::Tag(_) => formatter.write_str("tag"),
        }
    }
}

/// Active and pending `%TAG` handle tables, allocated lazily only when a document defines `%TAG`.
#[derive(Debug, Default)]
pub(crate) struct TagHandles {
    active: HashMap<String, String, GlobalHasher>,
    pending: HashMap<String, String, GlobalHasher>,
}

/// Reusable `'static` scanner scratch buffers checked out from [`crate::ParserBuf`].
#[derive(Debug, Default)]
pub(crate) struct ScannerScratch {
    pub(crate) flow_stack: Vec<(char, usize, bool)>,
    pub(crate) flow_allow_zero_indent: Vec<bool>,
    pub(crate) tag_handles: Option<Box<TagHandles>>,
}

impl ScannerScratch {
    pub(crate) fn clear_and_clamp(&mut self) {
        self.flow_stack.clear();
        if self.flow_stack.capacity() > 128 {
            self.flow_stack = Vec::new();
        }
        self.flow_allow_zero_indent.clear();
        if self.flow_allow_zero_indent.capacity() > 128 {
            self.flow_allow_zero_indent = Vec::new();
        }
        if let Some(ref mut handles) = self.tag_handles {
            handles.active.clear();
            handles.pending.clear();
            if handles.active.capacity() > 64 || handles.pending.capacity() > 64 {
                self.tag_handles = None;
            }
        }
    }

    pub(crate) fn release_all(&mut self) {
        self.flow_stack = Vec::new();
        self.flow_allow_zero_indent = Vec::new();
        self.tag_handles = None;
    }
}

/// An iterative scanner over a UTF-8 YAML input.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the flags are independent scanner options, not a state enum"
)]
pub(crate) struct Scanner<'a> {
    input: &'a str,
    pos: usize,
    /// Byte offset where the line containing `pos` begins (invariant: matches `line_start_of`).
    current_line_start: usize,
    /// Whether a block indicator (`-`, `?`, `:`) has been emitted on the current line outside flow.
    line_has_block_prefix: bool,
    /// Leading space count of `current_line_start` when already observed on a non-BOM line.
    current_line_indent: Option<usize>,
    /// The tokens a reader keeps, which count against `max_events`: every token but the stream's
    /// start and end and the line breaks, which no reader stores.
    tokens: usize,
    /// The anchors in the stream, which count against `max_anchors`.
    anchors: usize,
    flow_level: usize,
    /// Each open flow collection's opening indicator, where it is, and whether a `?` started its
    /// current entry, which a `,` ends.
    flow_stack: Vec<(char, usize, bool)>,
    /// In compat, where the last plain scalar that continued onto a flow collection's line short
    /// of its indent ended, and where that line's content starts. A key's `:` can't follow it.
    short_flow_scalar: Option<(usize, usize)>,
    /// Where the last token ended.
    last_token_end: usize,
    flow_allow_zero_indent: Vec<bool>,
    previous_flow_end: bool,
    yaml_directive_seen: bool,
    /// The handles this document's `%TAG` directives define, allocated lazily so documents without
    /// `%TAG` directives never initialize a `HashMap` or `RandomState`.
    tag_handles: Option<Box<TagHandles>>,
    /// The bytes that tag handles have expanded to in this stream.
    tag_expansion_bytes: usize,
    started: bool,
    finished: bool,
    /// The next call returns a depth-limit error: a flow start went over the depth limit, so the
    /// parser reports earlier errors first and never misreads a truncated flow-key line (AY-027).
    stop_after: bool,
    /// Whether any flow collection opener (`[` or `{`) has been emitted in this stream.
    saw_flow_start: bool,
    /// Byte offset of the flow opener that triggered `stop_after`.
    stop_depth_start: usize,
    initial_bom: bool,
    /// The byte after the stream-start run of byte order marks.
    initial_bom_end: usize,
    /// Where the text after a line's byte order mark starts, outside compat. A BOM takes up
    /// no column there, so document markers and directives may start at this position.
    after_line_bom: usize,
    compat_flow: bool,
    /// In compat, the anchor names ending in `:` defined so far in this document, each with the
    /// offset of its first definition. An alias keeps such a colon only if an anchor before it has
    /// that exact name.
    colon_anchors: Option<HashMap<&'a str, usize, GlobalHasher>>,
    /// Whether a document has started: a `---`, or any node token since the
    /// last `...`. A byte order mark may appear only outside a document or
    /// right before the next one.
    document_open: bool,
    /// Whether this document has a token other than its `---` and the root node's properties.
    /// Until it does, the next node is the root node.
    node_seen: bool,
    /// Whether a directive is waiting for its `---`, with no byte order mark allowed between.
    directive_pending: bool,
    /// Lines known to be a document prefix, so that a mark on any of them needs no new scan.
    known_prefix: std::ops::Range<usize>,
    /// The start of the last line that began with byte order marks, and where its marks end.
    bom_run: Cell<(usize, usize)>,
    /// Where the last checked run of blanks ends. Its later tabs get the first one's answer.
    tab_run_end: usize,
    /// Where the last `-`, `?`, or `:` indicator, `---`, or `...` ends. Blanks right after it, as
    /// at a line's start, are where a block collection's indentation would go.
    indicator_end: usize,
    /// Where the line of the last tab checked for indenting a block collection ends.
    checked_line_end: usize,
    /// The start of the last line a tab was checked on, and where the byte order marks that start
    /// it end, so that a line's tabs look at its marks once.
    line_marks: (usize, usize),
    /// The run of consecutive block entry indicators, an upper bound on compact block nesting.
    block_run: usize,
    /// The last line a quoted scalar's indent was counted on, and that indent.
    counted_indent: (usize, usize),
    /// Where the outermost open flow collection starts.
    flow_start: usize,
    /// The start of the last outermost flow collection whose owner a quoted scalar or a line in it
    /// asked for, that owner, and its column if it's a compact collection on the same line.
    flow_owner: (usize, Owner, Option<usize>),
    /// Whether the plain-scalar continuation fast path may reject a line. Tests turn it off to
    /// compare the fast path with the full analysis.
    #[cfg(test)]
    fast_continuation: bool,
}

impl<'a> Scanner<'a> {
    /// Create a scanner and check the input-size limit.
    pub(crate) fn new(input: &'a str, limits: &Limits) -> Result<Self, Box<Error>> {
        Self::with_compat(input, limits, false)
    }

    pub(crate) fn with_compat(
        input: &'a str,
        limits: &Limits,
        compat_flow: bool,
    ) -> Result<Self, Box<Error>> {
        Self::with_compat_and_scratch(input, limits, compat_flow, ScannerScratch::default())
    }

    pub(crate) fn with_compat_and_scratch(
        input: &'a str,
        limits: &Limits,
        compat_flow: bool,
        mut scratch: ScannerScratch,
    ) -> Result<Self, Box<Error>> {
        limits.validate().map_err(Box::new)?;
        limits.check_input(input.len()).map_err(|error| {
            Box::new(
                Error::from(error)
                    .with_span(Span::empty(0))
                    .with_location(crate::span::Location::new(1, 1)),
            )
        })?;
        scratch.flow_stack.clear();
        scratch.flow_allow_zero_indent.clear();
        if let Some(ref mut handles) = scratch.tag_handles {
            handles.active.clear();
            handles.pending.clear();
        }
        let initial_bom_end = input.len() - input.trim_start_matches('\u{feff}').len();
        let initial_bom = initial_bom_end > 0;
        let pos = initial_bom_end;
        Ok(Self {
            input,
            pos,
            current_line_start: 0,
            line_has_block_prefix: false,
            current_line_indent: None,
            tokens: 0,
            anchors: 0,
            flow_level: 0,
            flow_stack: scratch.flow_stack,
            short_flow_scalar: None,
            last_token_end: 0,
            flow_allow_zero_indent: scratch.flow_allow_zero_indent,
            previous_flow_end: false,
            yaml_directive_seen: false,
            tag_handles: scratch.tag_handles,
            tag_expansion_bytes: 0,
            started: false,
            finished: false,
            stop_after: false,
            saw_flow_start: false,
            stop_depth_start: 0,
            initial_bom,
            initial_bom_end,
            after_line_bom: if initial_bom && !compat_flow { pos } else { 0 },
            compat_flow,
            colon_anchors: None,
            document_open: false,
            node_seen: false,
            directive_pending: false,
            known_prefix: 0..0,
            bom_run: Cell::new((usize::MAX, 0)),
            tab_run_end: 0,
            indicator_end: usize::MAX,
            checked_line_end: 0,
            line_marks: (usize::MAX, 0),
            block_run: 0,
            counted_indent: (usize::MAX, 0),
            flow_start: 0,
            flow_owner: (usize::MAX, Owner::Unknown, None),
            #[cfg(test)]
            fast_continuation: true,
        })
    }

    /// The input length, used to choose a token-arena growth chunk.
    pub(crate) fn input_len(&self) -> usize {
        self.input.len()
    }

    /// The text being scanned.
    pub(crate) fn input(&self) -> &'a str {
        self.input
    }

    /// Whether compatibility mode is active.
    pub(crate) const fn is_compat(&self) -> bool {
        self.compat_flow
    }

    /// Current nesting depth of open flow collections (`[` and `{`).
    pub(crate) const fn flow_level(&self) -> usize {
        self.flow_level
    }

    /// Whether any flow collection opener (`[` or `{`) has been emitted so far.
    pub(crate) const fn saw_flow_start(&self) -> bool {
        self.saw_flow_start
    }

    #[cold]
    #[inline(never)]
    fn finish_depth_stop(&mut self, limits: &Limits) -> Result<Option<Token>, Box<Error>> {
        self.stop_after = false;
        self.finished = true;
        match limits.check_depth(self.flow_level) {
            Err(limit) => Err(self.limit_at(self.stop_depth_start, limit)),
            Ok(()) => Ok(None),
        }
    }

    /// Return the next token, or `None` after the stream end has been emitted.
    #[allow(
        clippy::too_many_lines,
        reason = "the routine is one linear state machine; splitting it would obscure its state"
    )]
    pub(crate) fn take_scratch(&mut self) -> ScannerScratch {
        let mut scratch = ScannerScratch {
            flow_stack: std::mem::take(&mut self.flow_stack),
            flow_allow_zero_indent: std::mem::take(&mut self.flow_allow_zero_indent),
            tag_handles: self.tag_handles.take(),
        };
        scratch.clear_and_clamp();
        scratch
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the token dispatch keeps every YAML indicator arm inline for branch locality"
    )]
    pub(crate) fn next_token(&mut self, limits: &Limits) -> Result<Option<Token>, Box<Error>> {
        if !self.started {
            self.started = true;
            return self.make(TokenKind::StreamStart, self.pos, self.pos, limits);
        }
        if self.finished {
            return Ok(None);
        }
        if self.stop_after {
            return self.finish_depth_stop(limits);
        }

        let bytes = self.input.as_bytes();
        loop {
            self.skip_insignificant()?;
            if self.pos >= bytes.len() {
                self.finished = true;
                return self.make(TokenKind::StreamEnd, self.pos, self.pos, limits);
            }
            let next_byte = bytes[self.pos];
            if next_byte == BOM_FIRST_BYTE
                && self.at_line_start()
                && self.peek() == Some('\u{feff}')
                && !(self.compat_flow && self.pos == 3 && self.initial_bom)
            {
                // libyaml skips a BOM at the start of any line, and the compat
                // scanner keeps that; YAML 1.2.2 allows one only in a document
                // prefix.
                if !self.compat_flow
                    && (self.directive_pending
                        || (self.document_open && !self.bom_starts_prefix(self.pos)))
                {
                    return Err(self.error_at(
                        self.pos,
                        ErrorCode::InvalidBom,
                        "a byte order mark can only start a document",
                    ));
                }
                self.pos += '\u{feff}'.len_utf8();
                if !self.compat_flow {
                    self.after_line_bom = self.pos;
                }
                continue;
            }
            if matches!(next_byte, b'\n' | b'\r') {
                let start = self.pos;
                self.consume_line_break();
                if self.flow_level > 0 {
                    if matches!(self.flow_stack.last(), Some(('[' | '{', _, _)))
                        && self.flow_allow_zero_indent.last() == Some(&false)
                    {
                        let mut content = self.pos;
                        while matches!(self.input[content..].chars().next(), Some(' ' | '\t')) {
                            content += 1;
                        }
                        let Some(&(open, _, explicit)) = self.flow_stack.last() else {
                            unreachable!("flow stack checked")
                        };
                        let next = self.input[content..].chars().next();
                        let closing = if open == '[' { ']' } else { '}' };
                        let spaces = leading_spaces(&self.input[self.pos..content]);
                        let short = match self.flow_line_indent() {
                            Some(required) => spaces < required,
                            None => content == self.pos,
                        };
                        // By default every line with content is held to it but a closing one. Compat
                        // holds only a line that an implicit flow key's `:` starts, as in
                        // `k: {x\n: y}` or `k: {"x"\n:y}`, which libyaml refuses too. libyaml reads
                        // an explicit key's, as in `k: {? x\n: y}`.
                        let held = if self.compat_flow {
                            next == Some(':')
                                && !explicit
                                && (self.colon_indicator(content) || self.previous_flow_end)
                        } else {
                            next.is_some_and(|ch| !is_line_break(ch) && ch != closing && ch != '#')
                        };
                        if short && held {
                            return Err(self.flow_indent_error(content));
                        }
                    }
                    // Line breaks separate flow tokens but are not block
                    // structure. Keeping them out of the token stream lets the
                    // parser see one logical flow collection across lines.
                    continue;
                }
                return self.make_line_break(start, self.pos);
            }
            break;
        }

        if self.flow_level > 0 && self.at_line_start() {
            let rest = &self.input[self.pos..];
            if (rest.starts_with("---") || rest.starts_with("..."))
                && self.marker_follows(self.pos, 3)
            {
                return Err(self.error_at(
                    self.pos,
                    ErrorCode::DocumentMarkerInFlow,
                    "document markers are not allowed inside flow collections",
                ));
            }
        }
        if self.flow_level == 0
            && self.compat_flow
            && self.pos == self.initial_bom_end()
            && self.initial_bom
            && self.input[self.pos..].starts_with("...\n")
            && let Some(token) = self.scan_document_marker(limits)?
        {
            return Ok(Some(token));
        }
        if self.flow_level == 0 && self.at_line_start() {
            if let Some(token) = self.scan_document_marker(limits)? {
                return Ok(Some(token));
            }
            if self.peek() == Some('%') {
                return self.scan_directive(limits);
            }
        }

        let start = self.pos;
        let Some(&byte) = bytes.get(start) else {
            self.finished = true;
            return self.make(TokenKind::StreamEnd, self.pos, self.pos, limits);
        };
        if is_forbidden_control_byte(byte) || is_quoted_only_non_printable(bytes, start) {
            return Err(self.non_printable_error_at(start));
        }
        match byte {
            b'[' => {
                let allow_zero = self.may_continue_at_column_zero();
                let token = self.make(
                    TokenKind::FlowSequenceStart {
                        end: UNMATCHED_FLOW,
                    },
                    start,
                    start + 1,
                    limits,
                )?;
                if self.flow_level == 0 {
                    self.flow_start = start;
                }
                self.flow_level += 1;
                self.flow_stack.push(('[', start, false));
                self.flow_allow_zero_indent.push(allow_zero);
                self.saw_flow_start = true;
                // The parser checks depth from this token. Stop reading ahead here and yield the
                // depth limit on the next call, so earlier errors still win while a truncated
                // flow-key line cannot masquerade as trailing content (AY-027).
                if limits.check_depth(self.flow_level).is_err() {
                    self.stop_after = true;
                    self.stop_depth_start = start;
                }
                self.pos += 1;
                Ok(token)
            }
            b']' => {
                self.pos += 1;
                self.flow_level = self.flow_level.saturating_sub(1);
                self.flow_stack.pop();
                self.flow_allow_zero_indent.pop();
                self.make(TokenKind::FlowSequenceEnd, start, self.pos, limits)
            }
            b'{' => {
                let allow_zero = self.may_continue_at_column_zero();
                let token = self.make(
                    TokenKind::FlowMappingStart {
                        end: UNMATCHED_FLOW,
                    },
                    start,
                    start + 1,
                    limits,
                )?;
                if self.flow_level == 0 {
                    self.flow_start = start;
                }
                self.flow_level += 1;
                self.flow_stack.push(('{', start, false));
                self.flow_allow_zero_indent.push(allow_zero);
                self.saw_flow_start = true;
                if limits.check_depth(self.flow_level).is_err() {
                    self.stop_after = true;
                    self.stop_depth_start = start;
                }
                self.pos += 1;
                Ok(token)
            }
            b'}' => {
                self.pos += 1;
                self.flow_level = self.flow_level.saturating_sub(1);
                self.flow_stack.pop();
                self.flow_allow_zero_indent.pop();
                self.make(TokenKind::FlowMappingEnd, start, self.pos, limits)
            }
            b',' => {
                if let Some(top) = self.flow_stack.last_mut() {
                    top.2 = false;
                }
                self.pos += 1;
                self.make(TokenKind::FlowEntry, start, self.pos, limits)
            }
            b'-' if self.indicator_follows(start, 1)
                && !(self.compat_flow
                    && self.flow_level > 0
                    && self
                        .flow_stack
                        .last()
                        .is_some_and(|&(_, _, explicit)| explicit)
                    && matches!(bytes.get(start + 1), Some(b',' | b']' | b'}'))) =>
            {
                self.pos += 1;
                self.make_indicator(TokenKind::BlockEntry, start, self.pos, limits)
            }
            // A flow `?` before a flow indicator is neither a key's indicator, which needs a
            // space or a line break after it (`ns-flow-map-entry`, `ns-flow-pair`), nor a plain
            // scalar, so `[?]`, `{?,a}`, and `[?[a]]` are errors. libyaml reads `{?}` and
            // `[?[a]]`, and refuses `[?]` only because it drops the `]`.
            b'?' if self.flow_level > 0
                && matches!(bytes.get(start + 1), Some(b',' | b'[' | b']' | b'{' | b'}')) =>
            {
                Err(self.error_at(
                    start,
                    ErrorCode::InvalidFlowKey,
                    "a flow collection's `?` must be separated from what follows it",
                ))
            }
            // In flow context libyaml treats every other `?` as an explicit-key
            // indicator. YAML 1.2.2 makes `?x` a plain scalar (HM87, 652Z);
            // the compatibility scanner keeps the libyaml reading.
            b'?' if (self.compat_flow && self.flow_level > 0)
                || self.indicator_follows(start, 1) =>
            {
                if let Some(top) = self.flow_stack.last_mut() {
                    top.2 = true;
                }
                self.pos += 1;
                self.make_indicator(TokenKind::Key, start, self.pos, limits)
            }
            b':' if self.colon_indicator(start)
                || (self.flow_level > 0 && self.previous_flow_end) =>
            {
                // In compat, an implicit key that ran onto a flow collection's line short of its
                // indent can't take a `:`: the grammar refuses the line, and libyaml the key's
                // lines. Only comments and line breaks may come between them.
                if let Some((end, line)) = self.short_flow_scalar
                    && self.last_token_end == end
                    && self
                        .flow_stack
                        .last()
                        .is_some_and(|&(_, _, explicit)| !explicit)
                {
                    return Err(self.flow_indent_error(line));
                }
                self.pos += 1;
                self.make_indicator(TokenKind::Value, start, self.pos, limits)
            }
            b'&' => self.scan_anchor(false, limits),
            b'*' => self.scan_anchor(true, limits),
            b'!' => self.scan_tag(limits),
            b'\'' => self.scan_single_quoted(limits),
            b'"' => self.scan_double_quoted(limits),
            b'|' => self.scan_block_scalar(start, true, limits),
            b'>' => self.scan_block_scalar(start, false, limits),
            // Indicators that cannot start a plain scalar (`ns-plain-first`);
            // libyaml rejects them too.
            b'%' | b'@' | b'`' => {
                let ch = char::from(byte);
                let error = self.error_at(
                    start,
                    ErrorCode::ReservedIndicator,
                    format!("{ch:?} is a reserved indicator and cannot start a plain scalar"),
                );
                Err(if self.reserved_after_bom(start) {
                    Box::new((*error).with_reserved_after_bom())
                } else {
                    error
                })
            }
            _ => self.scan_plain(limits),
        }
    }

    /// Byte offset where the line containing `offset` starts. Uses the tracked `current_line_start`
    /// in O(1) for positions on the current line and falls back to a backward scan for earlier lines.
    #[inline]
    pub(super) fn line_start(&self, offset: usize) -> usize {
        if offset >= self.current_line_start && offset <= self.pos {
            return self.current_line_start;
        }
        line_start_of(self.input.as_bytes(), offset)
    }

    #[cold]
    #[inline(never)]
    #[allow(
        clippy::unnecessary_box_returns,
        reason = "hot-path callers propagate Box<Error> to keep Result<_, Box<Error>> pointer-sized"
    )]
    pub(super) fn non_printable_error_at(&self, index: usize) -> Box<Error> {
        let error = non_printable_error(self.input, index);
        Box::new(match location_at(self.input, index, self.compat_flow) {
            Some(marker) => error.with_span(Span::empty(index)).with_location(marker),
            None => error,
        })
    }

    /// Reject any disallowed non-printable character inside `start..end` outside quoted scalars.
    fn check_non_printable_slice(&self, start: usize, end: usize) -> Result<(), Box<Error>> {
        let bytes = self.input.as_bytes();
        let mut i = start;
        while i < end {
            let byte = bytes[i];
            if is_forbidden_control_byte(byte) || is_quoted_only_non_printable(bytes, i) {
                return Err(self.non_printable_error_at(i));
            }
            i += 1;
        }
        Ok(())
    }

    /// Count a token that a reader keeps against `max_events`.
    fn count_token(&mut self, start: usize, limits: &Limits) -> Result<(), Box<Error>> {
        self.tokens += 1;
        limits
            .check_events(self.tokens)
            .map_err(|error| self.limit_at(start, error))
    }

    /// Emit a block indicator. These tokens only advance the token count,
    /// mark the document open, and clear flow adjacency.
    fn make_indicator(
        &mut self,
        kind: TokenKind,
        start: usize,
        end: usize,
        limits: &Limits,
    ) -> Result<Option<Token>, Box<Error>> {
        self.count_token(start, limits)?;
        if matches!(kind, TokenKind::BlockEntry) {
            self.block_run += 1;
            if let Err(limit) = limits.check_depth(self.block_run) {
                return Err(self.limit_at(start, limit));
            }
        } else {
            self.block_run = 0;
        }
        if self.flow_level == 0 {
            self.line_has_block_prefix = true;
        }
        self.previous_flow_end = false;
        self.last_token_end = end;
        self.document_open = true;
        self.node_seen = true;
        self.indicator_end = end;
        let span = self.span(start, end)?;
        Ok(Some(Token { kind, span }))
    }

    /// Emit a line break. The parser uses only the token kind; the span still
    /// belongs to the token for diagnostics and event consumers. No reader
    /// keeps a line break, so it doesn't count against `max_events`, and a
    /// blank or comment line costs nothing there.
    fn make_line_break(&mut self, start: usize, end: usize) -> Result<Option<Token>, Box<Error>> {
        self.block_run = 0;
        self.previous_flow_end = false;
        let span = self.span(start, end)?;
        Ok(Some(Token {
            kind: TokenKind::LineBreak,
            span,
        }))
    }

    #[inline]
    fn make(
        &mut self,
        kind: TokenKind,
        start: usize,
        end: usize,
        limits: &Limits,
    ) -> Result<Option<Token>, Box<Error>> {
        // No reader keeps the stream's start and end tokens: they become its
        // stream events, which count as events.
        if !matches!(
            kind,
            TokenKind::StreamStart | TokenKind::StreamEnd | TokenKind::LineBreak
        ) {
            self.block_run = 0;
        }
        if !matches!(kind, TokenKind::StreamStart | TokenKind::StreamEnd) {
            self.count_token(start, limits)?;
        }
        // A scalar's size is the larger of its text and its value: `\L` and
        // `\P` take 2 bytes of text and 3 of value.
        // A scalar whose value is empty still has its text to count.
        let scalar_bytes = match &kind {
            TokenKind::PlainScalar
            | TokenKind::SingleQuotedScalar
            | TokenKind::DoubleQuotedScalar => Some(end - start),
            TokenKind::Scalar(scalar) => Some((end - start).max(scalar.value.len())),
            _ => None,
        };
        if let Some(bytes) = scalar_bytes {
            limits
                .check_scalar_bytes(bytes)
                .map_err(|error| self.limit_at(start, error))?;
        }
        self.last_token_end = end;
        self.previous_flow_end = matches!(
            kind,
            TokenKind::FlowSequenceEnd
                | TokenKind::FlowMappingEnd
                | TokenKind::SingleQuotedScalar
                | TokenKind::DoubleQuotedScalar
        ) || matches!(
            kind,
            TokenKind::Scalar(ref scalar)
                if matches!(
                    scalar.style,
                    ScalarStyle::SingleQuoted | ScalarStyle::DoubleQuoted
                )
        );
        match kind {
            TokenKind::DocumentEnd => {
                self.document_open = false;
                self.node_seen = false;
                self.indicator_end = end;
            }
            TokenKind::DocumentStart { .. } => {
                self.document_open = true;
                self.node_seen = false;
                self.directive_pending = false;
                self.indicator_end = end;
            }
            TokenKind::Directive(_) => self.directive_pending = true,
            TokenKind::Tag(_) | TokenKind::Anchor => self.document_open = true,
            TokenKind::StreamStart | TokenKind::StreamEnd | TokenKind::LineBreak => {}
            _ => {
                self.document_open = true;
                self.node_seen = true;
            }
        }
        let span = self.span(start, end)?;
        Ok(Some(Token { kind, span }))
    }

    /// Whether the byte order mark at `position`, at the start of a line,
    /// begins a document prefix: everything up to the next `---` or `...` line,
    /// or to the end of the input, is blank lines and comments (YAML 1.2.2
    /// productions 202 and 211).
    fn bom_starts_prefix(&mut self, position: usize) -> bool {
        if self.known_prefix.contains(&position) {
            return true;
        }
        let mut line_start = position;
        loop {
            let mut text = &self.input[line_start..];
            while let Some(rest) = text.strip_prefix('\u{feff}') {
                text = rest;
            }
            let content = line_start + (self.input.len() - line_start - text.len());
            if (text.starts_with("---") || text.starts_with("..."))
                && self.marker_follows(content, 3)
            {
                self.known_prefix = position..content;
                return true;
            }
            let line_end = text.find(['\n', '\r']).unwrap_or(text.len());
            let line = text[..line_end].trim_start_matches([' ', '\t']);
            if !(line.is_empty() || line.starts_with('#')) {
                return false;
            }
            if line_end == text.len() {
                self.known_prefix = position..self.input.len();
                return true;
            }
            line_start = content + line_end + 1;
            if self.input.as_bytes()[content + line_end] == b'\r'
                && self.input.as_bytes().get(line_start) == Some(&b'\n')
            {
                line_start += 1;
            }
        }
    }

    #[inline]
    fn span(&self, start: usize, end: usize) -> Result<Span, Box<Error>> {
        if start <= end
            && end <= self.input.len()
            && self.input.is_char_boundary(start)
            && self.input.is_char_boundary(end)
        {
            Ok(Span::new(start, end))
        } else {
            Err(self.error_at(start, ErrorCode::UnexpectedToken, "invalid span position"))
        }
    }

    #[cold]
    #[inline(never)]
    #[allow(
        clippy::unnecessary_box_returns,
        reason = "hot-path callers propagate Box<Error> to keep Result<_, Box<Error>> pointer-sized"
    )]
    fn limit_at(&self, index: usize, limit: crate::limits::LimitExceeded) -> Box<Error> {
        let mut error = Error::from(limit);
        if let Some(marker) = location_at(self.input, index, self.compat_flow) {
            error = error.with_span(Span::empty(index)).with_location(marker);
        }
        Box::new(error)
    }

    /// Where the stream-start run of byte order marks ends.
    pub(super) fn initial_bom_end(&self) -> usize {
        self.initial_bom_end
    }

    /// Whether the byte at `index` follows a byte order mark at the start of a line.
    fn reserved_after_bom(&self, index: usize) -> bool {
        if self.initial_bom && index == self.initial_bom_end() {
            return true;
        }
        let Some(before) = self
            .input
            .get(..index)
            .and_then(|text| text.strip_suffix('\u{feff}'))
        else {
            return false;
        };
        before.is_empty() || before.ends_with(['\n', '\r'])
    }

    #[cold]
    #[inline(never)]
    #[allow(
        clippy::unnecessary_box_returns,
        reason = "hot-path callers propagate Box<Error> to keep Result<_, Box<Error>> pointer-sized"
    )]
    fn error_at(&self, index: usize, code: ErrorCode, message: impl Into<String>) -> Box<Error> {
        let marker = location_at(self.input, index, self.compat_flow);
        let mut error =
            Error::coded(ErrorKind::Scanner, code, message).with_span(Span::empty(index));
        if let Some(marker) = marker {
            error = error.with_location(marker);
        }
        Box::new(error)
    }

    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn peek_second(&self) -> Option<char> {
        let mut chars = self.input[self.pos..].chars();
        chars.next()?;
        chars.next()
    }

    fn at_line_start(&self) -> bool {
        if self.pos == self.current_line_start || self.pos == self.after_line_bom {
            return true;
        }
        matches!(
            self.input.as_bytes().get(self.pos.saturating_sub(1)),
            Some(b'\n' | b'\r')
        )
    }

    fn indicator_follows(&self, start: usize, offset: usize) -> bool {
        let Some(&byte) = self.input.as_bytes().get(start + offset) else {
            return true;
        };
        // A flow indicator only ends `-` or `?` inside a flow collection; in
        // block context `-,` and `?]` are plain scalars.
        matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
            || (self.flow_level > 0 && matches!(byte, b',' | b']' | b'}'))
    }

    fn colon_indicator(&self, start: usize) -> bool {
        let Some(&byte) = self.input.as_bytes().get(start + 1) else {
            return true;
        };
        matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
            || (self.flow_level > 0 && matches!(byte, b',' | b']' | b'}'))
    }

    fn skip_insignificant(&mut self) -> Result<(), Box<Error>> {
        let bytes = self.input.as_bytes();
        while self.pos < bytes.len() {
            match bytes[self.pos] {
                b' ' => {
                    let at_line_start =
                        self.pos == self.current_line_start && self.current_line_indent.is_none();
                    let run_start = self.pos;
                    self.pos += 1;
                    while self.pos < bytes.len() && bytes[self.pos] == b' ' {
                        self.pos += 1;
                    }
                    if at_line_start {
                        self.current_line_indent = Some(self.pos - run_start);
                    }
                }
                b'\t' => {
                    if self.pos == self.current_line_start && self.current_line_indent.is_none() {
                        self.current_line_indent = Some(0);
                    }
                    if self.flow_level == 0 && self.pos >= self.tab_run_end {
                        self.tab_run_end = self.end_of_blanks(self.pos);
                        if self.tab_indents_block_collection(self.pos, self.tab_run_end) {
                            return Err(self.error_at(
                                self.pos,
                                ErrorCode::InvalidTabIndentation,
                                "tab cannot separate a block structural indicator",
                            ));
                        }
                    }
                    if self.flow_level > 0
                        && !self.may_continue_at_column_zero()
                        && self.tab_only_flow_indentation()
                    {
                        return Err(self.error_at(
                            self.pos,
                            ErrorCode::InvalidTabIndentation,
                            "tabs cannot be the only flow-collection indentation",
                        ));
                    }
                    self.pos += 1;
                }
                b'#' if self.comment_is_legal() => {
                    let check_bom = !self.compat_flow;
                    let mut i = self.pos + 1;
                    while i < bytes.len() {
                        i = scalars::skip_printable_ascii_swar(bytes, i);
                        if i >= bytes.len() {
                            break;
                        }
                        let byte = bytes[i];
                        match byte {
                            b'\n' | b'\r' => break,
                            b'\t' => i += 1,
                            0x00..=0x1f | 0x7f => {
                                self.pos = i;
                                return Err(self.non_printable_error_at(i));
                            }
                            0xC2 if is_quoted_only_non_printable(bytes, i) => {
                                self.pos = i;
                                return Err(self.non_printable_error_at(i));
                            }
                            BOM_FIRST_BYTE => {
                                if check_bom
                                    && bytes.get(i + 1) == Some(&0xBB)
                                    && bytes.get(i + 2) == Some(&0xBF)
                                {
                                    self.pos = i;
                                    return Err(self.error_at(
                                        i,
                                        ErrorCode::InvalidBom,
                                        "a byte order mark can only start a document",
                                    ));
                                }
                                if is_quoted_only_non_printable(bytes, i) {
                                    self.pos = i;
                                    return Err(self.non_printable_error_at(i));
                                }
                                i += 1;
                            }
                            _ => {
                                i += 1;
                                while i < bytes.len()
                                    && bytes[i] >= 0x80
                                    && !matches!(bytes[i], 0xC2 | BOM_FIRST_BYTE)
                                {
                                    i += 1;
                                }
                            }
                        }
                    }
                    self.pos = i;
                }
                byte => {
                    if self.pos == self.current_line_start
                        && self.current_line_indent.is_none()
                        && byte != BOM_FIRST_BYTE
                    {
                        self.current_line_indent = Some(0);
                    }
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Whether the tab here is a line's first character with only blanks before its content.
    /// The first tab answers for the rest of its run, so a line pays for one scan.
    fn tab_only_flow_indentation(&self) -> bool {
        if !self.at_line_start() {
            return false;
        }
        let next = self.input[self.end_of_blanks(self.pos)..].chars().next();
        next.is_some_and(|ch| !matches!(ch, ']' | '}' | '#') && !is_line_break(ch))
    }

    fn end_of_blanks(&self, position: usize) -> usize {
        let rest = &self.input[position..];
        position + rest.len() - rest.trim_start_matches([' ', '\t']).len()
    }

    /// Whether the tab at `tab`, in blanks that end at `content`, would indent a block collection,
    /// which only spaces may do (YAML 1.2.2 section 6.1). It would where the blanks start a line,
    /// or follow a `-`, `?`, or `:` indicator, `---`, or `...`, and the content there starts a block
    /// collection. Elsewhere, as after a key or a node property, a tab separates as a space does.
    fn tab_indents_block_collection(&mut self, tab: usize, content: usize) -> bool {
        let blanks_start = self.input[..tab].trim_end_matches([' ', '\t']).len();
        let line_start = self.line_start(blanks_start);
        if self.line_marks.0 != line_start {
            let rest = &self.input[line_start..];
            let marks = rest.len() - rest.trim_start_matches('\u{feff}').len();
            self.line_marks = (line_start, line_start + marks);
        }
        let indents = blanks_start == self.indicator_end || blanks_start <= self.line_marks.1;
        if !indents {
            return false;
        }
        // Each tab on a line checks a later part of it, so the line's end is found once.
        if content >= self.checked_line_end {
            let rest = &self.input[content..];
            self.checked_line_end = content + rest.find(['\n', '\r']).unwrap_or(rest.len());
        }
        starts_block_collection(&self.input[content..self.checked_line_end], self.names())
    }

    fn comment_is_legal(&self) -> bool {
        if self.pos == 0 {
            return true;
        }
        let previous = self.input[..self.pos].chars().next_back();
        // A BOM at the start of a line leaves the comment at the start of
        // the line too (YAML 1.2.2 example 5.1). The stream-start run of marks
        // is skipped as one, so a comment right after it starts the line as well.
        let after_line_start_bom = previous == Some('\u{feff}')
            && (self.pos == self.after_line_bom || self.pos == self.initial_bom_end || {
                let bom = self.pos - '\u{feff}'.len_utf8();
                bom == 0 || self.input[..bom].ends_with(['\n', '\r'])
            });
        previous.is_none_or(is_blank)
            || after_line_start_bom
            || (self.compat_flow
                && self.flow_level > 0
                && previous.is_some_and(|ch| matches!(ch, '[' | '{' | ',' | '?')))
    }

    #[inline]
    fn consume_line_break(&mut self) {
        let bytes = self.input.as_bytes();
        let Some(&byte) = bytes.get(self.pos) else {
            return;
        };
        if byte == b'\r' && bytes.get(self.pos + 1) == Some(&b'\n') {
            self.pos += 2;
        } else if byte == b'\n' || byte == b'\r' {
            self.pos += 1;
        } else if let Some(ch) = self.peek() {
            self.pos += ch.len_utf8();
        }
        self.current_line_start = self.pos;
        self.line_has_block_prefix = false;
        self.current_line_indent = None;
    }

    fn scan_document_marker(&mut self, limits: &Limits) -> Result<Option<Token>, Box<Error>> {
        let start = self.pos;
        // In compat, a stream-start BOM is content for marker detection, so
        // `BOM---` is a plain scalar rather than an explicit document start.
        // By default a BOM is not content.
        if self.compat_flow && start == self.initial_bom_end() && self.initial_bom {
            let rest = &self.input[start..];
            if !rest.starts_with("...") || !rest[3..].chars().next().is_some_and(is_line_break) {
                return Ok(None);
            }
        }
        let rest = &self.input[start..];
        if rest.starts_with("---") && self.marker_follows(start, 3) {
            self.pos += 3;
            self.yaml_directive_seen = false;
            self.colon_anchors = None;
            if let Some(handles) = &mut self.tag_handles {
                handles.active.clear();
                handles.active.extend(handles.pending.drain());
            }
            return self.make(
                TokenKind::DocumentStart { explicit: true },
                start,
                self.pos,
                limits,
            );
        }
        if rest.starts_with("...") && self.marker_follows(start, 3) {
            self.pos += 3;
            self.yaml_directive_seen = false;
            self.colon_anchors = None;
            if let Some(handles) = &mut self.tag_handles {
                handles.active.clear();
                handles.pending.clear();
            }
            return self.make(TokenKind::DocumentEnd, start, self.pos, limits);
        }
        Ok(None)
    }

    fn marker_follows(&self, start: usize, offset: usize) -> bool {
        let after = start + offset;
        self.input
            .get(after..)
            .is_none_or(|rest| rest.chars().next().is_none_or(is_blank))
    }

    /// Whether a document marker, `---` or `...` with whitespace or the line's end after it,
    /// starts the line at `position`, in column 0.
    fn document_marker_starts_line(&self, position: usize) -> bool {
        let rest = &self.input[position..];
        (rest.starts_with("---") || rest.starts_with("..."))
            && self.line_start(position) == position
            && self.marker_follows(position, 3)
    }

    fn scan_directive(&mut self, limits: &Limits) -> Result<Option<Token>, Box<Error>> {
        let start = self.pos;
        self.pos += 1;
        let name_start = self.pos;
        while let Some(ch) = self.peek() {
            if is_blank(ch) {
                break;
            }
            self.pos += ch.len_utf8();
        }
        if self.pos == name_start {
            return Err(self.error_at(
                start,
                ErrorCode::InvalidDirective,
                "a directive needs a name",
            ));
        }
        let name_span = Span::new(name_start, self.pos);
        let name = &self.input[name_start..self.pos];
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.pos += 1;
        }
        let value_start = self.pos;
        while let Some(ch) = self.peek() {
            if is_line_break(ch) {
                break;
            }
            self.pos += ch.len_utf8();
        }
        self.check_non_printable_slice(name_start, self.pos)?;
        let value = self.input[value_start..self.pos].trim_end_matches(is_blank);
        let value_span = Span::new(value_start, value_start + value.len());
        // Directive names and parameters are `ns-char`s, which exclude the
        // byte order mark; libyaml rejects it too. A comment after them may hold one in compat,
        // as any comment may there, and as in libyaml.
        let directive = &self.input[name_start..self.pos];
        let checked = if self.compat_flow {
            let comment = directive
                .match_indices('#')
                .find(|(at, _)| directive[..*at].ends_with([' ', '\t']))
                .map_or(directive.len(), |(at, _)| at);
            &directive[..comment]
        } else {
            directive
        };
        if checked.contains('\u{feff}') {
            return Err(self.error_at(
                start,
                ErrorCode::InvalidBom,
                "a directive cannot contain a byte order mark",
            ));
        }
        if name == "YAML" {
            let version_end = value.find(is_blank).unwrap_or(value.len());
            let version = &value[..version_end];
            let trailing = value[version_end..].trim_start_matches(is_blank);
            let version_like = |candidate: &str| {
                candidate.strip_prefix("1.").is_some_and(|minor| {
                    !minor.is_empty() && minor.chars().all(|ch| ch.is_ascii_digit())
                })
            };
            let valid = version_like(version)
                && (trailing.is_empty() || trailing.starts_with('#') || version_like(trailing));
            if !valid {
                return Err(self.error_at(
                    start,
                    ErrorCode::InvalidDirective,
                    "invalid %YAML directive",
                ));
            }
            if self.yaml_directive_seen {
                return Err(self.error_at(
                    start,
                    ErrorCode::DuplicateDirective,
                    "duplicate %YAML directive",
                ));
            }
            self.yaml_directive_seen = true;
        }
        if name == "TAG" {
            let mut parts = value.split(is_blank).filter(|part| !part.is_empty());
            // Only a comment may follow the prefix, and the split leaves a blank before it.
            let (Some(handle), Some(prefix)) = (parts.next(), parts.next()) else {
                return Err(self.error_at(
                    start,
                    ErrorCode::InvalidDirective,
                    "%TAG requires a handle and a prefix",
                ));
            };
            if parts.next().is_some_and(|part| !part.starts_with('#')) {
                return Err(self.error_at(
                    start,
                    ErrorCode::InvalidDirective,
                    "%TAG requires a handle and a prefix",
                ));
            }
            // A handle is `!`, `!!`, or word characters between two `!`s;
            // libyaml also allows `_`, and the compat scanner keeps that.
            let named = handle
                .strip_prefix('!')
                .and_then(|rest| rest.strip_suffix('!'))
                .is_some_and(|name| {
                    !name.is_empty()
                        && name.chars().all(|ch| {
                            ch.is_ascii_alphanumeric()
                                || ch == '-'
                                || (self.compat_flow && ch == '_')
                        })
                });
            if !(handle == "!" || handle == "!!" || named) {
                return Err(self.error_at(
                    start,
                    ErrorCode::InvalidDirective,
                    format!("invalid %TAG handle {handle}"),
                ));
            }
            // One document may give a handle one prefix, even the same one twice (YAML 1.2.2
            // section 6.8.2.2).
            if self
                .tag_handles
                .as_ref()
                .is_some_and(|handles| handles.pending.contains_key(handle))
            {
                return Err(self.error_at(
                    start,
                    ErrorCode::DuplicateDirective,
                    format!("duplicate %TAG directive for {handle}"),
                ));
            }
            // A prefix is `!` and URI characters (a local prefix), or URI
            // characters that do not start with `!` or a flow indicator.
            // libyaml allows a leading flow indicator, and so does compat.
            let prefix_ok = match prefix.strip_prefix('!') {
                Some(local) => uri_text_ok(local),
                None => {
                    (self.compat_flow || prefix.chars().next().is_some_and(is_tag_char))
                        && uri_text_ok(prefix)
                }
            };
            if !prefix_ok {
                return Err(self.error_at(
                    start,
                    ErrorCode::InvalidDirective,
                    format!("invalid %TAG prefix {prefix}"),
                ));
            }
            // The prefix's %-escapes are decoded like a suffix's, as libyaml
            // does. `uri_text_ok` has checked their syntax.
            let prefix = decode_percent_escapes(prefix).map_err(|_| {
                self.error_at(
                    start,
                    ErrorCode::InvalidDirective,
                    "tag %-escapes do not form valid UTF-8",
                )
            })?;
            self.tag_handles
                .get_or_insert_with(Box::default)
                .pending
                .insert(handle.to_owned(), prefix);
        }
        self.make(
            TokenKind::Directive(Box::new(Directive {
                name: name_span,
                value: value_span,
            })),
            start,
            self.pos,
            limits,
        )
    }

    fn scan_anchor(&mut self, alias: bool, limits: &Limits) -> Result<Option<Token>, Box<Error>> {
        let start = self.pos;
        self.pos += 1;
        let name_start = self.pos;
        let bytes = self.input.as_bytes();
        while let Some(ch) = self.peek() {
            if is_blank(ch) || matches!(ch, '[' | ']' | '{' | '}' | ',') {
                break;
            }
            if is_forbidden_control_byte(bytes[self.pos])
                || is_quoted_only_non_printable(bytes, self.pos)
            {
                return Err(self.non_printable_error_at(self.pos));
            }
            // Anchor names are `ns-char`s, which exclude the BOM; libyaml
            // allows only word characters.
            if ch == '\u{feff}' {
                return Err(self.error_at(
                    self.pos,
                    ErrorCode::InvalidBom,
                    "an anchor name cannot contain a byte order mark",
                ));
            }
            // YAML 1.2.2 allows any non-space, non-flow-indicator character in
            // an anchor name, so `&an:chor` is the anchor `an:chor` (Y2GN).
            // libyaml stops at `?` and at a `:` followed by more text; the
            // compatibility scanner keeps that. It does not keep libyaml's stop
            // at a `:` before a blank or flow indicator (`&a: b`), which
            // 2SXE and W5VH rule out. Nor does it stop at a `:` in a flow
            // collection: libyaml reads that `:` as a value indicator, which
            // this scanner wouldn't, so stopping would match neither reading.
            if self.compat_flow && ch == '?' {
                let after = self.input[self.pos + 1..].chars().next();
                if compat_question_stops_name(after, self.flow_level > 0) {
                    break;
                }
            }
            if self.compat_flow && self.flow_level == 0 && ch == ':' && self.pos > name_start {
                let after = self.input[self.pos + 1..].chars().next();
                if compat_colon_stops_name(after) {
                    break;
                }
            }
            self.pos += ch.len_utf8();
        }
        if self.pos == name_start {
            return Err(self.error_at(
                start,
                ErrorCode::InvalidAnchor,
                "anchor/alias name must not be empty",
            ));
        }
        // libyaml ends a name at any `:`, so `*a: b` has the key `*a`. Compat follows it for an
        // alias, unless an anchor in this document has that exact name, as `&a: b` does (2SXE, W5VH).
        if alias
            && self.pos > name_start + 1
            && self.input[..self.pos].ends_with(':')
            && !self
                .names()
                .alias_keeps_colon(&self.input[name_start..self.pos])
        {
            self.pos -= 1;
        }
        // A node's properties are separated from its content (production
        // 96), so `&a[b]` is not an anchored sequence; libyaml rejects it too.
        if matches!(self.peek(), Some('[' | '{')) {
            return Err(self.error_at(
                self.pos,
                ErrorCode::InvalidAnchor,
                "an anchor or alias must be followed by white space",
            ));
        }
        let name = Cow::Borrowed(&self.input[name_start..self.pos]);
        if alias {
            self.make(TokenKind::Alias, start, self.pos, limits)
        } else {
            self.anchors += 1;
            limits
                .check_anchors(self.anchors)
                .map_err(|error| self.limit_at(start, error))?;
            if self.compat_flow && name.ends_with(':') {
                self.colon_anchors
                    .get_or_insert_with(|| HashMap::with_hasher(GlobalHasher))
                    .entry(&self.input[name_start..self.pos])
                    .or_insert(start);
            }
            self.make(TokenKind::Anchor, start, self.pos, limits)
        }
    }

    /// What decides where a name ends, for reading a line without scanning it.
    fn names(&self) -> NameRules<'_, 'a> {
        NameRules {
            compat: self.compat_flow,
            colon_anchors: self.colon_anchors.as_ref(),
            input: self.input,
        }
    }

    /// The prefix `handle` stands for, or `None` if nothing defines it. Each use counts the whole
    /// prefix, because every tag holds its own copy, so one long prefix would otherwise make each
    /// short tag cost that much memory.
    fn expand_tag_handle(
        &mut self,
        start: usize,
        handle: &str,
        limits: &Limits,
    ) -> Result<Option<String>, Box<Error>> {
        let custom = self
            .tag_handles
            .as_ref()
            .and_then(|handles| handles.active.get(handle));
        let prefix = match (custom, handle) {
            (Some(prefix), _) => prefix.as_str(),
            (None, "!") => "!",
            (None, "!!") => CORE_TAG_PREFIX,
            (None, _) => return Ok(None),
        };
        let total = self.tag_expansion_bytes.saturating_add(prefix.len());
        limits
            .check_tag_expansion(total)
            .map_err(|error| self.limit_at(start, error))?;
        let prefix = prefix.to_owned();
        self.tag_expansion_bytes = total;
        Ok(Some(prefix))
    }

    /// The tag a scanner produced from a resolved handle prefix and suffix, in the canonical form a
    /// composed value and the typed readers can reproduce from its full name. The handle/suffix
    /// split is presentation; the full name is what the tag means.
    fn resolved_tag(prefix: &str, suffix: &str) -> Tag {
        let mut full = String::with_capacity(prefix.len() + suffix.len());
        full.push_str(prefix);
        full.push_str(suffix);
        Tag::new("", full)
    }

    fn scan_tag(&mut self, limits: &Limits) -> Result<Option<Token>, Box<Error>> {
        let start = self.pos;
        self.pos += 1;
        // A shorthand's handle needs a suffix after it (YAML 1.2.2 production 99), so `!!` and
        // `!e!` alone are errors. `!` alone is the non-specific tag.
        let mut no_suffix = false;
        let after_flow_question = self.compat_flow
            && self.flow_level > 0
            && self
                .flow_stack
                .last()
                .is_some_and(|&(_, _, explicit)| explicit)
            && self.input[..start].ends_with('?');
        let tag = if self.peek() == Some('!') {
            self.pos += 1;
            let suffix_start = self.pos;
            self.consume_tag_suffix();
            // libyaml allows `!` in a secondary-handle tag suffix; after an unseparated flow `?`,
            // default mode reads `?!...` as a plain scalar while compat takes `?` as an explicit
            // key, so compat keeps libyaml's reading (`[?!!!,]`, `[?!!a!b,]`).
            if after_flow_question && self.peek() == Some('!') {
                while let Some(ch) = self.peek() {
                    if ch != '!' && !is_tag_char(ch) {
                        break;
                    }
                    self.pos += ch.len_utf8();
                }
            }
            let prefix = self
                .expand_tag_handle(start, "!!", limits)?
                .ok_or_else(|| {
                    self.error_at(
                        start,
                        ErrorCode::UndefinedTagHandle,
                        "undefined secondary tag handle",
                    )
                })?;
            no_suffix = self.pos == suffix_start;
            Self::resolved_tag(&prefix, &self.decode_tag_suffix(suffix_start)?)
        } else if self.peek() == Some('<') {
            self.pos += 1;
            let suffix_start = self.pos;
            let bytes = self.input.as_bytes();
            while let Some(ch) = self.peek() {
                if ch == '>' {
                    break;
                }
                if !is_uri_char(ch) {
                    if is_forbidden_control_byte(bytes[self.pos])
                        || is_quoted_only_non_printable(bytes, self.pos)
                    {
                        return Err(self.non_printable_error_at(self.pos));
                    }
                    return Err(self.error_at(
                        self.pos,
                        ErrorCode::InvalidTag,
                        "a verbatim tag may only hold URI characters; escape others as %XX",
                    ));
                }
                self.pos += ch.len_utf8();
            }
            if self.peek() != Some('>') {
                return Err(self.error_at(
                    start,
                    ErrorCode::InvalidTag,
                    "unterminated verbatim tag",
                ));
            }
            let suffix = &self.input[suffix_start..self.pos];
            // A verbatim tag holds at least one character (production 97), and `!` alone names
            // no local tag (example 6.25).
            if suffix.is_empty() || suffix == "!" {
                return Err(self.error_at(
                    start,
                    ErrorCode::InvalidTag,
                    "a verbatim tag needs a name",
                ));
            }
            // A verbatim tag is read as written, so a `%` is part of the name only when it starts
            // an escape, and the name must be one YAML can hold: a local tag or one with a scheme.
            if !uri_text_ok(suffix) {
                return Err(self.error_at(
                    start,
                    ErrorCode::InvalidTag,
                    "a `%` in a verbatim tag must start a two-digit escape",
                ));
            }
            if !verbatim_reads_back("", suffix) {
                return Err(self.error_at(
                    start,
                    ErrorCode::InvalidTag,
                    "a verbatim tag must name a local tag or a tag with a scheme",
                ));
            }
            self.pos += 1;
            Self::resolved_tag("", suffix)
        } else {
            let handle_start = self.pos;
            while let Some(ch) = self.peek() {
                if ch == '!' || !is_tag_char(ch) {
                    break;
                }
                self.pos += ch.len_utf8();
            }
            let handle_name = &self.input[handle_start..self.pos];
            let non_word_handle = !handle_name.is_empty()
                && !handle_name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
            if self.compat_flow
                && self.peek() == Some('!')
                && (handle_name.contains('@') || (after_flow_question && non_word_handle))
            {
                // libyaml reads a non-word handle such as `!@!` or `[?!)!,]` as a local tag
                // under the primary handle `!`, not as a named handle. A named handle holds only
                // word characters, so a later `!` here belongs to the tag's suffix, which still
                // holds only tag characters (and `!`).
                self.pos += 1;
                while let Some(ch) = self.peek() {
                    if ch != '!' && !is_tag_char(ch) {
                        break;
                    }
                    self.pos += ch.len_utf8();
                }
                let prefix = self.expand_tag_handle(start, "!", limits)?.ok_or_else(|| {
                    self.error_at(
                        start,
                        ErrorCode::UndefinedTagHandle,
                        "undefined primary tag handle",
                    )
                })?;
                Self::resolved_tag(&prefix, &self.decode_tag_suffix(handle_start)?)
            } else if self.peek() == Some('!') {
                self.pos += 1;
                let suffix_start = self.pos;
                self.consume_tag_suffix();
                let handle = &self.input[handle_start - 1..suffix_start];
                let prefix = self
                    .expand_tag_handle(start, handle, limits)?
                    .ok_or_else(|| {
                        self.error_at(
                            start,
                            ErrorCode::UndefinedTagHandle,
                            format!("undefined tag handle {handle:?}"),
                        )
                    })?;
                no_suffix = self.pos == suffix_start;
                Self::resolved_tag(&prefix, &self.decode_tag_suffix(suffix_start)?)
            } else {
                // `!` alone is the non-specific tag, which no `%TAG !` changes.
                let prefix = if self.pos == handle_start {
                    "!".to_owned()
                } else {
                    self.expand_tag_handle(start, "!", limits)?.ok_or_else(|| {
                        self.error_at(
                            start,
                            ErrorCode::UndefinedTagHandle,
                            "undefined primary tag handle",
                        )
                    })?
                };
                Self::resolved_tag(&prefix, &self.decode_tag_suffix(handle_start)?)
            }
        };
        if self.peek().is_some_and(|ch| {
            !(is_blank(ch) || (self.flow_level > 0 && matches!(ch, ',' | ']' | '}')))
        }) {
            let bytes = self.input.as_bytes();
            if is_forbidden_control_byte(bytes[self.pos])
                || is_quoted_only_non_printable(bytes, self.pos)
            {
                return Err(self.non_printable_error_at(self.pos));
            }
            return Err(self.error_at(
                self.pos,
                ErrorCode::InvalidTag,
                "a tag must be followed by whitespace or a line break",
            ));
        }
        if no_suffix {
            return Err(self.error_at(
                start,
                ErrorCode::InvalidTag,
                "a tag's handle needs a suffix after it",
            ));
        }
        self.make(TokenKind::Tag(tag), start, self.pos, limits)
    }

    /// The shorthand tag suffix from `start` to the current position, with
    /// `%XX` escapes decoded (YAML 1.2.2 §6.8.2; suite 6CK3). Verbatim tags
    /// are delivered as written.
    fn decode_tag_suffix(&self, start: usize) -> Result<String, Box<Error>> {
        decode_percent_escapes(&self.input[start..self.pos]).map_err(|error| match error {
            PercentError::Escape(index) => self.error_at(
                start + index,
                ErrorCode::InvalidTag,
                "invalid %-escape in tag",
            ),
            PercentError::Utf8 => self.error_at(
                start,
                ErrorCode::InvalidTag,
                "tag %-escapes do not form valid UTF-8",
            ),
        })
    }

    fn consume_tag_suffix(&mut self) {
        while let Some(ch) = self.peek() {
            if !is_tag_char(ch) {
                break;
            }
            self.pos += ch.len_utf8();
        }
    }
}
