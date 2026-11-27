//! One document's events, on demand.
//!
//! The grammar already runs on an explicit stack (see `Parser::run`), so it can stop between
//! two steps and go on later. A reader that builds its value from the events takes each one as
//! it needs it, and no vector of events, no full-document token buffer, and no tree made from
//! them stands between the scanner and the value. A step that emits a run of events, of scalar
//! entries or of directives, stops within the run, so the events that wait stay a few however
//! long the run is.

use std::collections::VecDeque;

use super::{Cont, EventSink, Opening, Parser, Step};
use crate::error::{Error, ErrorKind};
use crate::event::Event;
use crate::limits::Limits;
use crate::scanner::Scanner;
use crate::span::Span;

/// The events of a single-document input, read as they are asked for. A second document's start
/// is its last event before the stream's end, as `parse_document_with_consumer` gives it.
pub(crate) struct DocumentEvents<'a> {
    parser: Parser<'a>,
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
        Ok(Self {
            parser: Parser::new(
                scanner,
                EventSink::Queue {
                    events: VecDeque::with_capacity(2 * EventSink::QUEUE_ROOM),
                    root_entries: None,
                    room: EventSink::QUEUE_ROOM,
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

    /// The next event, or `None` after the stream's end. After an error there are no more.
    pub(crate) fn next_event(&mut self) -> Result<Option<Event<'a>>, Error> {
        if let Some(event) = self.parser.sink.waiting().and_then(VecDeque::pop_front) {
            return Ok(Some(event));
        }
        self.fill()?;
        Ok(self.parser.sink.waiting().and_then(VecDeque::pop_front))
    }

    /// Have the next event wait, so that [`Self::front`] gives it. The stream's end is an error
    /// here: a reader asks for an event where the grammar owes it one.
    pub(crate) fn ready(&mut self) -> Result<(), Error> {
        if self
            .parser
            .sink
            .waiting()
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

    /// Take the event that waits, if one does. Nothing is read to find one.
    pub(crate) fn pop(&mut self) -> Option<Event<'a>> {
        self.parser.sink.waiting().and_then(VecDeque::pop_front)
    }

    /// How many events wait for the reader.
    #[cfg(test)]
    pub(crate) fn waiting(&mut self) -> usize {
        self.parser.sink.waiting().map_or(0, |events| events.len())
    }

    /// The event that waits, which stays where it is.
    pub(crate) fn front(&self) -> Option<&Event<'a>> {
        match &self.parser.sink {
            EventSink::Queue { events, .. } => events.front(),
            _ => None,
        }
    }

    /// How many entries the root holds, once its start event waits or has been taken. Only a
    /// flow collection's are counted before they are read.
    pub(crate) fn root_entries(&self) -> Option<usize> {
        match &self.parser.sink {
            EventSink::Queue { root_entries, .. } => *root_entries,
            _ => None,
        }
    }

    /// Count the entries of the collection that just opened if its matching end event is already
    /// among the events waiting in the lookahead queue. Returns `None` if the collection extends
    /// past the waiting events or if an unexpected event appears.
    pub(crate) fn queued_entries(&self, sequence: bool) -> Option<usize> {
        let EventSink::Queue { events, .. } = &self.parser.sink else {
            return None;
        };
        let mut depth = 0_usize;
        let mut nodes = 0_usize;
        for event in events {
            match event {
                Event::SequenceStart { .. } | Event::MappingStart { .. } => {
                    if depth == 0 {
                        nodes += 1;
                    }
                    depth += 1;
                }
                Event::SequenceEnd { .. } => {
                    if depth == 0 {
                        return sequence.then_some(nodes);
                    }
                    depth -= 1;
                }
                Event::MappingEnd { .. } => {
                    if depth == 0 {
                        return (!sequence && nodes.is_multiple_of(2)).then_some(nodes / 2);
                    }
                    depth -= 1;
                }
                Event::Scalar { .. } | Event::Alias { .. } => {
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
    ///
    /// It reads a few events ahead of its reader, so that the grammar's state is put away and
    /// taken up once for several events. An error in what is read ahead is the document's, and
    /// the document's error is the one a reader reports, wherever its target stands.
    fn fill(&mut self) -> Result<(), Error> {
        self.fill_inner().map_err(|error| *error)
    }

    fn fill_inner(&mut self) -> Result<(), Box<Error>> {
        while !self.parser.sink.is_full() {
            // An error leaves the state finished, so nothing is read after it.
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
                // Past the last line too: directives with no document after them are an error.
                State::Directives(index) => self.open(index, true)?,
                State::Root { block } => self.root(block)?,
                State::Finished => return Ok(()),
            };
        }
        Ok(())
    }

    /// Read a document's directives and its start from line `index`, as `Parser::parse_stream`
    /// does.
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

    /// Run a root's steps, as `Parser::run` does, until enough events wait or the root ends.
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
