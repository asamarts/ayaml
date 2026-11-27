//! Event parser.
//!
//! The parser consumes scanner tokens, groups them into logical lines, and
//! emits spanned events for streams, documents, block and flow collections,
//! scalars, aliases, tags, and anchors. Composing values and resolving
//! scalars happen downstream. The grammar runs on an explicit stack (see
//! `Parser::run`), so nesting depth costs heap, not native stack.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;

use crate::composer::StreamingComposer;
use crate::error::{Error, ErrorCode, ErrorKind};
use crate::event::{Anchor, CollectionStyle, Event, ScalarStyle};
use crate::limits::Limits;
use crate::position::{column_on_line, has_line_break, lines_at_most, location_at};
use crate::scanner::{Scanner, Token, TokenKind, UNMATCHED_FLOW};
use crate::span::{Location, Span};
use crate::tag::Tag;
use crate::value::Value;

pub(crate) use cursor::EventCursor;
pub(crate) use pull::{DocumentEvents, QueuedEvent};
pub(crate) use state::Cont;
pub(crate) use window::WindowScratch;

mod cursor;
mod flow;
mod grammar;
#[cfg(kani)]
mod kani_proofs;
mod pull;
mod state;
#[cfg(test)]
mod tests;
mod window;

use state::{
    BlockState, Call, DUPLICATE_PROPERTIES, FlowLack, FlowState, NO_VALUE_INDEX, NodeBounds,
    Opening, Properties, Run, Step,
};
use window::LineWindow;
#[cfg(kani)]
use window::compact_shift;
#[cfg(test)]
use window::{Line, TOKEN_ARENA_MID_CHUNK, push_token, read_lines, reserve_tokens};
#[cfg(any(test, kani))]
use window::{
    TOKEN_ARENA_AHEAD, TOKEN_ARENA_CHUNK, TOKEN_ARENA_SMALL_CHUNK, arena_growth, estimated_growth,
    mid_input_token_capacity, prefix_token_capacity,
};

/// Convert a bounded parser index or count to `u32`, returning an event-count limit error if it
/// would overflow (unreachable below 4 billion tokens/lines/columns).
#[inline]
fn to_u32(value: usize, span: Span, input: &str, compat: bool) -> Result<u32, Box<Error>> {
    u32::try_from(value).map_err(|_| {
        Box::new(
            Error::from(crate::limits::LimitExceeded::new(
                crate::limits::LimitKind::Events,
                u64::from(u32::MAX),
                u64::try_from(value).unwrap_or(u64::MAX),
            ))
            .with_span(span)
            .with_input_location(input, compat),
        )
    })
}

/// Convert an index already known to fit in `u32` (for example a sub-offset of a `Run` or `Line`).
#[inline]
fn fit_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Decode an inline flow closing partner's relative token distance (`UNMATCHED_FLOW` means
/// unmatched).
#[inline]
pub(super) fn matched_flow_end(end: u32) -> Option<usize> {
    (end != UNMATCHED_FLOW).then_some(end as usize)
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

enum EventSink<'a, 's> {
    Events(Vec<Event<'a>>),
    Streaming(AssertUnwindSafe<&'s mut StreamingComposer>),
    Tape(AssertUnwindSafe<&'s mut crate::tape::TapeBuilder<'a>>),
    Typed(Box<crate::typed::Builder<'a>>),
    Spanned(Box<crate::spanned::Builder<'a>>),
    #[cfg(test)]
    Consumer(AssertUnwindSafe<Box<dyn EventConsumer<'a> + 'a>>),
    /// Events that wait for a reader to take them (see [`EventCursor`]).
    Queue {
        events: VecDeque<Event<'a>>,
        root_entries: Option<usize>,
        room: usize,
        want_root_entries: bool,
    },
    /// `'static` queued events for [`DocumentEvents`] so [`crate::ParserBuf`] can reuse the queue.
    StaticQueue {
        events: VecDeque<QueuedEvent>,
        root_entries: Option<usize>,
        room: usize,
        want_root_entries: bool,
    },
}

impl<'a> EventSink<'a, '_> {
    /// Default queue watermark for `DocumentEvents` (typed streaming). A step of the grammar
    /// emits a few events, and a run of scalar entries or directives stops when this many wait.
    const QUEUE_ROOM: usize = 16;

    /// Larger queue watermark for `EventCursor` (`parse_events`, `Deserializer`, and merge-key
    /// expansion), keeping the parser loop hot in instruction cache across batches while staying
    /// inside one 256-slot `VecDeque` allocation (~26 KiB).
    const CURSOR_QUEUE_ROOM: usize = 250;

    /// Whether a reader should take the waiting events before more are emitted.
    #[inline]
    fn is_full(&self) -> bool {
        match self {
            Self::Queue { events, room, .. } => events.len() >= *room,
            Self::StaticQueue { events, room, .. } => events.len() >= *room,
            _ => false,
        }
    }

    /// The events that wait for a reader, if this sink keeps them for one.
    fn waiting(&mut self) -> Option<&mut VecDeque<Event<'a>>> {
        match self {
            Self::Queue { events, .. } => Some(events),
            _ => None,
        }
    }

    /// The `'static` queued events waiting for `DocumentEvents`.
    fn waiting_static(&mut self) -> Option<&mut VecDeque<QueuedEvent>> {
        match self {
            Self::StaticQueue { events, .. } => Some(events),
            _ => None,
        }
    }

    /// Whether this sink uses root flow collection entry-count hints.
    #[inline]
    fn wants_collection_hint(&self) -> bool {
        match self {
            Self::Events(_) => false,
            Self::Queue {
                want_root_entries, ..
            }
            | Self::StaticQueue {
                want_root_entries, ..
            } => *want_root_entries,
            _ => true,
        }
    }

    /// Say how many entries the root flow collection holds, before its start event.
    fn reserve_collection(&mut self, entries: usize) {
        match self {
            Self::Streaming(composer) => composer.0.reserve_collection(entries),
            Self::Tape(builder) => builder.0.reserve_collection(entries),
            Self::Typed(builder) => builder.reserve_collection(entries),
            Self::Spanned(builder) => builder.reserve_collection(entries),
            #[cfg(test)]
            Self::Consumer(consumer) => consumer.0.reserve_collection(entries),
            Self::Queue { root_entries, .. } | Self::StaticQueue { root_entries, .. } => {
                *root_entries = Some(entries);
            }
            Self::Events(_) => {}
        }
    }

    #[inline]
    fn event(&mut self, event: Event<'a>, input: &'a str) -> Result<(), Box<Error>> {
        match self {
            Self::Queue { events, .. } => {
                events.push_back(event);
                Ok(())
            }
            Self::StaticQueue { events, .. } => {
                events.push_back(QueuedEvent::from_event(event, input));
                Ok(())
            }
            Self::Streaming(composer) => composer.0.event(event),
            Self::Tape(builder) => builder.0.event(event),
            Self::Spanned(builder) => builder.event(event).map_err(Box::new),
            _ => self.event_slow(event),
        }
    }

    #[cold]
    #[inline(never)]
    fn event_slow(&mut self, event: Event<'a>) -> Result<(), Box<Error>> {
        match self {
            Self::Events(events) => {
                // Double while small, then grow by half to avoid reserving twice the events that
                // fit on wide documents.
                if events.len() == events.capacity() {
                    let cap = events.capacity();
                    let growth = if cap < 4096 { cap.max(16) } else { cap / 2 };
                    events.reserve_exact(growth);
                }
                events.push(event);
                Ok(())
            }
            Self::Typed(builder) => builder.push_event(event),
            #[cfg(test)]
            Self::Consumer(consumer) => consumer.0.event(event).map_err(Box::new),
            Self::Queue { .. }
            | Self::StaticQueue { .. }
            | Self::Streaming(_)
            | Self::Tape(_)
            | Self::Spanned(_) => {
                unreachable!("hot event sinks handled inline")
            }
        }
    }

    /// Specialized scalar callback so internal composers, tape/typed/spanned builders
    /// bypass constructing `Event::Scalar` values on the hot path.
    #[inline]
    #[allow(
        clippy::too_many_arguments,
        reason = "internal hot-path callback unpacks scalar fields without constructing Event::Scalar"
    )]
    fn scalar(
        &mut self,
        value: Cow<'a, str>,
        raw: &'a str,
        style: ScalarStyle,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        span: Span,
        input: &'a str,
    ) -> Result<(), Box<Error>> {
        match self {
            Self::Streaming(composer) => {
                composer
                    .0
                    .scalar(value, style, tag.as_ref(), anchor.as_ref(), span)
            }
            Self::Tape(builder) => builder.0.scalar(
                value.as_ref(),
                matches!(value, Cow::Borrowed(_)),
                style,
                tag,
                anchor.as_ref(),
                span,
            ),
            Self::Spanned(builder) => {
                builder.push_scalar(value, raw, style, tag, anchor.is_some(), span);
                Ok(())
            }
            Self::Queue { events, .. } => {
                events.push_back(Event::Scalar {
                    value,
                    raw: Cow::Borrowed(raw),
                    style,
                    tag,
                    anchor,
                    span,
                });
                Ok(())
            }
            Self::StaticQueue { events, .. } => {
                events.push_back(QueuedEvent::from_scalar(
                    value,
                    style,
                    tag,
                    anchor.as_ref(),
                    span,
                    input,
                ));
                Ok(())
            }
            _ => self.scalar_slow(value, raw, style, tag, anchor, span),
        }
    }

    #[cold]
    #[inline(never)]
    fn scalar_slow(
        &mut self,
        value: Cow<'a, str>,
        raw: &'a str,
        style: ScalarStyle,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        span: Span,
    ) -> Result<(), Box<Error>> {
        match self {
            Self::Typed(builder) => {
                builder.push_scalar(value, raw, style, tag.as_ref(), anchor.as_ref(), span)
            }
            _ => self.event_slow(Event::Scalar {
                value,
                raw: Cow::Borrowed(raw),
                style,
                tag,
                anchor,
                span,
            }),
        }
    }

    /// Specialized scalar callback when the caller already holds a `'static` [`TokenText`]
    /// (quoted and boxed scalar tokens), avoiding `TokenText -> Cow -> memcmp -> TokenText`
    /// round-trips on [`EventSink::StaticQueue`].
    #[inline]
    #[allow(
        clippy::too_many_arguments,
        reason = "internal hot-path callback unpacks scalar fields without constructing Event::Scalar"
    )]
    fn scalar_token_text(
        &mut self,
        value: crate::scanner::TokenText,
        raw: &'a str,
        style: ScalarStyle,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        span: Span,
        input: &'a str,
    ) -> Result<(), Box<Error>> {
        if let Self::StaticQueue { events, .. } = self {
            events.push_back(QueuedEvent::from_token_text(
                value,
                style,
                tag,
                anchor.as_ref(),
                span,
                input,
            ));
            return Ok(());
        }
        self.scalar(value.into_cow(input), raw, style, tag, anchor, span, input)
    }

    /// Specialized callback for untagged, unanchored single-line plain scalars.
    #[inline]
    fn plain_scalar(&mut self, value: &'a str, span: Span) -> Result<(), Box<Error>> {
        match self {
            Self::Streaming(composer) => composer.0.plain_scalar(value, span),
            Self::Tape(builder) => builder.0.plain_scalar(value, span),
            Self::Spanned(builder) => {
                builder.push_plain_scalar(value, span);
                Ok(())
            }
            Self::Queue { events, .. } => {
                events.push_back(Event::Scalar {
                    value: Cow::Borrowed(value),
                    raw: Cow::Borrowed(value),
                    style: ScalarStyle::Plain,
                    tag: None,
                    anchor: None,
                    span,
                });
                Ok(())
            }
            Self::StaticQueue { events, .. } => {
                events.push_back(QueuedEvent::plain_scalar(value.is_empty(), span));
                Ok(())
            }
            _ => self.scalar_slow(
                Cow::Borrowed(value),
                value,
                ScalarStyle::Plain,
                None,
                None,
                span,
            ),
        }
    }
}

struct Parser<'a, 's> {
    window: LineWindow<'a>,
    sink: EventSink<'a, 's>,
    limits: Limits,
    input: &'a str,
    /// Word a malformed flow collection's error as libyaml does, and count positions as compat's
    /// scanner does.
    compat: bool,
    documents: usize,
    /// The events of the stream so far, its start and end included, which count against
    /// `max_events`.
    emitted: usize,
    /// Stop at a second document start, as a single-document entry point
    /// needs: the rest of the stream is never parsed.
    single_document: bool,
    /// Emit the second document start and stop, instead of reporting it here. The typed builder
    /// then builds the first document before it reports the trailing one (AY-027).
    stop_at_second: bool,
    /// Last computed `(line_index, byte_offset, 1-based column)` for incremental compact-block
    /// column lookup without rescanning from line start at each nesting level.
    column_cache: (u32, u32, u32),
    /// Reusable continuation stack inside [`Parser::run`].
    cont_overflow: Vec<Cont>,
}

impl<'a, 's> Parser<'a, 's> {
    fn new(
        scanner: Scanner<'a>,
        sink: EventSink<'a, 's>,
        limits: &Limits,
        input: &'a str,
        compat: bool,
        single_document: bool,
        stop_at_second: bool,
    ) -> Self {
        Self::with_scratch(
            scanner,
            sink,
            limits,
            input,
            compat,
            single_document,
            stop_at_second,
            WindowScratch::default(),
            Vec::new(),
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "internal parser constructor threads optional ParserBuf scratch buffers directly"
    )]
    fn with_scratch(
        scanner: Scanner<'a>,
        sink: EventSink<'a, 's>,
        limits: &Limits,
        input: &'a str,
        compat: bool,
        single_document: bool,
        stop_at_second: bool,
        window_scratch: WindowScratch,
        cont_overflow: Vec<Cont>,
    ) -> Self {
        Self {
            window: LineWindow::with_scratch(scanner, input, compat, window_scratch),
            sink,
            limits: limits.clone(),
            input,
            compat,
            documents: 0,
            emitted: 0,
            single_document,
            stop_at_second,
            column_cache: (u32::MAX, 0, 0),
            cont_overflow,
        }
    }
}

/// The number of top-level entries between a flow collection's opening and closing tokens. An
/// entry counts once it has content, so separators alone reserve nothing.
fn flow_entry_count(tokens: &[Token], open: usize, close: usize) -> usize {
    let mut depth = 0_usize;
    let mut entries = 0_usize;
    let mut content = false;
    for token in &tokens[open + 1..close] {
        match token.kind {
            TokenKind::FlowSequenceStart { .. } | TokenKind::FlowMappingStart { .. } => {
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

/// Modest initial capacity for eager `Vec<Event>` parses without pre-scanning the whole input.
fn initial_event_capacity(input: &str, max_events: usize) -> usize {
    let estimate = if input.len() <= 4096 {
        (input.len() / 16).clamp(8, 256)
    } else {
        let sample = &input.as_bytes()[..4096];
        let sample_lines = lines_at_most(sample);
        if sample_lines == 0 || sample.len() / sample_lines > 80 {
            let starts_flow = sample
                .iter()
                .find(|byte| !byte.is_ascii_whitespace())
                .is_some_and(|byte| matches!(byte, b'[' | b'{'));
            if starts_flow {
                (input.len() / 6).clamp(256, 4096)
            } else {
                16
            }
        } else {
            (input.len() / 16).clamp(256, 4096)
        }
    };
    estimate.min(max_events.max(8))
}

#[cfg(test)]
fn event_capacity(lines: &[Line], tokens: &[Token]) -> usize {
    let base_headroom = if lines.iter().any(|line| line.indent() > 0) {
        256
    } else {
        200
    };
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
    let scanner = Scanner::with_compat(input, limits, compat_flow).map_err(|error| *error)?;
    let mut parser = Parser::new(
        scanner,
        EventSink::Typed(Box::new(builder)),
        limits,
        input,
        compat_flow,
        true,
        true,
    );
    let stream_start = parser
        .window
        .ensure_started(&parser.limits)
        .map_err(|error| *error)?;
    parser
        .emit(Event::StreamStart { span: stream_start })
        .map_err(|error| *error)?;
    parser.parse_stream().map_err(|error| *error)?;
    let stream_end = parser
        .window
        .stream_end()
        .unwrap_or_else(|| Span::empty(input.len()));
    parser
        .emit(Event::StreamEnd { span: stream_end })
        .map_err(|error| *error)?;
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
    let scanner = Scanner::with_compat(input, limits, compat_flow).map_err(|error| *error)?;
    let mut parser = Parser::new(
        scanner,
        EventSink::Consumer(AssertUnwindSafe(consumer)),
        limits,
        input,
        compat_flow,
        true,
        true,
    );
    let stream_start = parser
        .window
        .ensure_started(&parser.limits)
        .map_err(|error| *error)?;
    parser
        .emit(Event::StreamStart { span: stream_start })
        .map_err(|error| *error)?;
    parser.parse_stream().map_err(|error| *error)?;
    let stream_end = parser
        .window
        .stream_end()
        .unwrap_or_else(|| Span::empty(input.len()));
    parser
        .emit(Event::StreamEnd { span: stream_end })
        .map_err(|error| *error)?;
    match parser.sink {
        EventSink::Consumer(mut consumer) => consumer.0.finish(),
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
    let scanner = Scanner::with_compat(input, limits, compat_flow).map_err(|error| *error)?;
    let mut parser = Parser::new(
        scanner,
        EventSink::Events(Vec::with_capacity(initial_event_capacity(
            input,
            limits.max_events,
        ))),
        limits,
        input,
        compat_flow,
        single_document,
        stop_at_second,
    );
    let stream_start = parser
        .window
        .ensure_started(&parser.limits)
        .map_err(|error| *error)?;
    parser
        .emit(Event::StreamStart { span: stream_start })
        .map_err(|error| *error)?;
    parser.parse_stream().map_err(|error| *error)?;
    let stream_end = parser
        .window
        .stream_end()
        .unwrap_or_else(|| Span::empty(input.len()));
    parser
        .emit(Event::StreamEnd { span: stream_end })
        .map_err(|error| *error)?;
    match parser.sink {
        EventSink::Events(events) => Ok(events),
        _ => unreachable!("event sink kind changed during parsing"),
    }
}

/// Parse a stream directly into a [`crate::SpannedTree`] without `EventCursor` queue buffering.
pub(crate) fn parse_spanned<'a>(
    input: &'a str,
    limits: &Limits,
    compat: bool,
) -> Result<crate::spanned::SpannedTree<'a>, Error> {
    let scanner = Scanner::with_compat(input, limits, compat).map_err(|error| *error)?;
    let mut parser = Parser::new(
        scanner,
        EventSink::Spanned(Box::new(crate::spanned::Builder::new(input, false))),
        limits,
        input,
        compat,
        false,
        false,
    );
    let stream_start = parser
        .window
        .ensure_started(&parser.limits)
        .map_err(|error| (*error).with_input_location(input, compat))?;
    parser
        .emit(Event::StreamStart { span: stream_start })
        .map_err(|error| (*error).with_input_location(input, compat))?;
    parser
        .parse_stream()
        .map_err(|error| (*error).with_input_location(input, compat))?;
    let stream_end = parser
        .window
        .stream_end()
        .unwrap_or_else(|| Span::empty(input.len()));
    parser
        .emit(Event::StreamEnd { span: stream_end })
        .map_err(|error| (*error).with_input_location(input, compat))?;
    let Parser { window, sink, .. } = parser;
    drop(window);
    match sink {
        EventSink::Spanned(builder) => Ok(builder.finish()),
        _ => unreachable!("event sink kind changed during parsing"),
    }
}

/// Compose a single document directly from the parser without allocating an event vector or
/// single-element document `Vec`. A second document is an error at its start.
pub(crate) fn parse_value(input: &str, limits: &Limits) -> Result<Value, Error> {
    let scanner = Scanner::with_compat(input, limits, false).map_err(|error| *error)?;
    let mut composer = StreamingComposer::new_single(limits.clone(), input.len());
    let mut parser = Parser::new(
        scanner,
        EventSink::Streaming(AssertUnwindSafe(&mut composer)),
        limits,
        input,
        false,
        true,
        false,
    );
    let stream_start = parser
        .window
        .ensure_started(&parser.limits)
        .map_err(|error| (*error).with_input_location(input, false))?;
    parser
        .emit(Event::StreamStart { span: stream_start })
        .map_err(|error| (*error).with_input_location(input, false))?;
    parser
        .parse_stream()
        .map_err(|error| (*error).with_input_location(input, false))?;
    let stream_end = parser
        .window
        .stream_end()
        .unwrap_or_else(|| Span::empty(input.len()));
    parser
        .emit(Event::StreamEnd { span: stream_end })
        .map_err(|error| (*error).with_input_location(input, false))?;
    drop(parser);
    composer
        .finish_single()
        .map_err(|error| error.with_input_location(input, false))
}

/// Compose a single document reusing internal scratch buffers from `buf`.
pub(crate) fn parse_value_in(
    input: &str,
    limits: &Limits,
    buf: &mut crate::scratch::ParserBuf,
) -> Result<Value, Error> {
    let window_scratch = buf.take_window_scratch();
    let cont_overflow = buf.take_cont_overflow();
    let composer_scratch = buf.take_composer_scratch();
    let mut composer =
        StreamingComposer::with_scratch(limits.clone(), input.len(), true, composer_scratch);
    let (res, ws, co) = drive_stream(
        input,
        limits,
        true,
        EventSink::Streaming(AssertUnwindSafe(&mut composer)),
        window_scratch,
        cont_overflow,
        true,
    );
    buf.put_window_scratch(ws);
    buf.put_cont_overflow(co);
    if let Err(err) = res {
        buf.put_composer_scratch(composer.take_scratch());
        return Err(err);
    }
    let (value, cs) = composer.finish_single_with_scratch();
    buf.put_composer_scratch(cs);
    value.map_err(|error| error.with_input_location(input, false))
}

/// Drive a streaming parser with optional [`crate::scratch::ParserBuf`] window/continuation reuse.
fn drive_stream<'a>(
    input: &'a str,
    limits: &Limits,
    single_document: bool,
    sink: EventSink<'a, '_>,
    mut window_scratch: WindowScratch,
    cont_overflow: Vec<Cont>,
    save_scratch: bool,
) -> (Result<(), Error>, WindowScratch, Vec<Cont>) {
    let scanner_scratch = std::mem::take(&mut window_scratch.scanner);
    let scanner = match Scanner::with_compat_and_scratch(input, limits, false, scanner_scratch) {
        Ok(s) => s,
        Err(error) => return (Err(*error), window_scratch, cont_overflow),
    };
    let mut parser = Parser::with_scratch(
        scanner,
        sink,
        limits,
        input,
        false,
        single_document,
        false,
        window_scratch,
        cont_overflow,
    );
    let result = (|| -> Result<(), Error> {
        let stream_start = parser
            .window
            .ensure_started(&parser.limits)
            .map_err(|error| (*error).with_input_location(input, false))?;
        parser
            .emit(Event::StreamStart { span: stream_start })
            .map_err(|error| (*error).with_input_location(input, false))?;
        parser
            .parse_stream()
            .map_err(|error| (*error).with_input_location(input, false))?;
        let stream_end = parser
            .window
            .stream_end()
            .unwrap_or_else(|| Span::empty(input.len()));
        parser
            .emit(Event::StreamEnd { span: stream_end })
            .map_err(|error| (*error).with_input_location(input, false))?;
        Ok(())
    })();
    if save_scratch {
        let ws = parser.window.take_scratch();
        let co = std::mem::take(&mut parser.cont_overflow);
        (result, ws, co)
    } else {
        (result, WindowScratch::default(), Vec::new())
    }
}

/// Compose values directly from the parser without retaining an event vector.
/// With `single_document`, a second document is an error at its start.
pub(crate) fn parse_documents(
    input: &str,
    limits: &Limits,
    single_document: bool,
) -> Result<Vec<Value>, Error> {
    parse_documents_with_scratch(input, limits, single_document, None)
}

/// Compose values reusing internal scratch buffers from `buf`.
pub(crate) fn parse_documents_in(
    input: &str,
    limits: &Limits,
    single_document: bool,
    buf: &mut crate::scratch::ParserBuf,
) -> Result<Vec<Value>, Error> {
    parse_documents_with_scratch(input, limits, single_document, Some(buf))
}

fn parse_documents_with_scratch(
    input: &str,
    limits: &Limits,
    single_document: bool,
    buf: Option<&mut crate::scratch::ParserBuf>,
) -> Result<Vec<Value>, Error> {
    let (window_scratch, cont_overflow, composer_scratch, buf_slot) = match buf {
        Some(b) => (
            b.take_window_scratch(),
            b.take_cont_overflow(),
            b.take_composer_scratch(),
            Some(b),
        ),
        None => (
            WindowScratch::default(),
            Vec::new(),
            crate::composer::ComposerScratch::default(),
            None,
        ),
    };
    let mut composer = StreamingComposer::with_scratch(
        limits.clone(),
        input.len(),
        single_document,
        composer_scratch,
    );
    let save = buf_slot.is_some();
    let (res, ws, co) = drive_stream(
        input,
        limits,
        single_document,
        EventSink::Streaming(AssertUnwindSafe(&mut composer)),
        window_scratch,
        cont_overflow,
        save,
    );
    if let Some(b) = buf_slot {
        b.put_window_scratch(ws);
        b.put_cont_overflow(co);
        if let Err(err) = res {
            b.put_composer_scratch(composer.take_scratch());
            return Err(err);
        }
        let (docs, cs) = composer.finish_with_scratch();
        b.put_composer_scratch(cs);
        return docs.map_err(|error| error.with_input_location(input, false));
    }
    res?;
    composer
        .finish()
        .map_err(|error| error.with_input_location(input, false))
}

/// Stream a single document directly into a flat-array [`crate::Tape`].
pub(crate) fn parse_tape<'a>(
    input: &'a str,
    limits: &Limits,
    buf: Option<&mut crate::scratch::ParserBuf>,
) -> Result<crate::tape::Tape<'a>, Error> {
    limits.check_input(input.len()).map_err(|error| {
        Error::from(error)
            .with_span(Span::empty(0))
            .with_location(Location::new(1, 1))
    })?;
    let (window_scratch, cont_overflow, tape_scratch, recycled, buf_slot) = match buf {
        Some(b) => (
            b.take_window_scratch(),
            b.take_cont_overflow(),
            b.take_tape_scratch(),
            b.take_recycled_tape(),
            Some(b),
        ),
        None => (
            WindowScratch::default(),
            Vec::new(),
            crate::tape::TapeScratch::default(),
            crate::tape::RecycledTape::default(),
            None,
        ),
    };
    let mut builder = crate::tape::TapeBuilder::new(
        input,
        limits,
        true,
        crate::limits::AliasBudget::default(),
        tape_scratch,
        recycled,
    );
    let save = buf_slot.is_some();
    let (res, ws, co) = drive_stream(
        input,
        limits,
        true,
        EventSink::Tape(AssertUnwindSafe(&mut builder)),
        window_scratch,
        cont_overflow,
        save,
    );
    if let Some(b) = buf_slot {
        b.put_window_scratch(ws);
        b.put_cont_overflow(co);
        if let Err(err) = res {
            b.put_tape_scratch(builder.take_scratch());
            return Err(err);
        }
        let (tape, ts) = builder.finish_single();
        b.put_tape_scratch(ts);
        return tape.map_err(|error| error.with_input_location(input, false));
    }
    res?;
    let (tape, _) = builder.finish_single();
    tape.map_err(|error| error.with_input_location(input, false))
}

/// Stream every document in a YAML stream directly into a flat-array [`crate::TapeDocuments`].
pub(crate) fn parse_tape_documents<'a>(
    input: &'a str,
    limits: &Limits,
    buf: Option<&mut crate::scratch::ParserBuf>,
) -> Result<crate::tape::TapeDocuments<'a>, Error> {
    limits.check_input(input.len()).map_err(|error| {
        Error::from(error)
            .with_span(Span::empty(0))
            .with_location(Location::new(1, 1))
    })?;
    let (window_scratch, cont_overflow, tape_scratch, recycled, buf_slot) = match buf {
        Some(b) => (
            b.take_window_scratch(),
            b.take_cont_overflow(),
            b.take_tape_scratch(),
            b.take_recycled_tape(),
            Some(b),
        ),
        None => (
            WindowScratch::default(),
            Vec::new(),
            crate::tape::TapeScratch::default(),
            crate::tape::RecycledTape::default(),
            None,
        ),
    };
    let mut builder = crate::tape::TapeBuilder::new(
        input,
        limits,
        false,
        crate::limits::AliasBudget::default(),
        tape_scratch,
        recycled,
    );
    let save = buf_slot.is_some();
    let (res, ws, co) = drive_stream(
        input,
        limits,
        false,
        EventSink::Tape(AssertUnwindSafe(&mut builder)),
        window_scratch,
        cont_overflow,
        save,
    );
    if let Some(b) = buf_slot {
        b.put_window_scratch(ws);
        b.put_cont_overflow(co);
        if let Err(err) = res {
            b.put_tape_scratch(builder.take_scratch());
            return Err(err);
        }
        let (docs, ts) = builder.finish_documents();
        b.put_tape_scratch(ts);
        return docs.map_err(|error| error.with_input_location(input, false));
    }
    res?;
    let (docs, _) = builder.finish_documents();
    docs.map_err(|error| error.with_input_location(input, false))
}

impl<'a> Parser<'a, '_> {
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
        let indent = self.window.get_line(index)?.indent();
        if index == 0 && indent > 0 {
            return Some("a document's first node cannot be indented".to_owned());
        }
        let (parent_offset, is_seq) = self.window.compat_parent(index, indent)?;
        let position = self.at(parent_offset);
        let kind = if is_seq { "sequence" } else { "mapping" };
        Some(format!(
            "this line is indented differently from the {kind} at line {} column {}",
            position.line(),
            position.column()
        ))
    }

    #[cold]
    fn at(&self, offset: usize) -> Location {
        location_at(self.input, offset, self.compat).unwrap_or_default()
    }

    #[inline]
    pub(super) fn column_at(&mut self, line: usize, offset: usize) -> usize {
        let (cached_line, cached_offset, cached_col) = self.column_cache;
        if cached_line as usize == line
            && offset >= cached_offset as usize
            && !has_line_break(&self.input.as_bytes()[cached_offset as usize..offset])
        {
            let slice = &self.input[cached_offset as usize..offset];
            let delta = if slice.is_ascii() {
                slice.len()
            } else {
                slice.chars().count()
            };
            let col = cached_col as usize + delta;
            if let (Ok(o), Ok(c)) = (u32::try_from(offset), u32::try_from(col)) {
                self.column_cache = (cached_line, o, c);
            }
            return col;
        }
        let col = column_on_line(self.input, offset, self.compat);
        if let (Ok(l), Ok(o), Ok(c)) = (
            u32::try_from(line),
            u32::try_from(offset),
            u32::try_from(col),
        ) {
            self.column_cache = (l, o, c);
        }
        col
    }

    /// Whether an implicit key starting at `start` and ending before `:` at `colon_start` lies on
    /// one line and fits within `MAX_IMPLICIT_KEY_CHARS` Unicode scalar values.
    #[inline]
    pub(super) fn implicit_key_fits_range(&self, start: usize, colon_start: usize) -> bool {
        if start > colon_start || colon_start > self.input.len() {
            return false;
        }
        // Every UTF-8 scalar value is at most 4 bytes, so any slice longer than
        // `4 * MAX_IMPLICIT_KEY_CHARS` bytes exceeds `MAX_IMPLICIT_KEY_CHARS` characters.
        if colon_start - start > MAX_IMPLICIT_KEY_CHARS * 4 {
            return false;
        }
        if has_line_break(&self.input.as_bytes()[start..colon_start]) {
            return false;
        }
        if colon_start - start <= MAX_IMPLICIT_KEY_CHARS {
            return true;
        }
        self.input[start..colon_start]
            .chars()
            .take(MAX_IMPLICIT_KEY_CHARS + 1)
            .count()
            <= MAX_IMPLICIT_KEY_CHARS
    }

    /// The number of top-level entries in the flow collection that opens at `tokens.start + start`,
    /// or `None` when the collection is malformed or not flow.
    fn flow_entries(&self, tokens: Run, start: usize) -> Option<usize> {
        let line_tokens = self.line_tokens(tokens.line());
        let open = tokens.start() + start;
        let delta = match line_tokens.get(open)?.kind {
            TokenKind::FlowSequenceStart { end } | TokenKind::FlowMappingStart { end } => {
                matched_flow_end(end)?
            }
            _ => return None,
        };
        let close = open.checked_add(delta)?;
        (close < line_tokens.len()).then(|| flow_entry_count(line_tokens, open, close))
    }

    /// Surface a deferred scanner error once parsing reaches the truncated EOF boundary, before
    /// synthesizing an empty node or closing a block collection at EOF.
    #[inline]
    pub(super) fn check_truncated_eof(&mut self, index: usize) -> Result<(), Box<Error>> {
        if self.window.has_scanner_error() && index >= self.window.loaded_end() {
            return self.window.take_scanner_error();
        }
        Ok(())
    }

    /// Surface a deferred scanner error once parsing reaches the end of a truncated final line,
    /// before synthesizing a null scalar or reporting a missing `:` on a truncated prefix.
    #[inline]
    pub(super) fn check_truncated_run_end(
        &mut self,
        run: Run,
        index: usize,
    ) -> Result<(), Box<Error>> {
        if self.window.truncated_last_line()
            && self.window.has_scanner_error()
            && index >= run.len()
            && run.line() + 1 == self.window.loaded_end()
            && run.end() == self.window.line(run.line()).len()
        {
            return self.window.take_scanner_error();
        }
        Ok(())
    }

    /// Whether `current_index` is the final line at `indent`, truncated mid-line before its `:`,
    /// and could still begin an implicit mapping key (so earlier syntax errors on that line are
    /// checked in order before surfacing the deferred scanner error).
    #[inline]
    pub(super) fn is_truncated_mapping_key_line(
        &self,
        current_index: usize,
        indent: usize,
    ) -> bool {
        self.window.truncated_last_line()
            && self.window.has_scanner_error()
            && current_index + 1 == self.window.loaded_end()
            && self.window.line(current_index).indent() == indent
            && !self.line_starts_with(current_index, |kind| {
                matches!(
                    kind,
                    TokenKind::DocumentStart { .. }
                        | TokenKind::DocumentEnd
                        | TokenKind::BlockEntry
                )
            })
    }

    /// Read every document of the stream.
    fn parse_stream(&mut self) -> Result<(), Box<Error>> {
        let mut index = 0;
        let mut directives = false;
        while self.ensure_line(index)? {
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
        self.window.take_scanner_error()
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
    ) -> Result<Opening<'a>, Box<Error>> {
        self.window.retire_before(index);
        while self.ensure_line(index)? {
            let Some(TokenKind::Directive(directive)) =
                self.line_first(index).map(|token| &token.kind)
            else {
                break;
            };
            let Some(span) = self.line_first(index).map(|token| token.span) else {
                break;
            };
            let event = Event::Directive {
                name: Cow::Borrowed(directive.name.slice(self.input).unwrap_or("")),
                value: Cow::Borrowed(directive.value.slice(self.input).unwrap_or("")),
                span,
            };
            self.emit(event)?;
            saw_directive = true;
            index += 1;
            self.window.retire_before(index);
            if self.sink.is_full() {
                return Ok(Opening::Directives(index));
            }
        }
        if !self.ensure_line(index)? {
            self.window.take_scanner_error()?;
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
            && self.window.line(index).len() == 1
            && self.line_starts_with(index, |kind| matches!(kind, TokenKind::DocumentEnd))
        {
            self.window.retire_before(index + 1);
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
            if self.window.line(index).is_empty() {
                index += 1;
                self.window.retire_before(index);
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

        if self.ensure_line(index)?
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
                    indent: self.window.line(index).indent(),
                    depth: 0,
                    tag: None,
                    anchor: None,
                    bounds: root,
                }),
                index,
                block,
            })
        } else {
            self.check_truncated_eof(index)?;
            let span = self.empty_span(index);
            self.emit_null_scalar(span)?;
            Ok(Opening::Root {
                call: None,
                index,
                block: false,
            })
        }
    }

    /// End the document whose root stops before line `index`, and return the line the next one
    /// starts at. `root_block` says whether the root is a block collection.
    fn close_document(&mut self, mut index: usize, root_block: bool) -> Result<usize, Box<Error>> {
        self.window.retire_before(index);
        let mut explicit_end = false;
        let mut document_end_span = self.last_span(index);
        if self.ensure_line(index)?
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
            self.window.retire_before(index);
        }
        self.emit(Event::DocumentEnd {
            explicit: explicit_end,
            span: document_end_span,
        })?;

        if !explicit_end
            && self.ensure_line(index)?
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
                Some(
                    TokenKind::PlainScalar
                    | TokenKind::SingleQuotedScalar
                    | TokenKind::DoubleQuotedScalar
                    | TokenKind::Scalar(_),
                ) if root_block
                    && self.window.line(index).indent() > 0
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
                Box::new(
                    (*error).with_context(
                        ", indented by a tab, which YAML doesn't count as indentation",
                    ),
                )
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
    fn run(&mut self, call: Call<'a>) -> Result<usize, Box<Error>> {
        let mut stack = std::mem::take(&mut self.cont_overflow);
        stack.clear();
        let result = (|| -> Result<usize, Box<Error>> {
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
        })();
        stack.clear();
        self.cont_overflow = stack;
        result
    }

    fn call(&mut self, call: Call<'a>, stack: &mut Vec<Cont>) -> Result<Step<'a>, Box<Error>> {
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
    ) -> Result<Step<'a>, Box<Error>> {
        match cont {
            Cont::NodeInline { rest, index } => {
                self.expect_consumed(
                    rest,
                    value,
                    ErrorCode::UnexpectedToken,
                    "unexpected token after node",
                )?;
                Ok(Step::Return(index as usize + 1))
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
                Ok(Step::Return(next_index as usize))
            }
            Cont::Sequence(mut state) => {
                state.current_index = fit_u32(value);
                self.sequence_loop(state, stack)
            }
            Cont::MappingExplicitKey(mut state) => {
                state.current_index = fit_u32(value);
                self.mapping_after_explicit_key(state, stack)
            }
            Cont::MappingImplicitKey {
                state,
                tokens,
                value_index,
            } => {
                let value_index = (value_index != NO_VALUE_INDEX).then_some(value_index as usize);
                let key = value_index.map_or(tokens, |index| tokens.until(index));
                self.expect_consumed(
                    key,
                    value,
                    ErrorCode::UnexpectedToken,
                    "unexpected token in mapping key",
                )?;
                self.mapping_after_implicit_key(state, tokens, value_index, stack)
            }
            Cont::MappingEntry(mut state) => {
                state.current_index = fit_u32(value);
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
                Ok(Step::Return(current_index as usize))
            }
            Cont::FlowSequenceItem { mut state, inline } => {
                if inline {
                    if value == 0 {
                        return Err(self.error(
                            self.token(state.tokens, state.index as usize).span,
                            ErrorCode::InvalidFlowCollection,
                            "expected a flow item",
                        ));
                    }
                    state.index = fit_u32(state.index as usize + value);
                } else {
                    state.index = fit_u32(value);
                }
                match self
                    .token_at(state.tokens, state.index as usize)
                    .map(|token| &token.kind)
                {
                    Some(TokenKind::FlowEntry) => state.index += 1,
                    Some(TokenKind::FlowSequenceEnd) => {}
                    _ => {
                        return Err(self.flow_error(
                            state.tokens,
                            state.index as usize,
                            FlowLack::Separator,
                        ));
                    }
                }
                self.flow_sequence_loop(state, stack)
            }
            Cont::FlowSequenceNext(state) => self.flow_sequence_loop(state, stack),
            Cont::FlowExplicitKey {
                tokens,
                start_span,
                key_start,
                key_end,
                depth,
            } => {
                let key = tokens.slice(key_start as usize, key_end as usize);
                self.expect_flow_consumed(key, value)?;
                self.flow_explicit_value(
                    tokens,
                    start_span,
                    key_end as usize,
                    depth as usize,
                    stack,
                )
            }
            Cont::FlowExplicitValue {
                tokens,
                start_span,
                value_start,
                value_end,
            } => {
                let value_tokens = tokens.slice(value_start as usize, value_end as usize);
                self.expect_flow_consumed(value_tokens, value)?;
                self.flow_mapping_end(tokens, start_span, value_end as usize)
            }
            Cont::ImplicitFlowKey {
                tokens,
                start_span,
                key_start,
                colon,
                depth,
            } => {
                let key = tokens.slice(key_start as usize, colon as usize);
                self.expect_flow_consumed(key, value)?;
                self.implicit_flow_value(tokens, start_span, colon as usize, depth as usize, stack)
            }
            Cont::ImplicitFlowValue {
                tokens,
                start_span,
                value_start,
                value_end,
            } => {
                let value_tokens = tokens.slice(value_start as usize, value_end as usize);
                self.expect_flow_consumed(value_tokens, value)?;
                self.flow_mapping_end(tokens, start_span, value_end as usize)
            }
            Cont::FlowMappingKey { state, key_start } => {
                let key = state.tokens.slice(key_start as usize, state.index as usize);
                self.expect_flow_consumed(key, value)?;
                self.flow_mapping_after_key(state, stack)
            }
            Cont::FlowMappingValue { state, value_start } => {
                let value_tokens = state
                    .tokens
                    .slice(value_start as usize, state.index as usize);
                self.expect_flow_consumed(value_tokens, value)?;
                self.flow_mapping_next(state, stack)
            }
            Cont::FlowMappingNext(state) => self.flow_mapping_next(state, stack),
        }
    }

    /// Fail unless a node consumed all of `run`.
    /// A flow key or value that stops short has something after it where `,` or a closing bracket
    /// belongs.
    fn expect_flow_consumed(&mut self, run: Run, consumed: usize) -> Result<(), Box<Error>> {
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
    ) -> Result<(), Box<Error>> {
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

fn value_indicator_index(tokens: &[Token]) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        match token.kind {
            TokenKind::FlowSequenceStart { .. } | TokenKind::FlowMappingStart { .. } => depth += 1,
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
fn flow_brackets_match(tokens: &[Token]) -> bool {
    let mut open = Vec::new();
    tokens.iter().all(|token| match token.kind {
        TokenKind::FlowSequenceStart { .. } => {
            open.push(TokenKind::FlowSequenceEnd);
            true
        }
        TokenKind::FlowMappingStart { .. } => {
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

fn has_value_indicator(tokens: &[Token]) -> bool {
    value_indicator_index(tokens).is_some()
}

fn is_mapping_line(tokens: &[Token]) -> bool {
    match tokens.first().map(|token| &token.kind) {
        Some(TokenKind::Key | TokenKind::Value) => true,
        // A document marker ends the collection, whatever follows it on its line.
        Some(TokenKind::DocumentStart { .. } | TokenKind::DocumentEnd) | None => false,
        Some(_) => has_value_indicator(tokens),
    }
}
