//! Document-bounded pull cursor over parser events.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;

use super::{Line, Lines, drop_truncated_block_line, parse_lines};
use crate::error::Error;
use crate::event::{Event, ScalarStyle};
use crate::limits::Limits;
use crate::position::LineStarts;
use crate::scanner::{Scanner, Token, TokenKind};
use crate::span::Span;

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
    positions: LineStarts,
    pending_line: Option<Vec<Token<'a>>>,
    pub(super) queue: VecDeque<Event<'a>>,
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
    /// Reject a second document at its start (mirrors single-document eager readers).
    single_document: bool,
    /// Deferred scanner failure encountered after scanning a flow opener.
    ///
    /// Scanner errors hold only owned metadata (no dynamic `source`), so wrapping in
    /// `AssertUnwindSafe` preserves `EventStream` and `Deserializer`'s `UnwindSafe` auto-trait.
    scanner_error: Option<AssertUnwindSafe<Error>>,
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

    pub(crate) fn new_single_document(input: &'a str, limits: &Limits) -> Result<Self, Error> {
        let mut cursor = Self::with_compat(input, limits, false)?;
        cursor.single_document = true;
        Ok(cursor)
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
            single_document: false,
            scanner_error: None,
        })
    }

    /// Consume the scanner's `StreamStart` token (preserving any leading BOM offset) once.
    pub(crate) fn take_stream_start_span(&mut self) -> Result<Span, Error> {
        if self.stream_started {
            return Ok(Span::empty(0));
        }
        self.stream_started = true;
        let result = (|| {
            let span = match self.scanner.next_token()? {
                Some(token) if matches!(token.kind, TokenKind::StreamStart) => token.span,
                _ => Span::empty(0),
            };
            self.events += 1;
            self.limits
                .check_events(self.events)
                .map_err(|error| self.limit_at(span, error))?;
            Ok(span)
        })()
        .map_err(|error: Error| error.with_location_from(&self.positions, self.input));
        if result.is_err() {
            self.stream_ended = true;
            self.queue.clear();
        }
        result
    }

    /// Take the next document's events without copying through `self.queue` when possible.
    pub(crate) fn next_document(&mut self) -> Result<Option<Vec<Event<'a>>>, Error> {
        let result = self
            .next_document_inner()
            .map_err(|error| error.with_location_from(&self.positions, self.input));
        if result.is_err() {
            self.stream_ended = true;
            self.queue.clear();
        }
        result
    }

    fn next_document_inner(&mut self) -> Result<Option<Vec<Event<'a>>>, Error> {
        if !self.stream_started {
            self.stream_started = true;
            let span = match self.scanner.next_token()? {
                Some(token) if matches!(token.kind, TokenKind::StreamStart) => token.span,
                _ => Span::empty(0),
            };
            self.events += 1;
            self.limits
                .check_events(self.events)
                .map_err(|error| self.limit_at(span, error))?;
        }
        loop {
            if !self.queue.is_empty() {
                let mut events = Vec::new();
                let mut saw_document = false;
                while let Some(event) = self.queue.pop_front() {
                    if matches!(event, Event::StreamStart { .. }) {
                        continue;
                    }
                    if matches!(event, Event::StreamEnd { .. }) {
                        self.stream_ended = true;
                        break;
                    }
                    if matches!(event, Event::DocumentStart { .. }) {
                        saw_document = true;
                    }
                    let end = matches!(event, Event::DocumentEnd { .. });
                    events.push(event);
                    if end {
                        break;
                    }
                }
                if saw_document {
                    return Ok(Some(events));
                }
                continue;
            }
            if self.stream_ended {
                return Ok(None);
            }
            if let Some(events) = self.load_chunk()? {
                let doc_starts = events
                    .iter()
                    .filter(|event| matches!(event, Event::DocumentStart { .. }))
                    .count();
                if doc_starts == 1 && matches!(events.last(), Some(Event::DocumentEnd { .. })) {
                    return Ok(Some(events));
                }
                self.queue = VecDeque::from(events);
            } else {
                self.stream_ended = true;
                let span = self
                    .stream_end
                    .unwrap_or_else(|| Span::empty(self.input.len()));
                self.events += 1;
                self.limits
                    .check_events(self.events)
                    .map_err(|error| self.limit_at(span, error))?;
                return Ok(None);
            }
        }
    }

    #[inline]
    pub(crate) fn next_event(&mut self) -> Result<Option<Event<'a>>, Error> {
        if let Some(event) = self.queue.pop_front() {
            return Ok(Some(event));
        }
        self.next_event_slow()
    }

    fn next_event_slow(&mut self) -> Result<Option<Event<'a>>, Error> {
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
            if let Some(mut events) = self.load_chunk()? {
                // The queue is empty here, so it takes the document's events as they stand,
                // where extending it would hold them twice while they are copied. The room
                // that the parser reserved and didn't fill goes back first: a reader builds
                // its value while the events wait here.
                events.shrink_to_fit();
                self.queue = VecDeque::from(events);
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
            let start = tokens.len();
            if let Some(pending) = self.pending_line.take() {
                tokens.extend(pending);
            } else if !self.read_line_into(&mut tokens)? {
                break;
            }
            let first_kind = &tokens[start].kind;
            let is_start = matches!(first_kind, TokenKind::DocumentStart { .. });
            let is_end = matches!(first_kind, TokenKind::DocumentEnd);
            if is_start && seen_document {
                self.pending_line = Some(tokens.split_off(start));
                break;
            }
            seen_document |= is_start || !matches!(first_kind, TokenKind::Directive(_));
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
            if let Some(error) = self.scanner_error.take() {
                return Err(error.0);
            }
            return Ok(None);
        }
        // The document ends where an eager parse would go on: at the next document's first
        // token, or at the stream end. When a deferred scanner error halted scanning before
        // `StreamEnd`, use `input.len()` to match eager `read_lines`. After a `...` line neither
        // is read yet, and nothing past that line is placed.
        let end = self
            .pending_line
            .as_ref()
            .and_then(|line| line.first())
            .map(|token| token.span.start())
            .or_else(|| self.stream_end.map(Span::start))
            .or_else(|| {
                if self.scanner_error.is_some() {
                    Some(self.input.len())
                } else {
                    tokens.last().map(|token| token.span.end())
                }
            })
            .unwrap_or(self.input.len());
        let (events, counted, documents) = parse_lines(
            Lines {
                lines,
                tokens,
                stream_start: None,
                stream_end: None,
                positions: self.positions.clone(),
                scanner_error: if self.pending_line.is_none() {
                    self.scanner_error.take().map(|error| error.0)
                } else {
                    None
                },
                #[cfg(test)]
                growths: 0,
            },
            &self.limits,
            self.input,
            self.events,
            self.documents,
            self.single_document,
            end,
            self.compat,
        )?;
        self.events = counted;
        self.documents = documents;
        Ok(Some(events))
    }

    /// Read the next non-empty logical line directly into `out`, returning `true` if any tokens
    /// were appended. Avoids allocating a temporary `Vec<Token>` per line.
    fn read_line_into(&mut self, out: &mut Vec<Token<'a>>) -> Result<bool, Error> {
        if self.eof {
            return Ok(false);
        }
        let start = out.len();
        loop {
            match self.scanner.next_token() {
                Ok(None) => {
                    self.eof = true;
                    return Ok(out.len() > start);
                }
                Ok(Some(token)) if matches!(token.kind, TokenKind::StreamStart) => {}
                Ok(Some(token)) if matches!(token.kind, TokenKind::StreamEnd) => {
                    self.eof = true;
                    self.stream_end = Some(token.span);
                    return Ok(out.len() > start);
                }
                Ok(Some(token)) if matches!(token.kind, TokenKind::LineBreak) => {
                    if out.len() > start {
                        return Ok(true);
                    }
                }
                Ok(Some(token)) => {
                    let block_scalar = token.kind.scalar_style().is_some_and(|style| {
                        matches!(style, ScalarStyle::Literal | ScalarStyle::Folded)
                    });
                    out.push(token);
                    if block_scalar {
                        return Ok(true);
                    }
                }
                Err(error) => {
                    if !self.scanner.saw_flow_start() {
                        return Err(error);
                    }
                    if self.scanner.flow_level() == 0 {
                        drop_truncated_block_line(out, start);
                    }
                    self.eof = true;
                    self.scanner_error = Some(AssertUnwindSafe(error));
                    return Ok(out.len() > start);
                }
            }
        }
    }
}
