//! Typed decoding from the parser's events, with no tree of the document.
//!
//! [`read`] gives the target a deserializer over one document's events. A scalar is built as
//! one node, read as the tree reader reads it, and dropped. A collection is read entry by entry
//! as its events arrive, so its entries are never all held at once. Only a node that is read as
//! a whole, or that a later read may need again, is built as a tree: a node with an anchor, what
//! an alias refers to, a collection that keeps its tag, a mapping key, and what an enum reads.
//!
//! What the tree's builder checks is checked here as the events pass: the nesting depth,
//! duplicate keys, anchors and the alias budget, and that there is one document.
//!
//! This reader's error is not always the one to report. The tree reader says what is wrong with
//! the document before anything about the target, wherever each stands, and this reader meets
//! them in document order. So after an error the document is built as a tree (`parse_document`),
//! and that tree's error, if it has one, is the answer. Only a document that builds reports the
//! error found here, which is then the target's. A refused input pays for the second read, and
//! its target has read what stands before the fault by then: the tree reader refuses a
//! document before its target sees any of it.
//!
//! A root that is built whole is the whole document as a tree. It is built before the target
//! reads, the stream is read to its end, and the tokens are freed, so the target's value is
//! built beside the tree alone, as the tree reader builds it.

use std::collections::HashSet;

use serde::Deserializer;
use serde::de::{Deserialize, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};

use super::{
    Builder, Node, NodeDeserializer, NodeOptions, SeenKey, SeenNodeKeys, check_consumed,
    collection_drops, inside_key, node_key_text, parse_document, plain_scalar, scalar_key_text,
    with_span,
};
use crate::error::{Error, ErrorCode, ErrorKind};
use crate::event::Event;
use crate::limits::Limits;
use crate::parser::DocumentEvents;
use crate::span::{Location, Span};

/// Read one document into what `seed` makes of it.
pub(super) fn read<'de, T: DeserializeSeed<'de>>(
    input: &'de str,
    limits: &Limits,
    options: NodeOptions,
    seed: T,
) -> Result<T::Value, Error> {
    let located = |error: Error| error.with_input_location(input, options.compat_flow);
    // An input over the limit is refused here as `parse_document` refuses it, before any work.
    limits.check_input(input.len()).map_err(|error| {
        Error::from(error)
            .with_span(Span::empty(0))
            .with_location(Location::new(1, 1))
    })?;
    // The scanner reads the whole input before the first event, here and for the tree, so its
    // error is the same one and needs no second read.
    let events = DocumentEvents::new(input, limits, options.compat_flow).map_err(located)?;
    let mut stream = Stream::new(events, input.len(), limits, options);
    let result = stream.document(|document| seed.deserialize(document));
    let fault = stream.fault.take();
    let error = match (result, fault) {
        (Ok(value), None) => return Ok(value),
        // A fault in the document stands, whatever the target made of the error it was given.
        (_, Some(fault)) => fault,
        (Err(error), None) => {
            // The target failed while reading; `read_with_target` already settled and checked the
            // rest of the root node. Validate the document/stream end on the already-scanned
            // tokens so valid YAML with a Serde mismatch skips a full second parse.
            if stream.validate_end().is_ok() && stream.fault.is_none() {
                return Err(located(error));
            }
            error
        }
    };
    // The tokens are freed before a tree is built to confirm exact document-order error precedence.
    drop(stream);
    Err(located(
        parse_document(input, limits, options)
            .err()
            .unwrap_or(error),
    ))
}

/// A document's events, and what reading them has to keep.
///
/// The builder's root is the one node that is built and not yet read. It is empty between
/// reads, but for a stream with no document, whose null is put there before the target reads.
pub(super) struct Stream<'de> {
    events: DocumentEvents<'de>,
    /// Builds each node that is kept whole, and holds the document's anchors and alias budget.
    builder: Builder<'de>,
    /// The collections that are open here and read entry by entry, the innermost last. The
    /// builder doesn't see them, and counts them as its base depth.
    open: Vec<Open<'de>>,
    /// What the event that waits is, once a read has looked at it.
    ahead: Ahead,
    /// Whether the node that a seed was given is still unread. A seed may read nothing.
    due: bool,
    /// Whether the stream has been read past the document's end, which a root that is built
    /// whole is before its target reads it.
    ended: bool,
    /// The first fault found in the document itself, as opposed to a value that doesn't suit
    /// its target. Nothing is read after it, and the read fails whatever the target returns.
    fault: Option<Error>,
}

/// What a read takes from the stream.
enum Taken {
    /// A node built whole, which waits as the builder's root.
    Built,
    /// A sequence, open and at its first item.
    Sequence(Span),
    /// A mapping, open and at its first key.
    Mapping(Span),
}

/// What the next event starts or ends, found once for the reads that ask.
#[derive(Clone, Copy)]
enum Ahead {
    /// The next event hasn't been looked at.
    Unknown,
    /// A node that is built whole: a scalar, an alias, or a collection with an anchor or with
    /// a tag that it keeps.
    Whole(Span),
    /// A sequence that is read entry by entry: one with no anchor, which no alias reads again,
    /// and with no tag that it keeps, which would make it one tagged value.
    Sequence(Span),
    /// A mapping that is read entry by entry.
    Mapping(Span),
    /// The end of a sequence or of a mapping.
    End { sequence: bool, span: Span },
}

impl Ahead {
    fn span(self) -> Span {
        match self {
            Self::Unknown => Span::empty(0),
            Self::Whole(span)
            | Self::Sequence(span)
            | Self::Mapping(span)
            | Self::End { span, .. } => span,
        }
    }
}

/// A collection that is read entry by entry.
enum Open<'de> {
    Sequence {
        /// The items that a visitor has asked for.
        read: usize,
        /// Total items known when the sequence opened, from root flow counting or lookahead.
        known: Option<usize>,
    },
    Mapping {
        /// The scalar keys of the entries read so far, where a duplicate is refused.
        seen: HashSet<SeenKey<'de>>,
        /// Non-scalar keys of the entries read so far, moved in by cached node hash without cloning.
        seen_nodes: Option<Box<SeenNodeKeys<'de>>>,
        /// The key whose value is still to be read.
        key: Option<Node<'de>>,
        /// The entries whose keys a visitor has asked for.
        read: usize,
        /// Total entries known when the mapping opened, from root flow counting or lookahead.
        known: Option<usize>,
    },
}

impl<'de> Stream<'de> {
    fn new(
        events: DocumentEvents<'de>,
        input_len: usize,
        limits: &Limits,
        options: NodeOptions,
    ) -> Self {
        Self {
            events,
            builder: Builder::streaming(input_len, limits, options),
            open: Vec::new(),
            ahead: Ahead::Unknown,
            due: false,
            ended: false,
            fault: None,
        }
    }

    fn document<R>(
        &mut self,
        target: impl for<'s> FnOnce(StreamDeserializer<'de, 's>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        loop {
            let next = match self.events.next_event() {
                Ok(event) => event,
                Err(error) => return Err(self.faulted(error)),
            };
            match next {
                Some(Event::StreamStart { .. } | Event::Directive { .. }) => {}
                Some(Event::DocumentStart { .. }) => break,
                // A stream with no document reads as an empty one, which the tree gives as a
                // null that the source doesn't write.
                Some(Event::StreamEnd { .. }) | None => {
                    let end = Span::empty(self.builder.input_len);
                    self.builder.root = Some(Node::null(end));
                    self.ahead = Ahead::Whole(end);
                    self.ended = true;
                    return target(StreamDeserializer { stream: self });
                }
                Some(event) => return Err(self.faulted(misplaced(&event))),
            }
        }
        let ahead = self.ahead()?;
        if matches!(ahead, Ahead::Whole(_))
            || self
                .events
                .should_build_whole(self.builder.options.drop_global_tags)
        {
            let span = ahead.span();
            self.build_root()?;
            self.ahead = Ahead::Whole(span);
        }
        let value = self.read_with_target(target)?;
        if let Err(error) = self.end() {
            return Err(self.faulted(error));
        }
        Ok(value)
    }

    /// Finish checking the document and trailing stream after a target-side error, using the
    /// already-scanned tokens so valid YAML skips a second parse.
    fn validate_end(&mut self) -> Result<(), Error> {
        if self.ended {
            return Ok(());
        }
        if self.due {
            self.skip()?;
        }
        if !self.open.is_empty() {
            self.close_to(0)?;
        }
        self.end()
    }

    /// Build the root as a tree, read the stream to its end, and free the tokens. The root is
    /// then all that is left of the document, and its target reads it beside nothing else.
    #[cold]
    fn build_root(&mut self) -> Result<(), Error> {
        if !self.ended
            && let Some(entries) = self.events.root_entries()
        {
            self.builder.reserve_collection(entries);
        }
        self.build()?;
        // The target may be reading already, as an enum at the root is, and may make something
        // else of the error it is given. What follows the root is a fault of the document's.
        if let Err(error) = self.end() {
            return Err(self.faulted(error));
        }
        self.events.release();
        Ok(())
    }

    /// Read from the root's end to the stream's, where a second document is refused.
    fn end(&mut self) -> Result<(), Error> {
        if std::mem::replace(&mut self.ended, true) {
            return Ok(());
        }
        match self.pull()? {
            Event::DocumentEnd { .. } => {}
            event => return Err(misplaced(&event)),
        }
        loop {
            match self.events.next_event()? {
                Some(Event::StreamEnd { .. }) | None => return Ok(()),
                Some(Event::Directive { .. }) => {}
                Some(Event::DocumentStart { span, .. }) => {
                    return Err(Error::coded(
                        ErrorKind::Parser,
                        ErrorCode::TrailingDocuments,
                        "expected a single YAML document, but found trailing documents",
                    )
                    .with_span(span));
                }
                Some(event) => return Err(misplaced(&event)),
            }
        }
    }

    /// Give the next node to `target`, and leave the stream after that node whatever the target
    /// did with it.
    ///
    /// A target that reads nothing leaves a node that still has to be checked, as the tree
    /// reader has checked what its target ignores. A target that fails leaves a node partly
    /// read. A visitor may take that error and go on to the next entry, which works with a tree,
    /// so the rest of the node is passed over here.
    #[inline]
    fn read_with_target<R>(
        &mut self,
        target: impl for<'s> FnOnce(StreamDeserializer<'de, 's>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let mark = self.open.len();
        self.due = true;
        let result = target(StreamDeserializer { stream: self });
        if self.due || self.open.len() > mark {
            self.settle(mark, result.is_ok())?;
        }
        result
    }

    /// Pass over what a target left of its node: all of it, or the collections it opened, down
    /// to the `mark` that were open before. A fault found on the way fails a read that
    /// succeeded, and is recorded for one that failed.
    #[cold]
    fn settle(&mut self, mark: usize, succeeded: bool) -> Result<(), Error> {
        if self.fault.is_some() {
            return Ok(());
        }
        let passed = if self.due {
            self.skip()
        } else {
            self.close_to(mark)
        };
        if succeeded { passed } else { Ok(()) }
    }

    #[inline]
    fn read_with<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<T::Value, Error> {
        self.read_with_target(|node| seed.deserialize(node))
    }

    /// Read the next node and keep nothing of it. Every check on it is still made.
    fn skip(&mut self) -> Result<(), Error> {
        IgnoredAny::deserialize(StreamDeserializer { stream: self }).map(drop)
    }

    /// Record a fault in the document, and give the error that stands for it. The fault is what
    /// `read` reports; the error only ends the target's reading.
    #[cold]
    fn faulted(&mut self, error: Error) -> Error {
        self.fault.get_or_insert(error);
        self.ahead = Ahead::Unknown;
        unreadable()
    }

    /// Have the next event wait. After a fault there is none.
    fn ready(&mut self) -> Result<(), Error> {
        if self.fault.is_some() {
            return Err(unreadable());
        }
        match self.events.ready() {
            Ok(()) => Ok(()),
            Err(error) => Err(self.faulted(error)),
        }
    }

    /// Take the next event.
    fn pull(&mut self) -> Result<Event<'de>, Error> {
        self.ready()?;
        self.ahead = Ahead::Unknown;
        self.events.pop().ok_or_else(unreadable)
    }

    /// What the next event starts or ends. The event stays where it is.
    #[inline]
    fn ahead(&mut self) -> Result<Ahead, Error> {
        match self.ahead {
            Ahead::Unknown => self.look(),
            ahead => Ok(ahead),
        }
    }

    fn look(&mut self) -> Result<Ahead, Error> {
        if self.fault.is_some() || self.events.front().is_none() {
            self.ready()?;
        }
        let ahead = match self.events.front() {
            Some(Event::SequenceStart {
                tag,
                anchor: None,
                span,
                ..
            }) if tag
                .as_ref()
                .is_none_or(|tag| collection_drops(tag, self.builder.options.drop_global_tags)) =>
            {
                Ahead::Sequence(*span)
            }
            Some(Event::MappingStart {
                tag,
                anchor: None,
                span,
                ..
            }) if tag
                .as_ref()
                .is_none_or(|tag| collection_drops(tag, self.builder.options.drop_global_tags)) =>
            {
                Ahead::Mapping(*span)
            }
            Some(Event::SequenceEnd { span }) => Ahead::End {
                sequence: true,
                span: *span,
            },
            Some(Event::MappingEnd { span }) => Ahead::End {
                sequence: false,
                span: *span,
            },
            Some(event) => Ahead::Whole(event.span()),
            None => return Err(unreadable()),
        };
        self.ahead = ahead;
        Ok(ahead)
    }

    /// Take the next node: a collection that is read entry by entry is opened, and any other
    /// node is built.
    #[inline]
    fn take(&mut self) -> Result<Taken, Error> {
        match self.ahead()? {
            Ahead::Sequence(span) => self.open(span, true),
            Ahead::Mapping(span) => self.open(span, false),
            // An end where a node has to stand is the builder's to refuse.
            Ahead::Whole(_) | Ahead::End { .. } | Ahead::Unknown => {
                self.build().map(|()| Taken::Built)
            }
        }
    }

    /// Open the collection whose start waits.
    fn open(&mut self, span: Span, sequence: bool) -> Result<Taken, Error> {
        self.due = false;
        self.pull()?;
        if let Err(error) = self.builder.limits.check_depth(self.open.len() + 1) {
            return Err(self.faulted(Error::from(error).with_span(span)));
        }
        let known = if self.open.is_empty() {
            self.events.root_entries()
        } else {
            None
        }
        .or_else(|| self.events.queued_entries(sequence));
        self.open.push(if sequence {
            Open::Sequence { read: 0, known }
        } else {
            Open::Mapping {
                seen: HashSet::new(),
                seen_nodes: None,
                key: None,
                read: 0,
                known,
            }
        });
        Ok(if sequence {
            Taken::Sequence(span)
        } else {
            Taken::Mapping(span)
        })
    }

    /// Build the next node as a tree, which then waits as the builder's root.
    fn build(&mut self) -> Result<(), Error> {
        self.due = false;
        self.ahead = Ahead::Unknown;
        if self.builder.root.is_some() {
            return Ok(());
        }
        if self.fault.is_some() || self.events.front().is_none() {
            self.ready()?;
        }
        // A scalar with no anchor is most of what is read. The builder would make this node of
        // it and no more, so it is made here, from the event where it waits.
        if let Some(Event::Scalar {
            value,
            raw,
            style,
            tag,
            anchor: None,
            span,
        }) = self.events.front()
        {
            let node = plain_scalar(
                value,
                raw,
                *style,
                tag.as_ref(),
                *span,
                self.builder.options.drop_global_tags,
            );
            self.events.discard();
            return match node {
                Ok(node) => {
                    self.builder.root = Some(node);
                    Ok(())
                }
                Err(error) => Err(self.faulted(error)),
            };
        }
        self.builder.base = self.open.len();
        loop {
            let event = self.pull()?;
            if let Err(error) = self.builder.push_node(event) {
                return Err(self.faulted(error));
            }
            if self.builder.stack.is_empty() {
                self.events.release_tokens();
                return Ok(());
            }
        }
    }

    /// Take the next node, built as a tree.
    fn whole(&mut self) -> Result<Node<'de>, Error> {
        self.build()?;
        self.builder.root.take().ok_or_else(unreadable)
    }

    /// Read the node that waits as the builder's root, and drop it.
    #[inline]
    fn read_built<R>(
        &mut self,
        read: impl FnOnce(NodeDeserializer<'de, '_>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let result = match &self.builder.root {
            Some(node) => read(NodeDeserializer::new(node)),
            None => Err(unreadable()),
        };
        self.builder.root = None;
        result
    }

    /// The span of the next node, if it is a collection that is read entry by entry.
    fn streamed(&mut self) -> Result<Option<Span>, Error> {
        Ok(match self.ahead()? {
            Ahead::Sequence(span) | Ahead::Mapping(span) => Some(span),
            _ => None,
        })
    }

    /// The span of the next node in the innermost open collection, which stays for the next
    /// read, or `None` at the collection's end, where it closes.
    #[inline]
    fn entry(&mut self) -> Result<Option<Span>, Error> {
        let within = matches!(self.open.last(), Some(Open::Sequence { .. }));
        match self.ahead()? {
            Ahead::End { sequence, .. } if sequence == within => {
                self.ahead = Ahead::Unknown;
                self.events.pop();
                self.open.pop();
                Ok(None)
            }
            ahead => Ok(Some(ahead.span())),
        }
    }

    /// Count one more entry that a visitor asked the innermost open collection for, and give
    /// its position.
    fn count(&mut self) -> usize {
        match self.open.last_mut() {
            Some(Open::Sequence { read, .. } | Open::Mapping { read, .. }) => {
                *read += 1;
                *read - 1
            }
            None => 0,
        }
    }

    /// The key of the innermost open mapping whose value is still to be read.
    fn key_slot(&mut self) -> Option<&mut Option<Node<'de>>> {
        match self.open.last_mut() {
            Some(Open::Mapping { key, .. }) => Some(key),
            _ => None,
        }
    }

    /// The next key of the innermost open mapping, or `None` at the mapping's end, where it
    /// closes. If a visitor left the value of the entry before it, that value is passed over.
    fn key(&mut self) -> Result<Option<Node<'de>>, Error> {
        if let Some(key) = self.key_slot().and_then(Option::take) {
            self.skip()?;
            self.entered(key)?;
        }
        if self.entry()?.is_none() {
            return Ok(None);
        }
        self.whole().map(Some)
    }

    /// Record a complete entry of the innermost open mapping. The tree's builder refuses a
    /// duplicate key once the entry's value is built, and this does at the same point.
    fn entered(&mut self, key: Node<'de>) -> Result<(), Error> {
        if self.builder.options.allow_duplicates || self.fault.is_some() {
            return Ok(());
        }
        let Some(Open::Mapping {
            seen, seen_nodes, ..
        }) = self.open.last_mut()
        else {
            return Ok(());
        };
        let duplicate = if let Some(text) = scalar_key_text(&key) {
            if seen.insert(text) {
                return Ok(());
            }
            key
        } else {
            match seen_nodes.get_or_insert_with(Box::default).insert(key) {
                Ok(()) => return Ok(()),
                Err(duplicate) => duplicate,
            }
        };
        Err(self.faulted(
            Error::new(
                ErrorKind::DuplicateKey,
                format!("duplicate mapping key: {}", node_key_text(&duplicate)),
            )
            .with_span(duplicate.span),
        ))
    }

    /// Read the innermost open collection to its end, and give how many entries a visitor asked
    /// it for and how many it left.
    fn finish(&mut self) -> Result<(usize, usize), Error> {
        let mut unread = 0;
        let read = match self.open.last() {
            Some(Open::Sequence { read, .. }) => {
                let read = *read;
                while self.entry()?.is_some() {
                    self.skip()?;
                    unread += 1;
                }
                read
            }
            Some(Open::Mapping { read, .. }) => {
                let read = *read;
                while let Some(key) = self.key()? {
                    if let Some(slot) = self.key_slot() {
                        *slot = Some(key);
                    }
                    unread += 1;
                }
                read
            }
            None => 0,
        };
        Ok((read, unread))
    }

    /// Close every collection that was opened since `mark` collections were open.
    fn close_to(&mut self, mark: usize) -> Result<(), Error> {
        while self.open.len() > mark {
            self.finish()?;
        }
        Ok(())
    }

    /// End the collection that a visitor has read as it liked, which was opened as number
    /// `level`. A visitor that stops early, such as a tuple's, leaves entries behind, which is
    /// an error once they are checked.
    fn close(&mut self, level: usize, kind: &str) -> Result<(), Error> {
        if self.fault.is_some() || self.open.len() <= level {
            return Ok(());
        }
        self.close_to(level + 1)?;
        let (read, unread) = self.finish()?;
        check_consumed(unread, read + unread, kind)
    }

    fn sequence<V: Visitor<'de>>(&mut self, span: Span, visitor: V) -> Result<V::Value, Error> {
        let level = self.open.len().saturating_sub(1);
        visitor
            .visit_seq(SequenceAccess {
                stream: self,
                level,
            })
            .and_then(|value| self.close(level, "sequence").map(|()| value))
            .map_err(|error| with_span(error, span))
    }

    fn mapping<V: Visitor<'de>>(&mut self, span: Span, visitor: V) -> Result<V::Value, Error> {
        let level = self.open.len().saturating_sub(1);
        visitor
            .visit_map(MappingAccess {
                stream: self,
                level,
            })
            .and_then(|value| self.close(level, "map").map(|()| value))
            .map_err(|error| with_span(error, span))
    }

    /// Whether the collection that was opened as number `level` still has entries to give. After
    /// a fault it has none, so a visitor that reads on past an error comes to an end.
    fn gives(&self, level: usize) -> bool {
        self.fault.is_none() && self.open.len() > level
    }

    /// How many entries the collection that was opened as number `level` has left, where that is
    /// known before they are read: for a root flow collection (counted by the parser) or any
    /// collection whose end is already among the events waiting in the lookahead queue.
    fn left(&self, level: usize) -> Option<usize> {
        if self.fault.is_some() {
            return None;
        }
        match self.open.get(level) {
            None => Some(0),
            Some(Open::Sequence { read, known } | Open::Mapping { read, known, .. }) => {
                known.map(|total| total.saturating_sub(*read))
            }
        }
    }
}

/// An event where a node or a document's boundary has to stand. The parser doesn't emit one;
/// the tree's builder refuses it too.
fn misplaced(event: &Event<'_>) -> Error {
    Error::new(ErrorKind::Composition, "unexpected event in node position").with_span(event.span())
}

/// What a target is told once the document is at fault. `read` reports the fault itself.
fn unreadable() -> Error {
    Error::new(ErrorKind::Composition, "the document could not be read")
}

/// A deserializer for the next node of a stream.
pub(super) struct StreamDeserializer<'de, 's> {
    stream: &'s mut Stream<'de>,
}

/// Deserializer methods that read a built node as the tree reader does, and a collection that
/// is read entry by entry as that reader's `deserialize_any` does: for a sequence or a mapping
/// with no tag, each of these methods of the tree reader comes to that.
macro_rules! as_the_tree_reads {
    ($($method:ident($($argument:ident: $kind:ty),*))*) => {
        $(
            fn $method<V: Visitor<'de>>(
                self,
                $($argument: $kind,)*
                visitor: V,
            ) -> Result<V::Value, Self::Error> {
                match self.stream.take()? {
                    Taken::Built => self
                        .stream
                        .read_built(|node| node.$method($($argument,)* visitor)),
                    Taken::Sequence(span) => self.stream.sequence(span, visitor),
                    Taken::Mapping(span) => self.stream.mapping(span, visitor),
                }
            }
        )*
    };
}

impl<'de> Deserializer<'de> for StreamDeserializer<'de, '_> {
    type Error = Error;

    as_the_tree_reads! {
        deserialize_any() deserialize_bool() deserialize_i8() deserialize_i16()
        deserialize_i32() deserialize_i64() deserialize_i128() deserialize_u8()
        deserialize_u16() deserialize_u32() deserialize_u64() deserialize_u128()
        deserialize_f32() deserialize_f64() deserialize_char() deserialize_str()
        deserialize_string() deserialize_bytes() deserialize_byte_buf() deserialize_unit()
        deserialize_unit_struct(name: &'static str) deserialize_seq()
        deserialize_tuple(length: usize)
        deserialize_tuple_struct(name: &'static str, length: usize) deserialize_map()
        deserialize_identifier() deserialize_ignored_any()
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        // A collection is not a null, so it is some value, read from here.
        if let Some(span) = self.stream.streamed()? {
            return visitor
                .visit_some(self)
                .map_err(|error| with_span(error, span));
        }
        self.stream.build()?;
        self.stream
            .read_built(|node| node.deserialize_option(visitor))
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        if let Some(span) = self.stream.streamed()? {
            return visitor
                .visit_newtype_struct(self)
                .map_err(|error| with_span(error, span));
        }
        self.stream.build()?;
        self.stream
            .read_built(|node| node.deserialize_newtype_struct(name, visitor))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        match self.stream.take()? {
            Taken::Built => self
                .stream
                .read_built(|node| node.deserialize_struct(name, fields, visitor)),
            // A struct reads a mapping and no sequence.
            Taken::Sequence(span) => Err(with_span(
                serde::de::Error::invalid_type(serde::de::Unexpected::Seq, &visitor),
                span,
            )),
            Taken::Mapping(span) => self.stream.mapping(span, visitor),
        }
    }

    /// An enum reads its node whole: a mapping names a variant only if it has one entry, which
    /// the tree reader finds out before it reads the variant's value.
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        if self.stream.open.is_empty() {
            self.stream.build_root()?;
        } else {
            self.stream.build()?;
        }
        self.stream
            .read_built(|node| node.deserialize_enum(name, variants, visitor))
    }
}

struct SequenceAccess<'de, 's> {
    stream: &'s mut Stream<'de>,
    /// Which of the stream's open collections this is.
    level: usize,
}

impl<'de> SeqAccess<'de> for SequenceAccess<'de, '_> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Self::Error> {
        if !self.stream.gives(self.level) {
            return Ok(None);
        }
        let Some(span) = self.stream.entry()? else {
            return Ok(None);
        };
        let index = self.stream.count();
        self.stream
            .read_with(seed)
            .map(Some)
            .map_err(|error| with_span(error, span).inside_index(index))
    }

    fn size_hint(&self) -> Option<usize> {
        self.stream.left(self.level)
    }
}

struct MappingAccess<'de, 's> {
    stream: &'s mut Stream<'de>,
    /// Which of the stream's open collections this is.
    level: usize,
}

impl<'de> MapAccess<'de> for MappingAccess<'de, '_> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        if !self.stream.gives(self.level) {
            return Ok(None);
        }
        let Some(key) = self.stream.key()? else {
            return Ok(None);
        };
        self.stream.count();
        let span = key.span;
        let read = seed
            .deserialize(NodeDeserializer::new(&key))
            .map_err(|error| with_span(error, span));
        // The key stays with the mapping whether or not it suited the target, so that its
        // value is read or passed over, and the entry is checked.
        if let Some(slot) = self.stream.key_slot() {
            *slot = Some(key);
        }
        read.map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        let key = self
            .stream
            .key_slot()
            .and_then(Option::take)
            .ok_or_else(|| {
                Error::coded(
                    ErrorKind::Serde,
                    ErrorCode::MapValueBeforeKey,
                    "map value requested before key",
                )
            })?;
        let span = self.stream.ahead()?.span();
        let value = self
            .stream
            .read_with(seed)
            .map_err(|error| inside_key(with_span(error, span), &key));
        self.stream.entered(key)?;
        value
    }

    fn size_hint(&self) -> Option<usize> {
        self.stream.left(self.level)
    }
}

#[cfg(test)]
mod tests {
    use serde::de::{Deserialize, IgnoredAny};

    use super::super::NodeKind;
    use super::{DocumentEvents, NodeOptions, Stream};
    use crate::limits::Limits;

    /// The tokens that are held when the target begins to read `input`, and those held once it
    /// has read it into a `T`.
    fn tokens<'de, T: Deserialize<'de>>(input: &'de str) -> (usize, usize) {
        let limits = Limits::default();
        let events = DocumentEvents::new(input, &limits, false).expect("a scanned input");
        let mut stream = Stream::new(events, input.len(), &limits, NodeOptions::default());
        let mut before = usize::MAX;
        stream
            .document(|document| {
                before = document.stream.events.tokens();
                T::deserialize(document)
            })
            .unwrap_or_else(|error| panic!("{input:?}: {error}"));
        (before, stream.events.tokens())
    }

    /// A root that is built whole is the document as a tree, so its target reads it with the
    /// tokens freed, as it reads the tree that the tree reader builds. A root that is read
    /// entry by entry needs them to its end.
    #[test]
    fn a_root_that_is_built_whole_is_read_after_the_tokens_are_freed() {
        #[derive(serde::Deserialize)]
        enum Kind {
            #[allow(dead_code, reason = "read, to see what is held meanwhile")]
            V(Vec<u8>),
            #[allow(dead_code, reason = "read, to see what is held meanwhile")]
            W { a: u8 },
        }

        for input in [
            "&a [1, 2, 3]\n",
            "&a\n- 1\n- 2\n",
            "!t [1, 2]\n",
            "&a {k: [1, 2]}\n",
            "text\n",
        ] {
            assert_eq!(tokens::<IgnoredAny>(input), (0, 0), "{input:?}");
        }
        // The tree of a root flow collection is built with room for the entries that the
        // parser counted, and no more.
        let input = "&a [1, 2, 3, 4, 5]\n";
        let limits = Limits::default();
        let events = DocumentEvents::new(input, &limits, false).expect("a scanned input");
        let mut stream = Stream::new(events, input.len(), &limits, NodeOptions::default());
        stream
            .document(|document| {
                // A node with an anchor is shared with the aliases that may follow it.
                let root = document.stream.builder.root.as_ref();
                let Some(NodeKind::Alias(shared)) = root.map(|node| &node.kind) else {
                    panic!("the root is built before its target reads");
                };
                let NodeKind::Sequence(items) = &shared.kind else {
                    panic!("a sequence");
                };
                assert_eq!((items.len(), items.capacity()), (5, 5));
                IgnoredAny::deserialize(document)
            })
            .expect("a value");
        for input in ["[1, 2, 3]\n", "- 1\n- 2\n", "k: [1, 2]\n", "k: &a [1, 2]\n"] {
            let (before, after) = tokens::<IgnoredAny>(input);
            assert!(before > 0 && after > 0, "{input:?}: {before}, {after}");
        }
        // An enum reads its node whole, which the stream learns when the target asks. At the
        // root that is the document, and the tokens are freed before the variant is read.
        for input in ["V: [1, 2, 3]\n", "W:\n  a: 1\n", "V:\n- 1\n"] {
            let (before, after) = tokens::<Kind>(input);
            assert!(before > 0, "{input:?}: {before}");
            assert_eq!(after, 0, "{input:?}");
        }
        let (_, after) = tokens::<Vec<Kind>>("- V: [1]\n- W: {a: 2}\n");
        assert!(after > 0, "an enum inside the root: {after}");
    }

    #[test]
    fn large_anchored_tagged_key_and_enum_shapes_release_tokens() {
        use std::fmt::Write as _;

        #[derive(serde::Deserialize)]
        enum Kind {
            #[allow(dead_code, reason = "read, to verify tokens are released")]
            V(Vec<u8>),
        }
        #[derive(serde::Deserialize)]
        struct Wrap {
            #[allow(dead_code, reason = "read, to verify tokens are released")]
            k: Kind,
        }

        let mut anchored_many = String::new();
        for i in 0..300 {
            let _ = writeln!(anchored_many, "- &a{i} {{x: {i}, y: w}}");
        }
        let big_seq = format!("[1{}]", ",1".repeat(600));
        let big_anchored = format!("k: &a {big_seq}\n");
        let big_tagged = format!("k: !t {big_seq}\n");
        let big_key = format!("? {big_seq}\n: 1\n");
        for (label, input) in [
            ("anchored_many", anchored_many.as_str()),
            ("big_anchored", big_anchored.as_str()),
            ("big_tagged", big_tagged.as_str()),
            ("big_key", big_key.as_str()),
        ] {
            assert_eq!(tokens::<IgnoredAny>(input), (0, 0), "{label}");
        }
        let big_enum = format!("k:\n  V: {big_seq}\n");
        let (_, after_enum) = tokens::<Wrap>(&big_enum);
        assert_eq!(after_enum, 0, "large non-root enum releases tokens");
    }
}
