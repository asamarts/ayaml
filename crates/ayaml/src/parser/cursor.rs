//! Streaming pull cursor over parser events.
//!
//! Backed directly by [`Parser`] and its bounded [`super::LineWindow`], so `parse_events`,
//! `SpannedTree::parse`, `Deserializer`, and `MergedEventStream` stream events incrementally
//! without buffering a whole document's tokens or events upfront.

use std::collections::VecDeque;
use std::fmt;

use super::{Cont, EventSink, Opening, Parser, Step};
use crate::error::Error;
use crate::event::Event;
use crate::limits::Limits;
use crate::scanner::Scanner;
use crate::span::Span;

/// Where the streaming cursor stands between yielded events.
#[derive(Clone, Copy, Debug)]
enum State {
    /// Before `StreamStart` has been emitted.
    Start,
    /// Between documents, at logical line `index`.
    Between(usize),
    /// After some of a document's directives, at logical line `index`.
    Directives(usize),
    /// Inside a document's root, paused between grammar steps.
    Root { block: bool },
    /// After `StreamEnd` or after an error.
    Finished,
}

/// A streaming pull cursor used by the public event traversal, `SpannedTree`, `Deserializer`,
/// and merge-key expansion.
pub(crate) struct EventCursor<'a> {
    parser: Parser<'a>,
    state: State,
    step: Option<Step<'a>>,
    stack: Vec<Cont>,
}

impl fmt::Debug for EventCursor<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventCursor")
            .field("state", &self.state)
            .field("documents", &self.parser.documents)
            .field("emitted", &self.parser.emitted)
            .field("compat", &self.parser.compat)
            .field("single_document", &self.parser.single_document)
            .finish_non_exhaustive()
    }
}

impl<'a> EventCursor<'a> {
    pub(crate) fn new(input: &'a str, limits: &Limits) -> Result<Self, Error> {
        Self::with_compat(input, limits, false)
    }

    pub(crate) fn new_single_document(input: &'a str, limits: &Limits) -> Result<Self, Error> {
        let mut cursor = Self::with_compat(input, limits, false)?;
        cursor.parser.single_document = true;
        Ok(cursor)
    }

    pub(crate) fn with_compat(
        input: &'a str,
        limits: &Limits,
        compat: bool,
    ) -> Result<Self, Error> {
        let scanner = Scanner::with_compat(input, limits, compat).map_err(|error| *error)?;
        Ok(Self {
            parser: Parser::new(
                scanner,
                EventSink::Queue {
                    events: VecDeque::new(),
                    root_entries: None,
                    room: EventSink::CURSOR_QUEUE_ROOM,
                    want_root_entries: false,
                },
                limits,
                input,
                compat,
                false,
                false,
            ),
            state: State::Start,
            step: None,
            stack: Vec::new(),
        })
    }

    #[cfg(test)]
    pub(super) fn queue_len_and_cap(&self) -> (usize, usize) {
        match &self.parser.sink {
            EventSink::Queue { events, .. } => (events.len(), events.capacity()),
            _ => (0, 0),
        }
    }

    fn fail(&mut self, error: Error) -> Error {
        self.state = State::Finished;
        self.step = None;
        self.stack.clear();
        if let Some(queue) = self.parser.sink.waiting() {
            queue.clear();
        }
        self.parser.window.release();
        error.with_input_location(self.parser.input, self.parser.compat)
    }

    /// Consume the scanner's `StreamStart` token (preserving any leading BOM offset) once.
    pub(crate) fn take_stream_start_span(&mut self) -> Result<Span, Error> {
        if !matches!(self.state, State::Start) {
            return Ok(Span::empty(0));
        }
        let result = (|| -> Result<Span, Box<Error>> {
            let span = self.parser.window.ensure_started(&self.parser.limits)?;
            self.parser.count_event(span)?;
            self.state = State::Between(0);
            Ok(span)
        })();
        result.map_err(|error| self.fail(*error))
    }

    /// Collect the next document's events (`Directive`* + `DocumentStart` .. `DocumentEnd`),
    /// streaming through the bounded line window without buffering whole-document tokens.
    pub(crate) fn next_document(&mut self) -> Result<Option<Vec<Event<'a>>>, Error> {
        if matches!(self.state, State::Start) {
            let _ = self.take_stream_start_span()?;
        }
        let mut events = Vec::new();
        let mut saw_document = false;
        loop {
            let Some(event) = self.next_event()? else {
                return Ok(saw_document.then_some(events));
            };
            match event {
                Event::StreamStart { .. } => {}
                Event::StreamEnd { .. } => {
                    return Ok(saw_document.then_some(events));
                }
                Event::DocumentStart { .. } => {
                    saw_document = true;
                    events.push(event);
                }
                Event::DocumentEnd { .. } => {
                    events.push(event);
                    return Ok(Some(events));
                }
                other => events.push(other),
            }
        }
    }

    #[inline]
    pub(crate) fn next_event(&mut self) -> Result<Option<Event<'a>>, Error> {
        if let Some(event) = self.parser.sink.waiting().and_then(VecDeque::pop_front) {
            return Ok(Some(event));
        }
        self.next_event_slow()
    }

    fn next_event_slow(&mut self) -> Result<Option<Event<'a>>, Error> {
        if matches!(self.state, State::Start) {
            // Defer syntax/content scanning until after `StreamStart` is yielded, matching
            // `EventStream`'s documented lazy contract.
            let result = (|| -> Result<Event<'a>, Box<Error>> {
                let span = self.parser.window.ensure_started(&self.parser.limits)?;
                self.parser.count_event(span)?;
                self.state = State::Between(0);
                Ok(Event::StreamStart { span })
            })();
            return match result {
                Ok(event) => Ok(Some(event)),
                Err(error) => Err(self.fail(*error)),
            };
        }
        if let Err(error) = self.fill_inner() {
            return Err(self.fail(*error));
        }
        Ok(self.parser.sink.waiting().and_then(VecDeque::pop_front))
    }

    fn fill_inner(&mut self) -> Result<(), Box<Error>> {
        if let Some(queue) = self.parser.sink.waiting()
            && queue.capacity() == 0
        {
            let sample = &self.parser.input.as_bytes()[..self.parser.input.len().min(4096)];
            let cap = if self.parser.input.len() <= 64 {
                8
            } else if self.parser.input.len() <= 1024 {
                64
            } else if crate::position::lines_at_most(sample) <= 4
                && !sample
                    .iter()
                    .find(|byte| !byte.is_ascii_whitespace())
                    .is_some_and(|byte| matches!(byte, b'[' | b'{'))
            {
                16
            } else {
                // `VecDeque::reserve_exact(255)` allocates one 256-slot power-of-two ring buffer
                // without stepping up to 512 slots on a multi-event grammar step at `room = 250`.
                255
            };
            queue.reserve_exact(cap);
        }
        while !self.parser.sink.is_full() {
            let state = std::mem::replace(&mut self.state, State::Finished);
            self.state = match state {
                State::Start => unreachable!("Start handled in next_event_slow"),
                State::Between(index) => {
                    if self.parser.ensure_line(index)? {
                        let next = self.open(index, false)?;
                        // Stop at a completed empty document so callers see document 1 before
                        // scanning document 2.
                        if matches!(next, State::Between(_))
                            && self
                                .parser
                                .sink
                                .waiting()
                                .is_some_and(|events| !events.is_empty())
                        {
                            self.state = next;
                            return Ok(());
                        }
                        next
                    } else {
                        self.parser.window.take_scanner_error()?;
                        self.end()?
                    }
                }
                State::Directives(index) => {
                    let next = self.open(index, true)?;
                    if matches!(next, State::Between(_)) {
                        self.state = next;
                        return Ok(());
                    }
                    next
                }
                State::Root { block } => {
                    let next = self.root(block)?;
                    if matches!(next, State::Between(_)) {
                        self.state = next;
                        return Ok(());
                    }
                    next
                }
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
        self.parser.window.release();
        Ok(State::Finished)
    }
}
