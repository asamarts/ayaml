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

use serde::Deserializer;
use serde::de::{Deserialize, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};

use super::stream_scratch::{
    StreamScratch, StreamSeenLarge, cow_to_key_text, insert_large_key_map, pop_seen_large,
    promote_seen_large,
};
use super::{
    Builder, Node, NodeDeserializer, NodeKind, NodeOptions, SeenNodeKeys, check_consumed,
    collection_drops, inside_key, node_key_text, parse_document, plain_scalar, scalar_key_text,
    with_span,
};
use crate::error::{Error, ErrorCode, ErrorKind};
use crate::event::ScalarStyle;
use crate::limits::Limits;
use crate::parser::{DocumentEvents, QueuedEvent};
use crate::resolver::{float_from_text, resolve_plain};
use crate::scanner::TokenText;
use crate::scratch::ParserBuf;
use crate::span::{Location, SPANNED_FIELDS, SPANNED_TOKEN, Span};
use crate::value::{Value, push_quoted_key};

/// Maximum scalar keys per streaming mapping kept on `Stream::seen_keys` with linear scan before
/// promoting to a cryptographic SipHash-1-3 hash map. Covers typical small structs and frontmatter
/// objects (up to 12 fields) in one compact reusable flat buffer with zero per-map hash allocation.
const STREAM_SEEN_SMALL_LIMIT: usize = 12;

/// Panic-safe guard returning [`StreamScratch`] to [`ParserBuf`] even on error or unwind from
/// a caller's [`Deserialize`] / [`DeserializeSeed`] implementation.
struct StreamScratchGuard<'de, 'b> {
    stream: Stream<'de>,
    buf: &'b mut ParserBuf,
}

impl Drop for StreamScratchGuard<'_, '_> {
    fn drop(&mut self) {
        self.buf.put_stream_scratch(self.stream.take_scratch());
    }
}

/// Read one document into what `seed` makes of it.
pub(super) fn read<'de, T: DeserializeSeed<'de>>(
    input: &'de str,
    limits: &Limits,
    options: NodeOptions,
    seed: T,
) -> Result<T::Value, Error> {
    limits.validate()?;
    let located = |error: Error| error.with_input_location(input, options.compat_flow);
    limits.check_input(input.len()).map_err(|error| {
        Error::from(error)
            .with_span(Span::empty(0))
            .with_location(Location::new(1, 1))
    })?;
    let events = DocumentEvents::new(input, limits, options.compat_flow).map_err(located)?;
    let mut stream = Stream::new(events, input.len(), limits, options);
    let result = stream.document(|document| seed.deserialize(document));
    let fault = stream.fault.take();
    let error = match (result, fault) {
        (Ok(value), None) => return Ok(value),
        (_, Some(fault)) => fault,
        (Err(error), None) => {
            if stream.validate_end().is_ok() && stream.fault.is_none() {
                return Err(located(error));
            }
            error
        }
    };
    drop(stream);
    Err(located(
        parse_document(input, limits, options)
            .err()
            .unwrap_or(error),
    ))
}

/// Read one document into what `seed` makes of it, reusing pooled scratch from `buf`.
pub(super) fn read_in<'de, T: DeserializeSeed<'de>>(
    input: &'de str,
    limits: &Limits,
    options: NodeOptions,
    seed: T,
    buf: &mut ParserBuf,
) -> Result<T::Value, Error> {
    limits.validate()?;
    let located = |error: Error| error.with_input_location(input, options.compat_flow);
    limits.check_input(input.len()).map_err(|error| {
        Error::from(error)
            .with_span(Span::empty(0))
            .with_location(Location::new(1, 1))
    })?;
    let scratch = buf.take_stream_scratch();
    let events = DocumentEvents::new_with_scratch(
        input,
        limits,
        options.compat_flow,
        scratch.window,
        scratch.queue,
        scratch.cont_overflow,
    )
    .map_err(located)?;
    let mut guard = StreamScratchGuard {
        stream: Stream::with_scratch(
            events,
            input.len(),
            limits,
            options,
            scratch.seen_keys,
            scratch.seen_large_top,
            scratch.seen_large_rest,
        ),
        buf,
    };
    let result = guard.stream.document(|document| seed.deserialize(document));
    let fault = guard.stream.fault.take();
    let error = match (result, fault) {
        (Ok(value), None) => return Ok(value),
        (_, Some(fault)) => fault,
        (Err(error), None) => {
            if guard.stream.validate_end().is_ok() && guard.stream.fault.is_none() {
                return Err(located(error));
            }
            error
        }
    };
    drop(guard);
    Err(located(
        parse_document(input, limits, options)
            .err()
            .unwrap_or(error),
    ))
}

/// An unanchored, untagged scalar event popped directly from `DocumentEvents` without constructing
/// an 80-byte `Node<'de>` or converting to `Event<'de>`.
struct UntaggedScalar {
    value: TokenText,
    style: ScalarStyle,
    span: Span,
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
    /// Shared flat stack of `'static` scalar keys for open small mappings (`<= STREAM_SEEN_SMALL_LIMIT`
    /// entries), eliminating per-mapping heap allocations and enabling `ParserBuf` reuse.
    seen_keys: Vec<TokenText>,
    /// Active promoted duplicate-key table for the innermost large open mapping (`(depth, map)`),
    /// kept inline on `Stream` to avoid both a `Box` allocation and widening `Open`.
    seen_large_top: Option<StreamSeenLarge>,
    /// Overflow stack of promoted duplicate-key tables when two or more nested mappings exceed
    /// `STREAM_SEEN_SMALL_LIMIT` simultaneously.
    seen_large_rest: Vec<StreamSeenLarge>,
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

/// A mapping key held between `next_key_seed` and `next_value_seed` (or skipped value).
enum PendingStreamKey<'de> {
    /// An unanchored, untagged scalar guaranteed to resolve as a YAML string, bypassing `Node<'de>`
    /// construction, `Event` conversion, and cloning when recorded into the seen-key set.
    String { text: TokenText, span: Span },
    /// Any other key (numbers, bools, nulls, tagged, anchored, or collection keys).
    Node(Box<Node<'de>>),
}

impl PendingStreamKey<'_> {
    #[inline]
    fn span(&self) -> Span {
        match self {
            Self::String { span, .. } => *span,
            Self::Node(node) => node.span,
        }
    }
}

/// Sentinel for `Open::{Sequence, Mapping}::known` when total entry count is unknown.
const UNKNOWN_ENTRIES: usize = usize::MAX;

/// A collection that is read entry by entry.
enum Open<'de> {
    Sequence {
        /// The items that a visitor has asked for.
        read: usize,
        /// Total items known when the sequence opened, or `UNKNOWN_ENTRIES`.
        known: usize,
    },
    Mapping {
        /// Start offset of this mapping's inline scalar keys in `Stream::seen_keys`.
        seen_start: usize,
        /// Non-scalar keys of the entries read so far, moved in by cached node hash without cloning.
        seen_nodes: Option<Box<SeenNodeKeys<'de>>>,
        /// The key whose value is still to be read.
        key: Option<PendingStreamKey<'de>>,
        /// The entries whose keys a visitor has asked for.
        read: usize,
        /// Total entries known when the mapping opened, or `UNKNOWN_ENTRIES`.
        known: usize,
        /// Whether this mapping promoted its scalar keys into `Stream::seen_large_top`/`rest`.
        has_large: bool,
    },
}

/// Whether an untagged, non-empty plain scalar's UTF-8 bytes are guaranteed to resolve as a YAML
/// string and cannot match `resolve_plain` or `float_from_text`.
#[inline]
fn is_unambiguous_plain_string_bytes(bytes: &[u8]) -> bool {
    match bytes.first() {
        None | Some(b'0'..=b'9' | b'+' | b'-' | b'.' | b'~') => false,
        Some(b'n' | b'N') => !matches!(bytes, b"null" | b"Null" | b"NULL"),
        Some(b't' | b'T') => !matches!(bytes, b"true" | b"True" | b"TRUE"),
        Some(b'f' | b'F') => !matches!(bytes, b"false" | b"False" | b"FALSE"),
        _ => true,
    }
}

impl<'de> Stream<'de> {
    fn new(
        events: DocumentEvents<'de>,
        input_len: usize,
        limits: &Limits,
        options: NodeOptions,
    ) -> Self {
        Self::with_scratch(
            events,
            input_len,
            limits,
            options,
            Vec::new(),
            None,
            Vec::new(),
        )
    }

    fn with_scratch(
        events: DocumentEvents<'de>,
        input_len: usize,
        limits: &Limits,
        options: NodeOptions,
        mut seen_keys: Vec<TokenText>,
        seen_large_top: Option<StreamSeenLarge>,
        mut seen_large_rest: Vec<StreamSeenLarge>,
    ) -> Self {
        seen_keys.clear();
        seen_large_rest.clear();
        Self {
            events,
            builder: Builder::streaming(input_len, limits, options),
            open: Vec::new(),
            seen_keys,
            seen_large_top,
            seen_large_rest,
            ahead: Ahead::Unknown,
            due: false,
            ended: false,
            fault: None,
        }
    }

    fn take_scratch(&mut self) -> StreamScratch {
        let (window, queue, cont_overflow) = self.events.take_scratch();
        let mut scratch = StreamScratch {
            window,
            queue,
            cont_overflow,
            seen_keys: std::mem::take(&mut self.seen_keys),
            seen_large_top: self.seen_large_top.take(),
            seen_large_rest: std::mem::take(&mut self.seen_large_rest),
        };
        scratch.clear_and_clamp();
        scratch
    }

    fn document<R>(
        &mut self,
        target: impl for<'s> FnOnce(StreamDeserializer<'de, 's>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        loop {
            let next = match self.events.next_queued() {
                Ok(event) => event,
                Err(error) => return Err(self.faulted(error)),
            };
            match next {
                Some(QueuedEvent::StreamStart { .. } | QueuedEvent::Directive { .. }) => {}
                Some(QueuedEvent::DocumentStart { .. }) => break,
                // A stream with no document reads as an empty one, which the tree gives as a
                // null that the source doesn't write.
                Some(QueuedEvent::StreamEnd { .. }) | None => {
                    let end = Span::empty(self.builder.input_len);
                    self.builder.root = Some(Node::null(end));
                    self.ahead = Ahead::Whole(end);
                    self.ended = true;
                    return target(StreamDeserializer { stream: self });
                }
                Some(event) => return Err(self.faulted(misplaced_span(event.span()))),
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

    /// Finish checking the document and trailing stream after a target-side error without
    /// rebuilding the full tree, so valid YAML skips a second parse.
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
        match self.pull_queued()? {
            QueuedEvent::DocumentEnd { .. } => {}
            event => return Err(misplaced_span(event.span())),
        }
        loop {
            match self.events.next_queued()? {
                Some(QueuedEvent::StreamEnd { .. }) | None => return Ok(()),
                Some(QueuedEvent::Directive { .. }) => {}
                Some(QueuedEvent::DocumentStart { span, .. }) => {
                    return Err(Error::coded(
                        ErrorKind::Parser,
                        ErrorCode::TrailingDocuments,
                        "expected a single YAML document, but found trailing documents",
                    )
                    .with_span(span));
                }
                Some(event) => return Err(misplaced_span(event.span())),
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

    /// Take the next raw `'static` queued event.
    #[inline]
    fn pull_queued(&mut self) -> Result<QueuedEvent, Error> {
        self.ready()?;
        self.ahead = Ahead::Unknown;
        self.events.pop_queued().ok_or_else(unreadable)
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
            Some(QueuedEvent::SequenceStart {
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
            Some(QueuedEvent::MappingStart {
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
            Some(QueuedEvent::SequenceEnd { span }) => Ahead::End {
                sequence: true,
                span: *span,
            },
            Some(QueuedEvent::MappingEnd { span }) => Ahead::End {
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
        self.ready()?;
        self.ahead = Ahead::Unknown;
        if self.events.pop_queued().is_none() {
            return Err(unreadable());
        }
        if let Err(error) = self.builder.limits.check_depth(self.open.len() + 1) {
            return Err(self.faulted(Error::from(error).with_span(span)));
        }
        let known = if self.open.is_empty() {
            self.events.root_entries()
        } else {
            None
        }
        .or_else(|| self.events.queued_entries(sequence));
        let known = match known {
            Some(entries) => Some(entries),
            None => match self.events.refill_for_hint() {
                Ok(true) => self.events.queued_entries(sequence),
                Ok(false) => None,
                Err(error) => return Err(self.faulted(error)),
            },
        };
        let known = known.unwrap_or(UNKNOWN_ENTRIES);
        let seen_start = self.seen_keys.len();
        self.open.push(if sequence {
            Open::Sequence { read: 0, known }
        } else {
            Open::Mapping {
                seen_start,
                seen_nodes: None,
                key: None,
                read: 0,
                known,
                has_large: false,
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
        // A scalar with no anchor is most of what is read. Pop it by value so an owned decoded
        // string moves straight into the `Node` without cloning or `Event` conversion.
        if matches!(
            self.events.front(),
            Some(QueuedEvent::Scalar { anchor: None, .. })
        ) && let Some(QueuedEvent::Scalar {
            value,
            style,
            tag,
            anchor: None,
            span,
        }) = self.events.pop_queued()
        {
            let input = self.events.input();
            let raw_str = span.slice(input).unwrap_or("");
            let value_cow = match value {
                TokenText::Slice(s) if s == span => std::borrow::Cow::Borrowed(raw_str),
                other => other.into_cow(input),
            };
            let raw = std::borrow::Cow::Borrowed(raw_str);
            let node = plain_scalar(
                value_cow,
                &raw,
                style,
                tag.as_ref(),
                span,
                self.builder.options.drop_global_tags,
            );
            return match node {
                Ok(node) => {
                    self.builder.root = Some(node);
                    Ok(())
                }
                Err(error) => Err(self.faulted(error)),
            };
        }
        self.builder.base = self.open.len();
        let input = self.events.input();
        loop {
            let event = self.pull_queued()?;
            if let Err(error) = self.builder.push_queued_node(event, input) {
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

    /// Read the node that waits as the builder's root, and drop it. Passing the node by value
    /// allows owned decoded strings to move into visitors via `visit_string` without copying.
    #[inline]
    fn read_built<R>(
        &mut self,
        read: impl FnOnce(NodeDeserializer<'de, '_>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        match self.builder.root.take() {
            Some(node) => read(NodeDeserializer::from_owned(node)),
            None => Err(unreadable()),
        }
    }

    /// The span of the next node, if it is a collection that is read entry by entry.
    fn streamed(&mut self) -> Result<Option<Span>, Error> {
        Ok(match self.ahead()? {
            Ahead::Sequence(span) | Ahead::Mapping(span) => Some(span),
            _ => None,
        })
    }

    /// Pop the next event directly when it is an unanchored, untagged scalar and no built root
    /// node waits, bypassing `Node<'de>` and `Event<'de>` construction on the scalar hot path.
    #[inline]
    fn take_untagged_scalar(&mut self) -> Result<Option<UntaggedScalar>, Error> {
        if self.builder.root.is_some() {
            return Ok(None);
        }
        if self.fault.is_some() || self.events.front().is_none() {
            self.ready()?;
        }
        if matches!(
            self.events.front(),
            Some(QueuedEvent::Scalar {
                anchor: None,
                tag: None,
                ..
            })
        ) && let Some(QueuedEvent::Scalar {
            value, style, span, ..
        }) = self.events.pop_queued()
        {
            self.due = false;
            self.ahead = Ahead::Unknown;
            return Ok(Some(UntaggedScalar { value, style, span }));
        }
        Ok(None)
    }

    /// Pop the innermost open collection and release any inline or promoted duplicate-key state.
    #[inline]
    fn pop_open(&mut self) {
        if let Some(Open::Mapping {
            seen_start,
            has_large,
            ..
        }) = self.open.pop()
        {
            self.seen_keys.truncate(seen_start);
            if has_large {
                let depth = self.open.len();
                pop_seen_large(&mut self.seen_large_top, &mut self.seen_large_rest, depth);
            }
        }
    }

    /// The span of the next node in the innermost open collection, which stays for the next
    /// read, or `None` at the collection's end, where it closes.
    #[inline]
    fn entry(&mut self) -> Result<Option<Span>, Error> {
        let within = matches!(self.open.last(), Some(Open::Sequence { .. }));
        match self.ahead()? {
            Ahead::End { sequence, .. } if sequence == within => {
                self.ahead = Ahead::Unknown;
                self.events.pop_queued();
                self.pop_open();
                Ok(None)
            }
            ahead => Ok(Some(ahead.span())),
        }
    }

    /// Count one more entry that a visitor asked the innermost open collection for, and give
    /// its position.
    #[inline]
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
    #[inline]
    fn key_slot(&mut self) -> Option<&mut Option<PendingStreamKey<'de>>> {
        match self.open.last_mut() {
            Some(Open::Mapping { key, .. }) => Some(key),
            _ => None,
        }
    }

    /// Span of the next waiting event without computing/storing the full `Ahead` classification.
    #[inline]
    fn peek_span(&mut self) -> Result<Span, Error> {
        match self.ahead {
            Ahead::Sequence(span)
            | Ahead::Mapping(span)
            | Ahead::Whole(span)
            | Ahead::End { span, .. } => return Ok(span),
            Ahead::Unknown => {}
        }
        if self.fault.is_some() || self.events.front().is_none() {
            self.ready()?;
        }
        self.events
            .front()
            .map(QueuedEvent::span)
            .ok_or_else(unreadable)
    }

    /// The next key of the innermost open mapping, or `None` at the mapping's end, where it
    /// closes. If a visitor left the value of the entry before it, that value is passed over.
    fn key(&mut self) -> Result<Option<PendingStreamKey<'de>>, Error> {
        if let Some(key) = self.key_slot().and_then(Option::take) {
            self.skip()?;
            self.entered(key)?;
        }
        if self.fault.is_none() && self.builder.root.is_none() {
            if self.events.front().is_none() {
                self.ready()?;
            }
            let input = self.events.input();
            match self.events.front() {
                Some(QueuedEvent::MappingEnd { .. }) => {
                    self.ahead = Ahead::Unknown;
                    self.events.pop_queued();
                    self.pop_open();
                    return Ok(None);
                }
                Some(QueuedEvent::Scalar {
                    value,
                    style,
                    tag: None,
                    anchor: None,
                    span,
                }) if *style != ScalarStyle::Plain
                    || (!span.is_empty()
                        && is_unambiguous_plain_string_bytes(value.as_bytes(input))) =>
                {
                    if let Some(QueuedEvent::Scalar { value, span, .. }) = self.events.pop_queued()
                    {
                        self.due = false;
                        self.ahead = Ahead::Unknown;
                        return Ok(Some(PendingStreamKey::String { text: value, span }));
                    }
                }
                _ => {}
            }
        }
        if self.entry()?.is_none() {
            return Ok(None);
        }
        self.whole()
            .map(|node| Some(PendingStreamKey::Node(Box::new(node))))
    }

    /// Record a complete entry of the innermost open mapping. The tree's builder refuses a
    /// duplicate key once the entry's value is built, and this does at the same point.
    fn entered(&mut self, key: PendingStreamKey<'de>) -> Result<(), Error> {
        if self.builder.options.allow_duplicates || self.fault.is_some() {
            return Ok(());
        }
        let depth = self.open.len().saturating_sub(1);
        let input = self.events.input();
        let Some(Open::Mapping {
            seen_start,
            seen_nodes,
            known,
            has_large,
            ..
        }) = self.open.last_mut()
        else {
            return Ok(());
        };
        let seen_start = *seen_start;
        let known_entries = (*known != UNKNOWN_ENTRIES).then_some(*known);
        let promoted_cap = known_entries.unwrap_or(64).max(STREAM_SEEN_SMALL_LIMIT * 2);
        let initial_small_cap = if self.builder.input_len <= 512 {
            STREAM_SEEN_SMALL_LIMIT
        } else {
            16
        };
        let is_large = *has_large;
        match key {
            PendingStreamKey::String { text, span } => {
                let duplicate = if is_large {
                    match self.seen_large_top.as_mut() {
                        Some((_, map)) => insert_large_key_map(map, text, input).err(),
                        None => None,
                    }
                } else {
                    let needle = text.as_bytes(input);
                    let slice = &self.seen_keys[seen_start..];
                    if slice
                        .iter()
                        .any(|existing| existing.as_bytes(input) == needle)
                    {
                        Some(text)
                    } else if slice.len() < STREAM_SEEN_SMALL_LIMIT {
                        if self.seen_keys.capacity() == 0 {
                            self.seen_keys.reserve_exact(initial_small_cap);
                        }
                        self.seen_keys.push(text);
                        return Ok(());
                    } else {
                        *has_large = true;
                        promote_seen_large(
                            &mut self.seen_keys,
                            seen_start,
                            &mut self.seen_large_top,
                            &mut self.seen_large_rest,
                            depth,
                            promoted_cap,
                            text,
                            input,
                        )
                        .err()
                    }
                };
                let Some(dup_text) = duplicate else {
                    return Ok(());
                };
                let mut formatted = String::new();
                push_quoted_key(dup_text.as_str(input), &mut formatted);
                Err(self.faulted(
                    Error::new(
                        ErrorKind::DuplicateKey,
                        format!("duplicate mapping key: {formatted}"),
                    )
                    .with_span(span),
                ))
            }
            PendingStreamKey::Node(key) => {
                let duplicate = if let Some(text) = scalar_key_text(&key) {
                    let inserted = if is_large {
                        let key_text = cow_to_key_text(text, key.span, input);
                        match self.seen_large_top.as_mut() {
                            Some((_, map)) => insert_large_key_map(map, key_text, input).is_ok(),
                            None => true,
                        }
                    } else {
                        let needle = text.as_bytes();
                        let slice = &self.seen_keys[seen_start..];
                        if slice
                            .iter()
                            .any(|existing| existing.as_bytes(input) == needle)
                        {
                            false
                        } else if slice.len() < STREAM_SEEN_SMALL_LIMIT {
                            if self.seen_keys.capacity() == 0 {
                                self.seen_keys.reserve_exact(initial_small_cap);
                            }
                            self.seen_keys.push(cow_to_key_text(text, key.span, input));
                            true
                        } else {
                            *has_large = true;
                            let key_text = cow_to_key_text(text, key.span, input);
                            promote_seen_large(
                                &mut self.seen_keys,
                                seen_start,
                                &mut self.seen_large_top,
                                &mut self.seen_large_rest,
                                depth,
                                promoted_cap,
                                key_text,
                                input,
                            )
                            .is_ok()
                        }
                    };
                    if inserted {
                        return Ok(());
                    }
                    *key
                } else {
                    match seen_nodes.get_or_insert_with(Box::default).insert(*key) {
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
        }
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
    #[inline]
    fn gives(&self, level: usize) -> bool {
        self.fault.is_none() && self.open.len() > level
    }

    /// How many entries the collection that was opened as number `level` has left, where that is
    /// known before they are read: for a root flow collection (counted by the parser) or any
    /// collection whose end is already among the events waiting in the lookahead queue.
    #[inline]
    fn left(&self, level: usize) -> Option<usize> {
        if self.fault.is_some() {
            return None;
        }
        match self.open.get(level) {
            None => Some(0),
            Some(Open::Sequence { read, known } | Open::Mapping { read, known, .. }) => {
                (*known != UNKNOWN_ENTRIES).then(|| known.saturating_sub(*read))
            }
        }
    }
}

/// An event where a node or a document's boundary has to stand. The parser doesn't emit one;
/// the tree's builder refuses it too.
fn misplaced_span(span: Span) -> Error {
    Error::new(ErrorKind::Composition, "unexpected event in node position").with_span(span)
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

/// Visit an unanchored, untagged scalar event directly with `deserialize_any` semantics, bypassing
/// `Node<'de>` and `Event<'de>` construction.
#[inline]
fn visit_untagged_scalar_any<'de, V: Visitor<'de>>(
    scalar: UntaggedScalar,
    input: &'de str,
    visitor: V,
) -> Result<V::Value, Error> {
    let UntaggedScalar { value, style, span } = scalar;
    let result = match value {
        TokenText::Slice(s) => {
            let text = s.slice(input).unwrap_or("");
            if style != ScalarStyle::Plain || is_unambiguous_plain_string_bytes(text.as_bytes()) {
                visitor.visit_borrowed_str(text)
            } else {
                match resolve_plain(text) {
                    None => visitor.visit_borrowed_str(text),
                    Some(Value::Null) => visitor.visit_unit(),
                    Some(Value::Bool(b)) => visitor.visit_bool(b),
                    Some(Value::Number(n)) => n.visit(visitor),
                    Some(_) => unreachable!("resolve_plain only returns Null, Bool, or Number"),
                }
            }
        }
        TokenText::Owned(s) => {
            if style != ScalarStyle::Plain || is_unambiguous_plain_string_bytes(s.as_bytes()) {
                visitor.visit_string(s)
            } else {
                match resolve_plain(&s) {
                    None => visitor.visit_string(s),
                    Some(Value::Null) => visitor.visit_unit(),
                    Some(Value::Bool(b)) => visitor.visit_bool(b),
                    Some(Value::Number(n)) => n.visit(visitor),
                    Some(_) => unreachable!("resolve_plain only returns Null, Bool, or Number"),
                }
            }
        }
    };
    result.map_err(|error| with_span(error, span))
}

/// Visit an unanchored, untagged scalar event directly with `deserialize_string` /
/// `deserialize_str` / `deserialize_identifier` / `deserialize_char` semantics.
#[inline]
fn visit_untagged_scalar_str<'de, V: Visitor<'de>>(
    scalar: UntaggedScalar,
    input: &'de str,
    visitor: V,
) -> Result<V::Value, Error> {
    let UntaggedScalar { value, span, .. } = scalar;
    let result = match value {
        TokenText::Slice(s) => visitor.visit_borrowed_str(s.slice(input).unwrap_or("")),
        TokenText::Owned(s) => visitor.visit_string(s),
    };
    result.map_err(|error| with_span(error, span))
}

/// Visit an unanchored, untagged scalar event directly with `deserialize_f64` / `deserialize_f32`
/// semantics (matching `NodeDeserializer::deserialize_f64`).
fn visit_untagged_scalar_f64<'de, V: Visitor<'de>>(
    scalar: UntaggedScalar,
    input: &'de str,
    visitor: V,
) -> Result<V::Value, Error> {
    if scalar.style != ScalarStyle::Plain {
        return visit_untagged_scalar_any(scalar, input, visitor);
    }
    let UntaggedScalar { value, span, .. } = scalar;
    let text = value.as_str(input);
    let raw_text = span.slice(input).unwrap_or("");
    match resolve_plain(text) {
        Some(Value::Number(number)) if number.is_f64() => visitor
            .visit_f64(number.as_f64().unwrap_or(f64::NAN))
            .map_err(|error| with_span(error, span)),
        Some(Value::Number(_)) => {
            if let Ok(float) = raw_text.parse::<f64>() {
                return visitor
                    .visit_f64(float)
                    .map_err(|error| with_span(error, span));
            }
            Err(Error::coded(
                ErrorKind::Serde,
                ErrorCode::InvalidType,
                format!("invalid type: integer `{raw_text}`, expected f64"),
            )
            .with_span(span))
        }
        None => {
            if let Some(float) = float_from_text(raw_text) {
                return visitor
                    .visit_f64(float)
                    .map_err(|error| with_span(error, span));
            }
            let result = match value {
                TokenText::Slice(s) => visitor.visit_borrowed_str(s.slice(input).unwrap_or("")),
                TokenText::Owned(s) => visitor.visit_string(s),
            };
            result.map_err(|error| with_span(error, span))
        }
        Some(Value::Null) => visitor.visit_unit().map_err(|error| with_span(error, span)),
        Some(Value::Bool(b)) => visitor
            .visit_bool(b)
            .map_err(|error| with_span(error, span)),
        Some(_) => unreachable!("resolve_plain only returns Null, Bool, or Number"),
    }
}

/// Deserializer methods for plain scalar targets (bools and integers) that bypass `Node<'de>` on
/// unanchored, untagged scalars and fall back to the tree/collection paths otherwise.
macro_rules! fast_any_scalar_reads {
    ($($method:ident)*) => {
        $(
            fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
                if let Some(scalar) = self.stream.take_untagged_scalar()? {
                    return visit_untagged_scalar_any(scalar, self.stream.events.input(), visitor);
                }
                match self.stream.take()? {
                    Taken::Built => self.stream.read_built(|node| node.$method(visitor)),
                    Taken::Sequence(span) => self.stream.sequence(span, visitor),
                    Taken::Mapping(span) => self.stream.mapping(span, visitor),
                }
            }
        )*
    };
}

/// Deserializer methods for string-like scalar targets (`str`, `string`, `identifier`, `char`) that
/// bypass `Node<'de>` on unanchored, untagged scalars.
macro_rules! fast_str_scalar_reads {
    ($($method:ident)*) => {
        $(
            fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
                if let Some(scalar) = self.stream.take_untagged_scalar()? {
                    return visit_untagged_scalar_str(scalar, self.stream.events.input(), visitor);
                }
                match self.stream.take()? {
                    Taken::Built => self.stream.read_built(|node| node.$method(visitor)),
                    Taken::Sequence(span) => self.stream.sequence(span, visitor),
                    Taken::Mapping(span) => self.stream.mapping(span, visitor),
                }
            }
        )*
    };
}

impl<'de> Deserializer<'de> for StreamDeserializer<'de, '_> {
    type Error = Error;

    fast_any_scalar_reads! {
        deserialize_any deserialize_bool deserialize_i8 deserialize_i16
        deserialize_i32 deserialize_i64 deserialize_i128 deserialize_u8
        deserialize_u16 deserialize_u32 deserialize_u64 deserialize_u128
    }

    fast_str_scalar_reads! {
        deserialize_str deserialize_string deserialize_identifier deserialize_char
    }

    as_the_tree_reads! {
        deserialize_bytes() deserialize_byte_buf() deserialize_unit()
        deserialize_unit_struct(name: &'static str) deserialize_seq()
        deserialize_tuple(length: usize)
        deserialize_tuple_struct(name: &'static str, length: usize) deserialize_map()
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        if let Some(scalar) = self.stream.take_untagged_scalar()? {
            return visit_untagged_scalar_f64(scalar, self.stream.events.input(), visitor);
        }
        match self.stream.take()? {
            Taken::Built => self.stream.read_built(|node| node.deserialize_f64(visitor)),
            Taken::Sequence(span) => self.stream.sequence(span, visitor),
            Taken::Mapping(span) => self.stream.mapping(span, visitor),
        }
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_f64(visitor)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        if let Some(scalar) = self.stream.take_untagged_scalar()? {
            return visitor
                .visit_unit()
                .map_err(|error| with_span(error, scalar.span));
        }
        match self.stream.take()? {
            Taken::Built => self
                .stream
                .read_built(|node| node.deserialize_ignored_any(visitor)),
            Taken::Sequence(span) => self.stream.sequence(span, visitor),
            Taken::Mapping(span) => self.stream.mapping(span, visitor),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        if self.stream.builder.root.is_none() {
            if self.stream.fault.is_some() || self.stream.events.front().is_none() {
                self.stream.ready()?;
            }
            let input = self.stream.events.input();
            if let Some(QueuedEvent::Scalar {
                value,
                style,
                tag: None,
                anchor: None,
                span,
            }) = self.stream.events.front()
            {
                let span = *span;
                let is_null = *style == ScalarStyle::Plain
                    && matches!(resolve_plain(value.as_str(input)), Some(Value::Null));
                if is_null {
                    self.stream.events.pop_queued();
                    self.stream.due = false;
                    self.stream.ahead = Ahead::Unknown;
                    return visitor.visit_none();
                }
                return visitor
                    .visit_some(self)
                    .map_err(|error| with_span(error, span));
            }
        }
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
        if name == SPANNED_TOKEN && fields == SPANNED_FIELDS {
            // Build the node whole so collection end spans and alias use-site spans match
            // NodeDeserializer and SpannedTree identically.
            if self.stream.open.is_empty() {
                self.stream.build_root()?;
            } else {
                self.stream.build()?;
            }
            return self
                .stream
                .read_built(|node| node.deserialize_struct(name, fields, visitor));
        }
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

/// Lightweight deserializer for a mapping key known to be an unambiguous YAML string, avoiding
/// constructing an 80 B `Node<'de>` on every mapping entry.
#[derive(Clone, Copy)]
struct StringKeyDeserializer<'de, 'a> {
    text: &'a TokenText,
    input: &'de str,
    span: Span,
}

impl<'de> StringKeyDeserializer<'de, '_> {
    #[inline]
    fn visit_str_like<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let result = match self.text {
            TokenText::Slice(s) => visitor.visit_borrowed_str(s.slice(self.input).unwrap_or("")),
            TokenText::Owned(s) => visitor.visit_str(s),
        };
        result.map_err(|error| with_span(error, self.span))
    }

    fn into_cow(self) -> std::borrow::Cow<'de, str> {
        match self.text {
            TokenText::Slice(s) => std::borrow::Cow::Borrowed(s.slice(self.input).unwrap_or("")),
            TokenText::Owned(s) => std::borrow::Cow::Owned(s.clone()),
        }
    }
}

impl<'de> Deserializer<'de> for StringKeyDeserializer<'de, '_> {
    type Error = Error;

    #[inline]
    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.visit_str_like(visitor)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let result = match self.text {
            TokenText::Slice(s) => {
                visitor.visit_borrowed_bytes(s.slice(self.input).unwrap_or("").as_bytes())
            }
            TokenText::Owned(s) => visitor.visit_bytes(s.as_bytes()),
        };
        result.map_err(|error| with_span(error, self.span))
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor
            .visit_some(self)
            .map_err(|error| with_span(error, self.span))
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor
            .visit_newtype_struct(self)
            .map_err(|error| with_span(error, self.span))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        if name == SPANNED_TOKEN && fields == SPANNED_FIELDS {
            let node = Node::new(NodeKind::String(self.into_cow()), self.span);
            return NodeDeserializer::from_owned(node).deserialize_struct(name, fields, visitor);
        }
        Err(with_span(
            serde::de::Error::invalid_type(
                serde::de::Unexpected::Str(self.text.as_str(self.input)),
                &visitor,
            ),
            self.span,
        ))
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let node = Node::new(NodeKind::String(self.into_cow()), self.span);
        NodeDeserializer::new(&node).deserialize_enum(name, variants, visitor)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        unit unit_struct seq tuple tuple_struct map identifier ignored_any
    }
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
        let span = key.span();
        let input = self.stream.events.input();
        let read = match &key {
            PendingStreamKey::String { text, span } => seed
                .deserialize(StringKeyDeserializer {
                    text,
                    input,
                    span: *span,
                })
                .map_err(|error| with_span(error, *span)),
            PendingStreamKey::Node(node) => seed
                .deserialize(NodeDeserializer::new(node))
                .map_err(|error| with_span(error, span)),
        };
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
        let span = self.stream.peek_span()?;
        let input = self.stream.events.input();
        let value = self.stream.read_with(seed).map_err(|error| {
            let error = with_span(error, span);
            match &key {
                PendingStreamKey::String { text, .. } => error.inside_key(text.as_str(input)),
                PendingStreamKey::Node(node) => inside_key(error, node),
            }
        });
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
        // Multi-line anchored documents stream through the bounded line window without buffering
        // all 3,000 tokens at once, keeping peak token storage to a small constant window.
        let limits = Limits::default();
        let events = DocumentEvents::new(&anchored_many, &limits, false).expect("a scanned input");
        let mut stream = Stream::new(events, anchored_many.len(), &limits, NodeOptions::default());
        stream
            .document(|document| IgnoredAny::deserialize(document))
            .expect("anchored_many");
        assert!(
            stream.events.peak_tokens() <= 512 && stream.events.peak_tokens_cap() <= 512,
            "anchored_many peak tokens={}, cap={}",
            stream.events.peak_tokens(),
            stream.events.peak_tokens_cap(),
        );
        let big_seq = format!("[1{}]", ",1".repeat(600));
        let big_anchored = format!("k: &a {big_seq}\n");
        let big_tagged = format!("k: !t {big_seq}\n");
        let big_key = format!("? {big_seq}\n: 1\n");
        for (label, input) in [
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

    #[test]
    fn flat_seen_keys_stack_and_nested_large_mappings_enforce_duplicates_accurately() {
        use crate::error::ErrorCode;
        use std::fmt::Write as _;

        // 1. Exactly STREAM_SEEN_SMALL_LIMIT (12) keys succeed, and a duplicate at index 0 or 11 fails.
        let mut small = String::new();
        for i in 0..12 {
            let _ = writeln!(small, "k{i}: {i}");
        }
        assert!(crate::from_str::<crate::Value>(&small).is_ok());
        for dup_key in ["k0", "k11", "\"k0\""] {
            let mut doc = small.clone();
            let _ = writeln!(doc, "{dup_key}: 99");
            let err = crate::from_str::<crate::Value>(&doc).expect_err("dup in small map");
            assert_eq!(err.code(), ErrorCode::DuplicateKey);
        }

        // 2. Nested large mappings (> 12 keys inside > 12 keys) plus a nested small mapping sharing
        //    key names with the outer mapping exercise `seen_large_top`, `seen_large_rest`, and
        //    `seen_keys.truncate(seen_start)` without cross-level contamination.
        let mut nested = String::new();
        for i in 0..14 {
            let _ = writeln!(nested, "outer_{i}: {i}");
        }
        nested.push_str("inner_large:\n");
        for i in 0..15 {
            let _ = writeln!(nested, "  outer_{i}: {i}");
        }
        nested.push_str("inner_small:\n");
        for i in 0..4 {
            let _ = writeln!(nested, "  outer_{i}: {i}");
        }
        nested.push_str("outer_tail: 100\n");
        let val: crate::Value = crate::from_str(&nested).expect("valid nested large/small maps");
        assert_eq!(val["outer_tail"].as_u64(), Some(100));
        assert_eq!(val["inner_large"]["outer_14"].as_u64(), Some(14));
        assert_eq!(val["inner_small"]["outer_3"].as_u64(), Some(3));

        // Duplicate in outer mapping AFTER closing nested large and small mappings must still fail.
        let mut outer_dup = nested.clone();
        outer_dup.push_str("outer_0: 999\n");
        let err = crate::from_str::<crate::Value>(&outer_dup).expect_err("outer dup after inner");
        assert_eq!(err.code(), ErrorCode::DuplicateKey);

        // Duplicate inside the nested large mapping itself must fail.
        let inner_dup = nested.replace("  outer_14: 14\n", "  outer_14: 14\n  outer_0: 77\n");
        let err = crate::from_str::<crate::Value>(&inner_dup).expect_err("inner large dup");
        assert_eq!(err.code(), ErrorCode::DuplicateKey);
    }

    #[test]
    fn direct_untagged_scalar_fast_paths_match_tree_oracle() {
        use std::collections::{BTreeMap, HashMap};

        #[derive(Debug, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
        enum KeyKind {
            Alpha,
            Beta,
        }

        #[derive(Debug, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
        enum NonUnitKey {
            Wrapped(u32),
        }

        #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
        struct RawBytesKey(Vec<u8>);

        impl<'de> Deserialize<'de> for RawBytesKey {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct BytesVisitor;
                impl serde::de::Visitor<'_> for BytesVisitor {
                    type Value = RawBytesKey;
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("raw bytes")
                    }
                    fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                        Ok(RawBytesKey(v.to_vec()))
                    }
                }
                deserializer.deserialize_byte_buf(BytesVisitor)
            }
        }

        #[derive(Debug, PartialEq, serde::Deserialize)]
        struct ScalarBag<'a> {
            b_true: bool,
            b_false: bool,
            i_small: i8,
            i_big: i128,
            u_big: u128,
            f_plain: f64,
            f_lead_zero: f64,
            f_int: f32,
            s_borrowed: &'a str,
            s_owned: String,
            empty_str: String,
            empty_opt: Option<String>,
            ch: char,
            opt_none: Option<u32>,
            opt_some: Option<u32>,
            opt_spanned: Option<crate::Spanned<u32>>,
            ignored_seq: IgnoredAny,
            ignored_scalar: IgnoredAny,
            spanned_key_map: HashMap<crate::Spanned<String>, u32>,
            #[serde(borrow)]
            spanned_borrowed_key_map: HashMap<crate::Spanned<&'a str>, u32>,
            byte_key_map: BTreeMap<RawBytesKey, u32>,
        }

        let yaml = concat!(
            "b_true: true\n",
            "b_false: false\n",
            "i_small: -42\n",
            "i_big: -170141183460469231731687303715884105728\n",
            "u_big: 340282366920938463463374607431768211455\n",
            "f_plain: 3.5\n",
            "f_lead_zero: 01\n",
            "f_int: 7\n",
            "s_borrowed: hello\n",
            "s_owned: \"escaped\\nline\"\n",
            "empty_str:\n",
            "empty_opt:\n",
            "ch: z\n",
            "opt_none: null\n",
            "opt_some: 19\n",
            "opt_spanned: 77\n",
            "ignored_seq: [1, 2, 3]\n",
            "ignored_scalar: plain_value\n",
            "spanned_key_map:\n",
            "  alpha: 1\n",
            "  \"beta\": 2\n",
            "  \"esc\\nkey\": 3\n",
            "spanned_borrowed_key_map:\n",
            "  plain_k: 10\n",
            "  \"quoted_k\": 20\n",
            "byte_key_map:\n",
            "  rawkey: 5\n",
        );

        let limits = Limits::default();
        let streamed: ScalarBag<'_> = crate::from_str_borrowed(yaml).expect("streamed");
        let tree: ScalarBag<'_> = crate::typed::from_str_by_tree(
            yaml,
            &limits,
            NodeOptions::default(),
            std::marker::PhantomData::<ScalarBag<'_>>,
        )
        .expect("tree");
        assert_eq!(streamed, tree);

        // Streamed non-root radix integers (`0x1F`, `0o7`, `0b1`) into `f64` exercise
        // `visit_untagged_scalar_f64` directly and fail identically to `NodeDeserializer`.
        for radix_yaml in ["x: 0x1F\n", "x: 0o7\n", "x: 0b1\n"] {
            let s_err = crate::from_str::<BTreeMap<String, f64>>(radix_yaml)
                .expect_err("stream radix float");
            let t_err = crate::typed::from_str_by_tree(
                radix_yaml,
                &limits,
                NodeOptions::default(),
                std::marker::PhantomData::<BTreeMap<String, f64>>,
            )
            .expect_err("tree radix float");
            assert_eq!(s_err.code(), t_err.code(), "{radix_yaml:?}");
            assert_eq!(s_err.span(), t_err.span(), "{radix_yaml:?}");
            assert_eq!(s_err.to_string(), t_err.to_string(), "{radix_yaml:?}");
        }

        // Non-unit enum key via `StringKeyDeserializer::deserialize_enum` fails identically.
        let non_unit_yaml = "Wrapped: 1\n";
        let s_nu = crate::from_str::<BTreeMap<NonUnitKey, u32>>(non_unit_yaml)
            .expect_err("stream non-unit enum key");
        let t_nu = crate::typed::from_str_by_tree(
            non_unit_yaml,
            &limits,
            NodeOptions::default(),
            std::marker::PhantomData::<BTreeMap<NonUnitKey, u32>>,
        )
        .expect_err("tree non-unit enum key");
        assert_eq!(s_nu.code(), t_nu.code());
        assert_eq!(s_nu.span(), t_nu.span());
        assert_eq!(s_nu.to_string(), t_nu.to_string());

        // Unit enum key via `StringKeyDeserializer::deserialize_enum`.
        let enum_yaml = "Alpha: 1\nBeta: 2\n";
        let s_map: BTreeMap<KeyKind, u32> = crate::from_str(enum_yaml).expect("stream enum key");
        let t_map: BTreeMap<KeyKind, u32> = crate::typed::from_str_by_tree(
            enum_yaml,
            &limits,
            NodeOptions::default(),
            std::marker::PhantomData::<BTreeMap<KeyKind, u32>>,
        )
        .expect("tree enum key");
        assert_eq!(s_map, t_map);

        // String key rejected when target expects an integer key.
        let bad_key_yaml = "alpha: 1\n";
        let s_bad =
            crate::from_str::<BTreeMap<u32, u32>>(bad_key_yaml).expect_err("stream bad key");
        let t_bad = crate::typed::from_str_by_tree(
            bad_key_yaml,
            &limits,
            NodeOptions::default(),
            std::marker::PhantomData::<BTreeMap<u32, u32>>,
        )
        .expect_err("tree bad key");
        assert_eq!(s_bad.code(), t_bad.code());
        assert_eq!(s_bad.span(), t_bad.span());
    }
}
