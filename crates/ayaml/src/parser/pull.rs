//! One document's events, on demand.
//!
//! The grammar already runs on an explicit stack (see `Parser::run`), so it can stop between
//! two steps and go on later. A reader that builds its value from the events takes each one as
//! it needs it, and no vector of events, no full-document token buffer, and no tree made from
//! them stands between the scanner and the value. A step that emits a run of events, of scalar
//! entries or of directives, stops within the run, so the events that wait stay a few however
//! long the run is.

use std::borrow::Cow;
use std::collections::VecDeque;

use super::{Cont, EventSink, Opening, Parser, Step, WindowScratch};
use crate::error::{Error, ErrorKind};
use crate::event::{Anchor, CollectionStyle, Event, ScalarStyle};
use crate::limits::Limits;
use crate::scanner::{Scanner, TokenText};
use crate::span::Span;
use crate::tag::Tag;

/// `'static` event stored inside [`DocumentEvents`]'s lookahead queue so [`crate::ParserBuf`]
/// can reuse the queue across repeated `from_str_in` parses with zero `unsafe`.
#[derive(Debug)]
pub(crate) enum QueuedEvent {
    StreamStart {
        span: Span,
    },
    StreamEnd {
        span: Span,
    },
    DocumentStart {
        explicit: bool,
        span: Span,
    },
    DocumentEnd {
        explicit: bool,
        span: Span,
    },
    Directive {
        name: TokenText,
        value: TokenText,
        span: Span,
    },
    SequenceStart {
        tag: Option<Tag>,
        anchor: Option<TokenText>,
        style: CollectionStyle,
        span: Span,
    },
    SequenceEnd {
        span: Span,
    },
    MappingStart {
        tag: Option<Tag>,
        anchor: Option<TokenText>,
        style: CollectionStyle,
        span: Span,
    },
    MappingEnd {
        span: Span,
    },
    Scalar {
        value: TokenText,
        style: ScalarStyle,
        tag: Option<Tag>,
        anchor: Option<TokenText>,
        span: Span,
    },
    Alias {
        anchor: TokenText,
        span: Span,
    },
}

#[inline]
fn anchor_to_token_text(anchor: &Anchor<'_>, span: Span, input: &str) -> TokenText {
    let text = anchor.as_str();
    let needle = text.as_bytes();
    let bytes = input.as_bytes();
    if span.len() == needle.len().saturating_add(1)
        && let Some(sigil) = bytes.get(span.start())
        && (*sigil == b'&' || *sigil == b'*')
        && bytes.get(span.start() + 1..span.end()) == Some(needle)
    {
        return TokenText::Slice(Span::new(span.start() + 1, span.end()));
    }
    // Empty anchored scalars (e.g. `a:\n  &x\n`) have `span = Span::empty(anchor_start)`, so
    // `&name` begins directly at `span.start()`.
    if bytes.get(span.start()) == Some(&b'&')
        && let Some(name_start) = span.start().checked_add(1)
        && let Some(name_end) = name_start.checked_add(needle.len())
        && bytes.get(name_start..name_end) == Some(needle)
    {
        return TokenText::Slice(Span::new(name_start, name_end));
    }
    let mut end = span.start().min(bytes.len());
    while end > 0 && matches!(bytes[end - 1], b' ' | b'\t' | b'\n' | b'\r') {
        end -= 1;
    }
    if end > needle.len()
        && let Some(name_start) = end.checked_sub(needle.len())
        && let Some(sigil_pos) = name_start.checked_sub(1)
        && bytes.get(sigil_pos) == Some(&b'&')
        && bytes.get(name_start..end) == Some(needle)
    {
        return TokenText::Slice(Span::new(name_start, end));
    }
    let search_start = span.start().saturating_sub(needle.len().saturating_add(64));
    let min_pos = search_start.saturating_add(needle.len()).saturating_add(1);
    let mut pos = end;
    while pos > min_pos {
        pos -= 1;
        let name_start = pos - needle.len();
        let sigil_pos = name_start - 1;
        if bytes.get(sigil_pos) == Some(&b'&') && bytes.get(name_start..pos) == Some(needle) {
            return TokenText::Slice(Span::new(name_start, pos));
        }
    }
    TokenText::Owned(text.to_owned())
}

#[inline]
fn cow_to_token_text(
    value: Cow<'_, str>,
    style: ScalarStyle,
    span: Span,
    input: &str,
) -> TokenText {
    match value {
        Cow::Borrowed(text) => {
            if text.is_empty() {
                return TokenText::EMPTY;
            }
            if let Some(raw) = span.slice(input) {
                if raw == text {
                    return TokenText::Slice(span);
                }
                if style == ScalarStyle::Plain {
                    let trimmed = raw.trim_end_matches([' ', '\t']);
                    if trimmed == text {
                        return TokenText::Slice(Span::new(
                            span.start(),
                            span.start() + trimmed.len(),
                        ));
                    }
                }
                if matches!(style, ScalarStyle::SingleQuoted | ScalarStyle::DoubleQuoted)
                    && span.len() >= 2
                {
                    let inner = Span::new(span.start() + 1, span.end() - 1);
                    if inner.slice(input) == Some(text) {
                        return TokenText::Slice(inner);
                    }
                }
            }
            TokenText::Owned(text.to_owned())
        }
        Cow::Owned(owned) => TokenText::Owned(owned),
    }
}

impl QueuedEvent {
    #[inline]
    pub(super) fn plain_scalar(empty: bool, span: Span) -> Self {
        Self::Scalar {
            value: if empty {
                TokenText::EMPTY
            } else {
                TokenText::Slice(span)
            },
            style: ScalarStyle::Plain,
            tag: None,
            anchor: None,
            span,
        }
    }

    #[inline]
    pub(super) fn from_token_text(
        value: TokenText,
        style: ScalarStyle,
        tag: Option<Tag>,
        anchor: Option<&Anchor<'_>>,
        span: Span,
        input: &str,
    ) -> Self {
        Self::Scalar {
            value,
            style,
            tag,
            anchor: anchor.map(|a| anchor_to_token_text(a, span, input)),
            span,
        }
    }

    #[inline]
    pub(super) fn from_scalar(
        value: Cow<'_, str>,
        style: ScalarStyle,
        tag: Option<Tag>,
        anchor: Option<&Anchor<'_>>,
        span: Span,
        input: &str,
    ) -> Self {
        Self::Scalar {
            value: cow_to_token_text(value, style, span, input),
            style,
            tag,
            anchor: anchor.map(|a| anchor_to_token_text(a, span, input)),
            span,
        }
    }

    #[inline]
    pub(super) fn from_event(event: Event<'_>, input: &str) -> Self {
        match event {
            Event::StreamStart { span } => Self::StreamStart { span },
            Event::StreamEnd { span } => Self::StreamEnd { span },
            Event::DocumentStart { explicit, span } => Self::DocumentStart { explicit, span },
            Event::DocumentEnd { explicit, span } => Self::DocumentEnd { explicit, span },
            Event::Directive { name, value, span } => {
                let raw = span.slice(input).unwrap_or("");
                let name_tt = if name.is_empty() {
                    TokenText::EMPTY
                } else if raw
                    .strip_prefix('%')
                    .is_some_and(|rest| rest.starts_with(name.as_ref()))
                {
                    let s = span.start() + 1;
                    TokenText::Slice(Span::new(s, s + name.len()))
                } else {
                    TokenText::Owned(name.into_owned())
                };
                let value_tt = if value.is_empty() {
                    TokenText::EMPTY
                } else if let Some(pos) = raw.rfind(value.as_ref()) {
                    let s = span.start() + pos;
                    TokenText::Slice(Span::new(s, s + value.len()))
                } else {
                    TokenText::Owned(value.into_owned())
                };
                Self::Directive {
                    name: name_tt,
                    value: value_tt,
                    span,
                }
            }
            Event::SequenceStart {
                tag,
                anchor,
                style,
                span,
            } => Self::SequenceStart {
                tag,
                anchor: anchor
                    .as_ref()
                    .map(|a| anchor_to_token_text(a, span, input)),
                style,
                span,
            },
            Event::SequenceEnd { span } => Self::SequenceEnd { span },
            Event::MappingStart {
                tag,
                anchor,
                style,
                span,
            } => Self::MappingStart {
                tag,
                anchor: anchor
                    .as_ref()
                    .map(|a| anchor_to_token_text(a, span, input)),
                style,
                span,
            },
            Event::MappingEnd { span } => Self::MappingEnd { span },
            Event::Scalar {
                value,
                style,
                tag,
                anchor,
                span,
                ..
            } => Self::from_scalar(value, style, tag, anchor.as_ref(), span, input),
            Event::Alias { anchor, span } => Self::Alias {
                anchor: anchor_to_token_text(&anchor, span, input),
                span,
            },
        }
    }

    #[inline]
    pub(crate) fn into_event(self, input: &str) -> Event<'_> {
        match self {
            Self::StreamStart { span } => Event::StreamStart { span },
            Self::StreamEnd { span } => Event::StreamEnd { span },
            Self::DocumentStart { explicit, span } => Event::DocumentStart { explicit, span },
            Self::DocumentEnd { explicit, span } => Event::DocumentEnd { explicit, span },
            Self::Directive { name, value, span } => Event::Directive {
                name: name.into_cow(input),
                value: value.into_cow(input),
                span,
            },
            Self::SequenceStart {
                tag,
                anchor,
                style,
                span,
            } => Event::SequenceStart {
                tag,
                anchor: anchor.map(|a| Anchor::new(a.into_cow(input))),
                style,
                span,
            },
            Self::SequenceEnd { span } => Event::SequenceEnd { span },
            Self::MappingStart {
                tag,
                anchor,
                style,
                span,
            } => Event::MappingStart {
                tag,
                anchor: anchor.map(|a| Anchor::new(a.into_cow(input))),
                style,
                span,
            },
            Self::MappingEnd { span } => Event::MappingEnd { span },
            Self::Scalar {
                value,
                style,
                tag,
                anchor,
                span,
            } => {
                let raw = span.slice(input).unwrap_or("");
                let value = match value {
                    TokenText::Slice(s) if s == span => Cow::Borrowed(raw),
                    other => other.into_cow(input),
                };
                Event::Scalar {
                    value,
                    raw: Cow::Borrowed(raw),
                    style,
                    tag,
                    anchor: anchor.map(|a| Anchor::new(a.into_cow(input))),
                    span,
                }
            }
            Self::Alias { anchor, span } => Event::Alias {
                anchor: Anchor::new(anchor.into_cow(input)),
                span,
            },
        }
    }

    #[inline]
    pub(crate) fn span(&self) -> Span {
        match self {
            Self::StreamStart { span }
            | Self::StreamEnd { span }
            | Self::DocumentStart { span, .. }
            | Self::DocumentEnd { span, .. }
            | Self::Directive { span, .. }
            | Self::SequenceStart { span, .. }
            | Self::SequenceEnd { span }
            | Self::MappingStart { span, .. }
            | Self::MappingEnd { span }
            | Self::Scalar { span, .. }
            | Self::Alias { span, .. } => *span,
        }
    }
}

/// The events of a single-document input, read as they are asked for. A second document's start
/// is its last event before the stream's end, as `parse_document_with_consumer` gives it.
pub(crate) struct DocumentEvents<'a> {
    parser: Parser<'a, 'a>,
    state: State,
    /// The step that a root's reading stopped before. It is kept apart from the state, which is
    /// taken up and put away at every turn of the grammar, because a step is several times its
    /// size.
    step: Option<Step<'a>>,
    /// The grammar's continuations while a root is being read.
    stack: Vec<Cont>,
}

/// Where the grammar stands between two events.
#[derive(Clone, Copy)]
enum State {
    /// Before the stream's start.
    Start,
    /// Between documents, at this line.
    Between(usize),
    /// After some of a document's directives, at this line.
    Directives(usize),
    /// In a document's root, about to take the step that waits. `block` says whether the root
    /// is a block collection.
    Root { block: bool },
    /// After the stream's end, or after an error.
    Finished,
}

impl<'a> DocumentEvents<'a> {
    /// Prepare a streaming parser over `input` without materializing the full document's tokens.
    pub(crate) fn new(input: &'a str, limits: &Limits, compat_flow: bool) -> Result<Self, Error> {
        let scanner = Scanner::with_compat(input, limits, compat_flow).map_err(|error| *error)?;
        let (room, cap) = if input.len() <= 512 {
            (7, 8)
        } else if input.len() <= 8_192 {
            (14, 16)
        } else {
            (EventSink::QUEUE_ROOM, 2 * EventSink::QUEUE_ROOM)
        };
        Ok(Self {
            parser: Parser::new(
                scanner,
                EventSink::StaticQueue {
                    events: VecDeque::with_capacity(cap),
                    root_entries: None,
                    room,
                    want_root_entries: true,
                },
                limits,
                input,
                compat_flow,
                true,
                true,
            ),
            state: State::Start,
            step: None,
            stack: Vec::new(),
        })
    }

    /// Prepare a streaming parser over `input` reusing pooled scratch from [`crate::ParserBuf`].
    pub(crate) fn new_with_scratch(
        input: &'a str,
        limits: &Limits,
        compat_flow: bool,
        mut window_scratch: WindowScratch,
        mut queue: VecDeque<QueuedEvent>,
        cont_overflow: Vec<Cont>,
    ) -> Result<Self, Error> {
        let scanner_scratch = std::mem::take(&mut window_scratch.scanner);
        let scanner = Scanner::with_compat_and_scratch(input, limits, compat_flow, scanner_scratch)
            .map_err(|error| *error)?;
        let (room, cap) = if input.len() <= 512 {
            (7, 8)
        } else if input.len() <= 8_192 {
            (14, 16)
        } else {
            (EventSink::QUEUE_ROOM, 2 * EventSink::QUEUE_ROOM)
        };
        queue.clear();
        if queue.capacity() < cap {
            queue.reserve_exact(cap);
        }
        let mut stack = cont_overflow;
        stack.clear();
        Ok(Self {
            parser: Parser::with_scratch(
                scanner,
                EventSink::StaticQueue {
                    events: queue,
                    root_entries: None,
                    room,
                    want_root_entries: true,
                },
                limits,
                input,
                compat_flow,
                true,
                true,
                window_scratch,
                Vec::new(),
            ),
            state: State::Start,
            step: None,
            stack,
        })
    }

    /// Extract reusable `'static` scratch buffers back into [`crate::ParserBuf`].
    pub(crate) fn take_scratch(&mut self) -> (WindowScratch, VecDeque<QueuedEvent>, Vec<Cont>) {
        let window_scratch = self.parser.window.take_scratch();
        let mut queue = match &mut self.parser.sink {
            EventSink::StaticQueue { events, .. } => std::mem::take(events),
            _ => VecDeque::new(),
        };
        queue.clear();
        if queue.capacity() > 256 {
            queue = VecDeque::new();
        }
        let mut cont_overflow = std::mem::take(&mut self.stack);
        cont_overflow.clear();
        if cont_overflow.capacity() > 256 {
            cont_overflow = Vec::new();
        }
        (window_scratch, queue, cont_overflow)
    }

    #[inline]
    pub(crate) fn input(&self) -> &'a str {
        self.parser.input
    }

    /// Top up the lookahead queue up to its configured watermark when a collection opens, so small
    /// collections see their closing event in the queue and supply an exact `size_hint()`.
    /// Returns `true` when additional events were pulled into the queue.
    pub(crate) fn refill_for_hint(&mut self) -> Result<bool, Error> {
        if matches!(self.state, State::Finished) || self.parser.sink.is_full() {
            return Ok(false);
        }
        self.fill()?;
        Ok(true)
    }

    /// The next raw `'static` queued event, or `None` after the stream's end.
    #[inline]
    pub(crate) fn next_queued(&mut self) -> Result<Option<QueuedEvent>, Error> {
        if let Some(event) = self
            .parser
            .sink
            .waiting_static()
            .and_then(VecDeque::pop_front)
        {
            return Ok(Some(event));
        }
        self.fill()?;
        Ok(self
            .parser
            .sink
            .waiting_static()
            .and_then(VecDeque::pop_front))
    }

    /// The next event, or `None` after the stream's end. After an error there are no more.
    #[cfg(test)]
    pub(crate) fn next_event(&mut self) -> Result<Option<Event<'a>>, Error> {
        let input = self.parser.input;
        if let Some(event) = self
            .parser
            .sink
            .waiting_static()
            .and_then(VecDeque::pop_front)
        {
            return Ok(Some(event.into_event(input)));
        }
        self.fill()?;
        Ok(self
            .parser
            .sink
            .waiting_static()
            .and_then(VecDeque::pop_front)
            .map(|e| e.into_event(input)))
    }

    /// Have the next event wait, so that [`Self::front`] gives it. The stream's end is an error
    /// here: a reader asks for an event where the grammar owes it one.
    pub(crate) fn ready(&mut self) -> Result<(), Error> {
        if self
            .parser
            .sink
            .waiting_static()
            .is_some_and(|events| !events.is_empty())
        {
            return Ok(());
        }
        self.fill()?;
        if self.front().is_some() {
            return Ok(());
        }
        Err(
            Error::new(ErrorKind::Composition, "unexpected end of event stream")
                .with_span(Span::empty(self.parser.input.len())),
        )
    }

    /// Take the raw `'static` [`QueuedEvent`] that waits without converting.
    #[inline]
    pub(crate) fn pop_queued(&mut self) -> Option<QueuedEvent> {
        self.parser
            .sink
            .waiting_static()
            .and_then(VecDeque::pop_front)
    }

    /// How many events wait for the reader.
    #[cfg(test)]
    pub(crate) fn waiting(&mut self) -> usize {
        self.parser
            .sink
            .waiting_static()
            .as_deref()
            .map_or(0, VecDeque::len)
    }

    /// The `'static` queued event that waits, which stays where it is.
    #[inline]
    pub(crate) fn front(&self) -> Option<&QueuedEvent> {
        match &self.parser.sink {
            EventSink::StaticQueue { events, .. } => events.front(),
            _ => None,
        }
    }

    /// How many entries the root holds, once its start event waits or has been taken. Only a
    /// flow collection's are counted before they are read.
    pub(crate) fn root_entries(&self) -> Option<usize> {
        match &self.parser.sink {
            EventSink::StaticQueue { root_entries, .. } => *root_entries,
            _ => None,
        }
    }

    /// Count the entries of the collection that just opened if its matching end event is already
    /// among the events waiting in the lookahead queue. Returns `None` if the collection extends
    /// past the waiting events or if an unexpected event appears.
    pub(crate) fn queued_entries(&self, sequence: bool) -> Option<usize> {
        let EventSink::StaticQueue { events, .. } = &self.parser.sink else {
            return None;
        };
        let mut depth = 0_usize;
        let mut nodes = 0_usize;
        for event in events {
            match event {
                QueuedEvent::SequenceStart { .. } | QueuedEvent::MappingStart { .. } => {
                    if depth == 0 {
                        nodes += 1;
                    }
                    depth += 1;
                }
                QueuedEvent::SequenceEnd { .. } => {
                    if depth == 0 {
                        return sequence.then_some(nodes);
                    }
                    depth -= 1;
                }
                QueuedEvent::MappingEnd { .. } => {
                    if depth == 0 {
                        return (!sequence && nodes.is_multiple_of(2)).then_some(nodes / 2);
                    }
                    depth -= 1;
                }
                QueuedEvent::Scalar { .. } | QueuedEvent::Alias { .. } => {
                    if depth == 0 {
                        nodes += 1;
                    }
                }
                _ => return None,
            }
        }
        None
    }

    /// Free the tokens and what was read with them, once every event has been taken. A reader
    /// that has built its document as a tree reads the tree without them, as a reader does that
    /// builds the tree before anything else.
    pub(crate) fn release(&mut self) {
        if matches!(self.state, State::Finished) && self.front().is_none() {
            self.clear_parser_storage();
        }
    }

    /// Free the token window and parser tables as soon as the parser has finished emitting all
    /// events for a large single-line flow input, even while the last few events still wait in
    /// the lookahead queue.
    pub(crate) fn release_tokens(&mut self) {
        if matches!(self.state, State::Finished) && self.parser.window.tokens_len() >= 1_024 {
            self.clear_parser_storage();
        }
    }

    fn clear_parser_storage(&mut self) {
        self.parser.window.release();
        self.stack = Vec::new();
    }

    /// Whether building the whole document as a tree (and freeing a large single-line flow token
    /// buffer before the target reads) avoids holding tokens, whole-node trees, and the target's
    /// value at once.
    pub(crate) fn should_build_whole(&self, drop_global_tags: bool) -> bool {
        self.parser.window.should_build_whole(drop_global_tags)
    }

    /// How many tokens are currently held in the window.
    #[cfg(test)]
    pub(crate) fn tokens(&self) -> usize {
        self.parser.window.tokens_len()
    }

    /// Current allocated capacity of the window's token buffer.
    #[cfg(test)]
    pub(crate) fn tokens_cap(&self) -> usize {
        self.parser.window.tokens_cap()
    }

    /// Peak tokens ever held simultaneously in the window.
    #[cfg(test)]
    pub(crate) fn peak_tokens(&self) -> usize {
        self.parser.window.peak_tokens_len()
    }

    /// Peak token buffer capacity ever allocated by the window.
    #[cfg(test)]
    pub(crate) fn peak_tokens_cap(&self) -> usize {
        self.parser.window.peak_tokens_cap()
    }

    /// Run the grammar until enough events wait or the stream is over.
    fn fill(&mut self) -> Result<(), Error> {
        self.fill_inner().map_err(|error| *error)
    }

    fn fill_inner(&mut self) -> Result<(), Box<Error>> {
        while !self.parser.sink.is_full() {
            let state = std::mem::replace(&mut self.state, State::Finished);
            self.state = match state {
                State::Start => {
                    let span = self.parser.window.ensure_started(&self.parser.limits)?;
                    self.parser.emit(Event::StreamStart { span })?;
                    State::Between(0)
                }
                State::Between(index) => {
                    if self.parser.ensure_line(index)? {
                        self.open(index, false)?
                    } else {
                        self.parser.window.take_scanner_error()?;
                        self.end()?
                    }
                }
                State::Directives(index) => self.open(index, true)?,
                State::Root { block } => self.root(block)?,
                State::Finished => return Ok(()),
            };
        }
        Ok(())
    }

    fn open(&mut self, index: usize, directives: bool) -> Result<State, Box<Error>> {
        Ok(match self.parser.open_document(index, directives)? {
            Opening::End => self.end()?,
            Opening::NoDocument(next) => State::Between(next),
            Opening::Directives(next) => State::Directives(next),
            Opening::Root {
                call: Some(call),
                block,
                ..
            } => {
                self.step = Some(Step::Call(call));
                State::Root { block }
            }
            Opening::Root {
                call: None,
                index,
                block,
            } => State::Between(self.parser.close_document(index, block)?),
        })
    }

    fn root(&mut self, block: bool) -> Result<State, Box<Error>> {
        let Some(mut step) = self.step.take() else {
            return Ok(State::Finished);
        };
        loop {
            step = match step {
                Step::Call(call) => self.parser.call(call, &mut self.stack)?,
                Step::Return(value) => match self.stack.pop() {
                    Some(cont) => self.parser.resume(cont, value, &mut self.stack)?,
                    None => {
                        return Ok(State::Between(self.parser.close_document(value, block)?));
                    }
                },
            };
            if self.parser.sink.is_full() {
                self.step = Some(step);
                return Ok(State::Root { block });
            }
        }
    }

    fn end(&mut self) -> Result<State, Box<Error>> {
        let span = self
            .parser
            .window
            .stream_end()
            .unwrap_or_else(|| Span::empty(self.parser.input.len()));
        self.parser.emit(Event::StreamEnd { span })?;
        Ok(State::Finished)
    }
}
