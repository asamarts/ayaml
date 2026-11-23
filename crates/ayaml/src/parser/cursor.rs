//! Document-bounded pull cursor over parser events.

use std::collections::VecDeque;

use super::{Line, Lines, parse_lines};
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
