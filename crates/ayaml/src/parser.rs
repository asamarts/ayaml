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
use crate::error::{Error, ErrorKind};
use crate::event::{Anchor, CollectionStyle, Event, ScalarStyle};
use crate::limits::Limits;
use crate::position::LineIndex;
use crate::scanner::{Scanner, Token, TokenKind};
use crate::span::{Marker, Span};
use crate::tag::Tag;
use crate::value::Value;

/// A logical line's token range in the parser's shared token arena.
#[derive(Clone, Debug)]
struct Line {
    start: usize,
    end: usize,
    indent: usize,
}

impl Line {
    fn new(tokens: &[Token<'_>], start: usize, end: usize) -> Self {
        let indent = tokens
            .get(start)
            .map_or(0, |token| token.span.start().column().saturating_sub(1));
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
        tokens.reserve_exact(chunk);
    }
    tokens.push(token);
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
                    lines.push(Line::new(&tokens, line_start, tokens.len()));
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
                    lines.push(Line::new(&tokens, line_start, tokens.len()));
                    line_start = tokens.len();
                }
            }
        }
    }
    if tokens.len() > line_start {
        lines.push(Line::new(&tokens, line_start, tokens.len()));
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
    emitted: usize,
    /// Stop at a second document start, as a single-document entry point
    /// needs: the rest of the stream is never parsed.
    single_document: bool,
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
    documents: usize,
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
            self.events += 1;
            let span = Span::empty(crate::span::Marker::new(0, 1, 1));
            self.limits
                .check_events(self.events)
                .map_err(|error| self.limit_at(span, error))?;
            return Ok(Some(Event::StreamStart { span }));
        }
        loop {
            if let Some(event) = self.queue.pop_front() {
                self.events += 1;
                self.limits
                    .check_events(self.events)
                    .map_err(|error| self.limit_at(event.span(), error))?;
                return Ok(Some(event));
            }
            if self.stream_ended {
                return Ok(None);
            }
            match self.load_chunk()? {
                Some(events) => {
                    self.events = self.events.saturating_add(events.len());
                    let span = events.first().map_or_else(
                        || Span::empty(crate::span::Marker::new(self.input.len(), 1, 1)),
                        Event::span,
                    );
                    self.limits
                        .check_events(self.events)
                        .map_err(|error| self.limit_at(span, error))?;
                    self.queue.extend(events);
                }
                None => {
                    self.stream_ended = true;
                    self.events += 1;
                    let span = Span::empty(crate::span::Marker::new(self.input.len(), 1, 1));
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
            lines.push(Line::new(&tokens, start, tokens.len()));
            if is_end {
                break;
            }
        }
        if lines.is_empty() {
            return Ok(None);
        }
        let events = parse_lines(
            Lines {
                lines,
                tokens,
                stream_start: None,
                stream_end: None,
            },
            &self.limits,
            self.input,
        )?;
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
            .unwrap_or_else(|| Span::empty(crate::span::Marker::new(self.input.len(), 1, 1)));
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

fn parse_lines<'a>(
    lines: Lines<'a>,
    limits: &Limits,
    input: &'a str,
) -> Result<Vec<Event<'a>>, Error> {
    let event_capacity = event_capacity(&lines.lines, &lines.tokens);
    let mut parser = Parser {
        lines: lines.lines,
        token_arena: lines.tokens,
        sink: EventSink::Events(Vec::with_capacity(event_capacity)),
        limits: limits.clone(),
        input,
        compat: false,
        documents: 0,
        emitted: 0,
        single_document: false,
    };
    parser.parse_stream()?;
    match parser.sink {
        EventSink::Events(events) => Ok(events),
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
    parse_events_with_compat(input, limits, false, false)
}

pub(crate) fn parse_events_compat<'a>(
    input: &'a str,
    limits: &Limits,
) -> Result<Vec<Event<'a>>, Error> {
    parse_events_with_compat(input, limits, true, false)
}

/// Parse the events of a single-document input. A second document is an
/// error at its start ("trailing documents").
pub(crate) fn parse_document_events<'a>(
    input: &'a str,
    limits: &Limits,
    compat_flow: bool,
) -> Result<Vec<Event<'a>>, Error> {
    parse_events_with_compat(input, limits, compat_flow, true)
}

fn parse_events_with_compat<'a>(
    input: &'a str,
    limits: &Limits,
    compat_flow: bool,
    single_document: bool,
) -> Result<Vec<Event<'a>>, Error> {
    let mut scanner = Scanner::with_compat(input, limits.clone(), compat_flow)?;
    let Lines {
        lines,
        tokens,
        stream_start,
        stream_end,
    } = read_lines(&mut scanner)?;

    let event_capacity = event_capacity(&lines, &tokens).saturating_add(2);
    let mut events = Vec::with_capacity(event_capacity);
    events.push(Event::StreamStart {
        span: stream_start.unwrap_or_else(|| Span::empty(crate::span::Marker::new(0, 1, 1))),
    });
    let mut parser = Parser {
        lines,
        token_arena: tokens,
        sink: EventSink::Events(events),
        limits: limits.clone(),
        input,
        compat: compat_flow,
        documents: 0,
        emitted: 0,
        single_document,
    };
    parser.parse_stream()?;
    match parser.sink {
        EventSink::Events(mut events) => {
            events.push(Event::StreamEnd {
                span: stream_end
                    .unwrap_or_else(|| Span::empty(crate::span::Marker::new(input.len(), 1, 1))),
            });
            Ok(events)
        }
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
    };
    parser.parse_stream()?;
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
                }
            }

            let mut explicit_end = false;
            if explicit_start || index < self.lines.len() {
                self.documents += 1;
                self.limits
                    .check_documents(self.documents)
                    .map_err(|error| self.limit_at(document_start_span, error))?;
                if self.single_document && self.documents > 1 {
                    return Err(self.error(
                        document_start_span,
                        "expected a single YAML document, but found trailing documents",
                    ));
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
                return Err(self.error(
                    self.line_first(index)
                        .map_or_else(|| self.empty_span(index), |token| token.span),
                    "unexpected content after document",
                ));
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
                self.expect_consumed(tokens, value, "unexpected token in mapping value")?;
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

    #[allow(clippy::too_many_arguments)]
    fn node(
        &mut self,
        mut index: usize,
        mut indent: usize,
        depth: usize,
        mut tag: Option<Tag>,
        mut anchor: Option<Anchor<'a>>,
        bounds: NodeBounds,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let span = self.empty_span(index);
        self.limits
            .check_depth(depth)
            .map_err(|error| self.limit_at(span, error))?;
        loop {
            let line = self.line_run(index);
            let (line_tag, line_anchor, rest_start) = self.properties(line)?;
            let inherited_tag = tag.clone();
            let inherited_anchor = anchor.clone();
            tag = line_tag.clone().or(tag);
            anchor = line_anchor.clone().or(anchor);
            let rest = line.from(rest_start);
            let duplicate = (inherited_tag.is_some() && line_tag.is_some())
                || (inherited_anchor.is_some() && line_anchor.is_some());
            if rest.is_empty() {
                if duplicate {
                    return Err(self.error(self.token(line, 0).span, DUPLICATE_PROPERTIES));
                }
                let next_index = index + 1;
                let has_properties = tag.is_some() || anchor.is_some();
                let next_indent = self.lines.get(next_index).map_or(0, |line| line.indent);
                // After a property-only line, a `-` line may start the node's
                // block sequence down to the parent's bound: a mapping value's
                // sequence may sit at the key's column, a sequence entry's
                // must be indented past its `-`.
                let next_is_indentless_sequence = next_indent >= bounds.sequence_indent
                    && self
                        .line_starts_with(next_index, |kind| matches!(kind, TokenKind::BlockEntry));
                // A block scalar header may sit at any column here, as in
                // libyaml; YAML 1.2.2 wants `min_indent`.
                let next_is_block_scalar = self.line_starts_with(next_index, |kind| {
                    matches!(
                        kind,
                        TokenKind::Scalar {
                            style: ScalarStyle::Literal | ScalarStyle::Folded,
                            ..
                        }
                    )
                });
                // Other content may sit left of the property line, down to
                // the parent's bound (`-\n    !t\n  x`).
                let next_is_node = next_index < self.lines.len()
                    && (next_indent >= indent.min(bounds.min_indent)
                        || next_is_indentless_sequence
                        || next_is_block_scalar)
                    && !self.line_starts_with(next_index, |kind| {
                        matches!(
                            kind,
                            TokenKind::DocumentStart { .. } | TokenKind::DocumentEnd
                        )
                    });
                if has_properties && next_is_node {
                    index = next_index;
                    indent = self.lines[index].indent;
                    continue;
                }
                let span = self.empty_span(index);
                let scalar = self.null_scalar_with(span, tag, anchor);
                self.emit(scalar)?;
                return Ok(Step::Return(index + 1));
            }

            if matches!(self.token(rest, 0).kind, TokenKind::BlockEntry) {
                if line_tag.is_some() || line_anchor.is_some() {
                    return Err(self.error(
                        self.token(rest, 0).span,
                        "properties cannot precede a compact block sequence",
                    ));
                }
                return Ok(Step::Call(Call::SequenceFirst {
                    index,
                    first: rest,
                    indent,
                    depth,
                    tag,
                    anchor,
                }));
            }
            if is_mapping_line(self.tokens(rest)) {
                // Properties on the same line as a block mapping key belong to
                // the key. A mapping's own properties must be on a preceding
                // property-only line or an enclosing flow collection.
                if line_tag.is_some() || line_anchor.is_some() {
                    return Ok(Step::Call(Call::MappingFirst {
                        first: line,
                        next_index: index + 1,
                        indent,
                        depth,
                        tag: inherited_tag,
                        anchor: inherited_anchor,
                    }));
                }
                return Ok(Step::Call(Call::MappingFirst {
                    first: rest,
                    next_index: index + 1,
                    indent,
                    depth,
                    tag,
                    anchor,
                }));
            }
            if duplicate {
                return Err(self.error(self.token(rest, 0).span, DUPLICATE_PROPERTIES));
            }
            stack.push(Cont::NodeInline { rest, index });
            return Ok(Step::Call(Call::Inline {
                tokens: rest,
                depth,
                tag,
                anchor,
            }));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn sequence_first(
        &mut self,
        index: usize,
        first: Run,
        indent: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let start_span = self.token(first, 0).span;
        self.limits
            .check_depth(depth + 1)
            .map_err(|error| self.limit_at(start_span, error))?;
        self.emit(Event::SequenceStart {
            tag,
            anchor,
            style: CollectionStyle::Block,
            span: start_span,
        })?;
        let state = BlockState {
            indent,
            depth,
            entries: 0,
            current_index: index + 1,
            first: Some(first.from(1)),
        };
        self.sequence_loop(state, stack)
    }

    fn sequence_loop(
        &mut self,
        mut state: BlockState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        loop {
            let (rest, next_index) = if let Some(rest) = state.first.take() {
                (rest, state.current_index)
            } else {
                let current_index = state.current_index;
                if current_index >= self.lines.len()
                    || self.lines[current_index].indent != state.indent
                    || !self.line_starts_with(current_index, |kind| {
                        matches!(kind, TokenKind::BlockEntry)
                    })
                {
                    self.emit(Event::SequenceEnd {
                        span: self.last_span(current_index),
                    })?;
                    return Ok(Step::Return(current_index));
                }
                (self.line_run(current_index).from(1), current_index + 1)
            };
            state.entries += 1;
            let span = self.last_span(state.current_index);
            self.limits
                .check_collection_entries(state.entries)
                .map_err(|error| self.limit_at(span, error))?;
            // The overwhelmingly common block-sequence entry is one scalar.
            // Emit it inline so a wide sequence does not pay a continuation
            // stack push/pop and nested parser call for every item.
            if let Some(token) = self.single_scalar(rest) {
                let event = Event::Scalar {
                    value: token.0.clone(),
                    raw: token.1.clone(),
                    style: token.2,
                    tag: None,
                    anchor: None,
                    span: token.3,
                };
                self.emit(event)?;
                state.current_index = next_index;
                continue;
            }
            let (indent, depth) = (state.indent, state.depth);
            stack.push(Cont::Sequence(state));
            return Ok(Step::Call(Call::SequenceItem {
                rest,
                next_index,
                indent,
                depth: depth + 1,
            }));
        }
    }

    /// A one-token scalar entry that needs no property or nested-node work.
    fn single_scalar(&self, run: Run) -> Option<(Cow<'a, str>, Cow<'a, str>, ScalarStyle, Span)> {
        if run.len() != 1 {
            return None;
        }
        let token = self.token(run, 0);
        match &token.kind {
            TokenKind::Scalar { value, raw, style } => {
                Some((value.clone(), raw.clone(), *style, token.span))
            }
            _ => None,
        }
    }

    fn sequence_item(
        &mut self,
        rest: Run,
        next_index: usize,
        indent: usize,
        depth: usize,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        if rest.is_empty() {
            if next_index < self.lines.len() && self.lines[next_index].indent > indent {
                return Ok(Step::Call(Call::Node {
                    index: next_index,
                    indent: self.lines[next_index].indent,
                    depth,
                    tag: None,
                    anchor: None,
                    bounds: NodeBounds::block_in(indent),
                }));
            }
            let span = self.empty_span(next_index);
            self.emit(self.null_scalar(span))?;
            return Ok(Step::Return(next_index));
        }
        let (tag, anchor, rest_start) = self.properties(rest)?;
        let content = rest.from(rest_start);
        if content.is_empty() {
            if tag.is_some() || anchor.is_some() {
                if next_index < self.lines.len() && self.lines[next_index].indent > indent {
                    return Ok(Step::Call(Call::Node {
                        index: next_index,
                        indent: self.lines[next_index].indent,
                        depth,
                        tag,
                        anchor,
                        bounds: NodeBounds::block_in(indent),
                    }));
                }
                let span = self.empty_span(next_index);
                self.emit(self.null_scalar_with(span, tag, anchor))?;
                return Ok(Step::Return(next_index));
            }
            let span = self.empty_span(next_index);
            self.emit(self.null_scalar(span))?;
            return Ok(Step::Return(next_index));
        }
        let content_first = self.token(content, 0);
        if matches!(content_first.kind, TokenKind::BlockEntry) {
            if tag.is_some() || anchor.is_some() {
                return Err(self.error(
                    content_first.span,
                    "properties cannot precede a compact block sequence",
                ));
            }
            let nested_indent = content_first.span.start().column().saturating_sub(1);
            return Ok(Step::Call(Call::SequenceFirst {
                index: next_index.saturating_sub(1),
                first: content,
                indent: nested_indent,
                depth,
                tag,
                anchor,
            }));
        }
        let explicit_key = matches!(content_first.kind, TokenKind::Key);
        if explicit_key || has_value_indicator(self.tokens(content)) {
            if explicit_key && (tag.is_some() || anchor.is_some()) {
                return Err(self.error(
                    self.token(content, 0).span,
                    "properties cannot precede an explicit mapping key",
                ));
            }
            // On a sequence item, properties before the first block key belong
            // to that key, not to the mapping node, so the mapping starts at
            // the first property (`- &x a: b` has its keys at column 2).
            let mapping_indent = self.token(rest, 0).span.start().column().saturating_sub(1);
            return Ok(Step::Call(Call::MappingFirst {
                first: rest,
                next_index,
                indent: mapping_indent,
                depth,
                tag: None,
                anchor: None,
            }));
        }
        stack.push(Cont::ItemInline {
            content,
            next_index,
        });
        Ok(Step::Call(Call::Inline {
            tokens: content,
            depth,
            tag,
            anchor,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn mapping_first(
        &mut self,
        first: Run,
        next_index: usize,
        indent: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let start_span = self.token(first, 0).span;
        self.limits
            .check_depth(depth + 1)
            .map_err(|error| self.limit_at(start_span, error))?;
        self.emit(Event::MappingStart {
            tag,
            anchor,
            style: CollectionStyle::Block,
            span: start_span,
        })?;
        let state = BlockState {
            indent,
            depth,
            entries: 0,
            current_index: next_index,
            first: Some(first),
        };
        self.mapping_loop(state, stack)
    }

    fn mapping_loop(
        &mut self,
        mut state: BlockState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        loop {
            let tokens = if let Some(first) = state.first.take() {
                first
            } else {
                let current_index = state.current_index;
                if current_index >= self.lines.len()
                    || self.lines[current_index].indent != state.indent
                    || !is_mapping_line(self.line_tokens(current_index))
                {
                    self.emit(Event::MappingEnd {
                        span: self.last_span(current_index),
                    })?;
                    return Ok(Step::Return(current_index));
                }
                state.current_index += 1;
                self.line_run(current_index)
            };
            let (indent, depth) = (state.indent, state.depth);

            // A very common explicit entry is `? key` followed by `: value`,
            // both single plain scalars. Emit the pair inline instead of paying
            // continuation frames for the key and value nodes.
            if matches!(self.token(tokens, 0).kind, TokenKind::Key) {
                let key_rest = tokens.from(1);
                let current_index = state.current_index;
                if let Some(key) = self.single_scalar(key_rest)
                    && current_index < self.lines.len()
                    && self.lines[current_index].indent == indent
                    && self.line_starts_with(current_index, |kind| matches!(kind, TokenKind::Value))
                {
                    let value = self.line_run(current_index).from(1);
                    if let Some(value) = self.single_scalar(value) {
                        state.entries += 1;
                        self.limits
                            .check_collection_entries(state.entries)
                            .map_err(|error| self.limit_at(key.3, error))?;
                        self.emit(Event::Scalar {
                            value: key.0,
                            raw: key.1,
                            style: key.2,
                            tag: None,
                            anchor: None,
                            span: key.3,
                        })?;
                        self.emit(Event::Scalar {
                            value: value.0,
                            raw: value.1,
                            style: value.2,
                            tag: None,
                            anchor: None,
                            span: value.3,
                        })?;
                        state.current_index = current_index + 1;
                        continue;
                    }
                }
            }

            if matches!(self.token(tokens, 0).kind, TokenKind::Key) {
                // `? <node>`: the rest of the line is a block-indented node, so a
                // `:` on this line belongs to a compact mapping inside the key.
                // The entry's value is a separate `:` line at this indentation.
                let key_rest = tokens.from(1);
                let current_index = state.current_index;
                if key_rest.is_empty() {
                    if current_index < self.lines.len() {
                        let next_indent = self.lines[current_index].indent;
                        let indentless_sequence = next_indent == indent
                            && self.line_starts_with(current_index, |kind| {
                                matches!(kind, TokenKind::BlockEntry)
                            });
                        if next_indent > indent || indentless_sequence {
                            stack.push(Cont::MappingExplicitKey(state));
                            return Ok(Step::Call(Call::Node {
                                index: current_index,
                                indent: next_indent,
                                depth: depth + 1,
                                tag: None,
                                anchor: None,
                                bounds: NodeBounds::block_out(indent),
                            }));
                        }
                    }
                    let span = Span::empty(self.token(tokens, 0).span.end());
                    self.emit(self.null_scalar(span))?;
                    return self.mapping_after_explicit_key(state, stack);
                }
                // Properties alone on the `?` line may be followed by an indentless
                // sequence, as on a mapping value's line.
                let (tag, anchor, rest_start) = self.properties(key_rest)?;
                if rest_start == key_rest.len()
                    && current_index < self.lines.len()
                    && self.lines[current_index].indent == indent
                    && self.line_starts_with(current_index, |kind| {
                        matches!(kind, TokenKind::BlockEntry)
                    })
                {
                    stack.push(Cont::MappingExplicitKey(state));
                    return Ok(Step::Call(Call::Node {
                        index: current_index,
                        indent,
                        depth: depth + 1,
                        tag,
                        anchor,
                        bounds: NodeBounds::block_out(indent),
                    }));
                }
                stack.push(Cont::MappingExplicitKey(state));
                return Ok(Step::Call(Call::SequenceItem {
                    rest: key_rest,
                    next_index: current_index,
                    indent,
                    depth: depth + 1,
                }));
            }

            let value_index = value_indicator_index(self.tokens(tokens));
            let key = value_index.map_or(tokens, |index| tokens.until(index));
            if !key.is_empty() {
                let key_tokens = self.tokens(key);
                let key_line = key_tokens[0].span.start().line();
                if key_tokens.iter().any(|token| {
                    token.span.start().line() != key_line || token.span.end().line() != key_line
                }) && flow_brackets_match(key_tokens)
                {
                    return Err(self.error(
                        key_tokens[0].span,
                        "implicit mapping keys must be on one line",
                    ));
                }
                if let Some(value_index) = value_index
                    && !implicit_key_fits(
                        key_tokens[0].span.start(),
                        self.token(tokens, value_index).span.start(),
                    )
                {
                    return Err(self.error(
                        key_tokens[0].span,
                        "an implicit mapping key may be at most 1024 characters long",
                    ));
                }
            }
            if key.is_empty() {
                let span = Span::empty(self.token(tokens, 0).span.start());
                self.emit(self.null_scalar(span))?;
                return self.mapping_after_implicit_key(state, tokens, value_index, stack);
            }
            stack.push(Cont::MappingImplicitKey {
                state,
                tokens,
                value_index,
                key,
            });
            return Ok(Step::Call(Call::Inline {
                tokens: key,
                depth,
                tag: None,
                anchor: None,
            }));
        }
    }

    fn mapping_after_explicit_key(
        &mut self,
        mut state: BlockState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        state.entries += 1;
        let span = self.empty_span(state.current_index);
        self.limits
            .check_collection_entries(state.entries)
            .map_err(|error| self.limit_at(span, error))?;
        let current_index = state.current_index;
        let has_value_line = current_index < self.lines.len()
            && self.lines[current_index].indent == state.indent
            && self.line_starts_with(current_index, |kind| matches!(kind, TokenKind::Value));
        if has_value_line {
            let value = self.line_run(current_index).from(1);
            state.current_index += 1;
            let (current_index, indent, depth) = (state.current_index, state.indent, state.depth);
            stack.push(Cont::MappingEntry(state));
            return Ok(Step::Call(Call::MappingValue {
                value,
                current_index,
                indent,
                depth,
                allow_compact: true,
            }));
        }
        let span = self.empty_span(current_index);
        self.emit(self.null_scalar(span))?;
        // Continue through the driver rather than calling `mapping_loop`:
        // a run of `?` entries must not nest a native frame per entry.
        stack.push(Cont::MappingEntry(state));
        Ok(Step::Return(current_index))
    }

    fn mapping_after_implicit_key(
        &mut self,
        mut state: BlockState,
        tokens: Run,
        value_index: Option<usize>,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        state.entries += 1;
        let span = self.token(tokens, 0).span;
        self.limits
            .check_collection_entries(state.entries)
            .map_err(|error| self.limit_at(span, error))?;
        let Some(value_index) = value_index else {
            return Err(self.error(
                self.token(tokens, 0).span,
                "explicit mapping key must be followed by a ':' line",
            ));
        };
        let (current_index, indent, depth) = (state.current_index, state.indent, state.depth);
        stack.push(Cont::MappingEntry(state));
        Ok(Step::Call(Call::MappingValue {
            value: tokens.from(value_index + 1),
            current_index,
            indent,
            depth,
            allow_compact: false,
        }))
    }

    fn mapping_value(
        &mut self,
        value: Run,
        current_index: usize,
        indent: usize,
        depth: usize,
        allow_compact: bool,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let (tag, anchor, rest_start) = self.properties(value)?;
        let content = value.from(rest_start);
        if content.is_empty() {
            let next_indent = self.lines.get(current_index).map_or(0, |line| line.indent);
            let indentless_sequence = next_indent == indent
                && self
                    .line_starts_with(current_index, |kind| matches!(kind, TokenKind::BlockEntry));
            if current_index < self.lines.len() && (next_indent > indent || indentless_sequence) {
                return Ok(Step::Call(Call::Node {
                    index: current_index,
                    indent: self.lines[current_index].indent,
                    depth: depth + 1,
                    tag,
                    anchor,
                    bounds: NodeBounds::block_out(indent),
                }));
            }
            let span = self.empty_span(current_index);
            self.emit(self.null_scalar_with(span, tag, anchor))?;
            return Ok(Step::Return(current_index));
        }
        let content_first = self.token(content, 0);
        if allow_compact && matches!(content_first.kind, TokenKind::BlockEntry) {
            if tag.is_some() || anchor.is_some() {
                return Err(self.error(
                    content_first.span,
                    "properties cannot precede a compact block sequence",
                ));
            }
            let sequence_indent = content_first.span.start().column().saturating_sub(1);
            return Ok(Step::Call(Call::SequenceFirst {
                index: current_index.saturating_sub(1),
                first: content,
                indent: sequence_indent,
                depth: depth + 1,
                tag,
                anchor,
            }));
        }
        let explicit_key = matches!(content_first.kind, TokenKind::Key);
        if allow_compact && (explicit_key || has_value_indicator(self.tokens(content))) {
            if explicit_key && (tag.is_some() || anchor.is_some()) {
                return Err(self.error(
                    self.token(content, 0).span,
                    "properties cannot precede an explicit mapping key",
                ));
            }
            // As on a sequence item: properties belong to the first key, and
            // the compact mapping starts at the first property.
            let mapping_indent = self.token(value, 0).span.start().column().saturating_sub(1);
            return Ok(Step::Call(Call::MappingFirst {
                first: value,
                next_index: current_index,
                indent: mapping_indent,
                depth: depth + 1,
                tag: None,
                anchor: None,
            }));
        }
        stack.push(Cont::ValueInline {
            value,
            current_index,
        });
        Ok(Step::Call(Call::Inline {
            tokens: value,
            depth: depth + 1,
            tag: None,
            anchor: None,
        }))
    }

    /// Parse one node from a run of a line's tokens. Returns how many
    /// tokens it consumed.
    fn inline(
        &mut self,
        tokens: Run,
        depth: usize,
        mut tag: Option<Tag>,
        mut anchor: Option<Anchor<'a>>,
    ) -> Result<Step<'a>, Error> {
        let mut index = 0;
        if self.starts_with_property(tokens) {
            let (own_tag, own_anchor, count) = self.read_properties(tokens)?;
            tag = own_tag.or(tag);
            anchor = own_anchor.or(anchor);
            index = count;
        }
        if index >= tokens.len() {
            if index > 0 {
                let span = Span::empty(self.token(tokens, index - 1).span.end());
                self.emit(self.null_scalar_with(span, tag, anchor))?;
                return Ok(Step::Return(index));
            }
            return Err(self.flow_error(tokens, 0, FlowLack::Node("expected a node".to_owned())));
        }
        let token = self.token(tokens, index);
        let span = token.span;
        match &token.kind {
            TokenKind::Scalar { value, raw, style } => {
                let event = Event::Scalar {
                    value: value.clone(),
                    raw: raw.clone(),
                    style: *style,
                    tag,
                    anchor,
                    span,
                };
                self.emit(event)?;
                Ok(Step::Return(index + 1))
            }
            TokenKind::Alias(name) => {
                if tag.is_some() || anchor.is_some() {
                    return Err(self.error(span, "aliases cannot carry tags or anchors"));
                }
                let event = Event::Alias {
                    anchor: Anchor::new(name.clone()),
                    span,
                };
                self.emit(event)?;
                Ok(Step::Return(index + 1))
            }
            TokenKind::FlowSequenceStart => Ok(Step::Call(Call::FlowSequence {
                tokens,
                start: index,
                depth,
                tag,
                anchor,
            })),
            TokenKind::FlowMappingStart => Ok(Step::Call(Call::FlowMapping {
                tokens,
                start: index,
                depth,
                tag,
                anchor,
            })),
            // Properties before `,`, `]`, or `}` belong to an empty node
            // (`[!t ]`, `[&a , b]`; YAML 1.2.2 production 161).
            TokenKind::FlowEntry | TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd
                if index > 0 =>
            {
                self.emit(self.null_scalar_with(Span::empty(span.start()), tag, anchor))?;
                Ok(Step::Return(index))
            }
            kind => {
                let problem = format!("unexpected {kind} in node position");
                Err(self.flow_error(tokens, index, FlowLack::Node(problem)))
            }
        }
    }

    fn flow_sequence(
        &mut self,
        tokens: Run,
        start: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let start_span = self.token(tokens, start).span;
        self.limits
            .check_depth(depth + 1)
            .map_err(|error| self.limit_at(start_span, error))?;
        self.emit(Event::SequenceStart {
            tag,
            anchor,
            style: CollectionStyle::Flow,
            span: start_span,
        })?;
        let state = FlowState {
            tokens,
            start_span,
            index: start + 1,
            entries: 0,
            depth,
        };
        self.flow_sequence_loop(state, stack)
    }

    fn flow_sequence_loop(
        &mut self,
        mut state: FlowState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let tokens = state.tokens;
        let index = state.index;
        if index >= tokens.len() {
            return Err(self.flow_error(tokens, index, FlowLack::Separator));
        }
        let token = self.token(tokens, index);
        if matches!(token.kind, TokenKind::FlowSequenceEnd) {
            let end_span = token.span;
            self.emit(Event::SequenceEnd { span: end_span })?;
            return Ok(Step::Return(index + 1));
        }
        if matches!(token.kind, TokenKind::FlowEntry) {
            return Err(self.flow_error(
                tokens,
                index,
                FlowLack::Node("unexpected empty flow sequence entry".to_owned()),
            ));
        }
        state.entries += 1;
        self.limits
            .check_collection_entries(state.entries)
            .map_err(|error| self.limit_at(token.span, error))?;
        let depth = state.depth + 1;
        if matches!(self.token(tokens, index).kind, TokenKind::Key) {
            stack.push(Cont::FlowSequenceItem {
                state,
                inline: false,
            });
            return Ok(Step::Call(Call::FlowExplicitMapping {
                tokens,
                start: index,
                depth,
            }));
        }
        if let Some(colon) = self.implicit_flow_colon(self.tokens(tokens), index) {
            stack.push(Cont::FlowSequenceItem {
                state,
                inline: false,
            });
            return Ok(Step::Call(Call::ImplicitFlowMapping {
                tokens,
                start: index,
                colon,
                depth,
            }));
        }
        stack.push(Cont::FlowSequenceItem {
            state,
            inline: true,
        });
        Ok(Step::Call(Call::Inline {
            tokens: tokens.from(index),
            depth,
            tag: None,
            anchor: None,
        }))
    }

    fn flow_explicit_mapping(
        &mut self,
        tokens: Run,
        start: usize,
        depth: usize,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let start_span = self.token(tokens, start).span;
        self.limits
            .check_depth(depth)
            .map_err(|error| self.limit_at(start_span, error))?;
        self.emit(Event::MappingStart {
            tag: None,
            anchor: None,
            style: CollectionStyle::Flow,
            span: start_span,
        })?;
        let key_start = start + 1;
        let key_end = self.find_flow_value(self.tokens(tokens), key_start);
        let key = tokens.slice(key_start, key_end);
        if key.is_empty() {
            self.emit(self.null_scalar(Span::empty(start_span.end())))?;
            return self.flow_explicit_value(tokens, start_span, key_end, depth, stack);
        }
        stack.push(Cont::FlowExplicitKey {
            tokens,
            start_span,
            key,
            key_end,
            depth,
        });
        Ok(Step::Call(Call::Inline {
            tokens: key,
            depth,
            tag: None,
            anchor: None,
        }))
    }

    fn flow_explicit_value(
        &mut self,
        tokens: Run,
        start_span: Span,
        key_end: usize,
        depth: usize,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let mut index = key_end;
        if index < tokens.len() && matches!(self.token(tokens, index).kind, TokenKind::Value) {
            index += 1;
            let value_end = self.find_flow_item_end(self.tokens(tokens), index);
            if index == value_end {
                let span = Span::empty(self.token(tokens, index - 1).span.end());
                self.emit(self.null_scalar(span))?;
                return self.flow_mapping_end(tokens, start_span, value_end);
            }
            let value = tokens.slice(index, value_end);
            stack.push(Cont::FlowExplicitValue {
                tokens,
                start_span,
                value,
                value_end,
            });
            return Ok(Step::Call(Call::Inline {
                tokens: value,
                depth,
                tag: None,
                anchor: None,
            }));
        }
        let span = self.empty_before(tokens, index, start_span);
        self.emit(self.null_scalar(span))?;
        self.flow_mapping_end(tokens, start_span, index)
    }

    /// End a single-pair flow mapping before the token at `index`.
    fn flow_mapping_end(
        &mut self,
        tokens: Run,
        start_span: Span,
        index: usize,
    ) -> Result<Step<'a>, Error> {
        let span = self.empty_before(tokens, index, start_span);
        self.emit(Event::MappingEnd { span })?;
        Ok(Step::Return(index))
    }

    fn implicit_flow_mapping(
        &mut self,
        tokens: Run,
        start: usize,
        colon: usize,
        depth: usize,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let start_span = self.token(tokens, start).span;
        self.limits
            .check_depth(depth)
            .map_err(|error| self.limit_at(start_span, error))?;
        self.emit(Event::MappingStart {
            tag: None,
            anchor: None,
            style: CollectionStyle::Flow,
            span: start_span,
        })?;
        let key_start = start
            + usize::from(matches!(
                self.token_at(tokens, start).map(|token| &token.kind),
                Some(TokenKind::Key)
            ));
        let key = tokens.slice(key_start, colon);
        if key.is_empty() {
            let span = Span::empty(self.token(tokens, colon).span.start());
            self.emit(self.null_scalar(span))?;
            return self.implicit_flow_value(tokens, start_span, colon, depth, stack);
        }
        stack.push(Cont::ImplicitFlowKey {
            tokens,
            start_span,
            key,
            colon,
            depth,
        });
        Ok(Step::Call(Call::Inline {
            tokens: key,
            depth,
            tag: None,
            anchor: None,
        }))
    }

    fn implicit_flow_value(
        &mut self,
        tokens: Run,
        start_span: Span,
        colon: usize,
        depth: usize,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let value_start = colon + 1;
        let value_end = self.find_flow_item_end(self.tokens(tokens), value_start);
        if value_start >= tokens.len() || value_start == value_end {
            let span = Span::empty(self.token(tokens, colon).span.end());
            self.emit(self.null_scalar(span))?;
            return self.flow_mapping_end(tokens, start_span, value_end);
        }
        let value = tokens.slice(value_start, value_end);
        stack.push(Cont::ImplicitFlowValue {
            tokens,
            start_span,
            value,
            value_end,
        });
        Ok(Step::Call(Call::Inline {
            tokens: value,
            depth,
            tag: None,
            anchor: None,
        }))
    }

    fn flow_mapping(
        &mut self,
        tokens: Run,
        start: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let start_span = self.token(tokens, start).span;
        self.limits
            .check_depth(depth + 1)
            .map_err(|error| self.limit_at(start_span, error))?;
        self.emit(Event::MappingStart {
            tag,
            anchor,
            style: CollectionStyle::Flow,
            span: start_span,
        })?;
        let state = FlowState {
            tokens,
            start_span,
            index: start + 1,
            entries: 0,
            depth,
        };
        self.flow_mapping_loop(state, stack)
    }

    fn flow_mapping_loop(
        &mut self,
        mut state: FlowState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let tokens = state.tokens;
        let mut index = state.index;
        if index >= tokens.len() {
            return Err(self.flow_error(tokens, index, FlowLack::Separator));
        }
        let token = self.token(tokens, index);
        if matches!(token.kind, TokenKind::FlowMappingEnd) {
            let end_span = token.span;
            self.emit(Event::MappingEnd { span: end_span })?;
            return Ok(Step::Return(index + 1));
        }
        if matches!(token.kind, TokenKind::FlowEntry) {
            return Err(self.flow_error(
                tokens,
                index,
                FlowLack::Node("unexpected empty flow mapping entry".to_owned()),
            ));
        }
        if matches!(token.kind, TokenKind::Key) {
            index += 1;
        }
        let key_end = self.find_flow_value(self.tokens(tokens), index);
        let key = tokens.slice(index, key_end);
        state.index = key_end;
        if key.is_empty() {
            // Where a key starts, after `{`, `,` or `?`, nothing stands for it, so a `]` is where a
            // node belongs, as in a sequence. After an entry with no `,`, a separator is missing.
            if index > 0
                && matches!(
                    self.token(tokens, index - 1).kind,
                    TokenKind::FlowMappingStart | TokenKind::FlowEntry | TokenKind::Key
                )
                && self
                    .token_at(tokens, key_end)
                    .is_some_and(|token| matches!(token.kind, TokenKind::FlowSequenceEnd))
            {
                return Err(self.flow_error(
                    tokens,
                    key_end,
                    FlowLack::Node("unexpected ] in node position".to_owned()),
                ));
            }
            let span = Span::empty(self.token(tokens, index.saturating_sub(1)).span.end());
            self.emit(self.null_scalar(span))?;
            return self.flow_mapping_after_key(state, stack);
        }
        let depth = state.depth + 1;
        stack.push(Cont::FlowMappingKey { state, key });
        Ok(Step::Call(Call::Inline {
            tokens: key,
            depth,
            tag: None,
            anchor: None,
        }))
    }

    fn flow_mapping_after_key(
        &mut self,
        mut state: FlowState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let tokens = state.tokens;
        let mut index = state.index;
        // A key ends at the end of its tokens, or at a `]` that closes nothing here.
        let Some(token) = self
            .token_at(tokens, index)
            .filter(|token| !matches!(token.kind, TokenKind::FlowSequenceEnd))
        else {
            return Err(self.flow_error(tokens, index, FlowLack::Separator));
        };
        state.entries += 1;
        let span = token.span;
        self.limits
            .check_collection_entries(state.entries)
            .map_err(|error| self.limit_at(span, error))?;
        if !matches!(self.token(tokens, index).kind, TokenKind::Value) {
            // Flow mappings permit omitted values: `{key, other: value}`.
            let span = self.empty_before(tokens, index, state.start_span);
            self.emit(self.null_scalar(span))?;
        } else {
            index += 1;
        }
        if index >= tokens.len()
            || matches!(
                self.token(tokens, index).kind,
                TokenKind::FlowEntry | TokenKind::FlowMappingEnd
            )
        {
            if matches!(
                self.token(tokens, index.saturating_sub(1)).kind,
                TokenKind::Value
            ) {
                let span = self.empty_before(tokens, index, state.start_span);
                self.emit(self.null_scalar(span))?;
            }
            state.index = index;
            // Through the driver, as for block `?` entries: `{:, :, …}`
            // must not nest a native frame per entry.
            stack.push(Cont::FlowMappingNext(state));
            return Ok(Step::Return(0));
        }
        let value_end = self.find_flow_item_end(self.tokens(tokens), index);
        // Only a `]` ends a value before it starts, since the other closers returned above.
        if value_end == index {
            return Err(self.flow_error(
                tokens,
                index,
                FlowLack::Node("unexpected ] in node position".to_owned()),
            ));
        }
        let value = tokens.slice(index, value_end);
        state.index = value_end;
        let depth = state.depth + 1;
        stack.push(Cont::FlowMappingValue { state, value });
        Ok(Step::Call(Call::Inline {
            tokens: value,
            depth,
            tag: None,
            anchor: None,
        }))
    }

    fn flow_mapping_next(
        &mut self,
        mut state: FlowState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        if state.index < state.tokens.len()
            && matches!(
                self.token(state.tokens, state.index).kind,
                TokenKind::FlowEntry
            )
        {
            state.index += 1;
        }
        self.flow_mapping_loop(state, stack)
    }

    fn implicit_flow_colon(&self, tokens: &[Token<'a>], start: usize) -> Option<usize> {
        let mut depth = 0usize;
        for (index, token) in tokens.iter().enumerate().skip(start) {
            match token.kind {
                TokenKind::FlowSequenceStart | TokenKind::FlowMappingStart => depth += 1,
                TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd => {
                    if depth == 0 {
                        return None;
                    }
                    depth -= 1;
                }
                TokenKind::Value if depth == 0 => {
                    let key_start = tokens[start].span.start();
                    let implicit_key = key_start.line() == token.span.start().line()
                        && implicit_key_fits(key_start, token.span.start());
                    if implicit_key || matches!(tokens[start].kind, TokenKind::Key) {
                        return Some(index);
                    }
                    return None;
                }
                TokenKind::FlowEntry if depth == 0 => return None,
                _ => {}
            }
        }
        None
    }

    /// Where the flow key from `start` ends. With no end in sight the parse goes on to the end of
    /// `tokens`, and finds what is wrong in order.
    fn find_flow_value(&self, tokens: &[Token<'a>], start: usize) -> usize {
        let mut depth = 0usize;
        for (index, token) in tokens.iter().enumerate().skip(start) {
            match token.kind {
                TokenKind::FlowSequenceStart | TokenKind::FlowMappingStart => depth += 1,
                TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd => {
                    if depth == 0 {
                        return index;
                    }
                    depth -= 1;
                }
                TokenKind::Value if depth == 0 => return index,
                TokenKind::FlowEntry if depth == 0 => return index,
                _ => {}
            }
        }
        tokens.len()
    }

    /// Where the flow item from `start` ends, on the same terms as [`Self::find_flow_value`].
    fn find_flow_item_end(&self, tokens: &[Token<'a>], start: usize) -> usize {
        let mut depth = 0usize;
        for (index, token) in tokens.iter().enumerate().skip(start) {
            match token.kind {
                TokenKind::FlowSequenceStart | TokenKind::FlowMappingStart => depth += 1,
                TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd => {
                    if depth == 0 {
                        return index;
                    }
                    depth -= 1;
                }
                TokenKind::FlowEntry if depth == 0 => return index,
                _ => {}
            }
        }
        tokens.len()
    }

    /// The error for a flow collection that fails at `index` of `tokens`, or at the end of the
    /// input past the last of them. Inside a collection it names the failure and the opening.
    #[cold]
    fn flow_error(&self, tokens: Run, index: usize, lack: FlowLack) -> Error {
        let failure = self.token_at(tokens, index).map(|token| token.span);
        let span = failure.unwrap_or_else(|| self.end_of_input());
        let Some((opening, sequence)) = self.open_flow(tokens, index) else {
            return self.error(
                span,
                match lack {
                    FlowLack::Separator => "expected ',' or a closing bracket".to_owned(),
                    FlowLack::Node(problem) => problem,
                },
            );
        };
        let (name, closing) = if sequence {
            ("sequence", ']')
        } else {
            ("mapping", '}')
        };
        let from = opening.start();
        let there = format!("at line {} column {}", from.line(), from.column());
        // An input that ends where an entry, a key or a value starts lacks a node, not a `,`, as
        // libyaml words it.
        let lack = match lack {
            FlowLack::Separator if self.compat && failure.is_none() => {
                let last = tokens
                    .len()
                    .checked_sub(1)
                    .and_then(|last| self.token_at(tokens, last));
                if last.is_some_and(|token| {
                    matches!(
                        token.kind,
                        TokenKind::FlowSequenceStart
                            | TokenKind::FlowMappingStart
                            | TokenKind::FlowEntry
                            | TokenKind::Key
                            | TokenKind::Value
                    )
                }) {
                    FlowLack::Node(String::new())
                } else {
                    FlowLack::Separator
                }
            }
            lack => lack,
        };
        // The failure's location goes between the two when the error is displayed.
        let (problem, context) = match lack {
            FlowLack::Separator if self.compat => (
                format!("did not find expected ',' or '{closing}'"),
                format!(", while parsing a flow {name} {there}"),
            ),
            FlowLack::Node(_) if self.compat => (
                "did not find expected node content".to_owned(),
                format!(", while parsing a flow {name} {there}"),
            ),
            FlowLack::Separator if failure.is_none() => (
                format!("unterminated flow {name}"),
                format!(", opened {there}"),
            ),
            FlowLack::Separator => (
                format!("expected ',' or '{closing}'"),
                format!(", in the flow {name} opened {there}"),
            ),
            FlowLack::Node(problem) => (problem, format!(", in the flow {name} opened {there}")),
        };
        self.error(span, problem).with_context(&context)
    }

    /// The innermost flow collection still open before `index` of `tokens`: its opening token's
    /// span, and whether it is a sequence. A closing bracket here closes what it follows, since
    /// the parse stops at the first one that doesn't match.
    fn open_flow(&self, tokens: Run, index: usize) -> Option<(Span, bool)> {
        let line = &self.lines[tokens.line];
        let before = (line.start + tokens.start + index).min(line.end);
        let mut closed = 0_usize;
        for token in self.token_arena[line.start..before].iter().rev() {
            match token.kind {
                TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd => closed += 1,
                TokenKind::FlowSequenceStart | TokenKind::FlowMappingStart if closed > 0 => {
                    closed -= 1;
                }
                TokenKind::FlowSequenceStart => return Some((token.span, true)),
                TokenKind::FlowMappingStart => return Some((token.span, false)),
                _ => {}
            }
        }
        None
    }

    /// An empty span at the end of the input, counted as the scanner counts it.
    fn end_of_input(&self) -> Span {
        let index = if self.compat {
            LineIndex::counting_line_boms(self.input)
        } else {
            LineIndex::new(self.input)
        };
        let end = self.input.len();
        Span::empty(
            index
                .marker(self.input, end)
                .unwrap_or_else(|| Marker::new(end, 1, 1)),
        )
    }

    /// A node's properties at the start of `run`, and how many tokens they are. A node
    /// has at most one tag and one anchor, in either order.
    #[inline]
    fn properties(&self, run: Run) -> Result<Properties<'a>, Error> {
        // Most nodes have none.
        if !self.starts_with_property(run) {
            return Ok((None, None, 0));
        }
        self.read_properties(run)
    }

    #[inline]
    fn starts_with_property(&self, run: Run) -> bool {
        matches!(
            self.tokens(run).first().map(|token| &token.kind),
            Some(TokenKind::Tag(_) | TokenKind::Anchor(_))
        )
    }

    fn read_properties(&self, run: Run) -> Result<Properties<'a>, Error> {
        let mut tag = None;
        let mut anchor = None;
        let mut index = 0;
        for token in self.tokens(run) {
            let duplicate = match &token.kind {
                TokenKind::Tag(value) => tag.replace(value.clone()).is_some(),
                TokenKind::Anchor(value) => anchor.replace(Anchor::new(value.clone())).is_some(),
                _ => break,
            };
            if duplicate {
                return Err(self.error(token.span, DUPLICATE_PROPERTIES));
            }
            index += 1;
        }
        Ok((tag, anchor, index))
    }

    /// All of line `index`'s tokens.
    fn line_first(&self, index: usize) -> Option<&Token<'a>> {
        let line = self.lines.get(index)?;
        if line.is_empty() {
            return None;
        }
        self.token_arena.get(line.start)
    }

    fn line_last(&self, index: usize) -> Option<&Token<'a>> {
        let line = self.lines.get(index)?;
        if line.is_empty() {
            return None;
        }
        line.end
            .checked_sub(1)
            .and_then(|end| self.token_arena.get(end))
    }

    fn line_tokens(&self, index: usize) -> &[Token<'a>] {
        let line = &self.lines[index];
        &self.token_arena[line.start..line.end]
    }

    fn take_line_first_span(&mut self, index: usize) -> Option<Span> {
        let line = self.lines.get_mut(index)?;
        if line.is_empty() {
            return None;
        }
        let span = self.token_arena[line.start].span;
        line.advance(1);
        Some(span)
    }

    fn line_run(&self, index: usize) -> Run {
        Run {
            line: index,
            start: 0,
            end: self.lines[index].len(),
        }
    }

    fn tokens(&self, run: Run) -> &[Token<'a>] {
        let line = &self.lines[run.line];
        &self.token_arena[line.start + run.start..line.start + run.end]
    }

    /// The token at `index` within `run`, which must exist.
    fn token(&self, run: Run, index: usize) -> &Token<'a> {
        &self.tokens(run)[index]
    }

    fn token_at(&self, run: Run, index: usize) -> Option<&Token<'a>> {
        self.tokens(run).get(index)
    }

    /// An empty span at the start of `run`'s token at `index`, or, past the
    /// end of the run, at the end of `start` (a flow collection's opening
    /// token).
    fn empty_before(&self, run: Run, index: usize, start: Span) -> Span {
        self.token_at(run, index).map_or_else(
            || Span::empty(start.end()),
            |token| Span::empty(token.span.start()),
        )
    }

    fn null_scalar(&self, span: Span) -> Event<'a> {
        self.null_scalar_with(span, None, None)
    }

    fn null_scalar_with(
        &self,
        span: Span,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
    ) -> Event<'a> {
        Event::Scalar {
            value: Cow::Borrowed(""),
            raw: Cow::Borrowed(""),
            style: ScalarStyle::Plain,
            tag,
            anchor,
            span,
        }
    }

    fn emit(&mut self, event: Event<'a>) -> Result<(), Error> {
        self.emitted = self.emitted.saturating_add(1);
        let span = event.span();
        self.limits
            .check_events(self.emitted)
            .map_err(|error| self.limit_at(span, error))?;
        self.sink.event(event)
    }

    fn line_starts_with(&self, index: usize, predicate: impl Fn(&TokenKind<'a>) -> bool) -> bool {
        self.line_first(index)
            .is_some_and(|token| predicate(&token.kind))
    }

    fn empty_span(&self, index: usize) -> Span {
        if self.lines.get(index).is_some()
            && let Some(token) = self.line_first(index)
        {
            let marker = token.span.start();
            return Span::empty(marker);
        }
        Span::empty(crate::span::Marker::new(self.input.len(), 1, 1))
    }

    fn last_span(&self, index: usize) -> Span {
        if index > 0
            && self.lines.get(index - 1).is_some()
            && let Some(token) = self.line_last(index - 1)
        {
            return Span::empty(token.span.end());
        }
        self.empty_span(index)
    }

    fn error(&self, span: Span, message: impl Into<String>) -> Error {
        Error::new(ErrorKind::Parser, message).with_span(span)
    }

    fn limit_at(&self, span: Span, limit: crate::limits::LimitExceeded) -> Error {
        Error::from(limit).with_span(span)
    }
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
fn implicit_key_fits(key: crate::span::Marker, colon: crate::span::Marker) -> bool {
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
    tokens
        .first()
        .is_some_and(|token| matches!(token.kind, TokenKind::Key | TokenKind::Value))
        || has_value_indicator(tokens)
}

#[cfg(test)]
mod tests {
    use super::parse_events;
    use crate::error::ErrorKind;
    use crate::event::Event;
    use crate::limits::Limits;

    fn events(input: &str) -> Vec<Event<'_>> {
        parse_events(input, &Limits::default()).expect("parse")
    }

    fn event_kinds(input: &str) -> Vec<&'static str> {
        events(input)
            .iter()
            .map(|event| match event {
                Event::StreamStart { .. } => "stream-start",
                Event::StreamEnd { .. } => "stream-end",
                Event::DocumentStart { .. } => "doc-start",
                Event::DocumentEnd { .. } => "doc-end",
                Event::Scalar { .. } => "scalar",
                Event::SequenceStart { .. } => "seq-start",
                Event::SequenceEnd { .. } => "seq-end",
                Event::MappingStart { .. } => "map-start",
                Event::MappingEnd { .. } => "map-end",
                Event::Directive { .. } => "directive",
                Event::Alias { .. } => "alias",
            })
            .collect()
    }

    #[test]
    fn simple_mapping_events() {
        let kinds = event_kinds("a: 1\nb: 2\n");
        assert_eq!(
            kinds,
            vec![
                "stream-start",
                "doc-start",
                "map-start",
                "scalar",
                "scalar",
                "scalar",
                "scalar",
                "map-end",
                "doc-end",
                "stream-end",
            ]
        );
    }

    #[test]
    fn block_sequence_events() {
        let kinds = event_kinds("- a\n- b\n");
        assert_eq!(
            kinds,
            vec![
                "stream-start",
                "doc-start",
                "seq-start",
                "scalar",
                "scalar",
                "seq-end",
                "doc-end",
                "stream-end",
            ]
        );
    }

    #[test]
    fn sequence_of_mappings_events() {
        let input = "- id: a\n  x: 1\n- id: b\n  x: 2\n";
        let kinds = event_kinds(input);
        assert!(kinds.contains(&"seq-start"));
        assert_eq!(kinds.iter().filter(|kind| **kind == "map-start").count(), 2);
        assert_eq!(kinds.iter().filter(|kind| **kind == "map-end").count(), 2);
    }

    #[test]
    fn nested_sequence_events() {
        let kinds = event_kinds("- - a\n  - b\n");
        assert_eq!(kinds.iter().filter(|kind| **kind == "seq-start").count(), 2);
        assert_eq!(kinds.iter().filter(|kind| **kind == "seq-end").count(), 2);
    }

    #[test]
    fn flow_sequence_and_mapping_events() {
        let kinds = event_kinds("a: [1, {b: 2}]\n");
        assert!(kinds.contains(&"seq-start"));
        assert!(kinds.contains(&"map-start"));
        assert!(kinds.contains(&"seq-end"));
        assert!(kinds.contains(&"map-end"));
    }

    #[test]
    fn anchors_tags_and_aliases_are_events() {
        let input = "base: &b !Custom {x: 1}\nuse: *b\n";
        let events = events(input);
        assert!(events.iter().any(|event| matches!(event, Event::MappingStart { anchor: Some(anchor), tag: Some(tag), .. } if anchor.as_str() == "b" && tag.to_string() == "!Custom")));
        assert!(
            events.iter().any(
                |event| matches!(event, Event::Alias { anchor, .. } if anchor.as_str() == "b")
            )
        );
    }

    #[test]
    fn multi_document_limit_is_enforced() {
        let limits = Limits {
            max_documents: 2,
            ..Limits::default()
        };
        let events = parse_events("a: 1\n---\nb: 2\n", &limits).expect("parse");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::DocumentStart { .. }))
                .count(),
            2
        );
        let error = parse_events("a: 1\n---\nb: 2\n", &Limits::compat()).expect_err("limit");
        assert_eq!(error.kind(), ErrorKind::Limit);
    }

    #[test]
    fn toolchain_fixture_parses_to_events() {
        let input = include_str!("../../../tests/fixtures/compat/toolchain-pins.yml");
        let events = parse_events(input, &Limits::default()).expect("toolchain fixture");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::MappingStart { .. }))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::SequenceStart { .. }))
        );
    }

    #[test]
    fn scalar_tags_fixture_parses_to_events() {
        let input = include_str!("../../../tests/fixtures/compat/scalar-tags-merges-aliases.yml");
        let events = parse_events(input, &Limits::default()).expect("scalar fixture");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Alias { .. }))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::MappingStart { tag: Some(_), .. }))
        );
    }

    #[test]
    fn empty_document_is_a_null_scalar() {
        let events = events("---\n");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Scalar { value, .. } if value.is_empty()))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::DocumentStart { explicit: true, .. }))
        );
    }

    #[test]
    fn nested_block_mapping_and_sequence() {
        let input = "root:\n  items:\n    - one\n    - two\n  next: 3\n";
        let kinds = event_kinds(input);
        assert_eq!(kinds.iter().filter(|kind| **kind == "map-start").count(), 2);
        assert_eq!(kinds.iter().filter(|kind| **kind == "seq-start").count(), 1);
    }

    #[test]
    fn scalar_spans_slice_the_original_input() {
        let input = "name: \"ayaml\"\n";
        let events = events(input);
        let scalar = events
            .iter()
            .find_map(|event| match event {
                Event::Scalar { value, span, .. } if value == "ayaml" => Some(*span),
                _ => None,
            })
            .expect("scalar");
        assert_eq!(scalar.slice(input), Some("\"ayaml\""));
    }

    #[test]
    fn comments_only_is_an_empty_stream() {
        let events = events("# comment only\n");
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], Event::StreamStart { .. }));
        assert!(matches!(events[1], Event::StreamEnd { .. }));
    }

    #[test]
    fn bom_crlf_and_comments_parse() {
        let events = events("\u{feff}a: 1 # comment\r\nb: 2\r\n");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::MappingStart { .. }))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Scalar { value, .. } if value == "1"))
        );
    }

    #[test]
    fn malformed_flow_is_a_parser_or_scanner_error() {
        let error = parse_events("a: [1,\n", &Limits::default()).expect_err("malformed flow");
        assert!(matches!(
            error.kind(),
            ErrorKind::Parser | ErrorKind::Scanner
        ));
    }

    #[test]
    fn block_scalar_events_are_emitted() {
        let input = "a: |\n  line1\n  line2\n";
        let events = events(input);
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar {
                value,
                style: crate::event::ScalarStyle::Literal,
                span,
                ..
            } if value == "line1\nline2\n" && span.slice(input) == Some("|\n  line1\n  line2\n")
        )));
    }

    #[test]
    fn explicit_and_complex_keys_are_supported() {
        let kinds = event_kinds("? key\n: value\n");
        assert!(kinds.contains(&"map-start"));
        assert!(kinds.contains(&"map-end"));

        let kinds = event_kinds("? [a, b]\n: value\n");
        assert!(kinds.contains(&"seq-start"));
        assert!(kinds.contains(&"map-start"));

        let kinds = event_kinds(": empty-key-value\n");
        assert!(kinds.contains(&"map-start"));
    }

    #[test]
    fn explicit_key_can_share_a_line_with_its_value() {
        let kinds = event_kinds("? key : value\n");
        assert!(kinds.contains(&"map-start"));
        assert!(kinds.contains(&"scalar"));
    }

    #[test]
    fn flow_explicit_and_empty_keys_are_supported() {
        let kinds = event_kinds("a: {? key: value}\n");
        assert!(kinds.contains(&"map-start"));
        assert!(kinds.contains(&"scalar"));

        let kinds = event_kinds("a: {? : value}\n");
        assert!(kinds.contains(&"map-start"));
        assert!(kinds.contains(&"scalar"));
    }

    #[test]
    fn flow_explicit_key_with_complex_key() {
        let kinds = event_kinds("a: {? [x, y]: value}\n");
        assert!(kinds.contains(&"seq-start"));
        assert!(kinds.contains(&"map-start"));
    }

    #[test]
    fn multiline_plain_scalars_fold() {
        let first_events = events("key: first\n  second\nother: x\n");
        assert!(first_events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, style: crate::event::ScalarStyle::Plain, .. }
                if value == "first second"
        )));
        assert!(first_events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, .. } if value == "x"
        )));

        let second_events = events("key:\n  first\n  second\n");
        assert!(second_events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, .. } if value == "first second"
        )));
    }

    #[test]
    fn plain_scalar_folding_stops_at_a_mapping_key() {
        let input = "a: first\n  second\n  nested: value\n";
        let error = parse_events(input, &Limits::default()).expect_err("invalid nested mapping");
        assert_eq!(error.kind(), ErrorKind::Parser);
    }

    #[test]
    fn block_scalar_does_not_swallow_the_next_line() {
        let events = events("literal: |\n  some\n  text\nfolded: >\n  some\n  text\n");
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, style: crate::event::ScalarStyle::Literal, .. } if value == "some\ntext\n"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, .. } if value == "folded"
        )));
    }

    #[test]
    fn indentless_sequences_are_mapping_values() {
        let events = events("key:\n- a\n- b\nother: c\n");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::SequenceStart { .. }))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, .. } if value == "b"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, .. } if value == "other"
        )));
    }

    #[test]
    fn standalone_properties_attach_to_the_next_node() {
        let tagged = events("--- !!map\n? a\n: b\n");
        assert!(tagged.iter().any(|event| matches!(
            event,
            Event::MappingStart { tag: Some(tag), .. } if tag.to_string() == "!!map"
        )));

        let anchored = events("&a\n- x\n");
        assert!(anchored.iter().any(|event| matches!(
            event,
            Event::SequenceStart { anchor: Some(anchor), .. } if anchor.as_str() == "a"
        )));
    }

    #[test]
    fn property_only_mapping_values_become_null_or_nested_nodes() {
        let null_anchored = events("a: &x\nb: *x\n");
        assert!(null_anchored.iter().any(|event| matches!(
            event,
            Event::Scalar { value, anchor: Some(anchor), .. }
                if value.is_empty() && anchor.as_str() == "x"
        )));
        assert!(null_anchored.iter().any(|event| matches!(
            event,
            Event::Alias { anchor, .. } if anchor.as_str() == "x"
        )));

        let nested = events("a: &x\n  b: c\n");
        assert!(nested.iter().any(|event| matches!(
            event,
            Event::MappingStart { anchor: Some(anchor), .. } if anchor.as_str() == "x"
        )));
    }

    #[test]
    fn flow_collections_span_lines_and_allow_omitted_values() {
        let sequence = events("[\n  a,\n  b\n]\n");
        assert!(sequence.iter().any(|event| matches!(
            event,
            Event::Scalar { value, .. } if value == "b"
        )));
        let mapping = events("{a, b: c}\n");
        assert!(mapping.iter().any(|event| matches!(
            event,
            Event::Scalar { value, .. } if value.is_empty()
        )));
    }

    #[test]
    fn implicit_documents_may_follow_explicit_end_markers() {
        let limits = Limits {
            max_documents: 2,
            ..Limits::default()
        };
        let events = parse_events("--- a\n...\nb: c\n", &limits).expect("parse");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::DocumentStart { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn block_scalar_header_at_eof_is_an_empty_scalar() {
        let events = events("--- |1-");
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, style: crate::event::ScalarStyle::Literal, .. }
                if value.is_empty()
        )));
    }

    #[test]
    fn standalone_properties_do_not_cross_a_dedent() {
        let events = events("key:\n  &a\nother: x\n");
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, anchor: Some(anchor), .. }
                if value.is_empty() && anchor.as_str() == "a"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, .. } if value == "other"
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            Event::MappingStart {
                anchor: Some(_),
                ..
            }
        )));
    }

    #[test]
    fn tag_directives_expand_custom_handles() {
        let events = events("%TAG !e! tag:example.com,2020:\n--- !e!foo bar\n");
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { tag: Some(tag), .. }
                if tag.handle() == "tag:example.com,2020:" && tag.suffix() == "foo"
        )));
    }

    #[test]
    fn undefined_tag_handles_are_errors() {
        let error = parse_events("--- !missing!foo bar\n", &Limits::default())
            .expect_err("undefined handle");
        assert_eq!(error.kind(), ErrorKind::Scanner);
    }

    #[test]
    fn block_mapping_key_properties_do_not_anchor_the_mapping() {
        let events = events("&a a: &b b\n*b : *a\n");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::MappingStart { anchor: None, .. }))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, anchor: Some(anchor), .. }
                if value == "a" && anchor.as_str() == "a"
        )));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Alias { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn sequence_item_mapping_key_properties_belong_to_the_key() {
        let events = events("- &a key: value\n");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::MappingStart { anchor: None, .. }))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, anchor: Some(anchor), .. }
                if value == "key" && anchor.as_str() == "a"
        )));
    }

    #[test]
    fn block_scalars_allow_tabs_in_content_and_root_indentation() {
        let tabbed = events("foo: |-\n \tbar\n");
        assert!(tabbed.iter().any(|event| matches!(
            event,
            Event::Scalar { value, style: crate::event::ScalarStyle::Literal, .. }
                if value == "\tbar"
        )));
        let root = events("--- >\nline1\nline2\n");
        assert!(root.iter().any(|event| matches!(
            event,
            Event::Scalar { value, style: crate::event::ScalarStyle::Folded, .. }
                if value == "line1 line2\n"
        )));
    }

    #[test]
    fn flow_sequences_accept_implicit_mappings_and_adjacent_colons() {
        let events = events("[foo: bar, \"key\":value]\n");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::MappingStart { .. }))
                .count(),
            2
        );
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, .. } if value == "value"
        )));
    }

    #[test]
    fn explicit_keys_support_null_and_block_sequence_values() {
        let null_keys = events("? a\n? b\nc:\n");
        assert!(
            null_keys
                .iter()
                .filter(|event| matches!(event, Event::Scalar { value, .. } if value.is_empty()))
                .count()
                >= 2
        );

        let sequence_key = events("? - a\n  - b\n: c\n");
        assert!(
            sequence_key
                .iter()
                .any(|event| matches!(event, Event::SequenceStart { .. }))
        );
        assert!(sequence_key.iter().any(|event| matches!(
            event,
            Event::Scalar { value, .. } if value == "c"
        )));
    }

    #[test]
    fn property_only_mapping_keys_are_null_scalars() {
        let events = events("- !!str : !!null\n");
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, tag: Some(tag), .. }
                if value.is_empty() && tag.to_string() == "!!str"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, tag: Some(tag), .. }
                if value.is_empty() && tag.to_string() == "!!null"
        )));
    }

    #[test]
    fn multiline_plain_sequence_items_may_start_with_a_dash() {
        let events = events("- single multiline\n - sequence entry\n");
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, .. } if value == "single multiline - sequence entry"
        )));
    }

    #[test]
    fn invalid_document_and_directive_forms_are_rejected() {
        for input in [
            "... invalid\n",
            "%YAML 1.2\n",
            "%YAML 1.2 foo\n---\n",
            "%YAML 1.2\n%YAML 1.2\n---\n",
        ] {
            assert!(
                parse_events(input, &Limits::default()).is_err(),
                "accepted invalid input {input:?}"
            );
        }
    }

    #[test]
    fn implicit_keys_must_stay_on_one_line() {
        let error = parse_events("\"a\n b\": 1\n", &Limits::default()).expect_err("multiline key");
        assert_eq!(error.kind(), ErrorKind::Parser);
    }

    #[test]
    fn aliases_cannot_carry_properties() {
        let error = parse_events("a: &x *y\n", &Limits::default()).expect_err("tagged alias");
        assert_eq!(error.kind(), ErrorKind::Parser);
    }

    #[test]
    fn empty_flow_entries_are_rejected() {
        assert!(parse_events("[ , a ]\n", &Limits::default()).is_err());
        assert!(parse_events("{ , a: b }\n", &Limits::default()).is_err());
    }

    #[test]
    fn compact_block_sequences_are_only_valid_after_explicit_value_lines() {
        let error = parse_events("key: - a\n", &Limits::default()).expect_err("inline sequence");
        assert_eq!(error.kind(), ErrorKind::Parser);

        let events = events("? - a\n: - b\n");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::SequenceStart { .. }))
        );
    }

    #[test]
    fn compact_mapping_explicit_key_and_value_are_supported() {
        let events = events("- ? earth: blue\n  : moon: white\n");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::MappingStart { .. }))
                .count(),
            3
        );
    }

    #[test]
    fn block_scalar_header_comments_require_whitespace() {
        let error =
            parse_events("key: ># comment\n  x\n", &Limits::default()).expect_err("header comment");
        assert_eq!(error.kind(), ErrorKind::Scanner);
    }

    #[test]
    fn quoted_continuations_require_parent_indentation() {
        assert!(parse_events("key: \"a\nb\"\n", &Limits::default()).is_err());
        let events = events("--- \"a\nb\"\n");
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, .. } if value == "a b"
        )));
    }

    #[test]
    fn tags_and_flow_markers_require_separation() {
        assert!(parse_events("!invalid{}tag scalar\n", &Limits::default()).is_err());
        assert!(parse_events("[a,\n--- ,\n]\n", &Limits::default()).is_err());
        assert!(parse_events("[a,#comment\n]\n", &Limits::default()).is_err());
    }

    #[test]
    fn property_only_flow_tags_before_delimiters_are_valid() {
        let events = events("{ !!str, foo: bar }\n");
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, tag: Some(tag), .. }
                if value.is_empty() && tag.to_string() == "!!str"
        )));
    }

    #[test]
    fn properties_cannot_precede_compact_block_sequences() {
        assert!(parse_events("&a - entry\n", &Limits::default()).is_err());
        assert!(parse_events("- &a - nested\n", &Limits::default()).is_err());
    }

    #[test]
    fn tabs_cannot_separate_block_indicators() {
        assert!(parse_events("-\t- a\n", &Limits::default()).is_err());
        assert!(parse_events("key:\n\tb: 1\n", &Limits::default()).is_err());
    }

    #[test]
    fn leading_block_scalar_lines_may_not_be_over_indented() {
        let error = parse_events("key: >\n \n  \n invalid\n", &Limits::default())
            .expect_err("over-indented leading line");
        assert_eq!(error.kind(), ErrorKind::Scanner);
    }

    #[test]
    fn tag_directives_are_scoped_to_one_document() {
        let limits = Limits {
            max_documents: 2,
            ..Limits::default()
        };
        let error = parse_events(
            "%TAG !e! tag:example.com,2020:\n--- !e!first x\n--- !e!second y\n",
            &limits,
        )
        .expect_err("undefined tag handle in second document");
        assert_eq!(error.kind(), ErrorKind::Scanner);
    }

    #[test]
    fn duplicate_tag_or_anchor_properties_are_rejected() {
        let error = parse_events("a: &x\n  &y value\n", &Limits::default())
            .expect_err("duplicate anchor properties");
        assert_eq!(error.kind(), ErrorKind::Parser);
    }

    #[test]
    fn implicit_flow_mapping_colons_must_share_the_key_line() {
        assert!(parse_events("[ key\n : value ]\n", &Limits::default()).is_err());
        let events = events("[? key\n value : item]\n");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::MappingStart { .. }))
        );
    }

    #[test]
    fn long_single_line_flow_sequences_do_not_scan_the_prefix_per_item() {
        let mut input = String::from("items: [");
        for index in 0..20_000 {
            if index > 0 {
                input.push(',');
            }
            input.push('x');
        }
        input.push_str("]\n");
        let events = events(&input);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::SequenceStart { .. }))
        );
    }

    #[test]
    fn flow_sequence_items_in_mapping_values_must_be_indented() {
        assert!(parse_events("key: [a,\nb]\n", &Limits::default()).is_err());
        let events = events("[\na,\nb\n]\n");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::SequenceStart { .. }))
        );
    }

    #[test]
    fn bom_at_a_line_start_is_ignored() {
        let events = events("\n\u{feff}");
        assert!(!events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, .. } if value == "\u{feff}"
        )));
    }

    #[test]
    fn block_scalar_without_final_line_break_has_no_trailing_newline() {
        let events = events("|\n  : ");
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, style: crate::event::ScalarStyle::Literal, .. }
                if value == ": "
        )));
    }

    #[test]
    fn trailing_blank_lines_do_not_extend_a_plain_scalar() {
        let events = events("x\n\n");
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Scalar { value, .. } if value == "x"
        )));
    }

    #[test]
    fn empty_flow_mapping_value_is_null() {
        let events = events("a: {b: }\n");
        assert!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Scalar { value, .. } if value.is_empty()))
                .count()
                >= 1
        );
    }
}
