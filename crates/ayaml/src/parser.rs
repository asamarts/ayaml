//! Event parser.
//!
//! The parser consumes scanner tokens, groups them into logical lines, and
//! emits spanned events for streams, documents, block and flow collections,
//! scalars, aliases, tags, and anchors. Composing values and resolving
//! scalars happen downstream. The grammar runs on an explicit stack (see
//! `Parser::run`), so nesting depth costs heap, not native stack.

use std::borrow::Cow;
use std::collections::VecDeque;

use crate::composer::StreamingComposer;
use crate::error::{Code, Error, ErrorKind};
use crate::event::{Anchor, CollectionStyle, Event, ScalarStyle};
use crate::limits::Limits;
use crate::position::LineIndex;
use crate::scanner::{Scanner, Token, TokenKind};
use crate::span::{Marker, Span};
use crate::tag::Tag;
use crate::value::Value;

mod flow;
mod grammar;
#[cfg(test)]
mod tests;

/// A logical line's token range in the parser's shared token arena.
#[derive(Clone, Debug)]
struct Line {
    start: usize,
    end: usize,
    indent: usize,
}

impl Line {
    /// The line of `tokens[start..end]`, from `input`. Its indent is its first token's column,
    /// less any tab before that token and what follows the tab: YAML indents with spaces alone
    /// (production 63), so a tab there separates and `k:\n\tv` holds `v` at indent 0.
    fn new(tokens: &[Token<'_>], start: usize, end: usize, input: &str) -> Self {
        let indent = tokens.get(start).map_or(0, |token| {
            let column = token.span.start().column().saturating_sub(1);
            let offset = token.span.start().index();
            let blanks = input.as_bytes()[..offset]
                .iter()
                .rev()
                .take_while(|byte| matches!(byte, b' ' | b'\t'))
                .count();
            match input[offset - blanks..offset].find('\t') {
                Some(tab) => column.saturating_sub(blanks - tab),
                None => column,
            }
        });
        Self { start, end, indent }
    }

    fn len(&self) -> usize {
        self.end - self.start
    }

    fn is_empty(&self) -> bool {
        self.start == self.end
    }

    fn advance(&mut self, count: usize) {
        self.start = self.start.saturating_add(count).min(self.end);
    }
}

/// Token-arena growth chunks. Power-of-two growth leaves large unused
/// capacity on wide inputs; fixed chunks bound the waste. Small inputs use a
/// smaller first chunk so the shrink-to-fit transient stays cheap.
const TOKEN_ARENA_CHUNK: usize = 8 * 1024;
const TOKEN_ARENA_SMALL_CHUNK: usize = 64;
const TOKEN_ARENA_MID_CHUNK: usize = 256;

fn push_token<'a>(tokens: &mut Vec<Token<'a>>, token: Token<'a>, chunk: usize) {
    if tokens.len() == tokens.capacity() {
        tokens.reserve_exact(arena_growth(tokens.len(), chunk));
    }
    tokens.push(token);
}

/// How many tokens a full arena of `len` grows by: its chunk while that's at least a sixteenth of
/// it, then a sixteenth. Growing by a fixed chunk alone copies the arena once per chunk, which is
/// quadratic in its length where `realloc` copies. A sixteenth keeps all the copying under 17
/// times the final length, and the unused capacity under a sixteenth of it. Up to 16 chunks the
/// arena grows as it did by chunks alone, so a typical file's peak doesn't move.
const fn arena_growth(len: usize, chunk: usize) -> usize {
    let sixteenth = len / 16;
    if sixteenth > chunk { sixteenth } else { chunk }
}

/// Logical lines read from the scanner, plus the stream boundary spans.
struct Lines<'a> {
    lines: Vec<Line>,
    tokens: Vec<Token<'a>>,
    stream_start: Option<Span>,
    stream_end: Option<Span>,
}

/// Group scanner tokens into logical lines as they arrive.
///
/// This avoids first collecting every token in one vector and then copying
/// them into per-line vectors. A block scalar consumes the rest of its line,
/// so it flushes the logical line immediately.
fn read_lines<'a>(scanner: &mut Scanner<'a>) -> Result<Lines<'a>, Error> {
    let input = scanner.input();
    let chunk = match scanner.input_len() {
        0..=4096 => TOKEN_ARENA_SMALL_CHUNK,
        4097..=32768 => TOKEN_ARENA_MID_CHUNK,
        _ => TOKEN_ARENA_CHUNK,
    };
    let mut lines = Vec::new();
    let mut tokens = Vec::new();
    let mut line_start = 0;
    let mut stream_start = None;
    let mut stream_end = None;
    while let Some(token) = scanner.next_token()? {
        match token.kind {
            TokenKind::StreamStart => stream_start = Some(token.span),
            TokenKind::StreamEnd => {
                stream_end = Some(token.span);
                break;
            }
            TokenKind::LineBreak => {
                if tokens.len() > line_start {
                    lines.push(Line::new(&tokens, line_start, tokens.len(), input));
                    line_start = tokens.len();
                }
            }
            _ => {
                let block_scalar = matches!(
                    token.kind,
                    TokenKind::Scalar {
                        style: ScalarStyle::Literal | ScalarStyle::Folded,
                        ..
                    }
                );
                push_token(&mut tokens, token, chunk);
                if block_scalar {
                    lines.push(Line::new(&tokens, line_start, tokens.len(), input));
                    line_start = tokens.len();
                }
            }
        }
    }
    if tokens.len() > line_start {
        lines.push(Line::new(&tokens, line_start, tokens.len(), input));
    }
    // Small token arenas gain more from exact capacity than the one-time copy
    // costs in peak memory. Large arenas keep their growth capacity so the
    // shrink copy does not dominate the parse peak.
    if tokens.len() <= 8192 {
        tokens.shrink_to_fit();
    }
    Ok(Lines {
        lines,
        tokens,
        stream_start,
        stream_end,
    })
}

enum EventSink<'a> {
    Events(Vec<Event<'a>>),
    Streaming(Box<StreamingComposer<'a>>),
}

impl<'a> EventSink<'a> {
    fn event(&mut self, event: Event<'a>) -> Result<(), Error> {
        match self {
            Self::Events(events) => {
                events.push(event);
                Ok(())
            }
            Self::Streaming(composer) => composer.event(event),
        }
    }
}

struct Parser<'a> {
    lines: Vec<Line>,
    token_arena: Vec<Token<'a>>,
    sink: EventSink<'a>,
    limits: Limits,
    input: &'a str,
    /// Word a malformed flow collection's error as libyaml does, and count positions as compat's
    /// scanner does.
    compat: bool,
    documents: usize,
    /// The events of the stream so far, its start and end included, which count against
    /// `max_events`. A document-sized parse starts from the count before it.
    emitted: usize,
    /// Stop at a second document start, as a single-document entry point
    /// needs: the rest of the stream is never parsed.
    single_document: bool,
    /// Emit the second document start and stop, instead of reporting it here. The typed builder
    /// then builds the first document before it reports the trailing one (AY-027).
    stop_at_second: bool,
    /// Where the lines end: the stream end, or for one document of a lazy read, the next
    /// document's first token. An empty node after the last line is placed here.
    end: Marker,
}

/// A document-bounded pull cursor used by the public event traversal.
#[derive(Debug)]
pub(crate) struct EventCursor<'a> {
    scanner: Scanner<'a>,
    limits: Limits,
    input: &'a str,
    pending_line: Option<Vec<Token<'a>>>,
    queue: VecDeque<Event<'a>>,
    stream_started: bool,
    stream_ended: bool,
    eof: bool,
    /// The scanner's stream end, once read, so the stream end event has its place.
    stream_end: Option<Span>,
    documents: usize,
    /// The events counted so far, each once, as an eager parse of the stream counts them.
    events: usize,
}

impl<'a> EventCursor<'a> {
    fn limit_at(&self, span: Span, limit: crate::limits::LimitExceeded) -> Error {
        Error::from(limit).with_span(span)
    }

    pub(crate) fn new(input: &'a str, limits: &Limits) -> Result<Self, Error> {
        let scanner = Scanner::new(input, limits.clone())?;
        Ok(Self {
            scanner,
            limits: limits.clone(),
            input,
            pending_line: None,
            queue: VecDeque::new(),
            stream_started: false,
            stream_ended: false,
            eof: false,
            stream_end: None,
            documents: 0,
            events: 0,
        })
    }

    pub(crate) fn next_event(&mut self) -> Result<Option<Event<'a>>, Error> {
        let result = self.next_event_inner();
        if result.is_err() {
            self.stream_ended = true;
            self.queue.clear();
        }
        result
    }

    fn next_event_inner(&mut self) -> Result<Option<Event<'a>>, Error> {
        if !self.stream_started {
            self.stream_started = true;
            // The scanner's stream start is where an eager parse puts the event,
            // after a byte order mark.
            let span = match self.scanner.next_token()? {
                Some(token) if matches!(token.kind, TokenKind::StreamStart) => token.span,
                _ => Span::empty(Marker::new(0, 1, 1)),
            };
            self.events += 1;
            self.limits
                .check_events(self.events)
                .map_err(|error| self.limit_at(span, error))?;
            return Ok(Some(Event::StreamStart { span }));
        }
        loop {
            // Each event was counted when its document was parsed.
            if let Some(event) = self.queue.pop_front() {
                return Ok(Some(event));
            }
            if self.stream_ended {
                return Ok(None);
            }
            match self.load_chunk()? {
                Some(events) => self.queue.extend(events),
                None => {
                    self.stream_ended = true;
                    let span = self
                        .stream_end
                        .unwrap_or_else(|| Span::empty(Marker::new(self.input.len(), 1, 1)));
                    self.events += 1;
                    self.limits
                        .check_events(self.events)
                        .map_err(|error| self.limit_at(span, error))?;
                    return Ok(Some(Event::StreamEnd { span }));
                }
            }
        }
    }

    fn load_chunk(&mut self) -> Result<Option<Vec<Event<'a>>>, Error> {
        let mut lines = Vec::new();
        let mut tokens = Vec::new();
        let mut seen_document = false;
        loop {
            let line_tokens = if let Some(tokens) = self.pending_line.take() {
                tokens
            } else {
                match self.read_line()? {
                    Some(tokens) => tokens,
                    None => break,
                }
            };
            let is_start = matches!(
                line_tokens.first().map(|token| &token.kind),
                Some(TokenKind::DocumentStart { .. })
            );
            let is_end = matches!(
                line_tokens.first().map(|token| &token.kind),
                Some(TokenKind::DocumentEnd)
            );
            if is_start && seen_document {
                self.pending_line = Some(line_tokens);
                break;
            }
            seen_document |= is_start
                || !matches!(
                    line_tokens.first().map(|token| &token.kind),
                    Some(TokenKind::Directive { .. })
                );
            let start = tokens.len();
            tokens.extend(line_tokens);
            lines.push(Line::new(&tokens, start, tokens.len(), self.input));
            if is_end {
                break;
            }
        }
        if lines.is_empty() {
            return Ok(None);
        }
        // The document ends where an eager parse would go on: at the next document's first
        // token, or at the stream end. After a `...` line neither is read yet, and nothing
        // past that line is placed.
        let end = self
            .pending_line
            .as_ref()
            .and_then(|line| line.first())
            .map(|token| token.span.start())
            .or_else(|| self.stream_end.map(Span::start))
            .or_else(|| tokens.last().map(|token| token.span.end()))
            .unwrap_or_else(|| Marker::new(self.input.len(), 1, 1));
        let (events, counted) = parse_lines(
            Lines {
                lines,
                tokens,
                stream_start: None,
                stream_end: None,
            },
            &self.limits,
            self.input,
            self.events,
            end,
        )?;
        self.events = counted;
        let documents = events
            .iter()
            .filter(|event| matches!(event, Event::DocumentStart { .. }))
            .count();
        self.documents = self.documents.saturating_add(documents);
        let span = events
            .iter()
            .find_map(|event| match event {
                Event::DocumentStart { span, .. } => Some(*span),
                _ => None,
            })
            .unwrap_or_else(|| Span::empty(Marker::new(self.input.len(), 1, 1)));
        self.limits
            .check_documents(self.documents)
            .map_err(|error| self.limit_at(span, error))?;
        Ok(Some(events))
    }

    fn read_line(&mut self) -> Result<Option<Vec<Token<'a>>>, Error> {
        if self.eof {
            return Ok(None);
        }
        let mut current = Vec::new();
        loop {
            match self.scanner.next_token()? {
                None => {
                    self.eof = true;
                    return Ok(if current.is_empty() {
                        None
                    } else {
                        Some(current)
                    });
                }
                Some(token) if matches!(token.kind, TokenKind::StreamStart) => {}
                Some(token) if matches!(token.kind, TokenKind::StreamEnd) => {
                    self.eof = true;
                    self.stream_end = Some(token.span);
                    return Ok(if current.is_empty() {
                        None
                    } else {
                        Some(current)
                    });
                }
                Some(token) if matches!(token.kind, TokenKind::LineBreak) => {
                    if !current.is_empty() {
                        return Ok(Some(current));
                    }
                }
                Some(token) => {
                    let block_scalar = matches!(
                        token.kind,
                        TokenKind::Scalar {
                            style: ScalarStyle::Literal | ScalarStyle::Folded,
                            ..
                        }
                    );
                    current.push(token);
                    if block_scalar {
                        return Ok(Some(current));
                    }
                }
            }
        }
    }
}

/// Parse one document's lines for the pull cursor. Its events count on from the `counted` events
/// before them, and the count after them comes back with them. `end` is where the lines stop.
fn parse_lines<'a>(
    lines: Lines<'a>,
    limits: &Limits,
    input: &'a str,
    counted: usize,
    end: Marker,
) -> Result<(Vec<Event<'a>>, usize), Error> {
    let event_capacity = event_capacity(&lines.lines, &lines.tokens);
    let mut parser = Parser {
        lines: lines.lines,
        token_arena: lines.tokens,
        sink: EventSink::Events(Vec::with_capacity(event_capacity)),
        limits: limits.clone(),
        input,
        compat: false,
        documents: 0,
        emitted: counted,
        single_document: false,
        stop_at_second: false,
        end,
    };
    parser.parse_stream()?;
    match parser.sink {
        EventSink::Events(events) => Ok((events, parser.emitted)),
        EventSink::Streaming(_) => unreachable!("event sink kind changed during parsing"),
    }
}

fn event_capacity(lines: &[Line], tokens: &[Token<'_>]) -> usize {
    let emitting = tokens
        .iter()
        .filter(|token| {
            !matches!(
                token.kind,
                TokenKind::LineBreak
                    | TokenKind::BlockEntry
                    | TokenKind::Key
                    | TokenKind::Value
                    | TokenKind::FlowEntry
                    | TokenKind::Anchor(_)
                    | TokenKind::Tag(_)
            )
        })
        .count();
    let headroom = if lines.iter().any(|line| line.indent > 0) {
        256
    } else {
        200
    };
    emitting.saturating_add(headroom)
}

/// Parse a UTF-8 input into spanned events.
pub(crate) fn parse_events<'a>(input: &'a str, limits: &Limits) -> Result<Vec<Event<'a>>, Error> {
    parse_events_with_compat(input, limits, false, false, false)
}

pub(crate) fn parse_events_compat<'a>(
    input: &'a str,
    limits: &Limits,
) -> Result<Vec<Event<'a>>, Error> {
    parse_events_with_compat(input, limits, true, false, false)
}

/// Parse the events of a single-document input. A second document is an
/// error at its start ("trailing documents").
pub(crate) fn parse_document_events<'a>(
    input: &'a str,
    limits: &Limits,
    compat_flow: bool,
) -> Result<Vec<Event<'a>>, Error> {
    // Stop at the second document's start and let the typed builder build the first one first
    // (AY-027). The second start is the marker the builder turns into a trailing-document error.
    parse_events_with_compat(input, limits, compat_flow, true, true)
}

fn parse_events_with_compat<'a>(
    input: &'a str,
    limits: &Limits,
    compat_flow: bool,
    single_document: bool,
    stop_at_second: bool,
) -> Result<Vec<Event<'a>>, Error> {
    let mut scanner = Scanner::with_compat(input, limits.clone(), compat_flow)?;
    let Lines {
        lines,
        tokens,
        stream_start,
        stream_end,
    } = read_lines(&mut scanner)?;

    let event_capacity = event_capacity(&lines, &tokens).saturating_add(2);
    let end = stream_end.map_or_else(|| Marker::new(input.len(), 1, 1), Span::start);
    let mut parser = Parser {
        lines,
        token_arena: tokens,
        sink: EventSink::Events(Vec::with_capacity(event_capacity)),
        limits: limits.clone(),
        input,
        compat: compat_flow,
        documents: 0,
        emitted: 0,
        single_document,
        stop_at_second,
        end,
    };
    parser.emit(Event::StreamStart {
        span: stream_start.unwrap_or_else(|| Span::empty(Marker::new(0, 1, 1))),
    })?;
    parser.parse_stream()?;
    parser.emit(Event::StreamEnd {
        span: stream_end.unwrap_or_else(|| Span::empty(Marker::new(input.len(), 1, 1))),
    })?;
    match parser.sink {
        EventSink::Events(events) => Ok(events),
        EventSink::Streaming(_) => unreachable!("event sink kind changed during parsing"),
    }
}

/// Compose values directly from the parser without retaining an event vector.
/// With `single_document`, a second document is an error at its start.
pub(crate) fn parse_documents(
    input: &str,
    limits: &Limits,
    single_document: bool,
) -> Result<Vec<Value>, Error> {
    let mut scanner = Scanner::new(input, limits.clone())?;
    let lines = read_lines(&mut scanner)?;
    let mut parser = Parser {
        lines: lines.lines,
        token_arena: lines.tokens,
        sink: EventSink::Streaming(Box::new(StreamingComposer::new(
            limits.clone(),
            input.len(),
        ))),
        limits: limits.clone(),
        input,
        compat: false,
        documents: 0,
        emitted: 0,
        single_document,
        stop_at_second: false,
        end: lines
            .stream_end
            .map_or_else(|| Marker::new(input.len(), 1, 1), Span::start),
    };
    // The composer ignores the stream events, but they count as an event parse counts them.
    parser.emit(Event::StreamStart {
        span: lines
            .stream_start
            .unwrap_or_else(|| Span::empty(Marker::new(0, 1, 1))),
    })?;
    parser.parse_stream()?;
    parser.emit(Event::StreamEnd {
        span: lines
            .stream_end
            .unwrap_or_else(|| Span::empty(Marker::new(input.len(), 1, 1))),
    })?;
    match parser.sink {
        EventSink::Streaming(composer) => composer.finish(),
        EventSink::Events(_) => unreachable!("event sink kind changed during parsing"),
    }
}

impl<'a> Parser<'a> {
    fn parse_stream(&mut self) -> Result<(), Error> {
        let mut index = 0;
        while index < self.lines.len() {
            let mut saw_directive = false;
            while let Some(TokenKind::Directive { name, value }) =
                self.line_first(index).map(|token| &token.kind)
            {
                let span = self.line_first(index).expect("directive").span;
                let event = Event::Directive {
                    name: name.clone(),
                    value: value.clone(),
                    span,
                };
                self.emit(event)?;
                saw_directive = true;
                index += 1;
            }
            if index >= self.lines.len() {
                if saw_directive {
                    return Err(self.error(self.empty_span(index), "directive without a document"));
                }
                break;
            }
            if saw_directive
                && !self.line_starts_with(index, |kind| {
                    matches!(kind, TokenKind::DocumentStart { .. })
                })
            {
                return Err(self.error(
                    self.line_first(index).expect("checked").span,
                    "directive must be followed by a document start",
                ));
            }

            // A `...` with no open document ends nothing: the stream grammar
            // allows any number of document suffixes between documents, so a
            // `...`-only stream has zero documents.
            if !saw_directive
                && self.lines[index].len() == 1
                && self.line_starts_with(index, |kind| matches!(kind, TokenKind::DocumentEnd))
            {
                index += 1;
                continue;
            }

            let mut explicit_start = false;
            let mut document_start_span = self
                .line_first(index)
                .map_or_else(|| self.empty_span(index), |token| token.span);
            if self.line_starts_with(index, |kind| {
                matches!(kind, TokenKind::DocumentStart { .. })
            }) {
                explicit_start = true;
                document_start_span = self
                    .take_line_first_span(index)
                    .unwrap_or(document_start_span);
                if self.lines[index].is_empty() {
                    index += 1;
                } else {
                    let line = self.line_run(index);
                    let (_, _, rest_start) = self.properties(line)?;
                    if rest_start > 0 && is_mapping_line(self.tokens(line.from(rest_start))) {
                        return Err(self.error(
                            self.token(line, 0).span,
                            "a document-start property cannot precede a same-line block mapping key",
                        ));
                    }
                    // A block collection starts after a line break (`s-l+block-collection`), so
                    // `--- a: b` and `--- - a` are errors, as in libyaml.
                    let tokens = self.tokens(line);
                    if rest_start == 0
                        && (matches!(tokens[0].kind, TokenKind::BlockEntry)
                            || is_mapping_line(tokens))
                    {
                        return Err(self.error(
                            tokens[0].span,
                            "a block collection cannot start on a `---` line",
                        ));
                    }
                }
            }

            let mut explicit_end = false;
            // Whether the root is a block collection, which a line after it that a tab seems to
            // indent was likely meant to be in.
            let mut root_block = false;
            if explicit_start || index < self.lines.len() {
                self.documents += 1;
                self.limits
                    .check_documents(self.documents)
                    .map_err(|error| self.limit_at(document_start_span, error))?;
                if self.single_document && self.documents > 1 {
                    if self.stop_at_second {
                        self.emit(Event::DocumentStart {
                            explicit: explicit_start,
                            span: document_start_span,
                        })?;
                        return Ok(());
                    }
                    return Err(self
                        .error(
                            document_start_span,
                            "expected a single YAML document, but found trailing documents",
                        )
                        .with_code(Code::TrailingDocuments));
                }
                self.emit(Event::DocumentStart {
                    explicit: explicit_start,
                    span: document_start_span,
                })?;

                if index < self.lines.len()
                    && !self.line_starts_with(index, |kind| matches!(kind, TokenKind::DocumentEnd))
                    && !self.line_starts_with(index, |kind| {
                        matches!(kind, TokenKind::DocumentStart { .. })
                    })
                {
                    let tokens = self.line_tokens(index);
                    root_block = tokens.first().is_some_and(|token| {
                        matches!(
                            token.kind,
                            TokenKind::BlockEntry | TokenKind::Key | TokenKind::Value
                        )
                    }) || has_value_indicator(tokens);
                    let root = NodeBounds {
                        min_indent: 0,
                        sequence_indent: 0,
                    };
                    index = self.run(Call::Node {
                        index,
                        indent: self.lines[index].indent,
                        depth: 0,
                        tag: None,
                        anchor: None,
                        bounds: root,
                    })?;
                } else {
                    let span = self.empty_span(index);
                    self.emit(self.null_scalar(span))?;
                }

                let mut document_end_span = self.last_span(index);
                if index < self.lines.len()
                    && self.line_starts_with(index, |kind| matches!(kind, TokenKind::DocumentEnd))
                {
                    explicit_end = true;
                    document_end_span = self
                        .take_line_first_span(index)
                        .unwrap_or(document_end_span);
                    if let Some(token) = self.line_first(index) {
                        return Err(self.error(token.span, "content after document end marker"));
                    }
                    index += 1;
                }
                self.emit(Event::DocumentEnd {
                    explicit: explicit_end,
                    span: document_end_span,
                })?;
            }

            if index < self.lines.len()
                && !explicit_end
                && !self.line_starts_with(index, |kind| {
                    matches!(kind, TokenKind::DocumentStart { .. })
                })
                && !self.line_starts_with(index, |kind| matches!(kind, TokenKind::DocumentEnd))
            {
                let tokens = self.line_tokens(index);
                let span = self
                    .line_first(index)
                    .map_or_else(|| self.empty_span(index), |token| token.span);
                let tabbed = root_block && tab_leads_line(self.input, span.start().index());
                let message = if tabbed {
                    "unexpected content after document"
                } else {
                    match tokens.first().map(|token| &token.kind) {
                        Some(TokenKind::Scalar {
                            style: ScalarStyle::Literal | ScalarStyle::Folded,
                            ..
                        }) => "a block scalar is not indented past its parent",
                        Some(TokenKind::BlockEntry) => {
                            "a block entry is indented less than its parent"
                        }
                        Some(TokenKind::Scalar { .. })
                            if root_block
                                && self.lines[index].indent > 0
                                && previous_line_is_blank(self.input, span.start().index()) =>
                        {
                            "a mapping line is missing a `:`"
                        }
                        _ => "unexpected content after document",
                    }
                };
                let error = self.error(span, message);
                // A node that only a tab seemed to indent under a key or an entry sits at its
                // line's left edge, so it ends up here.
                return Err(if tabbed {
                    error.with_context(
                        ", indented by a tab, which YAML doesn't count as indentation",
                    )
                } else {
                    error
                });
            }
        }
        Ok(())
    }

    /// Parse one node and return the index of the line after it.
    ///
    /// A grammar function below hands a call in tail position back as
    /// [`Step::Call`]; for any other call it first pushes the work that
    /// remains as a [`Cont`]. This loop runs the calls and feeds each result
    /// to the most recent continuation, so nesting depth and collection width
    /// cost heap, not native stack. Grammar functions may still call
    /// each other directly, but only along an acyclic graph: a direct call
    /// that can lead back to its caller nests a native frame per entry or
    /// level. Every loop over entries goes through this driver.
    fn run(&mut self, call: Call<'a>) -> Result<usize, Error> {
        let mut stack: Vec<Cont> = Vec::new();
        let mut step = self.call(call, &mut stack)?;
        loop {
            step = match step {
                Step::Call(call) => self.call(call, &mut stack)?,
                Step::Return(value) => match stack.pop() {
                    None => return Ok(value),
                    Some(cont) => self.resume(cont, value, &mut stack)?,
                },
            };
        }
    }

    fn call(&mut self, call: Call<'a>, stack: &mut Vec<Cont>) -> Result<Step<'a>, Error> {
        match call {
            Call::Node {
                index,
                indent,
                depth,
                tag,
                anchor,
                bounds,
            } => self.node(index, indent, depth, tag, anchor, bounds, stack),
            Call::SequenceFirst {
                index,
                first,
                indent,
                depth,
                tag,
                anchor,
            } => self.sequence_first(index, first, indent, depth, tag, anchor, stack),
            Call::SequenceItem {
                rest,
                next_index,
                indent,
                depth,
            } => self.sequence_item(rest, next_index, indent, depth, stack),
            Call::MappingFirst {
                first,
                next_index,
                indent,
                depth,
                tag,
                anchor,
            } => self.mapping_first(first, next_index, indent, depth, tag, anchor, stack),
            Call::MappingValue {
                value,
                current_index,
                indent,
                depth,
                allow_compact,
            } => self.mapping_value(value, current_index, indent, depth, allow_compact, stack),
            Call::Inline {
                tokens,
                depth,
                tag,
                anchor,
            } => self.inline(tokens, depth, tag, anchor),
            Call::FlowSequence {
                tokens,
                start,
                depth,
                tag,
                anchor,
            } => self.flow_sequence(tokens, start, depth, tag, anchor, stack),
            Call::FlowExplicitMapping {
                tokens,
                start,
                depth,
            } => self.flow_explicit_mapping(tokens, start, depth, stack),
            Call::ImplicitFlowMapping {
                tokens,
                start,
                colon,
                depth,
            } => self.implicit_flow_mapping(tokens, start, colon, depth, stack),
            Call::FlowMapping {
                tokens,
                start,
                depth,
                tag,
                anchor,
            } => self.flow_mapping(tokens, start, depth, tag, anchor, stack),
        }
    }

    fn resume(
        &mut self,
        cont: Cont,
        value: usize,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        match cont {
            Cont::NodeInline { rest, index } => {
                self.expect_consumed(rest, value, "unexpected token after node")?;
                Ok(Step::Return(index + 1))
            }
            Cont::ItemInline {
                content,
                next_index,
            } => {
                self.expect_consumed(content, value, "unexpected token after node")?;
                Ok(Step::Return(next_index))
            }
            Cont::Sequence(mut state) => {
                state.current_index = value;
                self.sequence_loop(state, stack)
            }
            Cont::MappingExplicitKey(mut state) => {
                state.current_index = value;
                self.mapping_after_explicit_key(state, stack)
            }
            Cont::MappingImplicitKey {
                state,
                tokens,
                value_index,
                key,
            } => {
                self.expect_consumed(key, value, "unexpected token in mapping key")?;
                self.mapping_after_implicit_key(state, tokens, value_index, stack)
            }
            Cont::MappingEntry(mut state) => {
                state.current_index = value;
                self.mapping_loop(state, stack)
            }
            Cont::ValueInline {
                value: tokens,
                current_index,
            } => {
                self.expect_consumed(tokens, value, "unexpected token in mapping value")
                    .map_err(|error| error.with_code(Code::UnexpectedMappingValue))?;
                Ok(Step::Return(current_index))
            }
            Cont::FlowSequenceItem { mut state, inline } => {
                if inline {
                    if value == 0 {
                        return Err(self.error(
                            self.token(state.tokens, state.index).span,
                            "expected a flow item",
                        ));
                    }
                    state.index += value;
                } else {
                    state.index = value;
                }
                match self
                    .token_at(state.tokens, state.index)
                    .map(|token| &token.kind)
                {
                    Some(TokenKind::FlowEntry) => state.index += 1,
                    Some(TokenKind::FlowSequenceEnd) => {}
                    _ => {
                        return Err(self.flow_error(
                            state.tokens,
                            state.index,
                            FlowLack::Separator,
                        ));
                    }
                }
                self.flow_sequence_loop(state, stack)
            }
            Cont::FlowExplicitKey {
                tokens,
                start_span,
                key,
                key_end,
                depth,
            } => {
                self.expect_flow_consumed(key, value)?;
                self.flow_explicit_value(tokens, start_span, key_end, depth, stack)
            }
            Cont::FlowExplicitValue {
                tokens,
                start_span,
                value: value_tokens,
                value_end,
            } => {
                self.expect_flow_consumed(value_tokens, value)?;
                self.flow_mapping_end(tokens, start_span, value_end)
            }
            Cont::ImplicitFlowKey {
                tokens,
                start_span,
                key,
                colon,
                depth,
            } => {
                self.expect_flow_consumed(key, value)?;
                self.implicit_flow_value(tokens, start_span, colon, depth, stack)
            }
            Cont::ImplicitFlowValue {
                tokens,
                start_span,
                value: value_tokens,
                value_end,
            } => {
                self.expect_flow_consumed(value_tokens, value)?;
                self.flow_mapping_end(tokens, start_span, value_end)
            }
            Cont::FlowMappingKey { state, key } => {
                self.expect_flow_consumed(key, value)?;
                self.flow_mapping_after_key(state, stack)
            }
            Cont::FlowMappingValue {
                state,
                value: value_tokens,
            } => {
                self.expect_flow_consumed(value_tokens, value)?;
                self.flow_mapping_next(state, stack)
            }
            Cont::FlowMappingNext(state) => self.flow_mapping_next(state, stack),
        }
    }

    /// Fail unless a node consumed all of `run`.
    /// A flow key or value that stops short has something after it where `,` or a closing bracket
    /// belongs.
    fn expect_flow_consumed(&self, run: Run, consumed: usize) -> Result<(), Error> {
        if consumed == run.len() {
            Ok(())
        } else {
            Err(self.flow_error(run, consumed, FlowLack::Separator))
        }
    }

    fn expect_consumed(&self, run: Run, consumed: usize, message: &str) -> Result<(), Error> {
        if consumed == run.len() {
            Ok(())
        } else {
            Err(self.error(self.token(run, consumed).span, message))
        }
    }
}

/// Whether only blanks, a tab among them, come before `offset` on its line.
/// Whether the line before the one that starts at `offset` holds only whitespace.
fn previous_line_is_blank(input: &str, offset: usize) -> bool {
    let line_start = input[..offset]
        .rfind(['\n', '\r'])
        .map_or(0, |break_at| break_at + 1);
    if line_start == 0 {
        return false;
    }
    let before = &input[..line_start - 1];
    let previous_start = before
        .rfind(['\n', '\r'])
        .map_or(0, |break_at| break_at + 1);
    before[previous_start..].trim().is_empty()
}

fn tab_leads_line(input: &str, offset: usize) -> bool {
    let before = &input[..offset];
    let rest = before.trim_end_matches([' ', '\t']);
    (rest.is_empty() || rest.ends_with(['\n', '\r'])) && before[rest.len()..].contains('\t')
}

fn value_indicator_index(tokens: &[Token<'_>]) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        match token.kind {
            TokenKind::FlowSequenceStart | TokenKind::FlowMappingStart => depth += 1,
            TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd => {
                depth = depth.saturating_sub(1);
            }
            TokenKind::Value if depth == 0 => return Some(index),
            _ => {}
        }
    }
    None
}

/// Whether each closing bracket among these tokens closes the last one opened. When one doesn't,
/// the parse of the key reports it, which says more than where the key began.
#[cold]
fn flow_brackets_match(tokens: &[Token<'_>]) -> bool {
    let mut open = Vec::new();
    tokens.iter().all(|token| match token.kind {
        TokenKind::FlowSequenceStart => {
            open.push(TokenKind::FlowSequenceEnd);
            true
        }
        TokenKind::FlowMappingStart => {
            open.push(TokenKind::FlowMappingEnd);
            true
        }
        TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd => {
            open.pop().as_ref() == Some(&token.kind)
        }
        _ => true,
    })
}

/// The longest implicit key, counted in characters from the key's first
/// character to its `:` (YAML 1.2.2 productions 154 and 155; libyaml uses
/// the same limit).
const MAX_IMPLICIT_KEY_CHARS: usize = 1024;

/// Whether an implicit key starting at `key` and ending before the `:` at
/// `colon`, on the same line, is short enough.
fn implicit_key_fits(key: Marker, colon: Marker) -> bool {
    colon.column().saturating_sub(key.column()) <= MAX_IMPLICIT_KEY_CHARS
}

fn has_value_indicator(tokens: &[Token<'_>]) -> bool {
    value_indicator_index(tokens).is_some()
}

/// Where a block node that starts on a later line may sit, relative to its
/// parent at column `n` (YAML 1.2.2 section 8.2): content lines at
/// `min_indent` (n + 1) or deeper, and the `-` lines of its block sequence
/// at `sequence_indent` (`seq-spaces` + 1).
#[derive(Clone, Copy, Debug)]
struct NodeBounds {
    min_indent: usize,
    sequence_indent: usize,
}

impl NodeBounds {
    /// A sequence entry's node, for an entry at column `n`.
    fn block_in(n: usize) -> Self {
        Self {
            min_indent: n + 1,
            sequence_indent: n + 1,
        }
    }

    /// A mapping value or explicit key's node, for a key at column `n`. Its
    /// block sequence may sit at the key's column.
    fn block_out(n: usize) -> Self {
        Self {
            min_indent: n + 1,
            sequence_indent: n,
        }
    }
}

const DUPLICATE_PROPERTIES: &str = "a node cannot have duplicate tag or anchor properties";

/// A node's tag and anchor, and how many tokens they take up.
type Properties<'a> = (Option<Tag>, Option<Anchor<'a>>, usize);

/// What a malformed flow collection lacks where it fails.
enum FlowLack {
    /// `,` or its closing bracket.
    Separator,
    /// A node, with what stands there instead.
    Node(String),
}

/// A run of one logical line's tokens in the shared token arena.
/// Every token slice the grammar works on is one; a flow collection is
/// always on one logical line.
#[derive(Clone, Copy, Debug)]
struct Run {
    line: usize,
    start: usize,
    end: usize,
}

impl Run {
    fn len(self) -> usize {
        self.end - self.start
    }

    fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// The tokens from `offset` on.
    fn from(self, offset: usize) -> Self {
        Self {
            start: self.start + offset,
            ..self
        }
    }

    /// The tokens before `offset`.
    fn until(self, offset: usize) -> Self {
        Self {
            end: self.start + offset,
            ..self
        }
    }

    /// The tokens in `start..end`, relative to this run.
    fn slice(self, start: usize, end: usize) -> Self {
        Self {
            line: self.line,
            start: self.start + start,
            end: self.start + end,
        }
    }
}

/// A block collection between entries.
#[derive(Debug)]
struct BlockState {
    indent: usize,
    depth: usize,
    entries: usize,
    /// The line after the last entry parsed so far.
    current_index: usize,
    /// The first entry's tokens, before it is parsed: a collection starts
    /// on the line, or part of the line, that opened it.
    first: Option<Run>,
}

/// A flow collection between entries.
#[derive(Debug)]
struct FlowState {
    tokens: Run,
    start_span: Span,
    /// The next token to parse, relative to `tokens`.
    index: usize,
    entries: usize,
    depth: usize,
}

/// A grammar function to run, with its arguments. Each returns a line
/// index (the block forms) or a token index relative to its run (the
/// flow and inline forms).
enum Call<'a> {
    Node {
        index: usize,
        indent: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        bounds: NodeBounds,
    },
    SequenceFirst {
        index: usize,
        first: Run,
        indent: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
    },
    SequenceItem {
        rest: Run,
        next_index: usize,
        indent: usize,
        depth: usize,
    },
    MappingFirst {
        first: Run,
        next_index: usize,
        indent: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
    },
    /// Returns the line after the value.
    MappingValue {
        value: Run,
        current_index: usize,
        indent: usize,
        depth: usize,
        allow_compact: bool,
    },
    /// Returns how many of `tokens` the node consumed.
    Inline {
        tokens: Run,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
    },
    FlowSequence {
        tokens: Run,
        start: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
    },
    FlowExplicitMapping {
        tokens: Run,
        start: usize,
        depth: usize,
    },
    ImplicitFlowMapping {
        tokens: Run,
        start: usize,
        colon: usize,
        depth: usize,
    },
    FlowMapping {
        tokens: Run,
        start: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
    },
}

/// The rest of a grammar function, waiting for a nested call's result.
#[derive(Debug)]
enum Cont {
    /// A node's inline content must consume the rest of line `index`.
    NodeInline {
        rest: Run,
        index: usize,
    },
    /// A sequence entry's inline content must consume `content`.
    ItemInline {
        content: Run,
        next_index: usize,
    },
    /// A block sequence entry ended at the returned line.
    Sequence(BlockState),
    /// An explicit block key ended at the returned line.
    MappingExplicitKey(BlockState),
    /// An implicit block key consumed the returned number of `key` tokens.
    MappingImplicitKey {
        state: BlockState,
        tokens: Run,
        value_index: Option<usize>,
        key: Run,
    },
    /// A block mapping value ended at the returned line.
    MappingEntry(BlockState),
    /// An inline block mapping value consumed the returned number of tokens.
    ValueInline {
        value: Run,
        current_index: usize,
    },
    /// A flow sequence entry ended at the returned index; with `inline`,
    /// the index is relative to the entry.
    FlowSequenceItem {
        state: FlowState,
        inline: bool,
    },
    FlowExplicitKey {
        tokens: Run,
        start_span: Span,
        key: Run,
        key_end: usize,
        depth: usize,
    },
    FlowExplicitValue {
        tokens: Run,
        start_span: Span,
        value: Run,
        value_end: usize,
    },
    ImplicitFlowKey {
        tokens: Run,
        start_span: Span,
        key: Run,
        colon: usize,
        depth: usize,
    },
    ImplicitFlowValue {
        tokens: Run,
        start_span: Span,
        value: Run,
        value_end: usize,
    },
    FlowMappingKey {
        state: FlowState,
        key: Run,
    },
    FlowMappingValue {
        state: FlowState,
        value: Run,
    },
    /// A flow mapping entry without a value node is complete.
    FlowMappingNext(FlowState),
}

/// What a grammar function does next: run a call in its place, or return.
enum Step<'a> {
    Call(Call<'a>),
    Return(usize),
}

fn is_mapping_line(tokens: &[Token<'_>]) -> bool {
    match tokens.first().map(|token| &token.kind) {
        Some(TokenKind::Key | TokenKind::Value) => true,
        // A document marker ends the collection, whatever follows it on its line.
        Some(TokenKind::DocumentStart { .. } | TokenKind::DocumentEnd) | None => false,
        Some(_) => has_value_indicator(tokens),
    }
}
