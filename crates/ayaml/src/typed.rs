#![allow(
    clippy::mutable_key_type,
    reason = "Node is immutable after construction; its hash cache is internal and never changes equality"
)]

//! Event-backed typed decoding with borrowed strings and source spans.
//!
//! This is a private bridge between the parser's spanned events and Serde. It
//! keeps simple scalar strings borrowed from the input buffer and preserves a
//! source span on every node so typed errors can point at the failing YAML
//! value. `from_value` continues to use the owned value deserializer.

use std::borrow::Cow;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;
#[cfg(feature = "libyaml-compat")]
use serde::Deserializer;
use serde::de::DeserializeSeed;
#[cfg(feature = "libyaml-compat")]
use serde::de::Visitor;

use crate::composer::Anchors;
use crate::error::{Error, ErrorCode, ErrorKind};
use crate::event::{Anchor, Event, ScalarStyle};
use crate::limits::{AliasBudget, Limits};
use crate::number::Number;
use crate::parser;
use crate::resolver::{resolve_explicit_core, resolve_plain};
use crate::span::{Location, Span};
use crate::tag::Tag;
use crate::value::Value;

mod doc_de;
mod key_text;
mod node_de;
mod stream;

pub use doc_de::DocumentDeserializer;

use key_text::{SeenNodeIndices, SeenNodeKeys, node_key_text, scalar_key_text};
use node_de::{NodeDeserializer, check_consumed, inside_key, with_span};

#[derive(Debug)]
pub(crate) struct Node<'de> {
    kind: NodeKind<'de>,
    text: Text<'de>,
    span: Span,
    /// The hash of the resolved kind, cached so nested mapping keys aren't traversed again.
    hash: AtomicU64,
}

/// The text a scalar node keeps beside what it resolves to.
///
/// A null, a boolean, or a number reads into a string target by its text, as the source or its
/// decoding gives it, and a null with no text is a value the source doesn't write. A string is
/// its own text, and keeps the plain text here only for a float target (see `deserialize_f64`).
#[derive(Clone, Debug)]
enum Text<'de> {
    /// No text: a value the source doesn't write, a collection, or a string that isn't plain.
    None,
    /// Closing byte offset of a collection (`SequenceEnd` / `MappingEnd`), used by [`crate::Spanned`]
    /// while keeping `Node.span` equal to the opening span for error-span parity with `Stream`.
    CollectionEnd(usize),
    /// No text, and not a value the source left out: what a bare variant name holds, which is
    /// nothing. It fills a unit variant and an optional one, and no collection (D87).
    Nothing,
    /// A plain scalar whose global tag was dropped by compatibility options: it reads as its text
    /// for `deserialize_any` and parses as the requested boolean or number for a typed target.
    DroppedTag,
    /// A plain scalar's text in the source.
    Plain(&'de str),
    /// The value of a quoted or block scalar that a tag makes a null, a boolean, or a number,
    /// where it is a slice of the source.
    Spelled(&'de str),
    /// The same, where only decoding gives the value, as an escape or a block scalar does.
    Decoded(Box<str>),
}

impl Text<'_> {
    /// The text, if the node keeps any.
    fn as_str(&self) -> Option<&str> {
        match self {
            Self::None | Self::CollectionEnd(_) | Self::Nothing | Self::DroppedTag => None,
            Self::Plain(text) | Self::Spelled(text) => Some(text),
            Self::Decoded(text) => Some(text),
        }
    }
}

impl Clone for Node<'_> {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind.clone(),
            text: self.text.clone(),
            span: self.span,
            hash: AtomicU64::new(self.hash.load(Ordering::Relaxed)),
        }
    }
}

#[derive(Clone, Debug)]
enum NodeKind<'de> {
    Null,
    Bool(bool),
    Number(Number),
    String(Cow<'de, str>),
    Sequence(Vec<Node<'de>>),
    Mapping(Vec<(Node<'de>, Node<'de>)>),
    Tagged(Tag, Box<Node<'de>>),
    Alias(Arc<Node<'de>>),
}

/// Mappings compare as sets of entries, as `Mapping` does, so two keys that differ only in the order
/// of their entries are duplicates.
impl PartialEq for NodeKind<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(left), Self::Bool(right)) => left == right,
            (Self::Number(left), Self::Number(right)) => left == right,
            (Self::String(left), Self::String(right)) => left == right,
            (Self::Sequence(left), Self::Sequence(right)) => left == right,
            (Self::Mapping(left), Self::Mapping(right)) => {
                left.len() == right.len() && {
                    let right: HashSet<_> = right.iter().collect();
                    left.iter().all(|entry| right.contains(entry))
                }
            }
            (Self::Tagged(left_tag, left), Self::Tagged(right_tag, right)) => {
                left_tag == right_tag && left == right
            }
            (Self::Alias(left), Self::Alias(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for NodeKind<'_> {}

impl Hash for NodeKind<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Null => {}
            Self::Bool(value) => value.hash(state),
            Self::Number(value) => value.hash(state),
            Self::String(value) => value.hash(state),
            Self::Sequence(items) => items.hash(state),
            Self::Mapping(entries) => {
                crate::mapping::hash_of_entries(entries.iter().map(|(key, value)| (key, value)))
                    .hash(state);
            }
            Self::Tagged(tag, value) => {
                tag.hash(state);
                value.hash(state);
            }
            Self::Alias(target) => target.hash(state),
        }
    }
}

/// Nodes compare by what they denote: an alias equals its target, so a key
/// written as `*a` duplicates the key that `&a` names.
impl PartialEq for Node<'_> {
    fn eq(&self, other: &Self) -> bool {
        resolve(self).kind == resolve(other).kind
    }
}

impl Eq for Node<'_> {}

impl Node<'_> {
    fn cached_hash(&self) -> u64 {
        let cached = self.hash.load(Ordering::Relaxed);
        if cached != 0 {
            return cached;
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        resolve(self).kind.hash(&mut hasher);
        let hash = hasher.finish().max(1);
        self.hash.store(hash, Ordering::Relaxed);
        hash
    }
}

impl Hash for Node<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.cached_hash().hash(state);
    }
}

impl<'de> Node<'de> {
    fn new(kind: NodeKind<'de>, span: Span) -> Self {
        Self {
            kind,
            text: Text::None,
            span,
            hash: AtomicU64::new(0),
        }
    }

    fn scalar(kind: NodeKind<'de>, text: Text<'de>, span: Span) -> Self {
        Self {
            kind,
            text,
            span,
            hash: AtomicU64::new(0),
        }
    }

    fn null(span: Span) -> Self {
        Self::new(NodeKind::Null, span)
    }

    /// The full byte span of this node (`start..end` for collections, matching `SpannedTree`),
    /// while keeping `self.span` at the opening token for error-span parity with `Stream`.
    pub(super) fn full_span(&self) -> Span {
        match (&self.kind, &self.text) {
            (_, Text::CollectionEnd(end)) => Span::new(self.span.start(), *end),
            (NodeKind::Tagged(_, inner), _) => {
                Span::new(self.span.start(), inner.full_span().end())
            }
            _ => self.span,
        }
    }
}

/// How the typed path reads a document.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NodeOptions {
    pub(crate) allow_duplicates: bool,
    pub(crate) compat_flow: bool,
    pub(crate) drop_global_tags: bool,
}

/// Decode one document with opt-in YAML 1.1 merge-key expansion enabled.
pub(crate) fn from_str_with_merged<'de, T: DeserializeSeed<'de>>(
    input: &'de str,
    limits: &Limits,
    seed: T,
) -> Result<T::Value, Error> {
    let (events, mut budget) = crate::merge::events::expand_single_document(input, limits)?;
    from_events_seed(events, input.len(), limits, &mut budget, seed)
        .map_err(|error| error.with_input_location(input, false))
}

/// Decode one document into a typed value, borrowing scalar strings from input.
pub(crate) fn from_str<'de, T: Deserialize<'de>>(
    input: &'de str,
    limits: &Limits,
) -> Result<T, Error> {
    from_str_with_options(input, limits, NodeOptions::default())
}

pub(crate) fn from_str_with_options<'de, T: Deserialize<'de>>(
    input: &'de str,
    limits: &Limits,
    options: NodeOptions,
) -> Result<T, Error> {
    from_str_seed_with_options(input, limits, options, std::marker::PhantomData::<T>)
}

/// A seed that reads any value with a visitor.
#[cfg(feature = "libyaml-compat")]
struct Any<V>(V);

#[cfg(feature = "libyaml-compat")]
impl<'de, V: Visitor<'de>> DeserializeSeed<'de> for Any<V> {
    type Value = V::Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<V::Value, D::Error> {
        deserializer.deserialize_any(self.0)
    }
}

#[cfg(feature = "libyaml-compat")]
pub(crate) fn from_str_visitor<'de, V: Visitor<'de>>(
    input: &'de str,
    limits: &Limits,
    options: NodeOptions,
    visitor: V,
) -> Result<V::Value, Error> {
    from_str_seed_with_options(input, limits, options, Any(visitor))
}

/// Decode one document with a seed. A small input is built as a tree and read from it, which
/// costs a document of a few bytes the least; a larger one is read from its events, so that no
/// tree of it is kept beside its tokens and its value (see `stream`). Both give the same value
/// or error. What a target can see of the difference is that a collection read from events
/// tells it no size, and that it has read the start of a document that is then refused. A build
/// with `--cfg ayaml_typed_from_events` reads every input from its events, which is how the
/// tests run on that reader at every size.
pub(crate) fn from_str_seed_with_options<'de, T: DeserializeSeed<'de>>(
    input: &'de str,
    limits: &Limits,
    options: NodeOptions,
    seed: T,
) -> Result<T::Value, Error> {
    if input.len() <= SMALL_INPUT_BYTES && !cfg!(ayaml_typed_from_events) {
        return from_str_by_tree(input, limits, options, seed);
    }
    stream::read(input, limits, options, seed)
}

/// Decode one document by building its whole tree first.
pub(crate) fn from_str_by_tree<'de, T: DeserializeSeed<'de>>(
    input: &'de str,
    limits: &Limits,
    options: NodeOptions,
    seed: T,
) -> Result<T::Value, Error> {
    let node = parse_document(input, limits, options)
        .map_err(|error| error.with_input_location(input, options.compat_flow))?;
    seed.deserialize(NodeDeserializer::from_owned(node))
        .map_err(|error| error.with_input_location(input, options.compat_flow))
}

/// Decode one document from its events, whatever its size. The tests compare this with
/// [`from_str_by_tree`].
#[cfg(feature = "bench-internals")]
pub(crate) fn from_str_by_events<'de, T: DeserializeSeed<'de>>(
    input: &'de str,
    limits: &Limits,
    options: NodeOptions,
    seed: T,
) -> Result<T::Value, Error> {
    stream::read(input, limits, options, seed)
}

/// Decode one document from an already-collected event slice, charging its
/// aliases to the stream's budget.
pub(crate) fn from_events<'de, T: Deserialize<'de>>(
    events: Vec<Event<'de>>,
    input_len: usize,
    limits: &Limits,
    budget: &mut AliasBudget,
) -> Result<T, Error> {
    let node = build(events, input_len, limits, NodeOptions::default(), budget)?;
    T::deserialize(NodeDeserializer::from_owned(node))
}

pub(crate) fn from_events_seed<'de, T: DeserializeSeed<'de>>(
    events: Vec<Event<'de>>,
    input_len: usize,
    limits: &Limits,
    budget: &mut AliasBudget,
    seed: T,
) -> Result<T::Value, Error> {
    let node = build(events, input_len, limits, NodeOptions::default(), budget)?;
    seed.deserialize(NodeDeserializer::from_owned(node))
}

/// Decode one document from an already-collected event slice with a seed whose lifetime is
/// independent of the input, preserving per-node source spans without materializing a `Value`.
pub(crate) fn from_events_seed_owned<'a, 'de, T: DeserializeSeed<'de>>(
    events: Vec<Event<'a>>,
    input_len: usize,
    limits: &Limits,
    budget: &mut AliasBudget,
    seed: T,
) -> Result<T::Value, Error> {
    let node = build(events, input_len, limits, NodeOptions::default(), budget)?;
    seed.deserialize(NodeDeserializer::owned_from_owned(node))
}

/// Inputs above this size are read from their events, and built as a tree only to report an
/// error. Smaller reads build the tree directly from the parser without collecting an intermediate
/// event vector.
const SMALL_INPUT_BYTES: usize = 4 * 1024;

/// Parse one document and build its node tree directly as events arrive.
fn parse_document<'de>(
    input: &'de str,
    limits: &Limits,
    options: NodeOptions,
) -> Result<Node<'de>, Error> {
    limits.validate()?;
    // The location is set here: finding it from the span would index the lines of an input
    // that is over the limit, which is the work the limit exists to refuse.
    limits.check_input(input.len()).map_err(|error| {
        Error::from(error)
            .with_span(Span::empty(0))
            .with_location(Location::new(1, 1))
    })?;
    let builder = Builder::streaming(input.len(), limits, options);
    parser::parse_document_typed(input, limits, options.compat_flow, builder)
}

fn build<'de>(
    events: Vec<Event<'de>>,
    input_len: usize,
    limits: &Limits,
    options: NodeOptions,
    budget: &mut AliasBudget,
) -> Result<Node<'de>, Error> {
    let mut builder = Builder {
        events: Some(events.into_iter()),
        limits: limits.clone(),
        input_len,
        anchors: Anchors::new(),
        budget: std::mem::take(budget),
        options,
        stack: Vec::new(),
        document: None,
        root: None,
        in_document: false,
        pending_capacity: None,
        base: 0,
    };
    let result = builder.build();
    *budget = builder.budget;
    result
}

pub(crate) struct Builder<'de> {
    /// A document's events, where they were collected before the builder was made.
    events: Option<std::vec::IntoIter<Event<'de>>>,
    limits: Limits,
    input_len: usize,
    anchors: Anchors<Anchored<'de>>,
    budget: AliasBudget,
    options: NodeOptions,
    stack: Vec<Frame<'de>>,
    document: Option<Node<'de>>,
    root: Option<Node<'de>>,
    in_document: bool,
    pending_capacity: Option<usize>,
    /// The collections open around what this builder builds, which count toward its depth.
    /// `stream` reads those itself and builds only the nodes inside them that it keeps whole.
    base: usize,
}

impl Builder<'_> {
    /// A builder that is fed events directly from the parser.
    pub(super) fn streaming(input_len: usize, limits: &Limits, options: NodeOptions) -> Self {
        Self {
            events: None,
            limits: limits.clone(),
            input_len,
            anchors: Anchors::new(),
            budget: AliasBudget::default(),
            options,
            stack: Vec::new(),
            document: None,
            root: None,
            in_document: false,
            pending_capacity: None,
            base: 0,
        }
    }

    pub(crate) fn reserve_collection(&mut self, entries: usize) {
        // A collection with more entries than the limit is refused, so it needs no more room.
        self.pending_capacity = Some(entries.min(self.limits.max_collection_entries));
    }
}

/// An anchored node, shared with every alias to it, with its size estimate
/// and nesting height computed once, bottom-up.
#[derive(Clone)]
struct Anchored<'de> {
    node: Arc<Node<'de>>,
    size: usize,
    height: usize,
}

/// A finished node with its size estimate and its nesting height (the
/// collections on its deepest path; zero for a scalar).
struct Built<'de> {
    node: Node<'de>,
    size: usize,
    height: usize,
}

/// A scalar key already seen in an open mapping, compared by its text alone so `1` and `"1"` are
/// one key to a target whose key type would keep only one of them. Non-scalar (collection) keys
/// are indexed separately by their cached node hash without cloning.
type SeenKey<'de> = Cow<'de, str>;

/// An open collection. The builder keeps these on an explicit stack, so
/// nesting depth costs heap, not native stack. The mapping frame is the larger variant because it
/// keeps the duplicate-key sets; one frame is live per open collection, never on the native stack.
#[allow(
    clippy::large_enum_variant,
    reason = "the mapping frame owns its duplicate-key set and is kept inline to avoid an allocation per open collection"
)]
enum Frame<'de> {
    Sequence {
        tag: Option<Tag>,
        anchor: Option<Anchor<'de>>,
        span: Span,
        items: Vec<Node<'de>>,
        size: usize,
        height: usize,
    },
    Mapping {
        tag: Option<Tag>,
        anchor: Option<Anchor<'de>>,
        span: Span,
        entries: Vec<(Node<'de>, Node<'de>)>,
        seen: HashSet<SeenKey<'de>>,
        /// Non-scalar keys indexed by cached node hash, referring into `entries` without cloning.
        seen_nodes: Option<Box<SeenNodeIndices>>,
        key: Option<Node<'de>>,
        size: usize,
        height: usize,
    },
}

impl<'de> Builder<'de> {
    pub(crate) fn push_event(&mut self, event: Event<'de>) -> Result<(), Box<Error>> {
        self.push(event).map_err(Box::new)
    }

    #[inline]
    pub(crate) fn push_scalar(
        &mut self,
        value: Cow<'de, str>,
        raw: &'de str,
        style: ScalarStyle,
        tag: Option<&Tag>,
        anchor: Option<&Anchor<'de>>,
        span: Span,
    ) -> Result<(), Box<Error>> {
        if !self.in_document {
            return Err(Box::new(self.error_at(span, "expected a document start")));
        }
        self.pending_capacity = None;
        if self.root.is_some() && self.stack.is_empty() {
            return Err(Box::new(self.error_at(span, "expected document end")));
        }
        let raw = Cow::Borrowed(raw);
        let node = plain_scalar(value, &raw, style, tag, span, self.options.drop_global_tags)
            .map_err(Box::new)?;
        let size = estimate_size(&node, self.limits.node_cost_bytes);
        let built = self.anchored(anchor, node, size, 0, Anchors::define);
        self.attach_built(built).map_err(Box::new)
    }

    pub(crate) fn finish_document_node(&mut self) -> Result<Node<'de>, Error> {
        self.finish_document()
    }
}

impl<'de> Builder<'de> {
    fn build(&mut self) -> Result<Node<'de>, Error> {
        let mut document = None;
        while let Some(event) = self.next_event() {
            match event {
                Event::StreamStart { .. } | Event::Directive { .. } => {}
                Event::StreamEnd { .. } => break,
                Event::DocumentStart { span, .. } => {
                    if document.is_some() {
                        return Err(Error::new(
                            ErrorKind::Parser,
                            "expected a single YAML document, but found trailing documents",
                        )
                        .with_span(span)
                        .with_code(ErrorCode::TrailingDocuments));
                    }
                    self.anchors.clear();
                    self.stack.clear();
                    self.root = None;
                    self.in_document = true;
                    loop {
                        let Some(event) = self.next_event() else {
                            return Err(self.error_at(
                                Span::empty(self.input_len),
                                "unexpected end of event stream",
                            ));
                        };
                        if let Event::DocumentEnd { span, .. } = event {
                            if !self.stack.is_empty() {
                                return Err(
                                    self.error_at(span, "unexpected event in node position")
                                );
                            }
                            let node = self
                                .root
                                .take()
                                .unwrap_or_else(|| Node::null(Span::empty(self.input_len)));
                            document = Some(node);
                            self.in_document = false;
                            break;
                        }
                        self.push_node(event)?;
                        if self.root.is_some() {
                            match self.next_event() {
                                Some(Event::DocumentEnd { .. }) => {
                                    let node = self
                                        .root
                                        .take()
                                        .unwrap_or_else(|| Node::null(Span::empty(self.input_len)));
                                    document = Some(node);
                                    self.in_document = false;
                                    break;
                                }
                                Some(event) => {
                                    return Err(
                                        self.error_at(event.span(), "expected document end")
                                    );
                                }
                                None => {
                                    return Err(self.error_at(
                                        Span::empty(self.input_len),
                                        "expected document end",
                                    ));
                                }
                            }
                        }
                    }
                }
                event => {
                    return Err(self.error_at(event.span(), "expected a document start"));
                }
            }
        }
        Ok(document.unwrap_or_else(|| Node::null(Span::empty(self.input_len))))
    }

    fn next_event(&mut self) -> Option<Event<'de>> {
        self.events.as_mut().and_then(Iterator::next)
    }

    fn push(&mut self, event: Event<'de>) -> Result<(), Error> {
        match event {
            Event::StreamStart { .. } | Event::Directive { .. } => Ok(()),
            Event::StreamEnd { .. } => {
                if self.in_document {
                    Err(self.error_at(
                        Span::empty(self.input_len),
                        "unexpected end of event stream",
                    ))
                } else {
                    Ok(())
                }
            }
            Event::DocumentStart { span, .. } => {
                if self.document.is_some() {
                    return Err(Error::new(
                        ErrorKind::Parser,
                        "expected a single YAML document, but found trailing documents",
                    )
                    .with_span(span)
                    .with_code(ErrorCode::TrailingDocuments));
                }
                if self.in_document {
                    return Err(self.error_at(
                        span,
                        if self.root.is_some() {
                            "expected document end"
                        } else {
                            "unexpected event in node position"
                        },
                    ));
                }
                self.anchors.clear();
                self.stack.clear();
                self.root = None;
                self.in_document = true;
                Ok(())
            }
            Event::DocumentEnd { span, .. } => {
                if !self.in_document {
                    return Err(self.error_at(span, "expected a document start"));
                }
                if !self.stack.is_empty() {
                    return Err(self.error_at(span, "unexpected event in node position"));
                }
                let node = self
                    .root
                    .take()
                    .unwrap_or_else(|| Node::null(Span::empty(self.input_len)));
                self.document = Some(node);
                self.in_document = false;
                Ok(())
            }
            event => {
                if !self.in_document {
                    return Err(self.error_at(event.span(), "expected a document start"));
                }
                self.push_node(event)
            }
        }
    }

    fn finish_document(&mut self) -> Result<Node<'de>, Error> {
        if self.in_document {
            return Err(self.error_at(
                Span::empty(self.input_len),
                "unexpected end of event stream",
            ));
        }
        Ok(self
            .document
            .take()
            .unwrap_or_else(|| Node::null(Span::empty(self.input_len))))
    }

    /// Feed one node event to the open document. A node that completes the root is kept until
    /// `DocumentEnd`; any other node attaches to the open collection on the stack.
    fn push_node(&mut self, event: Event<'de>) -> Result<(), Error> {
        let event_span = event.span();
        let capacity = self.pending_capacity.take().unwrap_or(0);
        if self.root.is_some() && self.stack.is_empty() {
            return Err(self.error_at(event_span, "expected document end"));
        }
        let built = match event {
            Event::Scalar {
                value,
                raw,
                style,
                tag,
                anchor,
                span,
            } => {
                let node = plain_scalar(
                    value,
                    &raw,
                    style,
                    tag.as_ref(),
                    span,
                    self.options.drop_global_tags,
                )?;
                let size = estimate_size(&node, self.limits.node_cost_bytes);
                self.anchored(anchor.as_ref(), node, size, 0, Anchors::define)
            }
            Event::Alias { anchor, span } => {
                let target = self
                    .anchors
                    .resolve(anchor.as_str())
                    .cloned()
                    .map_err(|(code, message)| self.error_at(span, message).with_code(code))?;
                self.budget
                    .charge(&self.limits, self.input_len, target.size)
                    .map_err(|error| error.with_span(span))?;
                // The alias nests its target here, so the target's collections count toward the
                // depth.
                if target.height > 0 {
                    self.limits
                        .check_depth(self.base + self.stack.len() + target.height)
                        .map_err(|error| Error::from(error).with_span(span))?;
                }
                Built {
                    node: Node::new(NodeKind::Alias(target.node), span),
                    size: target.size,
                    height: target.height,
                }
            }
            Event::SequenceStart {
                tag, anchor, span, ..
            } => {
                self.limits
                    .check_depth(self.base + self.stack.len() + 1)
                    .map_err(|error| Error::from(error).with_span(span))?;
                if let Some(anchor) = &anchor {
                    self.anchors.open(anchor.as_str());
                }
                self.stack.push(Frame::Sequence {
                    tag,
                    anchor,
                    span,
                    items: Vec::with_capacity(capacity),
                    size: self.limits.node_cost_bytes,
                    height: 0,
                });
                return Ok(());
            }
            Event::MappingStart {
                tag, anchor, span, ..
            } => {
                self.limits
                    .check_depth(self.base + self.stack.len() + 1)
                    .map_err(|error| Error::from(error).with_span(span))?;
                if let Some(anchor) = &anchor {
                    self.anchors.open(anchor.as_str());
                }
                self.stack.push(Frame::Mapping {
                    tag,
                    anchor,
                    span,
                    entries: Vec::with_capacity(capacity),
                    seen: HashSet::new(),
                    seen_nodes: None,
                    key: None,
                    size: self.limits.node_cost_bytes,
                    height: 0,
                });
                return Ok(());
            }
            Event::SequenceEnd { .. } => match self.stack.pop() {
                Some(Frame::Sequence {
                    tag,
                    anchor,
                    span,
                    items,
                    size,
                    height,
                }) => {
                    let node = apply_tag(
                        tag,
                        Node::scalar(
                            NodeKind::Sequence(items),
                            Text::CollectionEnd(event_span.end()),
                            span,
                        ),
                        self.options.drop_global_tags,
                    );
                    let size = size.saturating_add(tag_cost(&node, self.limits.node_cost_bytes));
                    self.anchored(anchor.as_ref(), node, size, height + 1, Anchors::close)
                }
                _ => return Err(self.error_at(event_span, "unexpected sequence end")),
            },
            Event::MappingEnd { .. } => match self.stack.pop() {
                Some(Frame::Mapping {
                    tag,
                    anchor,
                    span,
                    entries,
                    key: None,
                    size,
                    height,
                    ..
                }) => {
                    let node = apply_tag(
                        tag,
                        Node::scalar(
                            NodeKind::Mapping(entries),
                            Text::CollectionEnd(event_span.end()),
                            span,
                        ),
                        self.options.drop_global_tags,
                    );
                    let size = size.saturating_add(tag_cost(&node, self.limits.node_cost_bytes));
                    self.anchored(anchor.as_ref(), node, size, height + 1, Anchors::close)
                }
                _ => return Err(self.error_at(event_span, "unexpected mapping end")),
            },
            _ => return Err(self.error_at(event_span, "unexpected event in node position")),
        };
        self.attach_built(built)
    }

    #[inline]
    fn attach_built(&mut self, built: Built<'de>) -> Result<(), Error> {
        match self.stack.last_mut() {
            None => self.root = Some(built.node),
            Some(Frame::Sequence {
                items,
                size,
                height,
                ..
            }) => {
                *size = size.saturating_add(built.size);
                *height = (*height).max(built.height);
                items.push(built.node);
            }
            Some(Frame::Mapping {
                entries,
                seen,
                seen_nodes,
                key,
                size,
                height,
                ..
            }) => {
                *size = size.saturating_add(built.size);
                *height = (*height).max(built.height);
                match key.take() {
                    None => *key = Some(built.node),
                    Some(entry_key) => {
                        if !self.options.allow_duplicates {
                            let duplicate = if let Some(text) = scalar_key_text(&entry_key) {
                                !seen.insert(text)
                            } else {
                                seen_nodes
                                    .get_or_insert_with(Box::default)
                                    .insert_is_duplicate(entries, &entry_key, entries.len())
                            };
                            if duplicate {
                                return Err(Error::new(
                                    ErrorKind::DuplicateKey,
                                    format!("duplicate mapping key: {}", node_key_text(&entry_key)),
                                )
                                .with_span(entry_key.span));
                            }
                        }
                        entries.push((entry_key, built.node));
                    }
                }
            }
        }
        Ok(())
    }

    /// Finish a node. An anchored node moves into an `Arc` that its aliases
    /// share, and the tree refers to it through the same `Arc`, so the node
    /// is never copied.
    fn anchored(
        &mut self,
        anchor: Option<&Anchor<'de>>,
        node: Node<'de>,
        size: usize,
        height: usize,
        record: fn(&mut Anchors<Anchored<'de>>, &str, Anchored<'de>),
    ) -> Built<'de> {
        let Some(anchor) = anchor else {
            return Built { node, size, height };
        };
        let span = node.span;
        let end = node.full_span().end();
        let text = if end > span.end() {
            Text::CollectionEnd(end)
        } else {
            Text::None
        };
        let shared = Arc::new(node);
        let target = Anchored {
            node: Arc::clone(&shared),
            size,
            height,
        };
        record(&mut self.anchors, anchor.as_str(), target);
        Built {
            node: Node::scalar(NodeKind::Alias(shared), text, span),
            size,
            height,
        }
    }

    #[allow(
        clippy::unused_self,
        reason = "the method is part of a uniform grammar or diagnostic surface"
    )]
    fn error_at(&self, span: Span, message: impl Into<String>) -> Error {
        Error::new(ErrorKind::Composition, message).with_span(span)
    }
}

/// The node an alias or a shared anchored node refers to.
fn resolve<'a, 'de>(mut node: &'a Node<'de>) -> &'a Node<'de> {
    while let NodeKind::Alias(target) = &node.kind {
        node = target.as_ref();
    }
    node
}

/// Describe a node's actual kind to Serde's `invalid_type` error.
fn unexpected_node<'a>(node: &'a Node<'_>) -> serde::de::Unexpected<'a> {
    match &node.kind {
        NodeKind::Null => serde::de::Unexpected::Unit,
        NodeKind::Bool(value) => serde::de::Unexpected::Bool(*value),
        NodeKind::Number(value) => crate::serde_impl::unexpected_number(*value),
        NodeKind::String(value) => serde::de::Unexpected::Str(value),
        NodeKind::Sequence(_) => serde::de::Unexpected::Seq,
        NodeKind::Mapping(_) => serde::de::Unexpected::Map,
        NodeKind::Tagged(_, inner) => unexpected_node(inner),
        NodeKind::Alias(inner) => unexpected_node(inner),
    }
}

/// The node a target that isn't an enum, a string, or any value reads: past aliases, and past a
/// tag outside YAML's own `tag:yaml.org,2002:` names. That tag means what the application says,
/// not something a number or a sequence can hold, so `!port 80` reads as `80` into a `u16`. A
/// YAML tag that a node keeps, as a collection keeps `!!binary`, still reaches the target as an
/// enum.
fn untagged<'a, 'de>(node: &'a Node<'de>) -> &'a Node<'de> {
    let mut node = resolve(node);
    while let NodeKind::Tagged(tag, inner) = &node.kind
        && !tag.is_core()
    {
        node = resolve(inner);
    }
    node
}

/// What a collection's tag adds to the size of an alias to it: the node that holds the tag, and
/// its text, as the composer charges a tagged value.
fn tag_cost(node: &Node<'_>, node_cost: usize) -> usize {
    match &node.kind {
        NodeKind::Tagged(tag, _) => node_cost.saturating_add(tag.text_len()),
        _ => 0,
    }
}

/// Whether a collection drops this tag: a core tag there names a node of another kind, or
/// restates its kind. Under `drop_global_tags`, any non-core global tag is dropped too.
fn collection_drops(tag: &Tag, drop_global_tags: bool) -> bool {
    tag.is_core_collection()
        || tag.is_core_scalar()
        || (drop_global_tags && tag.is_non_core_global())
}

fn apply_tag(tag: Option<Tag>, node: Node<'_>, drop_global_tags: bool) -> Node<'_> {
    match tag {
        Some(tag) if collection_drops(&tag, drop_global_tags) => node,
        Some(tag) => {
            let span = node.span;
            Node::new(NodeKind::Tagged(tag, Box::new(node)), span)
        }
        None => node,
    }
}

/// The node of a scalar event, before any anchor on it is recorded.
fn plain_scalar<'de>(
    value: Cow<'de, str>,
    raw: &Cow<'de, str>,
    style: ScalarStyle,
    tag: Option<&Tag>,
    span: Span,
    drop_global_tags: bool,
) -> Result<Node<'de>, Error> {
    if drop_global_tags && tag.is_some_and(Tag::is_non_core_global) {
        let scalar_text = borrowed_or_owned(value, raw, style);
        let text = if style == ScalarStyle::Plain {
            Text::DroppedTag
        } else {
            Text::None
        };
        return Ok(Node::scalar(NodeKind::String(scalar_text), text, span));
    }
    let mut node =
        scalar_node(value, raw, style, tag, span).map_err(|error| error.with_span(span))?;
    if tag.is_some_and(|tag| tag.is_core() || tag.is_non_specific()) {
        forget_string_text(&mut node);
    }
    Ok(node)
}

/// Build a scalar node without materializing a `Value` when the scalar stays a string, and
/// resolving plain scalars at most once.
fn scalar_node<'de>(
    value: Cow<'de, str>,
    raw: &Cow<'de, str>,
    style: ScalarStyle,
    tag: Option<&Tag>,
    span: Span,
) -> Result<Node<'de>, Error> {
    let scalar_text = borrowed_or_owned(value, raw, style);
    match tag {
        None => {
            if style != ScalarStyle::Plain {
                return Ok(Node::scalar(
                    NodeKind::String(scalar_text),
                    Text::None,
                    span,
                ));
            }
            match resolve_plain(scalar_text.as_ref()) {
                None => Ok(Node::scalar(
                    NodeKind::String(scalar_text),
                    string_text(raw, style),
                    span,
                )),
                Some(resolved) => Ok(node_from_value(resolved, scalar_text, raw, style, span)),
            }
        }
        Some(tag) if tag.is_non_specific() => {
            let inner = Node::scalar(NodeKind::String(scalar_text), Text::None, span);
            Ok(Node::scalar(
                NodeKind::Tagged(tag.clone(), Box::new(inner)),
                Text::None,
                span,
            ))
        }
        Some(tag) if tag.is_core() => match tag.suffix() {
            "str" => Ok(Node::scalar(
                NodeKind::String(scalar_text),
                string_text(raw, style),
                span,
            )),
            "int" | "float" | "bool" | "null" => {
                let resolved = resolve_explicit_core(scalar_text.as_ref(), tag.suffix(), tag)
                    .map_err(|error| error.with_span(span))?;
                Ok(node_from_value(resolved, scalar_text, raw, style, span))
            }
            _ => {
                let inner = Node::scalar(NodeKind::String(scalar_text), Text::None, span);
                Ok(Node::scalar(
                    NodeKind::Tagged(tag.clone(), Box::new(inner)),
                    Text::None,
                    span,
                ))
            }
        },
        Some(tag) => {
            let inner = if style == ScalarStyle::Plain {
                match resolve_plain(scalar_text.as_ref()) {
                    None => {
                        // A custom tag doesn't make the scalar a string, its text does, so the
                        // string keeps that text as an untagged one would: `!port 080` reads into
                        // a float.
                        Node::scalar(NodeKind::String(scalar_text), string_text(raw, style), span)
                    }
                    Some(resolved) => node_from_value(resolved, scalar_text, raw, style, span),
                }
            } else {
                Node::scalar(NodeKind::String(scalar_text), Text::None, span)
            };
            Ok(Node::scalar(
                NodeKind::Tagged(tag.clone(), Box::new(inner)),
                Text::None,
                span,
            ))
        }
    }
}

/// The text a string keeps beside its value: a plain scalar's, which only a float target reads
/// (see `deserialize_f64`).
fn string_text<'de>(raw: &Cow<'de, str>, style: ScalarStyle) -> Text<'de> {
    match raw {
        Cow::Borrowed(text) if style == ScalarStyle::Plain => Text::Plain(text),
        _ => Text::None,
    }
}

fn node_from_value<'de>(
    value: Value,
    scalar_text: Cow<'de, str>,
    raw: &Cow<'de, str>,
    style: ScalarStyle,
    span: Span,
) -> Node<'de> {
    let (kind, scalar_text) = match value {
        Value::Null => (NodeKind::Null, scalar_text),
        Value::Bool(value) => (NodeKind::Bool(value), scalar_text),
        Value::Number(value) => (NodeKind::Number(value), scalar_text),
        Value::String(_) => (NodeKind::String(scalar_text.clone()), scalar_text),
        Value::Sequence(sequence) => (
            NodeKind::Sequence(
                sequence
                    .into_iter()
                    .map(|value| owned_node(value, span))
                    .collect(),
            ),
            scalar_text,
        ),
        Value::Mapping(mapping) => (
            NodeKind::Mapping(
                mapping
                    .into_iter()
                    .map(|(key, value)| (owned_node(key, span), owned_node(value, span)))
                    .collect(),
            ),
            scalar_text,
        ),
        Value::Tagged(tagged) => {
            let inner = node_from_value(tagged.value, scalar_text.clone(), raw, style, span);
            (NodeKind::Tagged(tagged.tag, Box::new(inner)), scalar_text)
        }
    };
    // A plain scalar keeps its source text. A quoted or block scalar reaches here only when a
    // tag makes it a null, a boolean, or a number, and keeps its value, which is what a string
    // target reads and what tells an explicit null from a value that isn't written.
    // The parser gives a plain scalar's text as a slice of the source. Text that isn't one is
    // kept all the same: a null or a number with no text would read as a value that isn't
    // written, and a number key would no longer name a field by its text.
    let text = match (style, raw, scalar_text) {
        (ScalarStyle::Plain, Cow::Borrowed(text), _) => Text::Plain(text),
        (ScalarStyle::Plain, Cow::Owned(text), _) => Text::Decoded(text.as_str().into()),
        (_, _, Cow::Borrowed(value)) => Text::Spelled(value),
        (_, _, Cow::Owned(value)) => Text::Decoded(value.into_boxed_str()),
    };
    Node::scalar(kind, text, span)
}

/// A string that a tag makes one, as in `!!str 01` or `! 01`, keeps no text, which only a float
/// target reads from a string (see `deserialize_f64`), so that it reads as a string there too.
fn forget_string_text(node: &mut Node<'_>) {
    let node = match &mut node.kind {
        NodeKind::Tagged(_, inner) => inner.as_mut(),
        _ => node,
    };
    if matches!(node.kind, NodeKind::String(_)) {
        node.text = Text::None;
    }
}

fn owned_node<'de>(value: Value, span: Span) -> Node<'de> {
    let kind = match value {
        Value::Null => NodeKind::Null,
        Value::Bool(value) => NodeKind::Bool(value),
        Value::Number(value) => NodeKind::Number(value),
        Value::String(value) => NodeKind::String(Cow::Owned(value)),
        Value::Sequence(sequence) => NodeKind::Sequence(
            sequence
                .into_iter()
                .map(|value| owned_node(value, span))
                .collect(),
        ),
        Value::Mapping(mapping) => NodeKind::Mapping(
            mapping
                .into_iter()
                .map(|(key, value)| (owned_node(key, span), owned_node(value, span)))
                .collect(),
        ),
        Value::Tagged(tagged) => {
            NodeKind::Tagged(tagged.tag, Box::new(owned_node(tagged.value, span)))
        }
    };
    Node::new(kind, span)
}

fn borrowed_or_owned<'de>(
    value: Cow<'de, str>,
    raw: &Cow<'de, str>,
    style: ScalarStyle,
) -> Cow<'de, str> {
    match value {
        Cow::Borrowed(borrowed) => Cow::Borrowed(borrowed),
        Cow::Owned(owned) => {
            if let Cow::Borrowed(raw) = raw {
                let candidate = match style {
                    // A plain scalar's text keeps the blanks before a comment or the line's end,
                    // and its value doesn't.
                    ScalarStyle::Plain => Some(raw.trim_end_matches([' ', '\t'])),
                    ScalarStyle::SingleQuoted | ScalarStyle::DoubleQuoted => {
                        raw.get(1..raw.len().saturating_sub(1))
                    }
                    ScalarStyle::Literal | ScalarStyle::Folded => None,
                };
                if let Some(candidate) = candidate
                    && candidate == owned
                {
                    return Cow::Borrowed(candidate);
                }
            }
            Cow::Owned(owned)
        }
    }
}

/// The size estimate of a scalar's node: its cost plus its text. Collections
/// and aliases are summed bottom-up by the builder instead.
///
/// A null, a boolean, or a number keeps its text beside what it resolves to, and a string
/// target copies that text at every alias to it. So such a node costs its text where that is
/// the larger, which only a number spelled at great length is: any other costs a node, as it
/// does where a value is composed.
fn estimate_size(node: &Node<'_>, node_cost: usize) -> usize {
    match &node.kind {
        NodeKind::Null | NodeKind::Bool(_) | NodeKind::Number(_) => {
            node_cost.max(node.text.as_str().map_or(0, str::len))
        }
        NodeKind::String(text) => node_cost.saturating_add(text.len()),
        NodeKind::Sequence(items) => items.iter().fold(node_cost, |size, item| {
            size.saturating_add(estimate_size(item, node_cost))
        }),
        NodeKind::Mapping(entries) => entries.iter().fold(node_cost, |size, (key, value)| {
            size.saturating_add(estimate_size(key, node_cost))
                .saturating_add(estimate_size(value, node_cost))
        }),
        NodeKind::Tagged(tag, value) => node_cost
            .saturating_add(tag.text_len())
            .saturating_add(estimate_size(value, node_cost)),
        NodeKind::Alias(target) => estimate_size(target, node_cost),
    }
}

#[cfg(test)]
mod tests;
