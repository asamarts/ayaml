//! Event parser.
//!
//! The parser consumes scanner tokens, groups them into logical lines, and
//! emits spanned events for streams, documents, block and flow collections,
//! scalars, aliases, tags, and anchors. Composing values and resolving
//! scalars happen downstream. The grammar runs on an explicit stack (see
//! `Parser::run`), so nesting depth costs heap, not native stack.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};

use crate::composer::StreamingComposer;
use crate::error::{Code, Error, ErrorKind};
use crate::event::{Anchor, CollectionStyle, Event, ScalarStyle};
use crate::limits::Limits;
use crate::position::LineIndex;
use crate::scanner::{Scanner, Token, TokenKind};
use crate::span::{Position, Span};
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
    fn new(
        tokens: &[Token<'_>],
        start: usize,
        end: usize,
        input: &str,
        positions: &LineIndex,
    ) -> Self {
        let indent = tokens.get(start).map_or(0, |token| {
            let offset = token.span.start();
            let column = positions
                .marker(input, offset)
                .map_or(0, |position| position.column().saturating_sub(1));
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
/// A first chunk for input dense in short lines, where more tokens share each allocation.
const TOKEN_ARENA_WIDE_CHUNK: usize = 16 * 1024;
const TOKEN_ARENA_SMALL_CHUNK: usize = 64;
const TOKEN_ARENA_MID_CHUNK: usize = 256;

fn push_token<'a>(tokens: &mut Vec<Token<'a>>, token: Token<'a>, chunk: usize, most: usize) {
    if tokens.len() == tokens.capacity() {
        // The scanner refuses the token after `most`, so no arena holds more than that.
        let room = most.saturating_sub(tokens.len()).max(1);
        tokens.reserve_exact(arena_growth(tokens.len(), chunk).min(room));
    }
    tokens.push(token);
}

/// How many tokens a full arena of `len` grows by before any estimate: its first chunk, and
/// then as many tokens as it holds. Only an input of at most 32 KiB gets past its first chunk
/// this way; a larger one has an estimate by then (see [`read_lines`]).
const fn arena_growth(len: usize, chunk: usize) -> usize {
    if len <= chunk { chunk } else { len }
}

/// Logical lines read from the scanner, plus the stream boundary spans.
#[allow(
    clippy::struct_field_names,
    reason = "the field name matches the domain term for this line collection"
)]
struct Lines<'a> {
    lines: Vec<Line>,
    tokens: Vec<Token<'a>>,
    stream_start: Option<Span>,
    stream_end: Option<Span>,
    positions: LineIndex,
    /// How many times the token arena was reallocated, which the tests bound.
    #[cfg(test)]
    growths: usize,
}

/// The token arena capacity to reserve before scanning a mid-sized document, or `None` to grow
/// as tokens arrive. Ordinary lines are dense enough that one estimate from the input length
/// avoids several arena copies; long-line inputs skip it, so a large scalar does not reserve an
/// arena it will not use.
fn mid_input_token_capacity(input_len: usize, line_count: usize, most: usize) -> Option<usize> {
    if (4097..=32768).contains(&input_len) && line_count > 0 && input_len / line_count <= 80 {
        Some((input_len / 10).min(most))
    } else {
        None
    }
}

/// How far one reservation may run ahead of the tokens already scanned. An estimate comes from
/// the input read so far, and the rest of the input may hold far fewer tokens, so a reservation
/// stays within this multiple of the tokens that exist. A document that keeps its density takes
/// a few reservations, each from a later and better estimate.
const TOKEN_ARENA_AHEAD: usize = 8;

/// How many tokens a full arena of `len` grows by once estimates have begun: up to `estimate`,
/// the tokens the whole input is expected to hold, and by a sixteenth of `len` or a chunk at
/// least. An estimate that expects few more tokens can be wrong, and growing by less would copy
/// the arena once for every few tokens. The arena never passes `most`, the tokens the limits
/// let a reader keep; a full arena that holds as many grows by one, for a token the scanner
/// has already counted.
const fn estimated_growth(len: usize, estimate: usize, chunk: usize, most: usize) -> usize {
    let sixteenth = len / 16;
    let least = if sixteenth > chunk { sixteenth } else { chunk };
    let wanted = estimate.saturating_sub(len);
    let growth = if wanted > least { wanted } else { least };
    let room = most.saturating_sub(len);
    let room = if room == 0 { 1 } else { room };
    if growth < room { growth } else { room }
}

/// Tokens to hold after scanning a prefix, or `None` before enough tokens show the density.
/// The estimate assumes the rest of the input has the prefix's token density. It is capped at
/// the input's length, since a token takes a byte at least, at `most`, the tokens the limits let
/// a reader keep, and at [`TOKEN_ARENA_AHEAD`] times the tokens already scanned. That last cap
/// is what keeps a comment-only rest, or one large scalar, from reserving an arena it will not
/// use.
fn prefix_token_capacity(
    tokens: usize,
    consumed: usize,
    input_len: usize,
    most: usize,
) -> Option<usize> {
    const THRESHOLD: usize = 1_024;
    if tokens < THRESHOLD || consumed == 0 {
        return None;
    }
    // In 128 bits: on a 32-bit target the product passes `usize` for an input of a few hundred
    // KiB. A sixty-fourth is added, because an estimate that falls a few tokens short costs
    // another growth step, which is larger than that margin.
    let estimate = tokens as u128 * input_len as u128 / consumed as u128;
    let estimate = usize::try_from(estimate + estimate / 64).unwrap_or(usize::MAX);
    let cap = input_len
        .min(most)
        .min(tokens.saturating_mul(TOKEN_ARENA_AHEAD));
    Some(estimate.min(cap).max(tokens))
}

/// When [`read_lines`] sizes the token arena from the density of the input scanned so far.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Estimating {
    /// The arena was reserved from the input length, and doubles if that fills.
    Never,
    /// No estimate yet: the first comes once enough tokens show the density.
    AtThreshold,
    /// An estimate was made, and the next comes when its reservation is full.
    WhenFull,
}

/// Group scanner tokens into logical lines as they arrive.
///
/// This avoids first collecting every token in one vector and then copying
/// them into per-line vectors. A block scalar consumes the rest of its line,
/// so it flushes the logical line immediately.
fn read_lines<'a>(scanner: &mut Scanner<'a>) -> Result<Lines<'a>, Error> {
    let input = scanner.input();
    let input_len = scanner.input_len();
    let chunk = match input_len {
        0..=4096 => TOKEN_ARENA_SMALL_CHUNK,
        4097..=32768 => TOKEN_ARENA_MID_CHUNK,
        _ => {
            // Short lines carry many tokens for their bytes, so a wider first chunk saves
            // reallocations. A few long lines, such as one large scalar, keep the small chunk so
            // an empty arena does not reserve a wide one.
            let lines = scanner.line_index().line_count();
            if lines > 0 && input_len / lines <= 10 {
                TOKEN_ARENA_WIDE_CHUNK
            } else {
                TOKEN_ARENA_CHUNK
            }
        }
    };
    let mut lines = Vec::new();
    let mut tokens = Vec::new();
    let line_count = scanner.line_index().line_count();
    let most = scanner.max_tokens();
    // A mid-sized input of short lines reserves once from its length. Any other estimates from
    // the tokens scanned so far: first at the threshold, then again each time the arena fills.
    let mut estimating = match mid_input_token_capacity(input_len, line_count, most) {
        Some(capacity) => {
            tokens.reserve_exact(capacity);
            Estimating::Never
        }
        None => Estimating::AtThreshold,
    };
    let mut line_start = 0;
    let mut stream_start = None;
    let mut stream_end = None;
    #[cfg(test)]
    let mut growths = 0;
    while let Some(token) = scanner.next_token()? {
        match token.kind {
            TokenKind::StreamStart => stream_start = Some(token.span),
            TokenKind::StreamEnd => {
                stream_end = Some(token.span);
                break;
            }
            TokenKind::LineBreak => {
                if tokens.len() > line_start {
                    lines.push(Line::new(
                        &tokens,
                        line_start,
                        tokens.len(),
                        input,
                        scanner.line_index(),
                    ));
                    line_start = tokens.len();
                }
            }
            _ => {
                let block_scalar = token.kind.scalar_style().is_some_and(|style| {
                    matches!(style, ScalarStyle::Literal | ScalarStyle::Folded)
                });
                let consumed = token.span.end();
                #[cfg(test)]
                let capacity_before = tokens.capacity();
                // A full arena that has had an estimate grows to a new one, made from every
                // token scanned so far and this one. It grows only when a token needs the room,
                // so an estimate that was exact reserves nothing after the last token.
                if estimating == Estimating::WhenFull
                    && tokens.len() == tokens.capacity()
                    && let Some(capacity) =
                        prefix_token_capacity(tokens.len() + 1, consumed, input_len, most)
                {
                    tokens.reserve_exact(estimated_growth(tokens.len(), capacity, chunk, most));
                }
                push_token(&mut tokens, token, chunk, most);
                if estimating == Estimating::AtThreshold
                    && let Some(capacity) =
                        prefix_token_capacity(tokens.len(), consumed, input_len, most)
                {
                    estimating = Estimating::WhenFull;
                    if capacity > tokens.capacity() {
                        tokens.reserve_exact(capacity - tokens.len());
                    }
                }
                #[cfg(test)]
                {
                    growths += usize::from(tokens.capacity() != capacity_before);
                }
                if block_scalar {
                    lines.push(Line::new(
                        &tokens,
                        line_start,
                        tokens.len(),
                        input,
                        scanner.line_index(),
                    ));
                    line_start = tokens.len();
                }
            }
        }
    }
    if tokens.len() > line_start {
        lines.push(Line::new(
            &tokens,
            line_start,
            tokens.len(),
            input,
            scanner.line_index(),
        ));
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
        positions: scanner.line_index().clone(),
        #[cfg(test)]
        growths,
    })
}

/// Events in fixed-size chunks, so a typed read does not retain a vector's spare capacity.
#[derive(Debug)]
pub(crate) struct EventChunks<'a> {
    chunks: Vec<Vec<Event<'a>>>,
    current: Vec<Event<'a>>,
    chunk_size: usize,
}

impl<'a> EventChunks<'a> {
    const MAX_CHUNK: usize = 8_192;

    fn with_chunk_size(chunk_size: usize) -> Self {
        let chunk_size = chunk_size.clamp(16, Self::MAX_CHUNK);
        Self {
            chunks: Vec::new(),
            current: Vec::with_capacity(chunk_size),
            chunk_size,
        }
    }

    fn push(&mut self, event: Event<'a>) {
        if self.current.len() == self.chunk_size {
            self.chunks.push(std::mem::replace(
                &mut self.current,
                Vec::with_capacity(self.chunk_size),
            ));
        }
        self.current.push(event);
    }

    pub(crate) fn into_chunks(mut self) -> Vec<Vec<Event<'a>>> {
        if !self.current.is_empty() {
            self.chunks.push(self.current);
        }
        self.chunks
    }
}

/// A consumer that receives events as the parser produces them.
pub(crate) trait EventConsumer<'a> {
    fn event(&mut self, event: Event<'a>) -> Result<(), Error>;

    /// Reserve room for a collection's entries before its start event. Sinks that don't build
    /// collections ignore this.
    fn reserve_collection(&mut self, _entries: usize) {}

    fn finish(&mut self) -> Result<(), Error>;
}

enum EventSink<'a> {
    Events(Vec<Event<'a>>),
    Chunks(EventChunks<'a>),
    Streaming(Box<StreamingComposer<'a>>),
    Consumer(Box<dyn EventConsumer<'a> + 'a>),
}

impl<'a> EventSink<'a> {
    fn reserve_collection(&mut self, entries: usize) {
        if let Self::Consumer(consumer) = self {
            consumer.reserve_collection(entries);
        }
    }

    fn event(&mut self, event: Event<'a>) -> Result<(), Error> {
        match self {
            Self::Events(events) => {
                // Grow by half instead of doubling. The capacity estimate is close but can be
                // short on nested documents; this avoids reserving twice the events that fit.
                if events.len() == events.capacity() {
                    events.reserve_exact((events.capacity() / 2).max(16));
                }
                events.push(event);
                Ok(())
            }
            Self::Chunks(events) => {
                events.push(event);
                Ok(())
            }
            Self::Streaming(composer) => composer.event(event),
            Self::Consumer(consumer) => consumer.event(event),
        }
    }
}

struct Parser<'a> {
    lines: Vec<Line>,
    token_arena: Vec<Token<'a>>,
    /// Matching closing token positions for every flow collection, by absolute token index.
    flow_ends: HashMap<usize, usize>,
    sink: EventSink<'a>,
    limits: Limits,
    input: &'a str,
    positions: LineIndex,
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
    end: usize,
}

/// A document-bounded pull cursor used by the public event traversal.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the flags describe independent scanner and stream states, not an enum"
)]
pub(crate) struct EventCursor<'a> {
    scanner: Scanner<'a>,
    limits: Limits,
    input: &'a str,
    positions: LineIndex,
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
    /// Use the compatibility scanner and parser rules.
    compat: bool,
}

impl<'a> EventCursor<'a> {
    #[allow(
        clippy::unused_self,
        reason = "the method is part of a uniform grammar or diagnostic surface"
    )]
    fn limit_at(&self, span: Span, limit: crate::limits::LimitExceeded) -> Error {
        Error::from(limit)
            .with_span(span)
            .with_location_from(&self.positions, self.input)
    }

    pub(crate) fn new(input: &'a str, limits: &Limits) -> Result<Self, Error> {
        Self::with_compat(input, limits, false)
    }

    pub(crate) fn with_compat(
        input: &'a str,
        limits: &Limits,
        compat: bool,
    ) -> Result<Self, Error> {
        let scanner = Scanner::with_compat(input, limits.clone(), compat)?;
        let positions = scanner.line_index().clone();
        Ok(Self {
            scanner,
            limits: limits.clone(),
            input,
            positions,
            pending_line: None,
            queue: VecDeque::new(),
            stream_started: false,
            stream_ended: false,
            eof: false,
            stream_end: None,
            documents: 0,
            events: 0,
            compat,
        })
    }

    pub(crate) fn next_event(&mut self) -> Result<Option<Event<'a>>, Error> {
        let result = self
            .next_event_inner()
            .map_err(|error| error.with_location_from(&self.positions, self.input));
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
                _ => Span::empty(0),
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
            if let Some(events) = self.load_chunk()? {
                self.queue.extend(events);
            } else {
                self.stream_ended = true;
                let span = self
                    .stream_end
                    .unwrap_or_else(|| Span::empty(self.input.len()));
                self.events += 1;
                self.limits
                    .check_events(self.events)
                    .map_err(|error| self.limit_at(span, error))?;
                return Ok(Some(Event::StreamEnd { span }));
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
                    Some(TokenKind::Directive(_))
                );
            let start = tokens.len();
            tokens.extend(line_tokens);
            lines.push(Line::new(
                &tokens,
                start,
                tokens.len(),
                self.input,
                &self.positions,
            ));
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
            .unwrap_or(self.input.len());
        let (events, counted) = parse_lines(
            Lines {
                lines,
                tokens,
                stream_start: None,
                stream_end: None,
                positions: self.positions.clone(),
                #[cfg(test)]
                growths: 0,
            },
            &self.limits,
            self.input,
            self.events,
            end,
            self.compat,
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
            .unwrap_or_else(|| Span::empty(self.input.len()));
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
                    let block_scalar = token.kind.scalar_style().is_some_and(|style| {
                        matches!(style, ScalarStyle::Literal | ScalarStyle::Folded)
                    });
                    current.push(token);
                    if block_scalar {
                        return Ok(Some(current));
                    }
                }
            }
        }
    }
}

/// Pair every flow collection's opening token with its closing token.
fn flow_ends(tokens: &[Token<'_>]) -> HashMap<usize, usize> {
    let mut ends = HashMap::new();
    let mut stack = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        match token.kind {
            TokenKind::FlowSequenceStart | TokenKind::FlowMappingStart => stack.push(index),
            TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd => {
                if let Some(open) = stack.pop() {
                    ends.insert(open, index);
                }
            }
            _ => {}
        }
    }
    ends
}

/// The number of top-level entries between a flow collection's opening and closing tokens. An
/// entry counts once it has content, so separators alone reserve nothing.
fn flow_entry_count(tokens: &[Token<'_>], open: usize, close: usize) -> usize {
    let mut depth = 0_usize;
    let mut entries = 0_usize;
    let mut content = false;
    for token in &tokens[open + 1..close] {
        match token.kind {
            TokenKind::FlowSequenceStart | TokenKind::FlowMappingStart => {
                content |= depth == 0;
                depth += 1;
            }
            TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd => {
                depth = depth.saturating_sub(1);
            }
            TokenKind::FlowEntry if depth == 0 => {
                entries += usize::from(content);
                content = false;
            }
            _ if depth == 0 => content = true,
            _ => {}
        }
    }
    entries + usize::from(content)
}

/// Parse one document's lines for the pull cursor. Its events count on from the `counted` events
/// before them, and the count after them comes back with them. `end` is where the lines stop.
fn parse_lines<'a>(
    lines: Lines<'a>,
    limits: &Limits,
    input: &'a str,
    counted: usize,
    end: usize,
    compat: bool,
) -> Result<(Vec<Event<'a>>, usize), Error> {
    let event_capacity = event_capacity(&lines.lines, &lines.tokens);
    let flow_ends = flow_ends(&lines.tokens);
    let mut parser = Parser {
        lines: lines.lines,
        token_arena: lines.tokens,
        flow_ends,
        sink: EventSink::Events(Vec::with_capacity(event_capacity)),
        limits: limits.clone(),
        input,
        positions: lines.positions,
        compat,
        documents: 0,
        emitted: counted,
        single_document: false,
        stop_at_second: false,
        end,
    };
    parser.parse_stream()?;
    match parser.sink {
        EventSink::Events(events) => Ok((events, parser.emitted)),
        EventSink::Chunks(_) | EventSink::Streaming(_) | EventSink::Consumer(_) => {
            unreachable!("event sink kind changed during parsing")
        }
    }
}

fn event_capacity(lines: &[Line], tokens: &[Token<'_>]) -> usize {
    let headroom = if lines.iter().any(|line| line.indent > 0) {
        256
    } else {
        200
    };
    // An event can represent an implicit collection that has no token of its own. Four fifths
    // of the token count plus a small headroom reserves close to the actual event count on
    // nested documents without doubling a short document's reservation; if it is still short,
    // EventSink grows by half rather than doubling.
    tokens
        .len()
        .saturating_mul(4)
        .saturating_div(5)
        .saturating_add(headroom)
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

pub(crate) fn parse_document_event_chunks<'a>(
    input: &'a str,
    limits: &Limits,
    compat_flow: bool,
) -> Result<EventChunks<'a>, Error> {
    let mut scanner = Scanner::with_compat(input, limits.clone(), compat_flow)?;
    let Lines {
        lines,
        tokens,
        stream_start,
        stream_end,
        positions,
        ..
    } = read_lines(&mut scanner)?;
    let chunk_size = event_capacity(&lines, &tokens).saturating_add(2);
    let flow_ends = flow_ends(&tokens);
    let end = stream_end.map_or(input.len(), Span::start);
    let mut parser = Parser {
        lines,
        token_arena: tokens,
        flow_ends,
        sink: EventSink::Chunks(EventChunks::with_chunk_size(chunk_size)),
        limits: limits.clone(),
        input,
        positions,
        compat: compat_flow,
        documents: 0,
        emitted: 0,
        single_document: true,
        stop_at_second: true,
        end,
    };
    parser.emit(Event::StreamStart {
        span: stream_start.unwrap_or_else(|| Span::empty(0)),
    })?;
    parser.parse_stream()?;
    parser.emit(Event::StreamEnd {
        span: stream_end.unwrap_or_else(|| Span::empty(input.len())),
    })?;
    match parser.sink {
        EventSink::Chunks(events) => Ok(events),
        EventSink::Events(_) | EventSink::Streaming(_) | EventSink::Consumer(_) => {
            unreachable!("event sink kind changed during parsing")
        }
    }
}

pub(crate) fn parse_document_with_consumer<'a>(
    input: &'a str,
    limits: &Limits,
    compat_flow: bool,
    consumer: Box<dyn EventConsumer<'a> + 'a>,
) -> Result<(), Error> {
    let mut scanner = Scanner::with_compat(input, limits.clone(), compat_flow)?;
    let Lines {
        lines,
        tokens,
        stream_start,
        stream_end,
        positions,
        ..
    } = read_lines(&mut scanner)?;
    let flow_ends = flow_ends(&tokens);
    let end = stream_end.map_or(input.len(), Span::start);
    let mut parser = Parser {
        lines,
        token_arena: tokens,
        flow_ends,
        sink: EventSink::Consumer(consumer),
        limits: limits.clone(),
        input,
        positions,
        compat: compat_flow,
        documents: 0,
        emitted: 0,
        single_document: true,
        stop_at_second: true,
        end,
    };
    parser.emit(Event::StreamStart {
        span: stream_start.unwrap_or_else(|| Span::empty(0)),
    })?;
    parser.parse_stream()?;
    parser.emit(Event::StreamEnd {
        span: stream_end.unwrap_or_else(|| Span::empty(input.len())),
    })?;
    match parser.sink {
        EventSink::Consumer(mut consumer) => consumer.finish(),
        EventSink::Events(_) | EventSink::Chunks(_) | EventSink::Streaming(_) => {
            unreachable!("event sink kind changed during parsing")
        }
    }
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
        positions,
        ..
    } = read_lines(&mut scanner)?;

    let event_capacity = event_capacity(&lines, &tokens).saturating_add(2);
    let flow_ends = flow_ends(&tokens);
    let end = stream_end.map_or(input.len(), Span::start);
    let mut parser = Parser {
        lines,
        token_arena: tokens,
        flow_ends,
        sink: EventSink::Events(Vec::with_capacity(event_capacity)),
        limits: limits.clone(),
        input,
        positions,
        compat: compat_flow,
        documents: 0,
        emitted: 0,
        single_document,
        stop_at_second,
        end,
    };
    parser.emit(Event::StreamStart {
        span: stream_start.unwrap_or_else(|| Span::empty(0)),
    })?;
    parser.parse_stream()?;
    parser.emit(Event::StreamEnd {
        span: stream_end.unwrap_or_else(|| Span::empty(input.len())),
    })?;
    match parser.sink {
        EventSink::Events(events) => Ok(events),
        EventSink::Chunks(_) | EventSink::Streaming(_) | EventSink::Consumer(_) => {
            unreachable!("event sink kind changed during parsing")
        }
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
    let flow_ends = flow_ends(&lines.tokens);
    let mut parser = Parser {
        lines: lines.lines,
        token_arena: lines.tokens,
        flow_ends,
        sink: EventSink::Streaming(Box::new(StreamingComposer::new(
            limits.clone(),
            input.len(),
        ))),
        limits: limits.clone(),
        input,
        positions: lines.positions,
        compat: false,
        documents: 0,
        emitted: 0,
        single_document,
        stop_at_second: false,
        end: lines.stream_end.map_or(input.len(), Span::start),
    };
    // The composer ignores the stream events, but they count as an event parse counts them.
    parser.emit(Event::StreamStart {
        span: lines.stream_start.unwrap_or_else(|| Span::empty(0)),
    })?;
    parser
        .parse_stream()
        .map_err(|error| error.with_location_from(&parser.positions, input))?;
    parser.emit(Event::StreamEnd {
        span: lines.stream_end.unwrap_or_else(|| Span::empty(input.len())),
    })?;
    let positions = parser.positions;
    match parser.sink {
        EventSink::Streaming(composer) => composer
            .finish()
            .map_err(|error| error.with_location_from(&positions, input)),
        EventSink::Events(_) | EventSink::Chunks(_) | EventSink::Consumer(_) => {
            unreachable!("event sink kind changed during parsing")
        }
    }
}

impl<'a> Parser<'a> {
    /// A compatibility-mode explanation for a line that does not fit the indentation of the
    /// collection before it. Real trailing content returns `None`.
    fn compat_indentation_message(&self, index: usize) -> Option<String> {
        let tokens = self.line_tokens(index);
        let structural = has_value_indicator(tokens)
            || tokens.first().is_some_and(|token| {
                matches!(
                    token.kind,
                    TokenKind::BlockEntry | TokenKind::Key | TokenKind::Value
                )
            });
        if !structural {
            return None;
        }
        let indent = self.lines.get(index)?.indent;
        if index == 0 && indent > 0 {
            return Some("a document's first node cannot be indented".to_owned());
        }
        let parent = (0..index)
            .rev()
            .find(|&line| self.lines[line].indent < indent)?;
        if indent != self.lines[parent].indent + 1 {
            return None;
        }
        let token = self.line_first(parent)?;
        let position = self.at(token.span.start());
        let kind = if matches!(token.kind, TokenKind::BlockEntry) {
            "sequence"
        } else {
            "mapping"
        };
        Some(format!(
            "this line is indented differently from the {kind} at line {} column {}",
            position.line(),
            position.column()
        ))
    }

    fn at(&self, offset: usize) -> Position {
        self.positions
            .marker(self.input, offset)
            .unwrap_or_default()
    }

    /// The number of top-level entries in the flow collection that opens at `tokens.start + start`,
    /// or `None` when the collection is malformed or not flow.
    fn flow_entries(&self, tokens: Run, start: usize) -> Option<usize> {
        let open = self.lines[tokens.line].start + tokens.start + start;
        let close = *self.flow_ends.get(&open)?;
        Some(flow_entry_count(&self.token_arena, open, close))
    }

    fn parse_stream(&mut self) -> Result<(), Error> {
        let mut index = 0;
        while index < self.lines.len() {
            let mut saw_directive = false;
            while let Some(TokenKind::Directive(directive)) =
                self.line_first(index).map(|token| &token.kind)
            {
                let Some(span) = self.line_first(index).map(|token| token.span) else {
                    break;
                };
                let event = Event::Directive {
                    name: directive.name.clone(),
                    value: directive.value.clone(),
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
                    self.line_first(index)
                        .map_or_else(|| self.empty_span(index), |token| token.span),
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
                let tabbed = root_block && tab_leads_line(self.input, span.start());
                let fallback = match tokens.first().map(|token| &token.kind) {
                    Some(kind)
                        if kind.scalar_style().is_some_and(|style| {
                            matches!(style, ScalarStyle::Literal | ScalarStyle::Folded)
                        }) =>
                    {
                        "a block scalar is not indented past its parent"
                    }
                    Some(TokenKind::BlockEntry) => "a block entry is indented less than its parent",
                    Some(TokenKind::PlainScalar | TokenKind::Scalar(_))
                        if root_block
                            && self.lines[index].indent > 0
                            && previous_line_is_blank(self.input, span.start()) =>
                    {
                        "a mapping line is missing a `:`"
                    }
                    _ => "unexpected content after document",
                }
                .to_owned();
                let message = if tabbed {
                    "unexpected content after document".to_owned()
                } else if self.compat {
                    self.compat_indentation_message(index).unwrap_or(fallback)
                } else {
                    fallback
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

    #[allow(
        clippy::match_same_arms,
        reason = "the arms stay separate to document each representation or state"
    )]
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
fn implicit_key_fits(key: Position, colon: Position) -> bool {
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
