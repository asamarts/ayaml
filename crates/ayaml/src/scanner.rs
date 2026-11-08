//! Lexical scanner.
//!
//! This is an iterative UTF-8 scanner that produces spanned tokens. The token
//! layer is deliberately kept below the public API.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;

use crate::error::{Error, ErrorKind};
use crate::event::ScalarStyle;
use crate::limits::Limits;
use crate::position::{LineIndex, starts_with_document_marker};
use crate::span::Span;
use crate::tag::Tag;

/// A lexical token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Token<'a> {
    pub kind: TokenKind<'a>,
    pub span: Span,
}

/// The prefix the built-in handle `!!` stands for.
const CORE_TAG_PREFIX: &str = "tag:yaml.org,2002:";
/// The first byte of a byte order mark in UTF-8. Other characters start with it too.
const BOM_FIRST_BYTE: u8 = 0xEF;

/// The token vocabulary the scanner currently produces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TokenKind<'a> {
    StreamStart,
    StreamEnd,
    DocumentStart {
        explicit: bool,
    },
    DocumentEnd,
    Directive {
        name: Cow<'a, str>,
        value: Cow<'a, str>,
    },
    BlockEntry,
    Key,
    Value,
    FlowSequenceStart,
    FlowSequenceEnd,
    FlowMappingStart,
    FlowMappingEnd,
    FlowEntry,
    LineBreak,
    Scalar {
        value: Cow<'a, str>,
        raw: Cow<'a, str>,
        style: ScalarStyle,
    },
    Anchor(Cow<'a, str>),
    Alias(Cow<'a, str>),
    Tag(Tag),
}

impl fmt::Display for TokenKind<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StreamStart => formatter.write_str("stream start"),
            Self::StreamEnd => formatter.write_str("stream end"),
            Self::DocumentStart { explicit } => {
                write!(formatter, "document start ({explicit})")
            }
            Self::DocumentEnd => formatter.write_str("document end"),
            Self::Directive { name, .. } => write!(formatter, "directive %{name}"),
            Self::BlockEntry => formatter.write_str("block entry"),
            Self::Key => formatter.write_str("key indicator"),
            Self::Value => formatter.write_str("value indicator"),
            Self::FlowSequenceStart => formatter.write_str("["),
            Self::FlowSequenceEnd => formatter.write_str("]"),
            Self::FlowMappingStart => formatter.write_str("{"),
            Self::FlowMappingEnd => formatter.write_str("}"),
            Self::FlowEntry => formatter.write_str(","),
            Self::LineBreak => formatter.write_str("line break"),
            Self::Scalar { .. } => formatter.write_str("scalar"),
            Self::Anchor(_) => formatter.write_str("anchor"),
            Self::Alias(_) => formatter.write_str("alias"),
            Self::Tag(_) => formatter.write_str("tag"),
        }
    }
}

/// An iterative scanner over a UTF-8 YAML input.
#[derive(Debug)]
pub(crate) struct Scanner<'a> {
    input: &'a str,
    pos: usize,
    line_index: LineIndex,
    limits: Limits,
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
    /// The handles this document's `%TAG` directives define. `!` and `!!` otherwise have their
    /// built-in prefixes, so most documents pay nothing here.
    tag_handles: HashMap<String, String>,
    /// The bytes that tag handles have expanded to in this stream.
    tag_expansion_bytes: usize,
    pending_tag_handles: Vec<(String, String)>,
    started: bool,
    finished: bool,
    initial_bom: bool,
    /// Where the text after a line's byte order mark starts, outside compat. A BOM takes up
    /// no column there, so document markers and directives may start at this position.
    after_line_bom: usize,
    compat_flow: bool,
    /// In compat, the anchor names ending in `:` defined so far in this document, each with the
    /// offset of its first definition. An alias keeps such a colon only if an anchor before it has
    /// that exact name.
    colon_anchors: Option<HashMap<&'a str, usize>>,
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
    /// The start of the plain scalar last checked for a block prefix, and the answer.
    block_prefix: (usize, bool),
    /// Where the line of the last tab checked for indenting a block collection ends.
    checked_line_end: usize,
    /// The start of the last line a tab was checked on, and where the byte order marks that start
    /// it end, so that a line's tabs look at its marks once.
    line_marks: (usize, usize),
    /// The last line a quoted scalar's indent was counted on, and that indent.
    counted_indent: (usize, usize),
    /// Where the outermost open flow collection starts.
    flow_start: usize,
    /// The start of the last outermost flow collection whose owner a quoted scalar or a line in it
    /// asked for, that owner, and its column if it's a compact collection on the same line.
    flow_owner: (usize, Owner, Option<usize>),
    /// Byte offsets of the non-printable characters that only a quoted
    /// scalar may hold, in order, and the first one not yet checked.
    quoted_only: Vec<usize>,
    quoted_only_next: usize,
}

impl<'a> Scanner<'a> {
    /// Create a scanner and check the input-size limit.
    pub(crate) fn new(input: &'a str, limits: Limits) -> Result<Self, Error> {
        Self::with_compat(input, limits, false)
    }

    pub(crate) fn with_compat(
        input: &'a str,
        limits: Limits,
        compat_flow: bool,
    ) -> Result<Self, Error> {
        limits.validate()?;
        limits.check_input(input.len()).map_err(|error| {
            Error::from(error).with_span(Span::empty(crate::span::Marker::new(0, 1, 1)))
        })?;
        let quoted_only = match non_printable_characters(input) {
            Ok(quoted_only) => quoted_only,
            Err(index) => {
                let marker = if compat_flow {
                    LineIndex::counting_line_boms(input).marker(input, index)
                } else {
                    LineIndex::new(input).marker(input, index)
                };
                let error = non_printable_error(input, index);
                return Err(match marker {
                    Some(marker) => error.with_span(Span::empty(marker)),
                    None => error,
                });
            }
        };
        let initial_bom = input.starts_with('\u{feff}');
        let pos = if initial_bom { 3 } else { 0 };
        Ok(Self {
            input,
            pos,
            line_index: if compat_flow {
                LineIndex::counting_line_boms(input)
            } else {
                LineIndex::new(input)
            },
            limits,
            tokens: 0,
            anchors: 0,
            flow_level: 0,
            flow_stack: Vec::new(),
            short_flow_scalar: None,
            last_token_end: 0,
            flow_allow_zero_indent: Vec::new(),
            previous_flow_end: false,
            yaml_directive_seen: false,
            tag_handles: HashMap::new(),
            tag_expansion_bytes: 0,
            pending_tag_handles: Vec::new(),
            started: false,
            finished: false,
            initial_bom,
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
            block_prefix: (usize::MAX, false),
            checked_line_end: 0,
            line_marks: (usize::MAX, 0),
            counted_indent: (usize::MAX, 0),
            flow_start: 0,
            flow_owner: (usize::MAX, Owner::Unknown, None),
            quoted_only,
            quoted_only_next: 0,
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

    /// Return the next token, or `None` after the stream end has been emitted.
    pub(crate) fn next_token(&mut self) -> Result<Option<Token<'a>>, Error> {
        if !self.started {
            self.started = true;
            return self.make(TokenKind::StreamStart, self.pos, self.pos);
        }
        if self.finished {
            return Ok(None);
        }

        loop {
            self.skip_insignificant()?;
            if self.pos >= self.input.len() {
                self.finished = true;
                return self.make(TokenKind::StreamEnd, self.pos, self.pos);
            }
            if self.at_line_start()
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
                    return Err(
                        self.error_at(self.pos, "a byte order mark can only start a document")
                    );
                }
                self.pos += '\u{feff}'.len_utf8();
                if !self.compat_flow {
                    self.after_line_bom = self.pos;
                }
                continue;
            }
            if self.peek().is_some_and(is_line_break) {
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
                    "document markers are not allowed inside flow collections",
                ));
            }
        }
        if self.flow_level == 0 && self.at_line_start() {
            if let Some(token) = self.scan_document_marker()? {
                return Ok(Some(token));
            }
            if self.peek() == Some('%') {
                return self.scan_directive();
            }
        }

        let start = self.pos;
        let ch = self.peek().expect("checked EOF");
        match ch {
            '[' => {
                if self.flow_level == 0 {
                    self.flow_start = start;
                }
                self.flow_level += 1;
                self.flow_stack.push(('[', start, false));
                self.flow_allow_zero_indent
                    .push(self.may_continue_at_column_zero());
                self.limits
                    .check_depth(self.flow_level)
                    .map_err(|error| self.limit_at(start, error))?;
                self.pos += 1;
                self.make(TokenKind::FlowSequenceStart, start, self.pos)
            }
            ']' => {
                self.pos += 1;
                self.flow_level = self.flow_level.saturating_sub(1);
                self.flow_stack.pop();
                self.flow_allow_zero_indent.pop();
                self.make(TokenKind::FlowSequenceEnd, start, self.pos)
            }
            '{' => {
                if self.flow_level == 0 {
                    self.flow_start = start;
                }
                self.flow_level += 1;
                self.flow_stack.push(('{', start, false));
                self.flow_allow_zero_indent
                    .push(self.may_continue_at_column_zero());
                self.limits
                    .check_depth(self.flow_level)
                    .map_err(|error| self.limit_at(start, error))?;
                self.pos += 1;
                self.make(TokenKind::FlowMappingStart, start, self.pos)
            }
            '}' => {
                self.pos += 1;
                self.flow_level = self.flow_level.saturating_sub(1);
                self.flow_stack.pop();
                self.flow_allow_zero_indent.pop();
                self.make(TokenKind::FlowMappingEnd, start, self.pos)
            }
            ',' => {
                if let Some(top) = self.flow_stack.last_mut() {
                    top.2 = false;
                }
                self.pos += 1;
                self.make(TokenKind::FlowEntry, start, self.pos)
            }
            '-' if self.indicator_follows(start, 1) => {
                self.pos += 1;
                self.make_indicator(TokenKind::BlockEntry, start, self.pos)
            }
            // In flow context libyaml treats every `?` as an explicit-key
            // indicator. YAML 1.2.2 makes `?x` a plain scalar (HM87, 652Z);
            // the compatibility scanner keeps the libyaml reading.
            '?' if (self.compat_flow && self.flow_level > 0)
                || self.indicator_follows(start, 1) =>
            {
                if let Some(top) = self.flow_stack.last_mut() {
                    top.2 = true;
                }
                self.pos += 1;
                self.make_indicator(TokenKind::Key, start, self.pos)
            }
            ':' if self.colon_indicator(start)
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
                self.make_indicator(TokenKind::Value, start, self.pos)
            }
            '&' => self.scan_anchor(false),
            '*' => self.scan_anchor(true),
            '!' => self.scan_tag(),
            '\'' => self.scan_single_quoted(),
            '"' => self.scan_double_quoted(),
            '|' => self.scan_block_scalar(start, true),
            '>' => self.scan_block_scalar(start, false),
            // Indicators that cannot start a plain scalar (`ns-plain-first`);
            // libyaml rejects them too.
            '%' | '@' | '`' => Err(self.error_at(
                start,
                format!("{ch:?} is a reserved indicator and cannot start a plain scalar"),
            )),
            _ => self.scan_plain(),
        }
    }

    /// Every non-printable character the scanner has passed must sit inside
    /// `quoted`, the quoted scalar being made, if any.
    fn check_quoted_only(&mut self, quoted: Option<std::ops::Range<usize>>) -> Result<(), Error> {
        while let Some(&index) = self.quoted_only.get(self.quoted_only_next) {
            if index >= self.pos {
                break;
            }
            if !quoted.as_ref().is_some_and(|range| range.contains(&index)) {
                let error = non_printable_error(self.input, index);
                return Err(match self.line_index.marker(self.input, index) {
                    Some(marker) => error.with_span(Span::empty(marker)),
                    None => error,
                });
            }
            self.quoted_only_next += 1;
        }
        Ok(())
    }

    /// Count a token that a reader keeps against `max_events`.
    fn count_token(&mut self, start: usize) -> Result<(), Error> {
        self.tokens += 1;
        self.limits
            .check_events(self.tokens)
            .map_err(|error| self.limit_at(start, error))
    }

    /// Emit a block indicator. These tokens only advance the token count,
    /// mark the document open, and clear flow adjacency.
    fn make_indicator(
        &mut self,
        kind: TokenKind<'a>,
        start: usize,
        end: usize,
    ) -> Result<Option<Token<'a>>, Error> {
        self.count_token(start)?;
        if self.quoted_only_next < self.quoted_only.len() {
            self.check_quoted_only(None)?;
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
    fn make_line_break(&mut self, start: usize, end: usize) -> Result<Option<Token<'a>>, Error> {
        if self.quoted_only_next < self.quoted_only.len() {
            self.check_quoted_only(None)?;
        }
        self.previous_flow_end = false;
        let span = self.span(start, end)?;
        Ok(Some(Token {
            kind: TokenKind::LineBreak,
            span,
        }))
    }

    fn make(
        &mut self,
        kind: TokenKind<'a>,
        start: usize,
        end: usize,
    ) -> Result<Option<Token<'a>>, Error> {
        // No reader keeps the stream's start and end tokens: they become its
        // stream events, which count as events.
        if !matches!(kind, TokenKind::StreamStart | TokenKind::StreamEnd) {
            self.count_token(start)?;
        }
        if self.quoted_only_next < self.quoted_only.len() {
            let quoted = matches!(
                kind,
                TokenKind::Scalar {
                    style: ScalarStyle::SingleQuoted | ScalarStyle::DoubleQuoted,
                    ..
                }
            );
            self.check_quoted_only(quoted.then_some(start..end))?;
        }
        // A scalar's size is the larger of its text and its value: `\L` and
        // `\P` take 2 bytes of text and 3 of value.
        if let TokenKind::Scalar { value, raw, .. } = &kind {
            self.limits
                .check_scalar_bytes(raw.len().max(value.len()))
                .map_err(|error| self.limit_at(start, error))?;
        }
        self.last_token_end = end;
        self.previous_flow_end = matches!(
            kind,
            TokenKind::FlowSequenceEnd
                | TokenKind::FlowMappingEnd
                | TokenKind::Scalar {
                    style: ScalarStyle::SingleQuoted | ScalarStyle::DoubleQuoted,
                    ..
                }
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
            TokenKind::Directive { .. } => self.directive_pending = true,
            TokenKind::Tag(_) | TokenKind::Anchor(_) => self.document_open = true,
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

    fn span(&self, start: usize, end: usize) -> Result<Span, Error> {
        self.line_index
            .span(self.input, start, end)
            .ok_or_else(|| self.error_at(start, "invalid span position"))
    }

    fn limit_at(&self, index: usize, limit: crate::limits::LimitExceeded) -> Error {
        let mut error = Error::from(limit);
        if let Some(marker) = self.line_index.marker(self.input, index) {
            error = error.with_span(Span::empty(marker));
        }
        error
    }

    fn error_at(&self, index: usize, message: impl Into<String>) -> Error {
        let span = self.line_index.marker(self.input, index).map(Span::empty);
        let mut error = Error::new(ErrorKind::Scanner, message);
        if let Some(span) = span {
            error = error.with_span(span);
        }
        error
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
        if self.pos == 0 || self.pos == self.after_line_bom {
            return true;
        }
        let previous = self.input.as_bytes().get(self.pos - 1).copied();
        if matches!(previous, Some(b'\n' | b'\r')) {
            return true;
        }
        if previous.is_some_and(|byte| byte < 0x80) {
            return false;
        }
        self.input[..self.pos]
            .chars()
            .next_back()
            .is_some_and(is_line_break)
    }

    fn indicator_follows(&self, start: usize, offset: usize) -> bool {
        let after = start + offset;
        let Some(rest) = self.input.get(after..) else {
            return true;
        };
        // A flow indicator only ends `-` or `?` inside a flow collection; in
        // block context `-,` and `?]` are plain scalars.
        match rest.chars().next() {
            None => true,
            Some(ch) => is_blank(ch) || (self.flow_level > 0 && matches!(ch, ',' | ']' | '}')),
        }
    }

    fn colon_indicator(&self, start: usize) -> bool {
        let after = start + 1;
        match self.input.get(after..).and_then(|rest| rest.chars().next()) {
            None => true,
            Some(ch) => is_blank(ch) || (self.flow_level > 0 && matches!(ch, ',' | ']' | '}')),
        }
    }

    fn skip_insignificant(&mut self) -> Result<(), Error> {
        loop {
            if self.pos >= self.input.len() {
                return Ok(());
            }
            let ch = self.peek().expect("checked EOF");
            match ch {
                ' ' => self.pos += 1,
                '\t' => {
                    if self.flow_level == 0 && self.pos >= self.tab_run_end {
                        self.tab_run_end = self.end_of_blanks(self.pos);
                        if self.tab_indents_block_collection(self.pos, self.tab_run_end) {
                            return Err(self.error_at(
                                self.pos,
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
                            "tabs cannot be the only flow-collection indentation",
                        ));
                    }
                    self.pos += 1;
                }
                '#' if self.comment_is_legal() => {
                    while let Some(ch) = self.peek() {
                        if is_line_break(ch) {
                            break;
                        }
                        if ch == '\u{feff}' && !self.compat_flow {
                            return Err(self.error_at(
                                self.pos,
                                "a byte order mark can only start a document",
                            ));
                        }
                        self.pos += ch.len_utf8();
                    }
                }
                _ if is_line_break(ch) => return Ok(()),
                _ => return Ok(()),
            }
        }
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
        let line_start = self.line_index.line_start(blanks_start);
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
        // the line too (YAML 1.2.2 example 5.1).
        let after_line_start_bom = previous == Some('\u{feff}')
            && (self.pos == self.after_line_bom || {
                let bom = self.pos - '\u{feff}'.len_utf8();
                bom == 0 || self.input[..bom].ends_with(['\n', '\r'])
            });
        previous.is_none_or(is_blank)
            || after_line_start_bom
            || (self.compat_flow
                && self.flow_level > 0
                && previous.is_some_and(|ch| matches!(ch, '[' | '{' | ',')))
    }

    fn consume_line_break(&mut self) {
        let ch = self.peek().expect("line break");
        self.pos += ch.len_utf8();
        if ch == '\r' && self.peek() == Some('\n') {
            self.pos += 1;
        }
    }

    fn scan_document_marker(&mut self) -> Result<Option<Token<'a>>, Error> {
        let start = self.pos;
        // In compat, a stream-start BOM is content for marker detection, so
        // `BOM---` is a plain scalar rather than an explicit document start.
        // By default a BOM is not content.
        if self.compat_flow && start == 3 && self.initial_bom {
            return Ok(None);
        }
        let rest = &self.input[start..];
        if rest.starts_with("---") && self.marker_follows(start, 3) {
            self.pos += 3;
            self.yaml_directive_seen = false;
            self.colon_anchors = None;
            self.tag_handles.clear();
            for (handle, prefix) in self.pending_tag_handles.drain(..) {
                self.tag_handles.insert(handle, prefix);
            }
            return self.make(TokenKind::DocumentStart { explicit: true }, start, self.pos);
        }
        if rest.starts_with("...") && self.marker_follows(start, 3) {
            self.pos += 3;
            self.yaml_directive_seen = false;
            self.colon_anchors = None;
            self.tag_handles.clear();
            self.pending_tag_handles.clear();
            return self.make(TokenKind::DocumentEnd, start, self.pos);
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
            && self.line_index.line_start(position) == position
            && self.marker_follows(position, 3)
    }

    fn scan_directive(&mut self) -> Result<Option<Token<'a>>, Error> {
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
            return Err(self.error_at(start, "a directive needs a name"));
        }
        let name = Cow::Borrowed(&self.input[name_start..self.pos]);
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
        let value = Cow::Borrowed(self.input[value_start..self.pos].trim_end_matches(is_blank));
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
            return Err(self.error_at(start, "a directive cannot contain a byte order mark"));
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
                return Err(self.error_at(start, "invalid %YAML directive"));
            }
            if self.yaml_directive_seen {
                return Err(self.error_at(start, "duplicate %YAML directive"));
            }
            self.yaml_directive_seen = true;
        }
        if name == "TAG" {
            let mut parts = value.split(is_blank).filter(|part| !part.is_empty());
            // Only a comment may follow the prefix, and the split leaves a blank before it.
            let (Some(handle), Some(prefix)) = (parts.next(), parts.next()) else {
                return Err(self.error_at(start, "%TAG requires a handle and a prefix"));
            };
            if parts.next().is_some_and(|part| !part.starts_with('#')) {
                return Err(self.error_at(start, "%TAG requires a handle and a prefix"));
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
                return Err(self.error_at(start, format!("invalid %TAG handle {handle}")));
            }
            // One document may give a handle one prefix, even the same one twice (YAML 1.2.2
            // section 6.8.2.2).
            if self
                .pending_tag_handles
                .iter()
                .any(|(seen, _)| seen == handle)
            {
                return Err(self.error_at(start, format!("duplicate %TAG directive for {handle}")));
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
                return Err(self.error_at(start, format!("invalid %TAG prefix {prefix}")));
            }
            // The prefix's %-escapes are decoded like a suffix's, as libyaml
            // does. `uri_text_ok` has checked their syntax.
            let prefix = decode_percent_escapes(prefix)
                .map_err(|_| self.error_at(start, "tag %-escapes do not form valid UTF-8"))?;
            self.pending_tag_handles.push((handle.to_owned(), prefix));
        }
        self.make(TokenKind::Directive { name, value }, start, self.pos)
    }

    fn scan_anchor(&mut self, alias: bool) -> Result<Option<Token<'a>>, Error> {
        let start = self.pos;
        self.pos += 1;
        let name_start = self.pos;
        while let Some(ch) = self.peek() {
            if is_blank(ch) || matches!(ch, '[' | ']' | '{' | '}' | ',') {
                break;
            }
            // Anchor names are `ns-char`s, which exclude the BOM; libyaml
            // allows only word characters.
            if ch == '\u{feff}' {
                return Err(
                    self.error_at(self.pos, "an anchor name cannot contain a byte order mark")
                );
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
                break;
            }
            if self.compat_flow && self.flow_level == 0 && ch == ':' && self.pos > name_start {
                let after = self.input[self.pos + 1..].chars().next();
                if after.is_some_and(|next| {
                    !is_blank(next) && !matches!(next, '[' | ']' | '{' | '}' | ',')
                }) {
                    break;
                }
            }
            self.pos += ch.len_utf8();
        }
        if self.pos == name_start {
            return Err(self.error_at(start, "anchor/alias name must not be empty"));
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
                "an anchor or alias must be followed by white space",
            ));
        }
        let name = Cow::Borrowed(&self.input[name_start..self.pos]);
        if alias {
            self.make(TokenKind::Alias(name), start, self.pos)
        } else {
            self.anchors += 1;
            self.limits
                .check_anchors(self.anchors)
                .map_err(|error| self.limit_at(start, error))?;
            if self.compat_flow && name.ends_with(':') {
                self.colon_anchors
                    .get_or_insert_with(HashMap::new)
                    .entry(&self.input[name_start..self.pos])
                    .or_insert(start);
            }
            self.make(TokenKind::Anchor(name), start, self.pos)
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
    fn expand_tag_handle(&mut self, start: usize, handle: &str) -> Result<Option<String>, Error> {
        let prefix = match (self.tag_handles.get(handle), handle) {
            (Some(prefix), _) => prefix.as_str(),
            (None, "!") => "!",
            (None, "!!") => CORE_TAG_PREFIX,
            (None, _) => return Ok(None),
        };
        let total = self.tag_expansion_bytes.saturating_add(prefix.len());
        self.limits
            .check_tag_expansion(total)
            .map_err(|error| self.limit_at(start, error))?;
        let prefix = prefix.to_owned();
        self.tag_expansion_bytes = total;
        Ok(Some(prefix))
    }

    fn scan_tag(&mut self) -> Result<Option<Token<'a>>, Error> {
        let start = self.pos;
        self.pos += 1;
        // A shorthand's handle needs a suffix after it (YAML 1.2.2 production 99), so `!!` and
        // `!e!` alone are errors. `!` alone is the non-specific tag.
        let mut no_suffix = false;
        let tag = if self.peek() == Some('!') {
            self.pos += 1;
            let suffix_start = self.pos;
            self.consume_tag_suffix();
            let prefix = self
                .expand_tag_handle(start, "!!")?
                .ok_or_else(|| self.error_at(start, "undefined secondary tag handle"))?;
            no_suffix = self.pos == suffix_start;
            Tag::new(prefix, self.decode_tag_suffix(suffix_start)?)
        } else if self.peek() == Some('<') {
            self.pos += 1;
            let suffix_start = self.pos;
            while let Some(ch) = self.peek() {
                if ch == '>' {
                    break;
                }
                if !is_uri_char(ch) {
                    return Err(self.error_at(
                        self.pos,
                        "a verbatim tag may only hold URI characters; escape others as %XX",
                    ));
                }
                self.pos += ch.len_utf8();
            }
            if self.peek() != Some('>') {
                return Err(self.error_at(start, "unterminated verbatim tag"));
            }
            let suffix = &self.input[suffix_start..self.pos];
            // A verbatim tag holds at least one character (production 97), and `!` alone names
            // no local tag (example 6.25).
            if suffix.is_empty() || suffix == "!" {
                return Err(self.error_at(start, "a verbatim tag needs a name"));
            }
            self.pos += 1;
            Tag::new("", suffix)
        } else {
            let handle_start = self.pos;
            while let Some(ch) = self.peek() {
                if ch == '!' || !is_tag_char(ch) {
                    break;
                }
                self.pos += ch.len_utf8();
            }
            if self.peek() == Some('!') {
                self.pos += 1;
                let suffix_start = self.pos;
                self.consume_tag_suffix();
                let handle = &self.input[handle_start - 1..suffix_start];
                let prefix = self.expand_tag_handle(start, handle)?.ok_or_else(|| {
                    self.error_at(start, format!("undefined tag handle {handle:?}"))
                })?;
                no_suffix = self.pos == suffix_start;
                Tag::new(prefix, self.decode_tag_suffix(suffix_start)?)
            } else {
                // `!` alone is the non-specific tag, which no `%TAG !` changes.
                let prefix = if self.pos == handle_start {
                    "!".to_owned()
                } else {
                    self.expand_tag_handle(start, "!")?
                        .ok_or_else(|| self.error_at(start, "undefined primary tag handle"))?
                };
                Tag::new(prefix, self.decode_tag_suffix(handle_start)?)
            }
        };
        if self.peek().is_some_and(|ch| {
            !(is_blank(ch) || (self.flow_level > 0 && matches!(ch, ',' | ']' | '}')))
        }) {
            return Err(self.error_at(
                self.pos,
                "a tag must be followed by whitespace or a line break",
            ));
        }
        if no_suffix {
            return Err(self.error_at(start, "a tag's handle needs a suffix after it"));
        }
        self.make(TokenKind::Tag(tag), start, self.pos)
    }

    /// The shorthand tag suffix from `start` to the current position, with
    /// `%XX` escapes decoded (YAML 1.2.2 §6.8.2; suite 6CK3). Verbatim tags
    /// are delivered as written.
    fn decode_tag_suffix(&self, start: usize) -> Result<String, Error> {
        decode_percent_escapes(&self.input[start..self.pos]).map_err(|error| match error {
            PercentError::Escape(index) => self.error_at(start + index, "invalid %-escape in tag"),
            PercentError::Utf8 => self.error_at(start, "tag %-escapes do not form valid UTF-8"),
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

    fn scan_single_quoted(&mut self) -> Result<Option<Token<'a>>, Error> {
        let start = self.pos;
        let mut held = None;
        self.pos += 1;
        let mut value = String::new();
        loop {
            let Some(ch) = self.peek() else {
                return Err(self.error_at(start, "unterminated single-quoted scalar"));
            };
            match ch {
                '\'' => {
                    if self.peek_second() == Some('\'') {
                        value.push('\'');
                        self.pos += 2;
                    } else {
                        self.pos += 1;
                        break;
                    }
                }
                ch if is_line_break(ch) => {
                    self.scan_quoted_line_break(&mut value, '\'', start, &mut held, 0, false)?;
                }
                ch => {
                    value.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        let raw_text = &self.input[start..self.pos];
        let raw = Cow::Borrowed(raw_text);
        let value = scalar_cow(
            value,
            raw_text
                .get(1..raw_text.len().saturating_sub(1))
                .unwrap_or(""),
        );
        self.make(
            TokenKind::Scalar {
                value,
                raw,
                style: ScalarStyle::SingleQuoted,
            },
            start,
            self.pos,
        )
    }

    /// Whether the lines of a flow node starting here may start at column 0. That holds in a
    /// document's root node and whatever it nests. Under a block collection they must be
    /// indented (YAML 1.2.2 productions 69, 137, and 141).
    fn may_continue_at_column_zero(&self) -> bool {
        self.flow_allow_zero_indent
            .last()
            .copied()
            .unwrap_or(!self.node_seen)
    }

    /// The indent the lines of a quoted scalar that starts at `start` are held to, and whether they
    /// may sit at exactly it. The block collection that holds the scalar, or the outermost flow
    /// collection it is in, sets it: in `k:\n  'a\n b'` and `k: [\n    'a\n b']` the lines need only
    /// pass `k`'s column (YAML 1.2.2 production 69). After a compact key or entry the collection
    /// sits right of the line's indent, so by default in `- k: 'a\n   b'` and `- - 'a\n   b'` the
    /// lines need three spaces. Where the collection isn't known, or isn't left of the scalar's
    /// line, or after a compact key in compat, the line sets it.
    fn quoted_indent(&mut self, start: usize) -> (usize, bool) {
        if self.may_continue_at_column_zero() {
            return (0, true);
        }
        let line_start = self.line_index.line_start(start);
        let prefix = self.input[line_start..start].trim_end_matches(is_blank);
        let prefix = prefix.strip_prefix('\u{feff}').unwrap_or(prefix);
        let (counted_line, mut indent) = self.counted_indent;
        if counted_line != line_start {
            // A mark's column doesn't count. libyaml has no such check, so it shouldn't get
            // stricter because of a mark.
            indent = leading_spaces(self.after_line_boms(line_start, self.input.len()));
            self.counted_indent = (line_start, indent);
        }
        let (owner, compact) = if self.flow_level > 0 {
            if self.flow_owner.0 != self.flow_start {
                self.flow_owner = (
                    self.flow_start,
                    self.owner_of(self.flow_start),
                    self.compact_column(self.flow_start),
                );
            }
            (self.flow_owner.1, self.flow_owner.2)
        } else {
            (self.owner_of(start), self.compact_column(start))
        };
        // Compat holds the lines to the line's indent there, as libyaml has no such check and
        // real files align them with the key, as in `- k: "a\n  b"`.
        if let Some(column) = compact
            && column > indent
            && !self.compat_flow
        {
            return (column, false);
        }
        match owner {
            Owner::Block(parent) if parent < indent => (parent, false),
            _ => (indent, prefix.is_empty()),
        }
    }

    /// The column of the block collection that a compact key or entry before `at` on its line
    /// opens, as `k` in `- k: 'a` or the inner `-` in `- - 'a`. Only default mode holds a quoted
    /// scalar to it, and there no mark takes a column.
    fn compact_column(&self, at: usize) -> Option<usize> {
        let line_start = self.line_index.line_start(at);
        match self.shape_of_line(line_start, &self.input[line_start..at]) {
            LineShape::Owner(Owner::Block(column)) => Some(column),
            _ => None,
        }
    }

    /// The spaces that a line of the outermost open flow collection needs before its content: one
    /// more than the column of the innermost block collection holding it, as `k` in
    /// `a:\n  k: [b,\n   c]` or in `- k: [b,\n   c]` (YAML 1.2.2 production 69). `None` where its
    /// lines may start at column 0, at the root, or where that collection isn't known.
    fn flow_line_indent(&mut self) -> Option<usize> {
        if self.may_continue_at_column_zero() {
            return None;
        }
        if self.flow_owner.0 != self.flow_start {
            self.flow_owner = (
                self.flow_start,
                self.owner_of(self.flow_start),
                self.compact_column(self.flow_start),
            );
        }
        match self.flow_owner.1 {
            Owner::Block(column) => Some(column + 1),
            _ => None,
        }
    }

    /// The error for a line of the innermost open flow collection, starting its content at `at`,
    /// that isn't indented enough. Such a line most often follows a collection left open, so the
    /// error names where that collection opened, as the parser's flow errors do.
    fn flow_indent_error(&self, at: usize) -> Error {
        let error = self.error_at(at, "flow collection entries must be indented");
        let Some(&(open, opened, _)) = self.flow_stack.last() else {
            return error;
        };
        let name = if open == '[' { "sequence" } else { "mapping" };
        match self.line_index.marker(self.input, opened) {
            Some(there) => error.with_context(&format!(
                ", in the flow {name} opened at line {} column {}",
                there.line(),
                there.column()
            )),
            None => error,
        }
    }

    /// Fold the line breaks at the current position into a quoted scalar's `value`, and check the
    /// next line's indentation. `held` holds the scalar's `quoted_indent` once a line break has
    /// asked for it, so a scalar that ends on its line never works it out.
    fn scan_quoted_line_break(
        &mut self,
        value: &mut String,
        quote: char,
        start: usize,
        held: &mut Option<(usize, bool)>,
        protected_len: usize,
        escaped: bool,
    ) -> Result<(), Error> {
        // White space before an escaped line break is content.
        while !escaped && value.len() > protected_len && value.ends_with([' ', '\t']) {
            value.pop();
        }
        let mut breaks = 0;
        loop {
            while matches!(self.peek(), Some(' ' | '\t')) {
                self.pos += 1;
            }
            if self.peek().is_some_and(is_line_break) {
                self.consume_line_break();
                breaks += 1;
            } else {
                break;
            }
        }
        if breaks == 0 {
            return Ok(());
        }
        let (start_indent, allow_same_indent) = match *held {
            Some(known) => known,
            None => *held.insert(self.quoted_indent(start)),
        };
        let line_start = self.input[..self.pos]
            .rfind(['\n', '\r'])
            .map_or(0, |index| index + 1);
        let indentation = &self.input[line_start..self.pos];
        let indent = indentation
            .chars()
            .take_while(|ch| *ch == ' ' || *ch == '\t')
            .count();
        // A closing quote may sit at any indentation, as a closing `]` or `}` may, so tabs alone
        // can come before it. Only this scalar's quote closes it, and in a single-quoted scalar
        // `''` is a quote in the text.
        let closes =
            self.peek() == Some(quote) && (quote == '"' || self.peek_second() != Some('\''));
        if !allow_same_indent && !closes && indentation.contains('\t') && !indentation.contains(' ')
        {
            return Err(self.error_at(
                line_start,
                "tabs cannot be the only quoted-scalar indentation",
            ));
        }
        let rest = &self.input[self.pos..];
        if indent == 0
            && (rest.starts_with("---") || rest.starts_with("..."))
            && rest
                .get(3..)
                .is_none_or(|tail| tail.chars().next().is_none_or(is_blank))
        {
            return Err(self.error_at(self.pos, "document marker inside quoted scalar"));
        }
        // Only the closing quote is exempt: a line that starts with the other kind of quote, or
        // with `''` in a single-quoted scalar, is text and is held to the indent.
        if !closes && (indent < start_indent || (!allow_same_indent && indent == start_indent)) {
            return Err(self.error_at(
                self.pos,
                "quoted scalar continuation must be indented more than its parent",
            ));
        }
        // A folded break is a space; each empty line after it, or after an
        // escaped break, is a line feed (YAML 1.2.2 productions 112, 73).
        if breaks == 1 && !escaped {
            value.push(' ');
        } else {
            for _ in 1..breaks {
                value.push('\n');
            }
        }
        Ok(())
    }

    fn scan_double_quoted(&mut self) -> Result<Option<Token<'a>>, Error> {
        let start = self.pos;
        let mut held = None;
        self.pos += 1;
        let mut value = String::new();
        let mut protected_len = 0;
        loop {
            let Some(ch) = self.peek() else {
                return Err(self.error_at(start, "unterminated double-quoted scalar"));
            };
            match ch {
                '"' => {
                    self.pos += 1;
                    break;
                }
                '\\' => {
                    let escape_start = self.pos;
                    self.pos += 1;
                    if self.peek().is_some_and(is_line_break) {
                        self.scan_quoted_line_break(
                            &mut value,
                            '"',
                            start,
                            &mut held,
                            protected_len,
                            true,
                        )?;
                        continue;
                    }
                    let Some(escape) = self.peek() else {
                        return Err(self.error_at(escape_start, "unterminated escape"));
                    };
                    self.pos += escape.len_utf8();
                    let before = value.len();
                    match escape {
                        '0' => value.push('\0'),
                        'a' => value.push('\u{0007}'),
                        'b' => value.push('\u{0008}'),
                        't' | '\t' => value.push('\t'),
                        'n' => value.push('\n'),
                        'v' => value.push('\u{000b}'),
                        'f' => value.push('\u{000c}'),
                        'r' => value.push('\r'),
                        'e' => value.push('\u{001b}'),
                        ' ' => value.push(' '),
                        '"' => value.push('"'),
                        '/' => value.push('/'),
                        '\\' => value.push('\\'),
                        'N' => value.push('\u{0085}'),
                        '_' => value.push('\u{00a0}'),
                        'L' => value.push('\u{2028}'),
                        'P' => value.push('\u{2029}'),
                        'x' => value.push(self.scan_hex_escape(2, escape_start)?),
                        'u' => value.push(self.scan_hex_escape(4, escape_start)?),
                        'U' => value.push(self.scan_hex_escape(8, escape_start)?),
                        other => {
                            return Err(
                                self.error_at(escape_start, format!("unknown escape \\{other}"))
                            );
                        }
                    }
                    if value.len() > before && value.ends_with([' ', '\t']) {
                        protected_len = value.len();
                    }
                }
                ch if is_line_break(ch) => {
                    self.scan_quoted_line_break(
                        &mut value,
                        '"',
                        start,
                        &mut held,
                        protected_len,
                        false,
                    )?;
                }
                ch => {
                    value.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        let raw_text = &self.input[start..self.pos];
        let raw = Cow::Borrowed(raw_text);
        let value = scalar_cow(
            value,
            raw_text
                .get(1..raw_text.len().saturating_sub(1))
                .unwrap_or(""),
        );
        self.make(
            TokenKind::Scalar {
                value,
                raw,
                style: ScalarStyle::DoubleQuoted,
            },
            start,
            self.pos,
        )
    }

    fn scan_hex_escape(&mut self, digits: usize, escape_start: usize) -> Result<char, Error> {
        let mut value = 0_u32;
        for _ in 0..digits {
            let Some(ch) = self.peek() else {
                return Err(self.error_at(escape_start, "short hexadecimal escape"));
            };
            let Some(digit) = ch.to_digit(16) else {
                return Err(self.error_at(self.pos, "invalid hexadecimal escape"));
            };
            value = value * 16 + digit;
            self.pos += ch.len_utf8();
        }
        char::from_u32(value)
            .ok_or_else(|| self.error_at(escape_start, "escape is not a Unicode scalar value"))
    }

    fn scan_block_scalar(
        &mut self,
        start: usize,
        literal: bool,
    ) -> Result<Option<Token<'a>>, Error> {
        self.pos += 1;
        // At most one chomping and one indentation indicator, in either order, right after the
        // `|` or `>` (YAML 1.2.2 production 162). Only blanks and a comment may follow them.
        let mut chomping = None;
        let mut explicit_indent = None;
        loop {
            match self.peek() {
                Some(ch @ ('+' | '-')) if chomping.is_none() => {
                    chomping = Some(if ch == '+' {
                        Chomping::Keep
                    } else {
                        Chomping::Strip
                    });
                    self.pos += 1;
                }
                Some(ch @ '1'..='9') if explicit_indent.is_none() => {
                    explicit_indent = Some(ch.to_digit(10).expect("digit") as usize);
                    self.pos += 1;
                }
                _ => break,
            }
        }
        let chomping = chomping.unwrap_or(Chomping::Clip);
        loop {
            match self.peek() {
                Some(' ' | '\t') => self.pos += 1,
                Some('#') => {
                    let previous = self.input[..self.pos].chars().next_back();
                    if !previous.is_some_and(is_blank) {
                        return Err(self.error_at(
                            self.pos,
                            "a block scalar header comment requires preceding whitespace",
                        ));
                    }
                    while let Some(ch) = self.peek() {
                        if is_line_break(ch) {
                            break;
                        }
                        // A comment holds no mark (production 75), here as elsewhere. Compat
                        // reads one, as libyaml does.
                        if ch == '\u{feff}' && !self.compat_flow {
                            return Err(self.error_at(
                                self.pos,
                                "a byte order mark can only start a document",
                            ));
                        }
                        self.pos += ch.len_utf8();
                    }
                    break;
                }
                Some(ch) if is_line_break(ch) => break,
                Some(ch) => {
                    return Err(self.error_at(
                        self.pos,
                        format!("unexpected {ch:?} in block scalar header"),
                    ));
                }
                None => break,
            }
        }
        if self.peek().is_some_and(|ch| !is_line_break(ch)) {
            return Err(self.error_at(start, "block scalar header must end at a line break"));
        }
        let base_indent = self.line_indent(start);
        let line_start = self.input[..start]
            .rfind(['\n', '\r'])
            .map_or(0, |index| index + 1);
        let prefix = self.input[line_start..start].trim_matches(is_blank);
        let root_context = prefix.is_empty() || prefix == "---" || prefix == "...";
        if self.peek().is_some() {
            self.consume_line_break();
        }
        let content_start = self.pos;
        // Content indentation is relative to the block collection that owns
        // the scalar: in `- key: |2` that is the mapping at column 2, not the
        // line's indentation. Root scalars count from column 0, as libyaml
        // does.
        let (detect_base, allow_base, explicit_base) = match self.owner_of(start) {
            Owner::Root => (0, true, 0),
            Owner::Block(n) => (n, false, n),
            Owner::Unknown => (
                base_indent,
                root_context,
                if prefix.is_empty() { 0 } else { base_indent },
            ),
        };
        let indent = explicit_indent.map_or_else(
            || self.detect_block_indent(content_start, detect_base, allow_base),
            |digits| Ok(explicit_base + digits),
        )?;

        // When the indentation is detected, leading empty lines may not be
        // indented more than the first non-empty content line (YAML 1.2.2
        // section 8.1.1.1). With an indentation indicator, a "blank" line
        // with more spaces than that is content. Tabs on a leading
        // empty line are ambiguous and rejected as scalar indentation.
        let mut probe = content_start;
        let mut first_content = None;
        while probe < self.input.len() {
            let line_end = self.input[probe..]
                .find(['\n', '\r'])
                .map_or(self.input.len(), |offset| probe + offset);
            let line = &self.input[probe..line_end];
            if line.chars().all(|ch| ch == ' ' || ch == '\t') {
                if line.contains('\t') && !line.contains(' ') {
                    return Err(self.error_at(
                        probe,
                        "tabs are not allowed as leading block-scalar indentation",
                    ));
                }
                probe = line_end;
                if probe < self.input.len() {
                    let ch = self.input[probe..].chars().next().expect("line break");
                    probe += ch.len_utf8();
                    if ch == '\r' && self.input[probe..].starts_with('\n') {
                        probe += 1;
                    }
                }
                continue;
            }
            first_content = Some((probe, self.count_spaces(probe)));
            break;
        }
        if let Some((_, first_spaces)) = first_content.filter(|_| explicit_indent.is_none()) {
            let mut leading = content_start;
            while leading < probe {
                let line_end = self.input[leading..]
                    .find(['\n', '\r'])
                    .map_or(self.input.len(), |offset| leading + offset);
                let spaces = self.count_spaces(leading);
                if !self.input[leading..line_end].contains('\t') && spaces > first_spaces {
                    return Err(self.error_at(
                        leading,
                        "leading empty block-scalar lines are more indented than the content",
                    ));
                }
                let line_end = self.input[leading..]
                    .find(['\n', '\r'])
                    .map_or(self.input.len(), |offset| leading + offset);
                leading = line_end;
                if leading < self.input.len() {
                    let ch = self.input[leading..].chars().next().expect("line break");
                    leading += ch.len_utf8();
                    if ch == '\r' && self.input[leading..].starts_with('\n') {
                        leading += 1;
                    }
                }
            }
        }

        // The scalar's lines, each followed by its line break if it has one, built as they are
        // read. Holding each line in a string of its own cost dozens of times the input on short
        // lines, before the scalar limit could see the scalar.
        let mut text = String::new();
        // Whether the last line with any text has a line break, which clipping keeps.
        let mut final_break = false;
        let mut seen_content = false;
        let mut end = content_start;
        loop {
            if self.pos >= self.input.len() {
                break;
            }
            let line_start = self.pos;
            let spaces = self.count_spaces(line_start);
            let after_spaces = line_start + spaces;
            let next = self.input[after_spaces..].chars().next();
            let blank = next.is_none_or(is_line_break);
            if blank {
                if let Some(ch) = next {
                    self.pos = after_spaces + ch.len_utf8();
                    if ch == '\r' && self.peek() == Some('\n') {
                        self.pos += 1;
                    }
                } else {
                    self.pos = after_spaces;
                }
                // Spaces past the indentation are content, on a leading line
                // too when an indicator fixes the indentation.
                let content_spaces =
                    if (seen_content || explicit_indent.is_some()) && spaces > indent {
                        spaces - indent
                    } else {
                        0
                    };
                // The suite treats a whitespace-only last line at end of input
                // as a complete line (JEF9, L24T); libyaml drops its line
                // break, and the compatibility scanner keeps that.
                let at_eof_line = next.is_none() && spaces > 0 && !self.compat_flow;
                let had_break = next.is_some() || at_eof_line;
                text.extend(std::iter::repeat_n(' ', content_spaces));
                if had_break {
                    text.push('\n');
                }
                if content_spaces > 0 {
                    final_break = had_break;
                }
                end = self.pos;
                continue;
            }
            if spaces < indent {
                break;
            }
            if indent == 0 {
                // Document markers at column 0 end any block scalar, including
                // one whose header follows `---` on the same line. A column-0
                // `#` line is content in YAML 1.2.2 (DK3J). libyaml indents a
                // root block scalar by at least one space, so there it is a
                // comment ending the scalar before any content; the compat
                // scanner keeps that wherever libyaml accepts the input.
                // A column-0 byte order mark always ends it there: libyaml
                // reads the BOM's line outside the scalar, so what follows
                // the BOM is a comment or an error, never lost text.
                let rest = &self.input[line_start..];
                if (rest.starts_with("---") && self.marker_follows(line_start, 3))
                    || (rest.starts_with("...") && self.marker_follows(line_start, 3))
                    || (self.compat_flow
                        && (rest.starts_with('\u{feff}')
                            || (!seen_content && rest.starts_with('#'))))
                {
                    break;
                }
            }
            if !self.compat_flow && self.input[line_start..].starts_with('\u{feff}') {
                // A BOM line ends the scalar when it starts a document prefix;
                // otherwise the BOM is an error below.
                if self.bom_starts_prefix(line_start) {
                    break;
                }
            }
            self.pos = line_start + indent;
            let text_start = self.pos;
            while let Some(ch) = self.peek() {
                if is_line_break(ch) {
                    break;
                }
                if ch == '\u{feff}' && !self.compat_flow {
                    return Err(
                        self.error_at(self.pos, "a byte order mark can only start a document")
                    );
                }
                self.pos += ch.len_utf8();
            }
            // A content line holds at least the character that made it one.
            text.push_str(&self.input[text_start..self.pos]);
            let had_break = self.peek().is_some_and(is_line_break);
            if had_break {
                self.consume_line_break();
                text.push('\n');
            }
            final_break = had_break;
            seen_content = true;
            end = self.pos;
        }
        self.pos = end;

        let value = build_block_scalar(text, final_break, literal, chomping);
        let raw = Cow::Borrowed(&self.input[start..end]);
        self.make(
            TokenKind::Scalar {
                value: Cow::Owned(value),
                raw,
                style: if literal {
                    ScalarStyle::Literal
                } else {
                    ScalarStyle::Folded
                },
            },
            start,
            end,
        )
    }

    /// The block structure owning the node that starts at `start` (see
    /// [`Owner`]). Only meaningful in block context.
    fn owner_of(&self, start: usize) -> Owner {
        let line_start = self.line_index.line_start(start);
        match self.shape_of_line(line_start, &self.input[line_start..start]) {
            LineShape::Owner(owner) => owner,
            LineShape::Open => self.owner_from_previous_lines(line_start),
            LineShape::Content => Owner::Unknown,
        }
    }

    /// What `text`, which starts the line at `line_start`, says about ownership. An owner after
    /// a mark that takes up a column is one deeper.
    fn shape_of_line(&self, line_start: usize, text: &str) -> LineShape {
        let shape = line_shape(text, self.names());
        match shape {
            LineShape::Owner(Owner::Block(indent))
                if text
                    .strip_prefix('\u{feff}')
                    .is_some_and(|rest| self.mark_takes_column(line_start, rest)) =>
            {
                LineShape::Owner(Owner::Block(indent + 1))
            }
            shape => shape,
        }
    }

    /// Whether the mark that `after_mark` follows, at the start of the line at `line_start`,
    /// takes up a column. In compat a later line's does, as libyaml reads it. The first line's
    /// doesn't, unless a document marker follows: that's text only because the mark counts, so
    /// the rest of the line reads that way too. No mark takes up a column by default.
    fn mark_takes_column(&self, line_start: usize, after_mark: &str) -> bool {
        self.compat_flow && (line_start > 0 || starts_with_document_marker(after_mark))
    }

    /// For a node that starts its line (after optional properties), the
    /// owner is whatever the previous non-empty line opened.
    fn owner_from_previous_lines(&self, line_start: usize) -> Owner {
        let mut end = line_start;
        loop {
            if end == 0 || (end == 3 && self.initial_bom) {
                return Owner::Root;
            }
            let mut previous_end = end;
            if self.input[..previous_end].ends_with('\n') {
                previous_end -= 1;
            }
            if self.input[..previous_end].ends_with('\r') {
                previous_end -= 1;
            }
            if previous_end == end {
                // A Unicode line break; the line index knows where it starts.
                let start = self.line_index.line_start(end.saturating_sub(1));
                previous_end = self.input[start..end]
                    .char_indices()
                    .rev()
                    .find(|(_, ch)| !is_line_break(*ch))
                    .map_or(start, |(offset, ch)| start + offset + ch.len_utf8());
            }
            let previous_start = self.line_index.line_start(previous_end);
            let line = self.after_line_boms(previous_start, previous_end);
            // In compat, the line can still hold its mark, and a comment may follow that.
            let comment_after_mark = line
                .strip_prefix('\u{feff}')
                .is_some_and(|rest| rest.trim_start_matches(is_blank).starts_with('#'));
            let code = if comment_after_mark {
                ""
            } else {
                strip_comment(line, self.names())
            };
            if code.trim_matches(is_blank).is_empty() {
                end = previous_start;
                continue;
            }
            if code.starts_with('%') {
                return Owner::Unknown;
            }
            if code.starts_with("...") && code[3..].trim_matches(is_blank).is_empty() {
                return Owner::Root;
            }
            match self.shape_of_line(previous_start, code) {
                LineShape::Owner(owner) => return owner,
                LineShape::Open => end = previous_start,
                LineShape::Content => return Owner::Unknown,
            }
        }
    }

    fn line_indent(&self, position: usize) -> usize {
        let line_start = self.line_index.line_start(position);
        let text = self.after_line_boms(line_start, self.input.len());
        // The spaces after a mark count only if it takes up a column. Otherwise, in compat, this
        // stops at the mark. By default, no mark is left here.
        match text.strip_prefix('\u{feff}') {
            Some(rest) if self.mark_takes_column(line_start, rest) => 1 + leading_spaces(rest),
            _ => leading_spaces(text),
        }
    }

    /// The text of `start..end`, a line's start onward, past any byte order marks there. They
    /// take up no column, except in compat, which keeps libyaml's reading of them as a column.
    fn after_line_boms(&self, start: usize, end: usize) -> &'a str {
        // Most lines hold no mark, and this runs for many of them, so the rest stays out of line.
        if !self.compat_flow && self.input.as_bytes().get(start) == Some(&BOM_FIRST_BYTE) {
            return self.after_bom_run(start, end);
        }
        &self.input[start..end]
    }

    /// The same for a line that may start with marks, reading a run of them once.
    #[cold]
    fn after_bom_run(&self, start: usize, end: usize) -> &'a str {
        let (known_start, known_content) = self.bom_run.get();
        let content = if known_start == start {
            known_content
        } else {
            let trimmed = self.input[start..].trim_start_matches('\u{feff}');
            let content = self.input.len() - trimmed.len();
            self.bom_run.set((start, content));
            content
        };
        &self.input[content.min(end)..end]
    }

    fn count_spaces(&self, position: usize) -> usize {
        self.input[position..]
            .chars()
            .take_while(|ch| *ch == ' ')
            .count()
    }

    fn detect_block_indent(
        &self,
        content_start: usize,
        base_indent: usize,
        allow_base: bool,
    ) -> Result<usize, Error> {
        let mut position = content_start;
        while position < self.input.len() {
            let spaces = self.count_spaces(position);
            let after = position + spaces;
            let next = self.input[after..].chars().next();
            if next.is_none() {
                return Ok(base_indent + 1);
            }
            if next.is_some_and(is_line_break) {
                position = after + next.expect("checked").len_utf8();
                continue;
            }
            if spaces < base_indent || (!allow_base && spaces == base_indent) {
                // A dedented line ends the scalar; it is not an error. This is
                // how an empty block scalar is represented.
                return Ok(base_indent + 1);
            }
            return Ok(spaces);
        }
        Ok(base_indent + 1)
    }

    fn scan_plain(&mut self) -> Result<Option<Token<'a>>, Error> {
        let start = self.pos;
        self.short_flow_scalar = None;
        if self.peek() == Some('#') {
            return Err(self.error_at(
                start,
                "a comment must be separated from the preceding token",
            ));
        }
        // Most plain scalars end on their own line. Defer the owner analysis
        // (which scans the line prefix) until a line break proves the scalar
        // may continue.
        let mut continuation = None;
        // Keep the common single-line scalar borrowed: only create an owned
        // value when a continuation is actually accepted.
        let mut value: Option<String> = None;
        loop {
            let content_start = self.pos;
            // This loop runs for every character of a plain scalar, so it walks the text once
            // rather than slicing the input again for each character.
            let text = &self.input[content_start..];
            let in_flow = self.flow_level > 0;
            let mut content_end = self.input.len();
            for (offset, ch) in text.char_indices() {
                let position = content_start + offset;
                if is_line_break(ch)
                    || (ch == '#' && self.is_blank_before(position))
                    || (in_flow && matches!(ch, '[' | ']' | '{' | '}' | ','))
                    || (ch == ':' && self.colon_indicator(position))
                {
                    content_end = position;
                    break;
                }
                if ch == '\u{feff}' && !self.compat_flow {
                    self.pos = position;
                    return Err(
                        self.error_at(position, "a byte order mark can only start a document")
                    );
                }
            }
            self.pos = content_end;
            if let Some(value) = value.as_mut() {
                value.push_str(&self.input[content_start..self.pos]);
                while value.ends_with([' ', '\t']) {
                    value.pop();
                }
            }
            if !self.peek().is_some_and(is_line_break) {
                break;
            }
            // A scalar that starts after a block prefix cannot continue on a
            // line at or below the prefix's indentation. Reject that common
            // case before scanning the line prefix for an owner.
            if self.has_block_prefix(start)
                && self
                    .next_content_indent()
                    .is_some_and(|indent| indent <= self.line_indent(start))
            {
                break;
            }
            let continuation = match continuation {
                Some(continuation) => continuation,
                None => {
                    let computed = self.plain_continuation(start);
                    continuation = Some(computed);
                    computed
                }
            };
            let first_end = self.pos;
            let Some(breaks) = self.try_plain_continuation(continuation)? else {
                break;
            };
            if value.is_none() {
                let mut first = String::with_capacity(first_end.saturating_sub(start) + 1);
                first.push_str(&self.input[start..first_end]);
                while first.ends_with([' ', '\t']) {
                    first.pop();
                }
                value = Some(first);
            }
            let value = value.as_mut().expect("initialized");
            if breaks == 1 {
                value.push(' ');
            } else {
                for _ in 1..breaks {
                    value.push('\n');
                }
            }
        }
        let end = self.pos;
        if end == start {
            return Err(self.error_at(start, "expected a token"));
        }
        if let Some((_, line)) = self.short_flow_scalar {
            self.short_flow_scalar = Some((end, line));
        }
        let raw_text = &self.input[start..end];
        let raw = Cow::Borrowed(raw_text);
        let value = match value {
            Some(value) => Cow::Owned(value),
            None => Cow::Borrowed(&self.input[start..self.trim_trailing_whitespace(start, end)]),
        };
        self.make(
            TokenKind::Scalar {
                value,
                raw,
                style: ScalarStyle::Plain,
            },
            start,
            end,
        )
    }

    /// The indentation of the next line that isn't empty after the current position, without
    /// changing scanner state. A line of spaces counts as that line.
    fn next_content_indent(&self) -> Option<usize> {
        let mut index = self.pos;
        while index < self.input.len() {
            let ch = self.input[index..].chars().next().expect("in bounds");
            if !is_line_break(ch) {
                break;
            }
            index += ch.len_utf8();
            if ch == '\r' && self.input[index..].starts_with('\n') {
                index += 1;
            }
        }
        if index >= self.input.len() {
            return None;
        }
        let mut indent = 0;
        while self.input[index..].starts_with(' ') {
            index += 1;
            indent += 1;
        }
        Some(indent)
    }

    /// How a plain scalar starting at `start` may continue on later lines.
    fn plain_continuation(&self, start: usize) -> Continuation {
        if self.flow_level > 0 {
            // Continuation lines inside flow collections are not measured
            // against block indentation. Analyzing the line prefix per flow
            // item would also be quadratic on a long single-line collection.
            return Continuation::Flow;
        }
        match self.owner_of(start) {
            // A root plain scalar continues at any indentation.
            Owner::Root => Continuation::Indent(0),
            // Inside a block collection indented by `n`, continuation lines
            // need at least `n + 1` spaces.
            Owner::Block(n) => Continuation::Indent(n + 1),
            Owner::Unknown => {
                let line_start = self.line_index.line_start(start);
                let prefix = &self.input[line_start..start];
                Continuation::Line {
                    start_indent: self.line_indent(start),
                    standalone: prefix.chars().all(|ch| is_blank(ch) || ch == '\u{feff}'),
                    allow_block_indicator: !prefix.contains(':'),
                }
            }
        }
    }

    /// Whether a block indicator or a `:` value indicator comes before `start` on its line,
    /// outside compat and flow collections. A `: ` in a node property, as in `&a: b`, is part of
    /// its name. A scalar asks once for each of its lines, and this reads the text once.
    fn has_block_prefix(&mut self, start: usize) -> bool {
        if self.compat_flow || self.flow_level > 0 {
            return false;
        }
        if self.block_prefix.0 != start {
            let prefix = &self.input[self.line_index.line_start(start)..start];
            let (text, index, owner) = block_prefix(prefix, false);
            let found = matches!(owner, Some(Owner::Block(_)))
                || matches!(walk_line(&text[index..], self.names()), LineStop::Value(_));
            self.block_prefix = (start, found);
        }
        self.block_prefix.1
    }

    fn try_plain_continuation(
        &mut self,
        continuation: Continuation,
    ) -> Result<Option<usize>, Error> {
        let saved = self.pos;
        let mut breaks = 0;
        // In a flow collection under a block collection, a line needs one more space than that
        // collection's column, and a tab doesn't count. Compat refuses only a tab left of that
        // column on a line that the scalar goes on to, as libyaml does, so `k: [a\n\tb]` is an
        // error in both modes and `k: [a\nb]` by default only. A closing line, as in `k: [a\n\t]`,
        // still reads.
        let flow_indent = match continuation {
            Continuation::Flow => self.flow_line_indent(),
            _ => None,
        };
        loop {
            if !self.peek().is_some_and(is_line_break) {
                break;
            }
            self.consume_line_break();
            breaks += 1;
            while matches!(self.peek(), Some(' ' | '\t')) {
                self.pos += 1;
            }
            if self.peek().is_some_and(is_line_break) {
                continue;
            }
            break;
        }
        if breaks == 0 || self.pos >= self.input.len() {
            self.pos = saved;
            return Ok(None);
        }
        // In a flow collection, an indicator on the next line ends the scalar, and the line
        // breaks before it aren't part of the value.
        // A document marker line ends it too, and the flow collection then refuses the marker, as
        // in `[a\n--- b]`: no node holds one (YAML 1.2.2 production 206).
        if self.flow_level > 0
            && (matches!(self.peek(), Some(',' | ']' | '}'))
                || (self.peek() == Some(':') && self.colon_indicator(self.pos))
                || self.document_marker_starts_line(self.pos))
        {
            self.pos = saved;
            return Ok(None);
        }
        let line_start = self.line_index.line_start(self.pos);
        let indent = self.input[line_start..self.pos]
            .chars()
            .take_while(|ch| *ch == ' ')
            .count();
        let bom_line = self.input[self.pos..].starts_with('\u{feff}');
        let (allowed, allow_block_indicator) = match continuation {
            // By default a short line ends the scalar, and the flow collection then refuses it.
            Continuation::Flow => (
                self.compat_flow || flow_indent.is_none_or(|required| indent >= required),
                false,
            ),
            Continuation::Indent(minimum) => (indent >= minimum, true),
            Continuation::Line {
                start_indent,
                standalone,
                allow_block_indicator,
            } => (
                if standalone {
                    indent >= start_indent
                } else {
                    indent > start_indent
                },
                allow_block_indicator,
            ),
        };
        // A line starting with a BOM ends the scalar by default; the BOM is
        // then a document prefix or an error. libyaml continues a scalar onto
        // it wherever the indentation allows, whatever the line holds, as
        // compat does: a colon on the line then makes a multi-line key, which
        // is an error, and a `#` after the BOM isn't a comment.
        if !allowed
            || (bom_line && !self.compat_flow)
            || (!bom_line
                && self.flow_level == 0
                && self.looks_like_block_start(self.pos, allow_block_indicator))
            || self.peek() == Some('#')
        {
            self.pos = saved;
            return Ok(None);
        }
        if let Some(required) = flow_indent
            && let Some(tab) = self.input[line_start..self.pos].find('\t')
            && tab < required
        {
            // Only compat gets here.
            return Err(self.error_at(
                line_start + tab,
                "a tab cannot indent a flow collection's line under a block collection",
            ));
        }
        if flow_indent.is_some_and(|required| indent < required) {
            // Only compat gets here, and `scan_plain` sets where the scalar ends.
            self.short_flow_scalar = Some((usize::MAX, self.pos));
        }
        Ok(Some(breaks))
    }

    /// Whether the line that `position` starts the content of ends a plain scalar rather than
    /// continuing it: a document marker, or block structure. A `---` or `...` is a marker only at
    /// column 0 with whitespace or the line's end after it, and a comment's `:` isn't an indicator.
    fn looks_like_block_start(&self, position: usize, allow_block_indicator: bool) -> bool {
        let rest = &self.input[position..];
        if self.document_marker_starts_line(position) {
            return true;
        }
        if rest.starts_with(": ") {
            return true;
        }
        if !allow_block_indicator && (rest.starts_with("- ") || rest.starts_with("? ")) {
            return true;
        }
        let mut chars = rest.chars().peekable();
        let mut previous = self.input[..position].chars().next_back();
        while let Some(ch) = chars.next() {
            if is_line_break(ch) || (ch == '#' && previous.is_some_and(is_blank)) {
                break;
            }
            if ch == ':' && chars.peek().is_none_or(|next| is_blank(*next)) {
                return true;
            }
            previous = Some(ch);
        }
        false
    }

    fn is_blank_before(&self, position: usize) -> bool {
        self.input[..position]
            .chars()
            .next_back()
            .is_some_and(is_blank)
    }

    fn trim_trailing_whitespace(&self, start: usize, end: usize) -> usize {
        let mut trimmed = end;
        while trimmed > start {
            let ch = self.input[..trimmed].chars().next_back().expect("nonempty");
            if ch == ' ' || ch == '\t' {
                trimmed -= ch.len_utf8();
            } else {
                break;
            }
        }
        trimmed
    }
}

/// The block structure that owns a node, as far as the context-free scanner
/// can tell from the text around it.
///
/// YAML measures the indentation of a node's continuation lines (plain
/// scalars) and content lines (block scalars) against the indentation `n` of
/// the block collection that contains the node, not against the line the node
/// starts on. In `- - a` the inner sequence is indented by 2 even though the
/// line is indented by 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    /// A root node (the spec's indentation -1).
    Root,
    /// A node inside a block collection indented by this many columns.
    Block(usize),
    /// The surrounding text does not determine the owner.
    Unknown,
}

/// How far a plain scalar's continuation lines must be indented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Continuation {
    /// Inside a flow collection.
    Flow,
    /// At least this many spaces.
    Indent(usize),
    /// Fallback when the owner is unknown: measured against the scalar's own
    /// line. Standalone scalars may continue at their own indentation.
    Line {
        start_indent: usize,
        standalone: bool,
        allow_block_indicator: bool,
    },
}

/// What a line (or the part of a line before a node) says about ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineShape {
    /// The text ends with structure that owns what follows it.
    Owner(Owner),
    /// Only whitespace or node properties: look at the previous line.
    Open,
    /// The text ends with content, so it doesn't own what follows.
    Content,
}

/// The number of spaces that `text` starts with.
fn leading_spaces(text: &str) -> usize {
    text.len() - text.trim_start_matches(' ').len()
}

/// Where a line's key or node starts: `text` without a mark it starts with, the offset in that of
/// what follows its leading spaces, an optional `---`, and block indicators (`- `, `? `, `: `), and
/// the owner those say it has.
fn block_prefix(text: &str, compat: bool) -> (&str, usize, Option<Owner>) {
    // In compat, a `---` after a mark is text, as the scanner reads it.
    let (text, marker_possible) = if compat {
        text.strip_prefix('\u{feff}')
            .map_or((text, true), |rest| (rest, false))
    } else {
        (text.trim_start_matches('\u{feff}'), true)
    };
    let bytes = text.as_bytes();
    let separator = |index: usize| index >= bytes.len() || matches!(bytes[index], b' ' | b'\t');
    let skip_blanks = |mut index: usize| {
        while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
            index += 1;
        }
        index
    };
    let mut index = 0;
    while index < bytes.len() && bytes[index] == b' ' {
        index += 1;
    }
    let mut owner = None;
    if marker_possible && index == 0 && text.starts_with("---") && separator(3) {
        owner = Some(Owner::Root);
        index = skip_blanks(3);
    }
    while index < bytes.len() && matches!(bytes[index], b'-' | b'?' | b':') && separator(index + 1)
    {
        // Indicators are ASCII, and so is everything before them on the line,
        // so the byte offset is the column.
        owner = Some(Owner::Block(index));
        index = skip_blanks(index + 1);
    }
    (text, index, owner)
}

/// Classify `text`, which starts at a line start: what `block_prefix` reads, an optional implicit
/// key with its `:`, and node properties. The last indicator or key decides the owner. `names`
/// ends anchor and alias names where the scanner does.
fn line_shape(text: &str, names: NameRules<'_, '_>) -> LineShape {
    let (text, index, owner) = block_prefix(text, names.compat);
    let rest = &text[index..];
    if let LineStop::Value(colon) = walk_line(rest, names) {
        return if only_properties(&rest[colon + 1..], names.compat) {
            LineShape::Owner(Owner::Block(text[..index].chars().count()))
        } else {
            LineShape::Content
        };
    }
    if only_properties(rest, names.compat) {
        owner.map_or(LineShape::Open, LineShape::Owner)
    } else {
        LineShape::Content
    }
}

/// What decides where an anchor or alias name ends on a line that isn't being scanned.
#[derive(Clone, Copy)]
struct NameRules<'s, 'a> {
    compat: bool,
    colon_anchors: Option<&'s HashMap<&'a str, usize>>,
    /// The text the names are part of, so a name's place in it says which anchors come before it.
    input: &'a str,
}

impl NameRules<'_, '_> {
    /// Whether an alias whose name ends in `:` keeps it. By default every name does. In compat,
    /// only one that an anchor before it already has does. `name` is part of the input: a line is
    /// read again after anchors later on it, or on later lines, have been scanned, and those
    /// don't count.
    fn alias_keeps_colon(self, name: &str) -> bool {
        !self.compat
            || self.colon_anchors.is_some_and(|names| {
                names.get(name).is_some_and(|&defined| {
                    name.as_ptr()
                        .addr()
                        .checked_sub(self.input.as_ptr().addr())
                        .is_some_and(|at| at <= self.input.len() && defined < at)
                })
            })
    }
}

/// Whether `text` holds nothing but node properties (anchors and tags).
fn only_properties(text: &str, compat: bool) -> bool {
    text.split([' ', '\t'])
        .filter(|token| !token.is_empty())
        .all(|token| {
            token.starts_with('!')
                || (token.starts_with('&')
                    && !(compat
                        && (1..token.len())
                            .any(|index| compat_name_stop(token.as_bytes(), index, 1))))
        })
}

/// Whether the compat scanner, outside a flow collection, ends the anchor or alias name that
/// starts at `name_start` before `bytes[index]`: libyaml stops at a `?`, and at a `:` followed by
/// more name text (see `scan_anchor`).
fn compat_name_stop(bytes: &[u8], index: usize, name_start: usize) -> bool {
    match bytes[index] {
        b'?' => true,
        b':' => {
            index > name_start
                && bytes.get(index + 1).is_some_and(|next| {
                    !matches!(next, b' ' | b'\t' | b'[' | b']' | b'{' | b'}' | b',')
                })
        }
        _ => false,
    }
}

/// Where walking a line's tokens stopped, as a byte offset into it.
#[derive(Debug, PartialEq, Eq)]
enum LineStop {
    /// A block mapping-value indicator: a `:` followed by whitespace or the end.
    Value(usize),
    /// A `#` that starts a comment.
    Comment(usize),
    /// The line ended first, or a flow collection ran past it.
    End,
}

/// Walk `text` from the start of a token to its first value indicator or comment, skipping quoted
/// scalars and flow collections.
fn walk_line(text: &str, names: NameRules<'_, '_>) -> LineStop {
    let bytes = text.as_bytes();
    let mut index = 0;
    // Inside a plain scalar, quotes, flow indicators, and property indicators are content:
    // `a [b: |` has the key `a [b`.
    let mut in_plain = false;
    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b'#' if index == 0 || matches!(bytes[index - 1], b' ' | b'\t') => {
                return LineStop::Comment(index);
            }
            b':' if bytes
                .get(index + 1)
                .is_none_or(|next| matches!(next, b' ' | b'\t')) =>
            {
                return LineStop::Value(index);
            }
            _ if in_plain => {}
            b'"' | b'\'' => index = closing_quote(bytes, index),
            // A tag, anchor, or alias runs to the next blank; a `:` in it, as in `!: |` or
            // `&a: x`, is part of the name. In compat an anchor or alias name ends where libyaml
            // ends it, and plain text starts there: `&a:: |` is the key `:` anchored `a`.
            b'!' | b'&' | b'*' => {
                let name_start = index + 1;
                while index + 1 < bytes.len() && !matches!(bytes[index + 1], b' ' | b'\t') {
                    if names.compat
                        && byte != b'!'
                        && compat_name_stop(bytes, index + 1, name_start)
                    {
                        break;
                    }
                    index += 1;
                }
                // An alias that gives up its last `:` leaves it as this line's indicator.
                if byte == b'*'
                    && index > name_start
                    && bytes[index] == b':'
                    && !names.alias_keeps_colon(&text[name_start..=index])
                {
                    return LineStop::Value(index);
                }
            }
            b'[' | b'{' => match flow_collection_end(bytes, index, names.compat) {
                Some(end) => index = end,
                None => return LineStop::End,
            },
            b' ' | b'\t' => {}
            _ => in_plain = true,
        }
        index += 1;
    }
    LineStop::End
}

/// Whether `line`, from where a node starts to its line's end, starts a block collection: a block
/// indicator, or an implicit key, which a `:` indicator follows on the line. The walk reads one
/// node, after any properties, so that a line with many nodes is read about once in all. A quoted
/// scalar, a flow collection, or an alias is a key only if the `:` comes right after it, and a
/// plain scalar ends at its `:`. A block scalar's header takes the rest of its line.
fn starts_block_collection(line: &str, names: NameRules<'_, '_>) -> bool {
    let bytes = line.as_bytes();
    let separated = |index: usize| {
        bytes
            .get(index + 1)
            .is_none_or(|next| matches!(next, b' ' | b'\t'))
    };
    let skip_blanks = |mut index: usize| {
        while matches!(bytes.get(index), Some(b' ' | b'\t')) {
            index += 1;
        }
        index
    };
    if matches!(bytes.first(), Some(b'-' | b'?')) && separated(0) {
        return true;
    }
    // Node properties, each running to a blank, or in compat to where libyaml ends an anchor's
    // name. What follows a name that ends early is plain text.
    let mut index = 0;
    while let Some(&property @ (b'!' | b'&')) = bytes.get(index) {
        let name_start = index + 1;
        index = name_start;
        while index < bytes.len()
            && !matches!(bytes[index], b' ' | b'\t')
            && !(names.compat && property == b'&' && compat_name_stop(bytes, index, name_start))
        {
            index += 1;
        }
        index = skip_blanks(index);
    }
    let end = match bytes.get(index) {
        // An empty key, as in `: a` or `&a : b`.
        Some(b':') if separated(index) => return true,
        // A block indicator after properties is the parser's to refuse, and the rest of these
        // can't start a plain scalar. Walking on from any of them would read on to the line's
        // end, and again for each tab after the next indicator.
        Some(b'-' | b'?') if separated(index) => return false,
        None | Some(b'#' | b'|' | b'>' | b',' | b']' | b'}' | b'%' | b'@' | b'`') => return false,
        Some(b'"' | b'\'') => {
            let close = closing_quote(bytes, index);
            if close >= bytes.len() {
                // It goes on past this line, so it can't be an implicit key.
                return false;
            }
            close + 1
        }
        Some(b'[' | b'{') => match flow_collection_end(bytes, index, names.compat) {
            Some(close) => close + 1,
            None => return false,
        },
        Some(b'*') => {
            let name_end = (index..bytes.len())
                .find(|&at| matches!(bytes[at], b' ' | b'\t'))
                .unwrap_or(bytes.len());
            // In compat an alias may give up its last `:`, which is then the key's indicator.
            if let LineStop::Value(_) = walk_line(&line[index..name_end], names) {
                return true;
            }
            name_end
        }
        Some(_) => return matches!(walk_line(&line[index..], names), LineStop::Value(_)),
    };
    let after = skip_blanks(end);
    bytes.get(after) == Some(&b':') && separated(after)
}

/// The offset of what closes the flow collection that opens at `open`, reading its tokens by the
/// scanner's rules, or `None` if it doesn't close on this line. What's quoted, named, or plain in
/// it can hold any of the indicators, so it takes all of these to find the end of a key. A `:` or
/// `?` before a flow indicator is an indicator to the scanner too, but plain text would stop right
/// there anyway, so only a blank after one counts here.
fn flow_collection_end(bytes: &[u8], open: usize, compat: bool) -> Option<usize> {
    let blank_follows = |index: usize| matches!(bytes.get(index + 1), Some(b' ' | b'\t'));
    let mut depth = 0_usize;
    let mut index = open;
    // A `:` after a quoted scalar or a closed collection is an indicator whatever follows it, past
    // any blanks: in `{"a" :"b"}`, `"b"` is quoted.
    let mut after_json_like = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if matches!(byte, b' ' | b'\t') {
            index += 1;
            continue;
        }
        let mut json_like = false;
        match byte {
            // A comment, because without a blank before it this is an error.
            b'#' => return None,
            b'[' | b'{' => depth += 1,
            b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
                json_like = true;
            }
            b',' => {}
            b'"' | b'\'' => {
                index = closing_quote(bytes, index);
                json_like = true;
            }
            // A verbatim tag can hold flow indicators.
            b'!' if bytes.get(index + 1) == Some(&b'<') => {
                while index + 1 < bytes.len() && bytes[index] != b'>' {
                    index += 1;
                }
            }
            // Here no `:` ends a name. Libyaml's stop at a `?` in compat leaves an indicator that
            // a name can't come before, so that's an error whichever way it's read.
            b'!' | b'&' | b'*' => {
                while bytes
                    .get(index + 1)
                    .is_some_and(|next| !matches!(next, b' ' | b'\t' | b',' | b']' | b'}'))
                {
                    index += 1;
                }
            }
            b'?' if compat || blank_follows(index) => {}
            b':' if after_json_like || blank_follows(index) => {}
            // A plain scalar, in which quotes and names are content. A `:` ends it only if the
            // arm above takes it, and otherwise this arm goes on from there.
            _ => {
                while let Some(&next) = bytes.get(index + 1) {
                    if matches!(next, b',' | b'[' | b']' | b'{' | b'}' | b':')
                        || (next == b'#' && matches!(bytes[index], b' ' | b'\t'))
                    {
                        break;
                    }
                    index += 1;
                }
            }
        }
        after_json_like = json_like;
        index += 1;
    }
    None
}

/// The offset of the quote that closes the scalar opening at `open`, or past the end if none does.
fn closing_quote(bytes: &[u8], open: usize) -> usize {
    let quote = bytes[open];
    let mut index = open + 1;
    while index < bytes.len() {
        if quote == b'"' && bytes[index] == b'\\' {
            index += 2;
            continue;
        }
        if bytes[index] == quote {
            if quote == b'\'' && bytes.get(index + 1) == Some(&b'\'') {
                index += 2;
                continue;
            }
            return index;
        }
        index += 1;
    }
    bytes.len()
}

/// `line` without a trailing comment.
fn strip_comment<'t>(line: &'t str, names: NameRules<'_, '_>) -> &'t str {
    let (text, mut from, _) = block_prefix(line, names.compat);
    let mark = line.len() - text.len();
    loop {
        match walk_line(&text[from..], names) {
            LineStop::Value(colon) => from += colon + 1,
            LineStop::Comment(comment) => return &line[..mark + from + comment],
            LineStop::End => return line,
        }
    }
}

/// Check the stream's characters. A character outside `nb-json`
/// (a C0 control other than tab, line feed, or carriage return) is an error
/// anywhere: its offset is the `Err`. A character in `nb-json` but outside
/// `c-printable` (DEL, a C1 control other than NEL, U+FFFE, or U+FFFF) may
/// appear only inside a quoted scalar (YAML 1.2.2 section 5.1); the `Ok`
/// lists their offsets for the scanner to check in context. Escapes in
/// double-quoted scalars can still produce any character.
fn non_printable_characters(input: &str) -> Result<Vec<usize>, usize> {
    let bytes = input.as_bytes();
    let mut quoted_only = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte < 0x80 {
            if byte < 0x20 && !matches!(byte, b'\t' | b'\n' | b'\r') {
                return Err(index);
            }
            if byte == 0x7f {
                quoted_only.push(index);
            }
            index += 1;
            continue;
        }
        let ch = input[index..]
            .chars()
            .next()
            .expect("index is a char boundary");
        let code = u32::from(ch);
        if !(code == 0x85
            || (0xa0..=0xd7ff).contains(&code)
            || (0xe000..=0xfffd).contains(&code)
            || code >= 0x1_0000)
        {
            quoted_only.push(index);
        }
        index += ch.len_utf8();
    }
    Ok(quoted_only)
}

fn non_printable_error(input: &str, index: usize) -> Error {
    let ch = input[index..]
        .chars()
        .next()
        .expect("index is a char boundary");
    Error::new(
        ErrorKind::Scanner,
        format!(
            "non-printable character U+{:04X} is not allowed in YAML",
            u32::from(ch)
        ),
    )
}

/// Why [`decode_percent_escapes`] failed.
enum PercentError {
    /// A `%` at this byte offset is not followed by two hex digits.
    Escape(usize),
    /// The decoded bytes are not UTF-8.
    Utf8,
}

/// Decode the `%XX` escapes in tag text (YAML 1.2.2 section 6.8.2).
fn decode_percent_escapes(raw: &str) -> Result<String, PercentError> {
    if !raw.contains('%') {
        return Ok(raw.to_owned());
    }
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = raw
                .get(index + 1..index + 3)
                .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .and_then(|digits| u8::from_str_radix(digits, 16).ok())
                .ok_or(PercentError::Escape(index))?;
            decoded.push(hex);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| PercentError::Utf8)
}

fn scalar_cow<'a>(value: String, candidate: &'a str) -> Cow<'a, str> {
    if value == candidate {
        Cow::Borrowed(candidate)
    } else {
        Cow::Owned(value)
    }
}

#[derive(Clone, Copy)]
enum Chomping {
    Strip,
    Clip,
    Keep,
}

/// Fold the lines of a folded block scalar, given as `raw`'s lines between line breaks, without
/// collecting them: only the line before a run of empty lines and the one after it matter.
fn fold_block_scalar(raw: &str) -> String {
    let mut value = String::with_capacity(raw.len());
    let mut lines = raw.split('\n');
    let Some(mut line) = lines.next() else {
        return value;
    };
    loop {
        value.push_str(line);
        // The lines between this one and the next with text, and that line if there is one.
        let mut empty = 0;
        let mut next = None;
        let mut last = true;
        for candidate in lines.by_ref() {
            last = false;
            if candidate.is_empty() {
                empty += 1;
            } else {
                next = Some(candidate);
                break;
            }
        }
        if last {
            break;
        }
        let Some(next) = next else {
            value.extend(std::iter::repeat_n('\n', empty));
            break;
        };
        let breaks = empty + 1;
        let more_indented = line.starts_with([' ', '\t']) || next.starts_with([' ', '\t']);
        if line.is_empty() || more_indented {
            value.extend(std::iter::repeat_n('\n', breaks));
        } else if breaks == 1 {
            value.push(' ');
        } else {
            value.extend(std::iter::repeat_n('\n', breaks - 1));
        }
        line = next;
    }
    value
}

/// A block scalar's value from its lines, `text`, each with its line break if it had one, and
/// whether the last line with any text had one.
fn build_block_scalar(
    text: String,
    final_break: bool,
    literal: bool,
    chomping: Chomping,
) -> String {
    let mut value = if literal {
        text
    } else {
        fold_block_scalar(&text)
    };
    match chomping {
        Chomping::Strip => {
            while value.ends_with('\n') {
                value.pop();
            }
        }
        Chomping::Clip => {
            while value.ends_with('\n') {
                value.pop();
            }
            if final_break && !value.is_empty() {
                value.push('\n');
            }
        }
        Chomping::Keep => {}
    }
    value
}

/// A URI character (production 39, `ns-uri-char`), counting `%` alone; the
/// two hex digits after it are checked when the escape is decoded.
fn is_uri_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            '-' | '#'
                | ';'
                | '/'
                | '?'
                | ':'
                | '@'
                | '&'
                | '='
                | '+'
                | '$'
                | ','
                | '_'
                | '.'
                | '!'
                | '~'
                | '*'
                | '\''
                | '('
                | ')'
                | '['
                | ']'
                | '%'
        )
}

/// A tag shorthand character (production 40, `ns-tag-char`): a URI character
/// other than `!` and the flow indicators.
/// Whether `text` is URI characters, with each `%` starting a `%XX` escape.
fn uri_text_ok(text: &str) -> bool {
    let bytes = text.as_bytes();
    text.char_indices().all(|(index, ch)| {
        is_uri_char(ch)
            && (ch != '%'
                || (bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
                    && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit)))
    })
}

fn is_tag_char(ch: char) -> bool {
    is_uri_char(ch) && !matches!(ch, '!' | ',' | '[' | ']' | '{' | '}')
}

/// YAML white space (production 33, `s-white`) or a line break. Other
/// Unicode spaces, such as U+00A0, are ordinary content characters.
fn is_blank(ch: char) -> bool {
    matches!(ch, ' ' | '\t') || is_line_break(ch)
}

/// Whether `ch` is a YAML 1.2 line break. NEL, LS, and PS were line breaks
/// in YAML 1.1 and libyaml; in YAML 1.2 they are content.
fn is_line_break(ch: char) -> bool {
    matches!(ch, '\n' | '\r')
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        Chomping, LineShape, LineStop, NameRules, Owner, Scanner, TokenKind, build_block_scalar,
        closing_quote, line_shape, strip_comment, walk_line,
    };
    use crate::event::ScalarStyle;
    use crate::limits::Limits;
    use crate::tag::Tag;

    fn tokens(input: &str) -> Vec<TokenKind<'_>> {
        let mut scanner = Scanner::new(input, Limits::default()).expect("scanner");
        let mut tokens = Vec::new();
        while let Some(token) = scanner.next_token().expect("token") {
            tokens.push(token.kind);
        }
        tokens
    }

    fn scalar(input: &str) -> (String, ScalarStyle, String) {
        let tokens = tokens(input);
        for token in tokens {
            if let TokenKind::Scalar { value, raw, style } = token {
                return (value.into_owned(), style, raw.into_owned());
            }
        }
        panic!("no scalar in {input:?}");
    }

    /// The anchor names `input` leaves for its aliases to match, sorted.
    fn colon_anchors(input: &str, compat: bool) -> Option<Vec<String>> {
        let mut scanner = Scanner::with_compat(input, Limits::default(), compat).expect("scanner");
        while scanner.next_token().expect("token").is_some() {}
        let mut names: Vec<String> = scanner
            .colon_anchors?
            .into_keys()
            .map(str::to_owned)
            .collect();
        names.sort();
        Some(names)
    }

    fn rules(compat: bool) -> NameRules<'static, 'static> {
        NameRules {
            compat,
            colon_anchors: None,
            input: "",
        }
    }

    #[test]
    fn a_line_ends_an_alias_name_as_the_scanner_does() {
        // The anchor `a:` is defined on the second line, after the first.
        let input = "*a:\n&a: 1\n*a:\n*b:";
        let known = HashMap::from([("a:", 4)]);
        let none = NameRules {
            compat: true,
            colon_anchors: None,
            input,
        };
        let anchored = NameRules {
            compat: true,
            colon_anchors: Some(&known),
            input,
        };
        let default = NameRules {
            compat: false,
            colon_anchors: None,
            input,
        };
        let (before, after, other) = (&input[..3], &input[10..13], &input[14..]);
        let owner = LineShape::Owner(Owner::Block(0));
        // The colon is this line's indicator, unless an anchor before it has the whole name.
        assert_eq!(line_shape(after, none), owner);
        assert_eq!(line_shape(after, anchored), LineShape::Content);
        assert_eq!(line_shape(before, anchored), owner);
        assert_eq!(line_shape(other, anchored), owner);
        assert_eq!(line_shape(after, default), LineShape::Content);
        // No colon to give up, or none but the name.
        assert_eq!(line_shape("*ab", none), LineShape::Content);
        assert_eq!(line_shape("*:", none), LineShape::Content);
        // Only an alias gives one up, and a flow key's alias is inside its key.
        assert_eq!(line_shape("&a:", none), LineShape::Open);
        assert_eq!(line_shape("!a:", none), LineShape::Open);
        assert_eq!(line_shape("{*a: 1}:", none), owner);
    }

    #[test]
    fn only_compat_keeps_anchor_names_and_only_those_ending_in_a_colon() {
        assert_eq!(
            colon_anchors("x: &a: 1\ny: &b 2\nz: &c:d: 3\n", true),
            Some(vec!["a:".to_owned()])
        );
        assert_eq!(colon_anchors("x: &a 1\ny: &b 2\n", true), None);
        assert_eq!(colon_anchors("x: &a: 1\ny: &b 2\n", false), None);
        assert_eq!(colon_anchors("x: &a: 1\n---\ny: 2\n", true), None);
        assert_eq!(colon_anchors("x: &a: 1\n...\n", true), None);
    }

    #[test]
    fn a_line_walk_stops_where_the_scanner_would() {
        // A key that ends at the last `:` on the line, in both modes.
        for text in [
            "a: c",
            "a:",
            "a:\tc",
            "a#b: c",
            "a \"b: c",
            "a [b: c",
            "&a !t k: c",
            "&a\tk: c",
            "\"a: b\": c",
            "\"a\\\": b\": c",
            "\"a\\\\\": c",
            "'a: b': c",
            "'\\': c",
            "'a'': b': c",
            "[a]: c",
            "{a: b}: c",
            "[a#b]: c",
            "[\"]: \"]: c",
            "[']: ']: c",
            "['a'']: ']: c",
            "[\"\\\"]: \"]: c",
            "['\\']: c",
            "{\"a\":\"]: \"}: c",
            "{\"a\" :\"]: \"}: c",
            "{\"a\"\t:\"]: \"}: c",
            "{[a] :\"]: \"}: c",
            "['a' :\"]: \"]: c",
            "{\"a\"::\"b}: c",
            "[x &a: \"]: \"]: c",
            "[x !a: \"]: \"]: c",
            "{x *a: \"]: \"}: c",
            "{a:\t\"]: \"}: c",
            "{a:\"b: c}: d",
            "[&a,\"]: \"]: c",
            "[&a\t\"]: \"]: c",
            "{&a}: c",
            "[!t,\"]: \"]: c",
            "[&a:b \"]: \"]: c",
            "[!<t:]> \"]: \"]: c",
            "{!<t:]> : }: c",
            "[a[b]]: c",
        ] {
            for compat in [false, true] {
                assert_eq!(
                    walk_line(text, rules(compat)),
                    LineStop::Value(text.rfind(':').unwrap()),
                    "{text:?} compat={compat}"
                );
            }
        }
        // A comment, which no later `:` or bracket changes.
        for text in [
            "# a: b",
            "a # b: c",
            "a\t# b: c",
            "'a' # b: c",
            "[a] # b: c",
            "&a # b: c",
        ] {
            for compat in [false, true] {
                assert_eq!(
                    walk_line(text, rules(compat)),
                    LineStop::Comment(text.find('#').unwrap()),
                    "{text:?} compat={compat}"
                );
            }
        }
        // Nothing that ends a key on this line.
        for text in [
            "",
            "a",
            "a:b",
            "a#b",
            "!t",
            "&a b",
            "[",
            "[a",
            "\"a: b",
            "'a",
            "[\"a: b]: c",
            "[a, # b]: c",
            "[a # b]: c",
            "[a\t# b]: c",
            "&:b: c",
            "[a[b]: c",
            "[a{b}: c",
            "{a: [b}: c",
            "[!<t:]: c",
            "!a:b: c",
        ] {
            for compat in [false, true] {
                assert_eq!(
                    walk_line(text, rules(compat)),
                    LineStop::End,
                    "{text:?} compat={compat}"
                );
            }
        }
    }

    #[test]
    fn a_line_walk_reads_names_and_a_flow_question_mark_by_mode() {
        let walk = |text: &str| (walk_line(text, rules(false)), walk_line(text, rules(true)));
        let last = |text: &str| LineStop::Value(text.rfind(':').unwrap());
        for text in ["&a:b: c", "*a:b: c", "&a?b: c", "&a?b \"c: d"] {
            assert_eq!(
                walk(text),
                (LineStop::End, LineStop::Value(text.find(": ").unwrap())),
                "{text:?}"
            );
        }
        let text = "[?\"]: \"]: c";
        assert_eq!(
            walk(text),
            (LineStop::Value(text.find(':').unwrap()), last(text))
        );
        let text = "{?\"a: b}: c";
        assert_eq!(walk(text), (last(text), LineStop::End));
        let text = "[?\t\"]: \"]: c";
        assert_eq!(walk(text), (last(text), last(text)));
    }

    #[test]
    fn a_closing_quote_is_the_one_the_scanner_stops_at() {
        for (text, close) in [
            ("\"a\"", 2),
            ("'a'", 2),
            ("\"a\"\"b\"", 2),
            ("'a''b'", 5),
            ("'a\\'", 3),
            ("\"a\\\"b\"", 5),
            ("\"a\\\\\"", 4),
            ("\"a", 2),
            ("'a''", 4),
        ] {
            assert_eq!(closing_quote(text.as_bytes(), 0), close, "{text:?}");
        }
    }

    #[test]
    fn a_comment_comes_off_the_line_it_ends() {
        for compat in [false, true] {
            for (line, code) in [
                ("a: b", "a: b"),
                ("a: b # c: d", "a: b "),
                ("a: # c", "a: "),
                ("a: b: c # d", "a: b: c "),
                ("# c", ""),
                ("- # c", "- "),
                ("- [' # ']: # c", "- [' # ']: "),
                ("- [\"]: \"]: # c \"", "- [\"]: \"]: "),
                ("? {\", \"}: # c \"", "? {\", \"}: "),
                ("- a#b: # c", "- a#b: "),
                ("\u{feff}a: # c", "\u{feff}a: "),
                ("[a, # b", "[a, # b"),
            ] {
                assert_eq!(
                    strip_comment(line, rules(compat)),
                    code,
                    "{line:?} compat={compat}"
                );
            }
        }
    }

    #[test]
    fn block_tokens_and_indicators() {
        let kinds = tokens("a: 1\n- b\n");
        assert!(matches!(kinds[0], TokenKind::StreamStart));
        assert!(kinds.iter().any(|kind| matches!(kind, TokenKind::Value)));
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, TokenKind::BlockEntry))
        );
        assert!(matches!(kinds.last(), Some(TokenKind::StreamEnd)));
    }

    #[test]
    fn flow_tokens_are_nested_and_comma_separated() {
        let kinds = tokens("[1, {a: 2}]");
        assert!(matches!(kinds[1], TokenKind::FlowSequenceStart));
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, TokenKind::FlowEntry))
        );
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, TokenKind::FlowMappingStart))
        );
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, TokenKind::FlowMappingEnd))
        );
    }

    #[test]
    fn block_scalars_support_literal_folding_and_chomping() {
        let (value, style, raw) = scalar("|\n  a\n  b\n");
        assert_eq!(value, "a\nb\n");
        assert_eq!(style, ScalarStyle::Literal);
        assert_eq!(raw, "|\n  a\n  b\n");

        let (value, style, _) = scalar(">\n  a\n  b\n");
        assert_eq!(value, "a b\n");
        assert_eq!(style, ScalarStyle::Folded);

        let (value, _, _) = scalar("|-\n  a\n  b\n");
        assert_eq!(value, "a\nb");

        let (value, _, _) = scalar("|+\n  a\n\n");
        assert_eq!(value, "a\n\n");
    }

    /// How a block scalar's value was built from a list of its lines, before the scanner built
    /// one string as it read them.
    fn block_scalar_from_lines(
        lines: &[(String, bool)],
        literal: bool,
        chomping: Chomping,
    ) -> String {
        let mut raw = String::new();
        for (text, had_break) in lines {
            raw.push_str(text);
            if *had_break {
                raw.push('\n');
            }
        }
        let mut value = if literal {
            raw
        } else {
            let lines: Vec<&str> = raw.split('\n').collect();
            let mut value = String::new();
            let mut index = 0;
            while index < lines.len() {
                let line = lines[index];
                value.push_str(line);
                if index + 1 == lines.len() {
                    break;
                }
                let mut next = index + 1;
                while next < lines.len() && lines[next].is_empty() {
                    next += 1;
                }
                if next == lines.len() {
                    for _ in index + 1..lines.len() {
                        value.push('\n');
                    }
                    break;
                }
                let breaks = next - index;
                let more_indented =
                    line.starts_with([' ', '\t']) || lines[next].starts_with([' ', '\t']);
                if line.is_empty() || more_indented {
                    for _ in 0..breaks {
                        value.push('\n');
                    }
                } else if breaks == 1 {
                    value.push(' ');
                } else {
                    for _ in 0..breaks - 1 {
                        value.push('\n');
                    }
                }
                index = next;
            }
            value
        };
        let final_break = lines
            .iter()
            .rev()
            .find(|(text, _)| !text.is_empty())
            .is_some_and(|(_, had_break)| *had_break);
        match chomping {
            Chomping::Strip => {
                while value.ends_with('\n') {
                    value.pop();
                }
            }
            Chomping::Clip => {
                while value.ends_with('\n') {
                    value.pop();
                }
                if final_break && !value.is_empty() {
                    value.push('\n');
                }
            }
            Chomping::Keep => {}
        }
        value
    }

    /// Building one string as the lines are read gives what the list of lines gave, for every
    /// sequence of up to five lines of these kinds, with or without a last line break.
    #[test]
    fn block_scalars_built_as_read_match_the_list_of_lines() {
        let kinds = ["", "a", "b c", " a", "\ta", "  "];
        let mut sequences: Vec<Vec<&str>> = vec![Vec::new()];
        let mut checked = 0;
        for _ in 0..5 {
            sequences = sequences
                .iter()
                .flat_map(|sequence| {
                    kinds.iter().map(move |kind| {
                        let mut longer = sequence.clone();
                        longer.push(kind);
                        longer
                    })
                })
                .collect();
            for sequence in &sequences {
                for last_break in [true, false] {
                    let lines: Vec<(String, bool)> = sequence
                        .iter()
                        .enumerate()
                        .map(|(at, text)| {
                            ((*text).to_owned(), last_break || at + 1 < sequence.len())
                        })
                        .collect();
                    let text: String = lines
                        .iter()
                        .map(|(text, had_break)| {
                            format!("{text}{}", if *had_break { "\n" } else { "" })
                        })
                        .collect();
                    let final_break = lines
                        .iter()
                        .rev()
                        .find(|(text, _)| !text.is_empty())
                        .is_some_and(|(_, had_break)| *had_break);
                    for literal in [true, false] {
                        for chomping in [Chomping::Strip, Chomping::Clip, Chomping::Keep] {
                            assert_eq!(
                                build_block_scalar(text.clone(), final_break, literal, chomping),
                                block_scalar_from_lines(&lines, literal, chomping),
                                "{lines:?}, literal {literal}"
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(
            checked,
            (1..=5).map(|n| 6_usize.pow(n) * 2 * 6).sum::<usize>()
        );
    }

    #[test]
    fn block_scalar_indent_indicator_is_relative_to_the_header() {
        let (value, _, _) = scalar("|2\n    a\n");
        assert_eq!(value, "  a\n");
    }

    #[test]
    fn single_quotes_decode_doubled_quote() {
        let (value, style, raw) = scalar("'it''s'");
        assert_eq!(value, "it's");
        assert_eq!(style, ScalarStyle::SingleQuoted);
        assert_eq!(raw, "'it''s'");
    }

    #[test]
    fn double_quotes_decode_escapes() {
        let (value, style, _) = scalar("\"a\\n\\u00e9\"");
        assert_eq!(value, "a\né");
        assert_eq!(style, ScalarStyle::DoubleQuoted);
    }

    #[test]
    fn multiline_quoted_scalars_fold_line_breaks() {
        let (value, style, _) = scalar("'a\n  b'");
        assert_eq!(value, "a b");
        assert_eq!(style, ScalarStyle::SingleQuoted);

        let (value, style, _) = scalar("\"a\n  b\"");
        assert_eq!(value, "a b");
        assert_eq!(style, ScalarStyle::DoubleQuoted);

        let (value, _, _) = scalar("'a\n\n  b'");
        assert_eq!(value, "a\nb");
    }

    #[test]
    fn escaped_line_break_in_double_quotes_is_a_continuation() {
        let (value, _, _) = scalar("\"a\\\n  b\"");
        assert_eq!(value, "ab");
    }

    #[test]
    fn anchors_aliases_tags_and_directives() {
        let kinds = tokens("%YAML 1.2\n---\na: &b !Custom {x: 1}\nc: *b\n");
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, TokenKind::Directive { name, .. } if name == "YAML"))
        );
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, TokenKind::DocumentStart { explicit: true }))
        );
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, TokenKind::Anchor(name) if name == "b"))
        );
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, TokenKind::Alias(name) if name == "b"))
        );
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, TokenKind::Tag(tag) if tag == &Tag::local("Custom")))
        );
    }

    #[test]
    fn comments_are_skipped_and_line_breaks_are_tokens() {
        let kinds = tokens("a: 1 # comment\nb: 2\n");
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, TokenKind::LineBreak))
        );
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, TokenKind::Scalar { value, .. } if value == "1"))
        );
        assert!(!kinds.iter().any(
            |kind| matches!(kind, TokenKind::Scalar { value, .. } if value.contains("comment"))
        ));
    }

    #[test]
    fn crlf_and_bom_are_handled() {
        let kinds = tokens("\u{feff}a: 1\r\nb: 2\r\n");
        assert!(matches!(kinds[0], TokenKind::StreamStart));
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, TokenKind::LineBreak))
        );
    }

    #[test]
    fn unicode_span_is_byte_based() {
        let mut scanner = Scanner::new("é: 1", Limits::default()).expect("scanner");
        let mut spans = Vec::new();
        while let Some(token) = scanner.next_token().expect("token") {
            if let TokenKind::Scalar { value, .. } = &token.kind
                && value == "é"
            {
                spans.push(token.span);
            }
        }
        assert_eq!(spans.len(), 1);
        assert_eq!((spans[0].start().index(), spans[0].end().index()), (0, 2));
    }

    #[test]
    fn unterminated_quotes_and_unknown_escapes_error() {
        let mut scanner = Scanner::new("\"unterminated", Limits::default()).expect("scanner");
        let error = loop {
            match scanner.next_token() {
                Ok(Some(_)) => {}
                Ok(None) => panic!("expected an error"),
                Err(error) => break error,
            }
        };
        assert_eq!(error.kind(), crate::error::ErrorKind::Scanner);

        let mut scanner = Scanner::new("\"\\q\"", Limits::default()).expect("scanner");
        let error = loop {
            match scanner.next_token() {
                Ok(Some(_)) => {}
                Ok(None) => panic!("expected an error"),
                Err(error) => break error,
            }
        };
        assert_eq!(error.kind(), crate::error::ErrorKind::Scanner);
    }

    #[test]
    fn input_limit_is_enforced_before_scanning() {
        let error = Scanner::new(
            "x",
            Limits {
                max_input_bytes: 0,
                ..Limits::default()
            },
        )
        .expect_err("input limit");
        assert_eq!(error.kind(), crate::error::ErrorKind::Limit);
    }
}
