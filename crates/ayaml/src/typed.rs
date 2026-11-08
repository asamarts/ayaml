//! Event-backed typed decoding with borrowed strings and source spans.
//!
//! This is a private bridge between the parser's spanned events and Serde. It
//! keeps simple scalar strings borrowed from the input buffer and preserves a
//! source span on every node so typed errors can point at the failing YAML
//! value. `from_value` continues to use the owned value deserializer.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use serde::de::{DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::composer::Anchors;
use crate::error::{Error, ErrorKind};
use crate::event::{Anchor, Event, ScalarStyle};
use crate::limits::{AliasBudget, Limits};
use crate::number::Number;
use crate::parser;
use crate::resolver::{float_from_text, resolve_scalar};
use crate::span::{Marker, Span};
use crate::tag::Tag;
use crate::value::Value;

#[derive(Clone, Debug)]
struct Node<'de> {
    kind: NodeKind<'de>,
    text: Option<Cow<'de, str>>,
    style: ScalarStyle,
    span: Span,
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

impl Hash for Node<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        resolve(self).kind.hash(state);
    }
}

impl<'de> Node<'de> {
    fn new(kind: NodeKind<'de>, span: Span) -> Self {
        Self {
            kind,
            text: None,
            style: ScalarStyle::Plain,
            span,
        }
    }

    fn scalar(kind: NodeKind<'de>, text: Cow<'de, str>, style: ScalarStyle, span: Span) -> Self {
        Self {
            kind,
            text: Some(text),
            style,
            span,
        }
    }

    fn null(span: Span) -> Self {
        Self::new(NodeKind::Null, span)
    }
}

/// How the typed path reads a document.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NodeOptions {
    pub(crate) allow_duplicates: bool,
    pub(crate) compat_flow: bool,
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
    let node = parse_document(input, limits, options)?;
    T::deserialize(NodeDeserializer::new(&node))
}

#[cfg(feature = "libyaml-compat")]
pub(crate) fn from_str_visitor<'de, V: Visitor<'de>>(
    input: &'de str,
    limits: &Limits,
    options: NodeOptions,
    visitor: V,
) -> Result<V::Value, Error> {
    let node = parse_document(input, limits, options)?;
    NodeDeserializer::new(&node).deserialize_any(visitor)
}

#[cfg(feature = "libyaml-compat")]
pub(crate) fn from_str_seed_with_options<'de, T: DeserializeSeed<'de>>(
    input: &'de str,
    limits: &Limits,
    options: NodeOptions,
    seed: T,
) -> Result<T::Value, Error> {
    let node = parse_document(input, limits, options)?;
    seed.deserialize(NodeDeserializer::new(&node))
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
    T::deserialize(NodeDeserializer::new(&node))
}

pub(crate) fn from_events_seed<'de, T: DeserializeSeed<'de>>(
    events: Vec<Event<'de>>,
    input_len: usize,
    limits: &Limits,
    budget: &mut AliasBudget,
    seed: T,
) -> Result<T::Value, Error> {
    let node = build(events, input_len, limits, NodeOptions::default(), budget)?;
    seed.deserialize(NodeDeserializer::new(&node))
}

/// Parse one document and build its node tree.
fn parse_document<'de>(
    input: &'de str,
    limits: &Limits,
    options: NodeOptions,
) -> Result<Node<'de>, Error> {
    limits
        .check_input(input.len())
        .map_err(|error| Error::from(error).with_span(Span::empty(Marker::new(0, 1, 1))))?;
    let events = parser::parse_document_events(input, limits, options.compat_flow)?;
    build(
        events,
        input.len(),
        limits,
        options,
        &mut AliasBudget::default(),
    )
}

fn build<'de>(
    events: Vec<Event<'de>>,
    input_len: usize,
    limits: &Limits,
    options: NodeOptions,
    budget: &mut AliasBudget,
) -> Result<Node<'de>, Error> {
    Builder {
        events,
        index: 0,
        limits: limits.clone(),
        input_len,
        anchors: Anchors::new(),
        budget,
        options,
    }
    .build()
}

struct Builder<'de, 'b> {
    events: Vec<Event<'de>>,
    index: usize,
    limits: Limits,
    input_len: usize,
    anchors: Anchors<Anchored<'de>>,
    budget: &'b mut AliasBudget,
    options: NodeOptions,
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

/// An open collection. The builder keeps these on an explicit stack, so
/// nesting depth costs heap, not native stack.
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
        seen: HashMap<Node<'de>, Span>,
        key: Option<Node<'de>>,
        size: usize,
        height: usize,
    },
}

impl<'de> Builder<'de, '_> {
    fn build(mut self) -> Result<Node<'de>, Error> {
        let mut document = None;
        while self.index < self.events.len() {
            match &self.events[self.index] {
                Event::StreamStart { .. } | Event::Directive { .. } => {
                    self.index += 1;
                }
                Event::StreamEnd { .. } => {
                    self.index += 1;
                    break;
                }
                Event::DocumentStart { .. } => {
                    if document.is_some() {
                        return Err(Error::new(
                            ErrorKind::Parser,
                            "expected a single YAML document, but found trailing documents",
                        ));
                    }
                    self.index += 1;
                    self.anchors.clear();
                    let node = self.root()?;
                    match self.events.get(self.index) {
                        Some(Event::DocumentEnd { .. }) => self.index += 1,
                        _ => return Err(self.error_at(self.span(), "expected document end")),
                    }
                    document = Some(node);
                }
                _ => return Err(self.error_at(self.span(), "expected a document start")),
            }
        }
        Ok(document.unwrap_or_else(|| Node::null(Span::empty(Marker::new(self.input_len, 1, 1)))))
    }

    /// Build one document's root node.
    fn root(&mut self) -> Result<Node<'de>, Error> {
        let mut stack: Vec<Frame<'de>> = Vec::new();
        loop {
            let span = self.span();
            let Some(slot) = self.events.get_mut(self.index) else {
                return Err(self.error_at(span, "unexpected end of event stream"));
            };
            // Each event is read once, so it can be moved out of the vector.
            let event = std::mem::replace(slot, Event::StreamEnd { span });
            self.index += 1;
            let built = match event {
                Event::Scalar {
                    value,
                    raw,
                    style,
                    tag,
                    anchor,
                    span,
                } => {
                    let resolved = resolve_scalar(value.as_ref(), style, tag.as_ref())
                        .map_err(|error| error.with_span(span))?;
                    let mut node = node_from_value(resolved, &value, &raw, style, span);
                    if tag.as_ref().is_some_and(|tag| {
                        tag.is_core() || (tag.handle() == "!" && tag.suffix().is_empty())
                    }) {
                        forget_string_text(&mut node);
                    }
                    let size = estimate_size(&node, self.limits.node_cost_bytes);
                    self.anchored(anchor.as_ref(), node, size, 0, Anchors::define)
                }
                Event::Alias { anchor, span } => {
                    let target = self
                        .anchors
                        .resolve(anchor.as_str())
                        .cloned()
                        .map_err(|message| self.error_at(span, message))?;
                    self.budget
                        .charge(&self.limits, self.input_len, target.size)
                        .map_err(|error| error.with_span(span))?;
                    // The alias nests its target here, so the target's
                    // collections count toward the depth.
                    if target.height > 0 {
                        self.limits
                            .check_depth(stack.len() + target.height)
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
                        .check_depth(stack.len() + 1)
                        .map_err(|error| Error::from(error).with_span(span))?;
                    if let Some(anchor) = &anchor {
                        self.anchors.open(anchor.as_str());
                    }
                    stack.push(Frame::Sequence {
                        tag,
                        anchor,
                        span,
                        items: Vec::new(),
                        size: self.limits.node_cost_bytes,
                        height: 0,
                    });
                    continue;
                }
                Event::MappingStart {
                    tag, anchor, span, ..
                } => {
                    self.limits
                        .check_depth(stack.len() + 1)
                        .map_err(|error| Error::from(error).with_span(span))?;
                    if let Some(anchor) = &anchor {
                        self.anchors.open(anchor.as_str());
                    }
                    stack.push(Frame::Mapping {
                        tag,
                        anchor,
                        span,
                        entries: Vec::new(),
                        seen: HashMap::new(),
                        key: None,
                        size: self.limits.node_cost_bytes,
                        height: 0,
                    });
                    continue;
                }
                Event::SequenceEnd { .. } => match stack.pop() {
                    Some(Frame::Sequence {
                        tag,
                        anchor,
                        span,
                        items,
                        size,
                        height,
                    }) => {
                        let node = apply_tag(tag, Node::new(NodeKind::Sequence(items), span));
                        let size =
                            size.saturating_add(tag_cost(&node, self.limits.node_cost_bytes));
                        self.anchored(anchor.as_ref(), node, size, height + 1, Anchors::close)
                    }
                    _ => return Err(self.error_at(span, "unexpected sequence end")),
                },
                Event::MappingEnd { .. } => match stack.pop() {
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
                        let node = apply_tag(tag, Node::new(NodeKind::Mapping(entries), span));
                        let size =
                            size.saturating_add(tag_cost(&node, self.limits.node_cost_bytes));
                        self.anchored(anchor.as_ref(), node, size, height + 1, Anchors::close)
                    }
                    _ => return Err(self.error_at(span, "unexpected mapping end")),
                },
                _ => return Err(self.error_at(span, "unexpected event in node position")),
            };
            match stack.last_mut() {
                None => return Ok(built.node),
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
                                if let Some(first_span) = seen.get(&entry_key) {
                                    return Err(Error::new(
                                        ErrorKind::DuplicateKey,
                                        format!(
                                            "duplicate mapping key: {}",
                                            node_key_text(&entry_key)
                                        ),
                                    )
                                    .with_span(*first_span));
                                }
                                seen.insert(entry_key.clone(), entry_key.span);
                            }
                            entries.push((entry_key, built.node));
                        }
                    }
                }
            }
        }
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
        let shared = Arc::new(node);
        let target = Anchored {
            node: Arc::clone(&shared),
            size,
            height,
        };
        record(&mut self.anchors, anchor.as_str(), target);
        Built {
            node: Node::new(NodeKind::Alias(shared), span),
            size,
            height,
        }
    }

    fn span(&self) -> Span {
        self.events.get(self.index).map_or_else(
            || Span::empty(Marker::new(self.input_len, 1, 1)),
            Event::span,
        )
    }

    fn error_at(&self, span: Span, message: impl Into<String>) -> Error {
        Error::new(ErrorKind::Composition, message).with_span(span)
    }
}

/// A human-readable mapping key for typed diagnostics.
fn node_key_text(node: &Node<'_>) -> String {
    match &resolve(node).kind {
        NodeKind::Null => "null".to_owned(),
        NodeKind::Bool(value) => value.to_string(),
        NodeKind::Number(value) => value.to_string(),
        NodeKind::String(value) => format!("{value:?}"),
        other => format!("{other:?}"),
    }
}

/// The node an alias or a shared anchored node refers to.
fn resolve<'a, 'de>(mut node: &'a Node<'de>) -> &'a Node<'de> {
    while let NodeKind::Alias(target) = &node.kind {
        node = target.as_ref();
    }
    node
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

fn apply_tag(tag: Option<Tag>, node: Node<'_>) -> Node<'_> {
    match tag {
        // Core collection tags restate the node kind, so they're dropped
        // rather than kept as a tagged wrapper.
        Some(tag) if tag.is_core_collection() => node,
        Some(tag) => {
            let span = node.span;
            Node::new(NodeKind::Tagged(tag, Box::new(node)), span)
        }
        None => node,
    }
}

fn node_from_value<'de>(
    value: Value,
    text: &Cow<'de, str>,
    raw: &Cow<'de, str>,
    style: ScalarStyle,
    span: Span,
) -> Node<'de> {
    let scalar_text = borrowed_or_owned(text, raw, style);
    let kind = match value {
        Value::Null => NodeKind::Null,
        Value::Bool(value) => NodeKind::Bool(value),
        Value::Number(value) => NodeKind::Number(value),
        Value::String(_) => NodeKind::String(scalar_text.clone()),
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
            let inner = node_from_value(tagged.value, text, raw, style, span);
            NodeKind::Tagged(tagged.tag, Box::new(inner))
        }
    };
    Node::scalar(kind, scalar_text, style, span)
}

/// A string that a tag makes one, as in `!!str 01` or `! 01`, keeps no text, which only a float
/// target reads from a string (see `deserialize_f64`), so that it reads as a string there too.
fn forget_string_text(node: &mut Node<'_>) {
    let node = match &mut node.kind {
        NodeKind::Tagged(_, inner) => inner.as_mut(),
        _ => node,
    };
    if matches!(node.kind, NodeKind::String(_)) {
        node.text = None;
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
    value: &Cow<'de, str>,
    raw: &Cow<'de, str>,
    style: ScalarStyle,
) -> Cow<'de, str> {
    let raw = match raw {
        Cow::Borrowed(raw) => Some(*raw),
        Cow::Owned(_) => None,
    };
    let candidate = raw.and_then(|raw| match style {
        // A plain scalar's text keeps the blanks before a comment or the line's end, and its
        // value doesn't.
        ScalarStyle::Plain => Some(raw.trim_end_matches([' ', '\t'])),
        ScalarStyle::SingleQuoted | ScalarStyle::DoubleQuoted => {
            raw.get(1..raw.len().saturating_sub(1))
        }
        ScalarStyle::Literal | ScalarStyle::Folded => None,
    });
    match candidate {
        Some(candidate) if candidate == value.as_ref() => Cow::Borrowed(candidate),
        _ => Cow::Owned(value.as_ref().to_owned()),
    }
}

/// The size estimate of a scalar's node: its cost plus its text. Collections
/// and aliases are summed bottom-up by the builder instead.
fn estimate_size(node: &Node<'_>, node_cost: usize) -> usize {
    match &node.kind {
        NodeKind::Null | NodeKind::Bool(_) | NodeKind::Number(_) => node_cost,
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

fn with_span(error: Error, span: Span) -> Error {
    if error.span().is_none() {
        error.with_span(span)
    } else {
        error
    }
}

struct NodeDeserializer<'de, 'a> {
    node: &'a Node<'de>,
}

/// Deserializer methods for targets that read a node as `deserialize_any` does, past an
/// application tag (see `untagged`).
macro_rules! plain_targets {
    ($($method:ident)*) => {
        $(
            fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
                NodeDeserializer::new(untagged(self.node)).deserialize_any(visitor)
            }
        )*
    };
}

impl<'de, 'a> NodeDeserializer<'de, 'a> {
    fn new(node: &'a Node<'de>) -> Self {
        Self { node }
    }

    /// The node to read. Aliases form no cycles: an anchor is defined only
    /// once its node is complete, so an alias cannot reach itself.
    fn target(&self) -> &'a Node<'de> {
        resolve(self.node)
    }
}

impl<'de, 'a> Deserializer<'de> for NodeDeserializer<'de, 'a> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = self.target();
        let span = node.span;
        let result = match &node.kind {
            NodeKind::Null => visitor.visit_unit(),
            NodeKind::Bool(value) => visitor.visit_bool(*value),
            NodeKind::Number(value) => value.visit(visitor),
            NodeKind::String(Cow::Borrowed(value)) => visitor.visit_borrowed_str(value),
            NodeKind::String(Cow::Owned(value)) => visitor.visit_str(value),
            NodeKind::Sequence(sequence) => {
                let mut access = NodeSeqAccess {
                    iter: sequence.iter(),
                    len: sequence.len(),
                };
                visitor.visit_seq(&mut access).and_then(|value| {
                    // A visitor that stops early, such as a tuple's, leaves
                    // elements behind.
                    check_consumed(access.iter.len(), sequence.len(), "sequence").map(|()| value)
                })
            }
            NodeKind::Mapping(entries) => {
                let mut access = NodeMapAccess {
                    iter: entries.iter(),
                    entry: None,
                };
                visitor.visit_map(&mut access).and_then(|value| {
                    check_consumed(access.iter.len(), entries.len(), "map").map(|()| value)
                })
            }
            // A tagged node is an enum whose variant names the tag, so a
            // `Value` keeps the tag.
            NodeKind::Tagged(tag, value) => {
                return visitor
                    .visit_enum(NodeEnumAccess {
                        variant: tag.serde_name(),
                        variant_span: span,
                        value: NodeValue::Ref(value),
                        in_path: false,
                    })
                    .map_err(|error| with_span(error, span));
            }
            NodeKind::Alias(_) => unreachable!("aliases are resolved by target"),
        };
        result.map_err(|error| with_span(error, span))
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = self.target();
        let span = node.span;
        if matches!(&node.kind, NodeKind::Null) {
            visitor.visit_none()
        } else {
            visitor
                .visit_some(NodeDeserializer::new(node))
                .map_err(|error| with_span(error, span))
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let span = self.target().span;
        visitor
            .visit_newtype_struct(self)
            .map_err(|error| with_span(error, span))
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_string(visitor)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = self.target();
        let span = node.span;
        let result = match &node.kind {
            NodeKind::String(Cow::Borrowed(value)) => visitor.visit_borrowed_str(value),
            NodeKind::String(Cow::Owned(value)) => visitor.visit_str(value),
            NodeKind::Tagged(_, value) => {
                return NodeDeserializer::new(value)
                    .deserialize_string(visitor)
                    .map_err(|error| with_span(error, span));
            }
            _ => match &node.text {
                Some(Cow::Borrowed(value)) => visitor.visit_borrowed_str(value),
                Some(Cow::Owned(value)) => visitor.visit_str(value),
                None => {
                    return NodeDeserializer::new(node)
                        .deserialize_any(visitor)
                        .map_err(|error| with_span(error, span));
                }
            },
        };
        result.map_err(|error| with_span(error, span))
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = self.target();
        let span = node.span;
        let result = match &node.kind {
            NodeKind::String(Cow::Borrowed(value)) => {
                visitor.visit_borrowed_bytes(value.as_bytes())
            }
            NodeKind::String(Cow::Owned(value)) => visitor.visit_bytes(value.as_bytes()),
            NodeKind::Tagged(_, value) => {
                return NodeDeserializer::new(value)
                    .deserialize_bytes(visitor)
                    .map_err(|error| with_span(error, span));
            }
            _ => {
                return NodeDeserializer::new(node)
                    .deserialize_any(visitor)
                    .map_err(|error| with_span(error, span));
            }
        };
        result.map_err(|error| with_span(error, span))
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = untagged(self.node);
        let span = node.span;
        if is_empty_scalar(node) {
            return visitor
                .visit_seq(EmptySeqAccess)
                .map_err(|error| with_span(error, span));
        }
        null_is_not_a_collection(node, &visitor)?;
        NodeDeserializer::new(node)
            .deserialize_any(visitor)
            .map_err(|error| with_span(error, span))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let node = untagged(self.node);
        let span = node.span;
        if is_empty_scalar(node) {
            return visitor
                .visit_map(EmptyMapAccess)
                .map_err(|error| with_span(error, span));
        }
        null_is_not_a_collection(node, &visitor)?;
        NodeDeserializer::new(node)
            .deserialize_any(visitor)
            .map_err(|error| with_span(error, span))
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = untagged(self.node);
        let span = node.span;
        if is_empty_scalar(node) {
            return visitor
                .visit_map(EmptyMapAccess)
                .map_err(|error| with_span(error, span));
        }
        null_is_not_a_collection(node, &visitor)?;
        NodeDeserializer::new(node)
            .deserialize_any(visitor)
            .map_err(|error| with_span(error, span))
    }

    /// A field name is the key's text: `1: x` names the field `"1"`, not the
    /// second field.
    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_string(visitor)
    }

    /// A character is read from the scalar's text, so `5` is `'5'`.
    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_string(visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let node = self.target();
        let span = node.span;
        let (variant, variant_span, value) = match &node.kind {
            // A tag names the variant by its whole name (`Tag::variant_name`), so `!<tag:x:B>`
            // and `!e!B` under `%TAG !e! tag:x:` select the same variant.
            NodeKind::Tagged(tag, value) => (tag.variant_name(), span, NodeValue::Ref(value)),
            NodeKind::String(variant) => (
                variant.as_ref().to_owned(),
                span,
                NodeValue::Null(Node::new(NodeKind::Null, span)),
            ),
            // `true` and `1` name variants renamed to them.
            NodeKind::Null | NodeKind::Bool(_) | NodeKind::Number(_) if node.text.is_some() => (
                node.text.as_deref().unwrap_or_default().to_owned(),
                span,
                NodeValue::Null(Node::new(NodeKind::Null, span)),
            ),
            NodeKind::Mapping(entries) => {
                let mut iter = entries.iter();
                let Some((key, value)) = iter.next() else {
                    return Err(crate::Error::new(
                        crate::ErrorKind::Serde,
                        "enum mapping must contain one variant",
                    )
                    .with_span(span));
                };
                if iter.next().is_some() {
                    return Err(crate::Error::new(
                        crate::ErrorKind::Serde,
                        "enum mapping must contain one variant",
                    )
                    .with_span(span));
                }
                let variant_span = key.span;
                let NodeKind::String(variant) = &resolve(key).kind else {
                    return Err(crate::Error::new(
                        crate::ErrorKind::Serde,
                        "enum variant key must be a string",
                    )
                    .with_span(variant_span));
                };
                (
                    variant.as_ref().to_owned(),
                    variant_span,
                    NodeValue::Ref(value),
                )
            }
            _ => {
                return Err(crate::Error::new(
                    crate::ErrorKind::Serde,
                    "enum value must be tagged or a single-entry mapping",
                )
                .with_span(span));
            }
        };
        visitor
            .visit_enum(NodeEnumAccess {
                variant,
                variant_span,
                value,
                in_path: true,
            })
            .map_err(|error| with_span(error, span))
    }

    /// A float target parses a plain scalar from its source text, so `01` is
    /// 1.0 and a radix integer such as `0x1F` is rejected. Quoted scalars are
    /// strings and are not parsed this way.
    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = untagged(self.node);
        if let NodeKind::Number(number) = &node.kind
            && number.is_f64()
        {
            return visitor
                .visit_f64(number.as_f64().expect("a float converts to f64"))
                .map_err(|error| with_span(error, node.span));
        }
        // A plain scalar reads from its text, so an integer wider than 64 bits reads, and so does
        // a number spelling with a leading zero, as in `01`, which resolves to a string. Other text,
        // such as `inf` or `1e999`, doesn't, and nor does a string that a tag makes one, which
        // keeps no text.
        if node.style == ScalarStyle::Plain
            && let Some(text) = node.text.as_deref()
        {
            match &node.kind {
                NodeKind::Number(_) => {
                    if let Ok(value) = text.parse::<f64>() {
                        return visitor
                            .visit_f64(value)
                            .map_err(|error| with_span(error, node.span));
                    }
                    // A radix integer is a number to the resolver, but not text that parses
                    // into a float.
                    return Err(crate::Error::new(
                        crate::ErrorKind::Serde,
                        format!("invalid type: integer `{text}`, expected f64"),
                    )
                    .with_span(node.span));
                }
                NodeKind::String(_) => {
                    if let Some(value) = float_from_text(text) {
                        return visitor
                            .visit_f64(value)
                            .map_err(|error| with_span(error, node.span));
                    }
                }
                _ => {}
            }
        }
        NodeDeserializer::new(node).deserialize_any(visitor)
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_f64(visitor)
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        _length: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        NodeDeserializer::new(untagged(self.node)).deserialize_any(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _length: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_tuple(0, visitor)
    }

    plain_targets! {
        deserialize_bool deserialize_i8 deserialize_i16 deserialize_i32 deserialize_i64
        deserialize_i128 deserialize_u8 deserialize_u16 deserialize_u32 deserialize_u64
        deserialize_u128 deserialize_unit
    }

    serde::forward_to_deserialize_any! {
        ignored_any
    }
}

/// An empty scalar, such as a missing value, reads as an empty sequence,
/// map, or struct; an explicit `~` or `null` does not.
fn is_empty_scalar(node: &Node<'_>) -> bool {
    matches!(node.kind, NodeKind::Null) && node.text.as_deref().is_none_or(str::is_empty)
}

/// An explicit `~` or `null` is not an empty collection. It's reported before
/// the visitor sees it, so even a visitor that accepts a unit fails.
fn null_is_not_a_collection(
    node: &Node<'_>,
    expected: &dyn serde::de::Expected,
) -> Result<(), Error> {
    if matches!(node.kind, NodeKind::Null) {
        Err(with_span(
            serde::de::Error::invalid_type(serde::de::Unexpected::Unit, expected),
            node.span,
        ))
    } else {
        Ok(())
    }
}

/// Fail when a visitor left `remaining` of `total` elements unread.
fn check_consumed(remaining: usize, total: usize, kind: &str) -> Result<(), Error> {
    if remaining == 0 {
        Ok(())
    } else {
        Err(serde::de::Error::invalid_length(
            total,
            &format!("fewer elements in the {kind}").as_str(),
        ))
    }
}

struct EmptySeqAccess;

impl<'de> SeqAccess<'de> for EmptySeqAccess {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        _seed: T,
    ) -> Result<Option<T::Value>, Self::Error> {
        Ok(None)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(0)
    }
}

struct EmptyMapAccess;

impl<'de> MapAccess<'de> for EmptyMapAccess {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        _seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        Ok(None)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        _seed: V,
    ) -> Result<V::Value, Self::Error> {
        Err(Error::new(
            crate::ErrorKind::Serde,
            "map value requested before key",
        ))
    }
}

enum NodeValue<'de, 'a> {
    Ref(&'a Node<'de>),
    Null(Node<'de>),
}

impl<'de, 'a> NodeValue<'de, 'a> {
    fn node(&self) -> &Node<'de> {
        match self {
            Self::Ref(node) => node,
            Self::Null(node) => node,
        }
    }
}

struct NodeSeqAccess<'de, 'a> {
    iter: std::slice::Iter<'a, Node<'de>>,
    len: usize,
}

impl<'de, 'a> SeqAccess<'de> for NodeSeqAccess<'de, 'a> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Self::Error> {
        let Some(node) = self.iter.next() else {
            return Ok(None);
        };
        let span = node.span;
        seed.deserialize(NodeDeserializer::new(node))
            .map(Some)
            .map_err(|error| with_span(error, span).inside_index(self.len - self.iter.len() - 1))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

struct NodeMapAccess<'de, 'a> {
    iter: std::slice::Iter<'a, (Node<'de>, Node<'de>)>,
    /// The entry whose key was read and whose value wasn't.
    entry: Option<&'a (Node<'de>, Node<'de>)>,
}

impl<'de, 'a> MapAccess<'de> for NodeMapAccess<'de, 'a> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        let Some(entry) = self.iter.next() else {
            return Ok(None);
        };
        let key = &entry.0;
        let span = key.span;
        self.entry = Some(entry);
        seed.deserialize(NodeDeserializer::new(key))
            .map(Some)
            .map_err(|error| with_span(error, span))
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        let (key, value) = self.entry.take().ok_or_else(|| {
            crate::Error::new(crate::ErrorKind::Serde, "map value requested before key")
        })?;
        let span = value.span;
        seed.deserialize(NodeDeserializer::new(value))
            .map_err(|error| inside_key(with_span(error, span), key))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

struct NodeEnumAccess<'de, 'a> {
    variant: String,
    variant_span: Span,
    value: NodeValue<'de, 'a>,
    /// Whether an error inside names the variant. A target that isn't an enum takes the tag as a
    /// tag, and the document has no key by that name.
    in_path: bool,
}

impl<'de, 'a> EnumAccess<'de> for NodeEnumAccess<'de, 'a> {
    type Error = Error;
    type Variant = NodeVariantAccess<'de, 'a>;

    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Self::Variant), Self::Error> {
        let node = Node::new(
            NodeKind::String(Cow::Owned(self.variant)),
            self.variant_span,
        );
        let variant = seed
            .deserialize(NodeDeserializer::new(&node))
            .map_err(|error| with_span(error, self.variant_span))?;
        Ok((
            variant,
            NodeVariantAccess {
                name: self.in_path.then_some(node),
                value: self.value,
            },
        ))
    }
}

struct NodeVariantAccess<'de, 'a> {
    /// The variant's name, if an error inside gives it.
    name: Option<Node<'de>>,
    value: NodeValue<'de, 'a>,
}

impl NodeVariantAccess<'_, '_> {
    #[cold]
    fn inside_variant(&self, error: Error) -> Error {
        match &self.name {
            Some(name) => inside_key(error, name),
            None => error,
        }
    }
}

impl<'de, 'a> VariantAccess<'de> for NodeVariantAccess<'de, 'a> {
    type Error = Error;

    /// A variant's value reads past an application tag, as a target that isn't an enum does, so
    /// `Nothing: !t` is the unit variant `Nothing`.
    fn unit_variant(self) -> Result<(), Self::Error> {
        let node = untagged(self.value.node());
        if matches!(&node.kind, NodeKind::Null) {
            Ok(())
        } else {
            Err(self.inside_variant(
                crate::Error::new(crate::ErrorKind::Serde, "expected a unit enum variant")
                    .with_span(node.span),
            ))
        }
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(
        self,
        seed: T,
    ) -> Result<T::Value, Self::Error> {
        let node = self.value.node();
        let span = node.span;
        seed.deserialize(NodeDeserializer::new(node))
            .map_err(|error| self.inside_variant(with_span(error, span)))
    }

    fn tuple_variant<V: Visitor<'de>>(
        self,
        length: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        NodeDeserializer::new(self.value.node())
            .deserialize_tuple(length, visitor)
            .map_err(|error| self.inside_variant(error))
    }

    /// A tag with no value, as in `!Point`, fills a struct variant's defaults, as an empty value
    /// fills a struct. A bare name, as in `Point`, names a unit variant, so it fills none.
    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let result = match &self.value {
            NodeValue::Ref(node) => {
                NodeDeserializer::new(node).deserialize_struct("", fields, visitor)
            }
            NodeValue::Null(node) => NodeDeserializer::new(node).deserialize_any(visitor),
        };
        result.map_err(|error| self.inside_variant(error))
    }
}

/// Say that an error came from inside the value of this key. A scalar key reads as its value,
/// so `0x10:` and `16:` are one path, whichever typed reader reports it.
#[cold]
fn inside_key(error: Error, key: &Node<'_>) -> Error {
    let mut node = resolve(key);
    while let NodeKind::Tagged(_, inner) = &node.kind {
        node = resolve(inner);
    }
    match &node.kind {
        NodeKind::String(text) => error.inside_key(text),
        NodeKind::Null => error.inside_key("null"),
        NodeKind::Bool(value) => error.inside_key(&value.to_string()),
        NodeKind::Number(value) => error.inside_key(&value.to_string()),
        _ => error.inside_collection_key(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(input: &str) -> Node<'_> {
        let limits = Limits::default();
        let events = parser::parse_events(input, &limits).expect("events");
        super::build(
            events,
            input.len(),
            &limits,
            NodeOptions {
                allow_duplicates: true,
                ..NodeOptions::default()
            },
            &mut AliasBudget::default(),
        )
        .expect("node")
    }

    #[test]
    fn aliases_are_reference_nodes_not_expanded_copies() {
        let node = build("a: &a {x: 1}\nb: *a\n");
        let NodeKind::Mapping(entries) = &node.kind else {
            panic!("root mapping");
        };
        let value = entries
            .iter()
            .find(|(key, _)| matches!(&key.kind, NodeKind::String(text) if text == "b"))
            .map(|(_, value)| value)
            .expect("b value");
        let NodeKind::Alias(target) = &value.kind else {
            panic!("alias node");
        };
        let NodeKind::Mapping(target_entries) = &target.kind else {
            panic!("alias target mapping");
        };
        assert_eq!(target_entries.len(), 1);
    }
}
