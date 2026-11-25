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
use crate::error::{Error, ErrorCode, ErrorKind};
use crate::event::{Anchor, CollectionStyle, Event, ScalarStyle};
use crate::limits::Limits;
use crate::position::LineStarts;
use crate::scanner::{Scanner, Token, TokenKind};
use crate::span::{Location, Span};
use crate::tag::Tag;
use crate::value::Value;

pub(crate) use cursor::EventCursor;
pub(crate) use pull::DocumentEvents;

mod cursor;
mod flow;
mod grammar;
mod pull;
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
        positions: &LineStarts,
    ) -> Self {
        let indent = tokens.get(start).map_or(0, |token| {
            let offset = token.span.start();
            let bytes = input.as_bytes();
            let mut i = offset;
            let mut first_tab: Option<usize> = None;
            while i > 0 {
                match bytes[i - 1] {
                    b' ' => i -= 1,
                    b'\t' => {
                        i -= 1;
                        first_tab = Some(i);
                    }
                    _ => break,
                }
            }
            if i == 0 || matches!(bytes[i - 1], b'\n' | b'\r') {
                first_tab.map_or(offset - i, |tab| tab - i)
            } else {
                let column = positions
                    .marker(input, offset)
                    .map_or(0, |position| position.column().saturating_sub(1));
                match first_tab {
                    Some(tab) => column.saturating_sub(offset - tab),
                    None => column,
                }
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

fn reserve_tokens(
    tokens: &mut Vec<Token<'_>>,
    additional: usize,
    most: usize,
    span: Span,
    positions: &LineStarts,
    input: &str,
) -> Result<(), Error> {
    tokens.try_reserve_exact(additional).map_err(|_| {
        let observed = u64::try_from(tokens.len().saturating_add(additional)).unwrap_or(u64::MAX);
        let limit = u64::try_from(most).unwrap_or(u64::MAX);
        Error::from(crate::limits::LimitExceeded::new(
            crate::limits::LimitKind::Events,
            limit,
            observed,
        ))
        .with_span(span)
        .with_location_from(positions, input)
    })
}

#[cfg(test)]
fn push_token<'a>(
    tokens: &mut Vec<Token<'a>>,
    token: Token<'a>,
    chunk: usize,
    most: usize,
    positions: &LineStarts,
    input: &str,
) -> Result<(), Error> {
    if tokens.len() == tokens.capacity() {
        // The scanner refuses the token after `most`, so no arena holds more than that.
        let room = most.saturating_sub(tokens.len()).max(1);
        let additional = arena_growth(tokens.len(), chunk).min(room);
        reserve_tokens(tokens, additional, most, token.span, positions, input)?;
    }
    tokens.push(token);
    Ok(())
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
    positions: LineStarts,
    /// A non-limit scanner failure deferred until the parser checks earlier tokens, so raising
    /// `max_depth` cannot swap an earlier parser error for a later scanner error.
    scanner_error: Option<Error>,
    /// How many times the token arena was reallocated, which the tests bound.
    #[cfg(test)]
    growths: usize,
}

fn is_limit_error(error: &Error) -> bool {
    error.kind() == ErrorKind::Limit || error.limit().is_some()
}

/// Whether `tokens` contains a flow collection opener (`[` or `{`).
///
/// The scanner only stops early at `max_depth` on a flow opener (`stop_after`), so deferring a
/// later non-limit scanner error precisely when a flow opener was already scanned keeps error
/// reporting monotonic across all `max_depth` ceilings while preserving eager scanner errors when
/// no flow opener precedes them.
fn has_flow_start(tokens: &[Token<'_>]) -> bool {
    tokens.iter().any(|token| {
        matches!(
            token.kind,
            TokenKind::FlowSequenceStart | TokenKind::FlowMappingStart
        )
    })
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
/// The estimate projects the remaining bytes from the higher of the whole prefix's density and
/// the density since the previous estimate `(last_tokens, last_consumed)`, so a dense tail after
/// a sparse comment or block-scalar head converges in a few reservations. It is capped at the
/// input's length, since a token takes a byte at least, at `most`, the tokens the limits let a
/// reader keep, and at [`TOKEN_ARENA_AHEAD`] times the tokens already scanned. That last cap is
/// what keeps a comment-only rest, or one large scalar, from reserving an arena it will not use.
fn prefix_token_capacity(
    tokens: usize,
    consumed: usize,
    last_tokens: usize,
    last_consumed: usize,
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
    let overall = tokens as u128 * input_len as u128 / consumed as u128;
    let remaining = input_len.saturating_sub(consumed) as u128;
    let recent_tokens = tokens.saturating_sub(last_tokens) as u128;
    let recent_bytes = consumed.saturating_sub(last_consumed) as u128;
    let recent = if recent_tokens > 0 && recent_bytes > 0 {
        (tokens as u128).saturating_add(recent_tokens * remaining / recent_bytes)
    } else {
        0
    };
    let estimate = overall.max(recent);
    let estimate = usize::try_from(estimate.saturating_add(estimate / 64)).unwrap_or(usize::MAX);
    let cap = input_len
        .min(most)
        .min(tokens.saturating_mul(TOKEN_ARENA_AHEAD));
    Some(estimate.min(cap).max(tokens))
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
    if let Some(capacity) = mid_input_token_capacity(input_len, line_count, most) {
        reserve_tokens(
            &mut tokens,
            capacity,
            most,
            Span::empty(0),
            scanner.line_index(),
            input,
        )?;
    }
    let mut last_tokens = 0_usize;
    let mut last_consumed = 0_usize;
    let mut line_start = 0;
    let mut stream_start = None;
    let mut stream_end = None;
    let mut scanner_error = None;
    #[cfg(test)]
    let mut growths = 0;
    loop {
        let token = match scanner.next_token() {
            Ok(Some(token)) => token,
            Ok(None) => break,
            Err(error) => {
                if is_limit_error(&error) || !has_flow_start(&tokens) {
                    return Err(error);
                }
                scanner_error = Some(error);
                break;
            }
        };
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
                if tokens.is_empty() {
                    // Start the recent-density window at the first content token so leading
                    // comments or blank lines do not depress the first full-arena estimate.
                    last_consumed = token.span.start();
                }
                #[cfg(test)]
                let capacity_before = tokens.capacity();
                if tokens.len() == tokens.capacity() {
                    let room = most.saturating_sub(tokens.len()).max(1);
                    let additional = if let Some(capacity) = prefix_token_capacity(
                        tokens.len() + 1,
                        consumed,
                        last_tokens,
                        last_consumed,
                        input_len,
                        most,
                    ) {
                        last_tokens = tokens.len() + 1;
                        last_consumed = consumed;
                        estimated_growth(tokens.len(), capacity, chunk, most)
                    } else {
                        arena_growth(tokens.len(), chunk).min(room)
                    };
                    reserve_tokens(
                        &mut tokens,
                        additional,
                        most,
                        token.span,
                        scanner.line_index(),
                        input,
                    )?;
                }
                tokens.push(token);
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
        scanner_error,
        #[cfg(test)]
        growths,
    })
}

/// A consumer that receives events as the parser produces them.
#[cfg(test)]
pub(crate) trait EventConsumer<'a> {
    fn event(&mut self, event: Event<'a>) -> Result<(), Error>;

    /// Reserve room for a collection's entries before its start event. Sinks that don't build
    /// collections ignore this.
    fn reserve_collection(&mut self, _entries: usize) {}

    fn finish(&mut self) -> Result<(), Error>;
}

enum EventSink<'a> {
    Events(Vec<Event<'a>>),
    Streaming(Box<StreamingComposer<'a>>),
    Typed(Box<crate::typed::Builder<'a>>),
    #[cfg(test)]
    Consumer(Box<dyn EventConsumer<'a> + 'a>),
    /// Events that wait for a reader to take them (see [`DocumentEvents`]), and how many
    /// entries the root holds, where the grammar counts them before it reads them.
    Queue {
        events: VecDeque<Event<'a>>,
        root_entries: Option<usize>,
    },
}

impl<'a> EventSink<'a> {
    /// The events a queue holds before the grammar stops for its reader. A step of the grammar
    /// emits a few events, and a run of scalar entries or of directives stops when this many
    /// wait.
    const QUEUE_ROOM: usize = 16;

    /// Whether a reader should take the waiting events before more are emitted.
    fn is_full(&self) -> bool {
        matches!(self, Self::Queue { events, .. } if events.len() >= Self::QUEUE_ROOM)
    }

    /// The events that wait for a reader, if this sink keeps them for one.
    fn waiting(&mut self) -> Option<&mut VecDeque<Event<'a>>> {
        match self {
            Self::Queue { events, .. } => Some(events),
            _ => None,
        }
    }

    /// Say how many entries the root flow collection holds, before its start event.
    fn reserve_collection(&mut self, entries: usize) {
        match self {
            Self::Typed(builder) => builder.reserve_collection(entries),
            #[cfg(test)]
            Self::Consumer(consumer) => consumer.reserve_collection(entries),
            Self::Queue { root_entries, .. } => *root_entries = Some(entries),
            Self::Events(_) | Self::Streaming(_) => {}
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
            Self::Streaming(composer) => composer.event(event),
            Self::Typed(builder) => builder.push_event(event),
            #[cfg(test)]
            Self::Consumer(consumer) => consumer.event(event),
            Self::Queue { events, .. } => {
                events.push_back(event);
                Ok(())
            }
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
    positions: LineStarts,
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
    /// Deferred non-limit scanner failure recorded after earlier tokens were scanned.
    scanner_error: Option<Error>,
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
        scanner_error: lines.scanner_error,
    };
    parser.parse_stream()?;
    match parser.sink {
        EventSink::Events(events) => Ok((events, parser.emitted)),
        _ => unreachable!("event sink kind changed during parsing"),
    }
}

fn event_capacity(lines: &[Line], tokens: &[Token<'_>]) -> usize {
    let base_headroom = if lines.iter().any(|line| line.indent > 0) {
        256
    } else {
        200
    };
    // An event can represent an implicit collection that has no token of its own. Four fifths
    // of the token count plus headroom reserves close to the actual event count on large
    // documents, while scaling small documents proportionally to token + line count avoids
    // reserving ~35-42 KiB for tiny configs.
    let large = tokens
        .len()
        .saturating_mul(4)
        .saturating_div(5)
        .saturating_add(base_headroom);
    if tokens.len() >= 512 {
        large
    } else {
        tokens
            .len()
            .saturating_add(lines.len())
            .saturating_add(8)
            .min(large)
    }
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
#[cfg(test)]
pub(crate) fn parse_document_events<'a>(
    input: &'a str,
    limits: &Limits,
    compat_flow: bool,
) -> Result<Vec<Event<'a>>, Error> {
    // Stop at the second document's start and let the typed builder build the first one first
    // (AY-027). The second start is the marker the builder turns into a trailing-document error.
    parse_events_with_compat(input, limits, compat_flow, true, true)
}

/// Parse one document directly into a typed [`crate::typed::Builder`] without collecting an
/// intermediate event vector.
pub(crate) fn parse_document_typed<'a>(
    input: &'a str,
    limits: &Limits,
    compat_flow: bool,
    builder: crate::typed::Builder<'a>,
) -> Result<crate::typed::Node<'a>, Error> {
    let mut scanner = Scanner::with_compat(input, limits.clone(), compat_flow)?;
    let Lines {
        lines,
        tokens,
        stream_start,
        stream_end,
        positions,
        scanner_error,
        ..
    } = read_lines(&mut scanner)?;
    let flow_ends = flow_ends(&tokens);
    let end = stream_end.map_or(input.len(), Span::start);
    let mut parser = Parser {
        lines,
        token_arena: tokens,
        flow_ends,
        sink: EventSink::Typed(Box::new(builder)),
        limits: limits.clone(),
        input,
        positions,
        compat: compat_flow,
        documents: 0,
        emitted: 0,
        single_document: true,
        stop_at_second: true,
        end,
        scanner_error,
    };
    parser.emit(Event::StreamStart {
        span: stream_start.unwrap_or_else(|| Span::empty(0)),
    })?;
    parser.parse_stream()?;
    parser.emit(Event::StreamEnd {
        span: stream_end.unwrap_or_else(|| Span::empty(input.len())),
    })?;
    match parser.sink {
        EventSink::Typed(mut builder) => builder.finish_document_node(),
        _ => unreachable!("event sink kind changed during parsing"),
    }
}

#[cfg(test)]
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
        scanner_error,
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
        scanner_error,
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
        _ => unreachable!("event sink kind changed during parsing"),
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
        scanner_error,
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
        scanner_error,
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
        _ => unreachable!("event sink kind changed during parsing"),
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
        scanner_error: lines.scanner_error,
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
        _ => unreachable!("event sink kind changed during parsing"),
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

    fn at(&self, offset: usize) -> Location {
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

    /// Read every document of the stream.
    fn parse_stream(&mut self) -> Result<(), Error> {
        let mut index = 0;
        let mut directives = false;
        while index < self.lines.len() {
            let opening = self.open_document(index, directives)?;
            // Only a sink that keeps events for a reader stops among the directives.
            directives = matches!(opening, Opening::Directives(_));
            index = match opening {
                Opening::End => return Ok(()),
                Opening::NoDocument(next) | Opening::Directives(next) => next,
                Opening::Root { call, index, block } => {
                    let index = match call {
                        Some(call) => self.run(call)?,
                        None => index,
                    };
                    self.close_document(index, block)?
                }
            };
        }
        if let Some(error) = self.scanner_error.take() {
            return Err(error);
        }
        Ok(())
    }

    /// Read from line `index` to a document's root: its directives and its start. The root is
    /// the call that reads it, or none where the document is empty and its null is emitted here.
    /// `saw_directive` says that the lines before `index` were this document's directives: a
    /// reader that takes events as it needs them is handed a few directives at a time, and
    /// comes back here for the rest.
    fn open_document(
        &mut self,
        mut index: usize,
        mut saw_directive: bool,
    ) -> Result<Opening<'a>, Error> {
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
            if self.sink.is_full() {
                return Ok(Opening::Directives(index));
            }
        }
        if index >= self.lines.len() {
            if let Some(error) = self.scanner_error.take() {
                return Err(error);
            }
            if saw_directive {
                return Err(self.error(
                    self.empty_span(index),
                    ErrorCode::DirectiveWithoutDocument,
                    "directive without a document",
                ));
            }
            return Ok(Opening::End);
        }
        if saw_directive
            && !self.line_starts_with(index, |kind| {
                matches!(kind, TokenKind::DocumentStart { .. })
            })
        {
            return Err(self.error(
                self.line_first(index)
                    .map_or_else(|| self.empty_span(index), |token| token.span),
                ErrorCode::DirectiveWithoutDocument,
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
            return Ok(Opening::NoDocument(index + 1));
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
                        ErrorCode::InvalidDocumentStart,
                        "a document-start property cannot precede a same-line block mapping key",
                    ));
                }
                // A block collection starts after a line break (`s-l+block-collection`), so
                // `--- a: b` and `--- - a` are errors, as in libyaml.
                let tokens = self.tokens(line);
                if rest_start == 0
                    && (matches!(tokens[0].kind, TokenKind::BlockEntry) || is_mapping_line(tokens))
                {
                    return Err(self.error(
                        tokens[0].span,
                        ErrorCode::InvalidDocumentStart,
                        "a block collection cannot start on a `---` line",
                    ));
                }
            }
        }

        // A line that is neither a directive nor a lone `...` starts a document, with a `---`
        // or without one.
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
                return Ok(Opening::End);
            }
            return Err(self.error(
                document_start_span,
                ErrorCode::TrailingDocuments,
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
            let tokens = self.line_tokens(index);
            // Whether the root is a block collection, which a line after it that a tab seems to
            // indent was likely meant to be in.
            let block = tokens.first().is_some_and(|token| {
                matches!(
                    token.kind,
                    TokenKind::BlockEntry | TokenKind::Key | TokenKind::Value
                )
            }) || has_value_indicator(tokens);
            let root = NodeBounds {
                min_indent: 0,
                sequence_indent: 0,
            };
            Ok(Opening::Root {
                call: Some(Call::Node {
                    index,
                    indent: self.lines[index].indent,
                    depth: 0,
                    tag: None,
                    anchor: None,
                    bounds: root,
                }),
                index,
                block,
            })
        } else {
            let span = self.empty_span(index);
            self.emit(self.null_scalar(span))?;
            Ok(Opening::Root {
                call: None,
                index,
                block: false,
            })
        }
    }

    /// End the document whose root stops before line `index`, and return the line the next one
    /// starts at. `root_block` says whether the root is a block collection.
    fn close_document(&mut self, mut index: usize, root_block: bool) -> Result<usize, Error> {
        let mut explicit_end = false;
        let mut document_end_span = self.last_span(index);
        if index < self.lines.len()
            && self.line_starts_with(index, |kind| matches!(kind, TokenKind::DocumentEnd))
        {
            explicit_end = true;
            document_end_span = self
                .take_line_first_span(index)
                .unwrap_or(document_end_span);
            if let Some(token) = self.line_first(index) {
                return Err(self.error(
                    token.span,
                    ErrorCode::ContentAfterDocumentEnd,
                    "content after document end marker",
                ));
            }
            index += 1;
        }
        self.emit(Event::DocumentEnd {
            explicit: explicit_end,
            span: document_end_span,
        })?;

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
            let (fallback_code, fallback) = match tokens.first().map(|token| &token.kind) {
                Some(kind)
                    if kind.scalar_style().is_some_and(|style| {
                        matches!(style, ScalarStyle::Literal | ScalarStyle::Folded)
                    }) =>
                {
                    (
                        ErrorCode::InvalidBlockScalarIndentation,
                        "a block scalar is not indented past its parent",
                    )
                }
                Some(TokenKind::BlockEntry) => (
                    ErrorCode::InvalidBlockIndentation,
                    "a block entry is indented less than its parent",
                ),
                Some(TokenKind::PlainScalar | TokenKind::Scalar(_))
                    if root_block
                        && self.lines[index].indent > 0
                        && previous_line_is_blank(self.input, span.start()) =>
                {
                    (
                        ErrorCode::MissingMappingColon,
                        "a mapping line is missing a `:`",
                    )
                }
                _ => (
                    ErrorCode::TrailingContent,
                    "unexpected content after document",
                ),
            };
            let (code, message) = if tabbed {
                (
                    ErrorCode::InvalidTabIndentation,
                    "unexpected content after document".to_owned(),
                )
            } else if self.compat {
                self.compat_indentation_message(index).map_or_else(
                    || (fallback_code, fallback.to_owned()),
                    |message| (ErrorCode::InvalidBlockIndentation, message),
                )
            } else {
                (fallback_code, fallback.to_owned())
            };
            let error = self.error(span, code, message);
            // A node that only a tab seemed to indent under a key or an entry sits at its
            // line's left edge, so it ends up here.
            return Err(if tabbed {
                error.with_context(", indented by a tab, which YAML doesn't count as indentation")
            } else {
                error
            });
        }
        Ok(index)
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
                self.expect_consumed(
                    rest,
                    value,
                    ErrorCode::UnexpectedToken,
                    "unexpected token after node",
                )?;
                Ok(Step::Return(index + 1))
            }
            Cont::ItemInline {
                content,
                next_index,
            } => {
                self.expect_consumed(
                    content,
                    value,
                    ErrorCode::UnexpectedToken,
                    "unexpected token after node",
                )?;
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
                self.expect_consumed(
                    key,
                    value,
                    ErrorCode::UnexpectedToken,
                    "unexpected token in mapping key",
                )?;
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
                self.expect_consumed(
                    tokens,
                    value,
                    ErrorCode::UnexpectedMappingValue,
                    "unexpected token in mapping value",
                )?;
                Ok(Step::Return(current_index))
            }
            Cont::FlowSequenceItem { mut state, inline } => {
                if inline {
                    if value == 0 {
                        return Err(self.error(
                            self.token(state.tokens, state.index).span,
                            ErrorCode::InvalidFlowCollection,
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
    fn expect_flow_consumed(&mut self, run: Run, consumed: usize) -> Result<(), Error> {
        if consumed == run.len() {
            Ok(())
        } else {
            Err(self.flow_error(run, consumed, FlowLack::Separator))
        }
    }

    fn expect_consumed(
        &self,
        run: Run,
        consumed: usize,
        code: ErrorCode,
        message: &str,
    ) -> Result<(), Error> {
        if consumed == run.len() {
            Ok(())
        } else {
            Err(self.error(self.token(run, consumed).span, code, message))
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
fn implicit_key_fits(key: Location, colon: Location) -> bool {
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
    /// A node, with its error code and what stands there instead.
    Node(ErrorCode, String),
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

/// What the lines at a document's start lead to.
enum Opening<'a> {
    /// The stream has no more to read, or the reader stops at a second document.
    End,
    /// A line that opens no document, such as a lone `...`. The stream goes on at this line.
    NoDocument(usize),
    /// Directives of a document, with more of them or its start still to read from this line.
    Directives(usize),
    /// A document. Its root is read by `call`, or is empty; either way it stops before line
    /// `index` or the line that the call returns. `block` says whether it is a block collection.
    Root {
        call: Option<Call<'a>>,
        index: usize,
        block: bool,
    },
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
