//! Composition: turn a spanned event stream into `Value`s.
//!
//! The composer resolves scalars, builds ordered mappings and sequences,
//! registers anchors, materializes aliases within the alias budget, and
//! rejects duplicate mapping keys. Merge keys are deliberately not applied
//! here; an explicit adapter owns that behavior.
//!
//! Open collections live on an explicit stack, so nesting costs heap rather
//! than native stack. An anchor records where its node sits in the
//! tree being built, plus the node's size estimate and nesting height, so a
//! node is copied only when an alias materializes it.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::error::{Error, ErrorCode, ErrorKind};
use crate::event::{Anchor, Event, ScalarStyle};
use crate::limits::{AliasBudget, Limits};
use crate::mapping::{GlobalHasher, Mapping, SMALL_LIMIT};
use crate::resolver::{resolve_plain, resolve_scalar_cow};
use crate::span::Span;
use crate::tag::{Tag, TaggedValue};
use crate::value::Value;

/// Compose every document in an event stream.
pub(crate) fn compose_documents(
    events: &[Event<'_>],
    input_len: usize,
    limits: &Limits,
) -> Result<Vec<Value>, Error> {
    compose_documents_with_budget(
        events.iter().cloned(),
        input_len,
        limits,
        &mut AliasBudget::default(),
    )
}

/// Compose every document in an event stream, charging aliases to a budget
/// that the caller keeps across calls.
pub(crate) fn compose_documents_with_budget<'a>(
    events: impl IntoIterator<Item = Event<'a>>,
    input_len: usize,
    limits: &Limits,
    budget: &mut AliasBudget,
) -> Result<Vec<Value>, Error> {
    let mut composer = StreamingComposer::with_budget(limits.clone(), input_len, *budget);
    for event in events {
        composer.event(event).map_err(|error| *error)?;
    }
    let (documents, spent) = composer.finish_with_budget()?;
    *budget = spent;
    Ok(documents)
}

/// Tag a collection. A core tag on a collection names a node of another kind, or restates its
/// kind, and is dropped; a custom tag is kept.
fn apply_tag(tag: Option<&Tag>, value: Value) -> Value {
    match tag {
        Some(tag) if !tag.is_core_collection() && !tag.is_core_scalar() => {
            Value::from(TaggedValue::new(tag.clone(), value))
        }
        _ => value,
    }
}

/// The size estimate of a scalar's value. Collections and aliases are summed
/// bottom-up by the composer instead.
fn estimate_size(value: &Value, node_cost: usize) -> usize {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => node_cost,
        Value::String(text) => node_cost.saturating_add(text.len()),
        Value::Sequence(sequence) => sequence.iter().fold(node_cost, |size, item| {
            size.saturating_add(estimate_size(item, node_cost))
        }),
        Value::Mapping(mapping) => mapping.iter().fold(node_cost, |size, (key, value)| {
            size.saturating_add(estimate_size(key, node_cost))
                .saturating_add(estimate_size(value, node_cost))
        }),
        Value::Tagged(tagged) => node_cost
            .saturating_add(tagged.tag.text_len())
            .saturating_add(estimate_size(&tagged.value, node_cost)),
    }
}

/// The anchors of the document being read, each with the nearest preceding node that
/// has it. A collection's anchor comes before its content, so from its start until its
/// end the collection is the nearest node for that anchor. No finished value can stand
/// for it yet, so an alias there is rejected, even when an earlier node has the anchor.
#[derive(Debug)]
pub(crate) struct Anchors<T> {
    nodes: HashMap<String, AnchorNode<T>, GlobalHasher>,
}

#[derive(Debug)]
enum AnchorNode<T> {
    /// A collection with this anchor hasn't ended.
    Open,
    Finished(T),
}

impl<T> Anchors<T> {
    pub(crate) fn new() -> Self {
        Self {
            nodes: HashMap::with_hasher(GlobalHasher),
        }
    }

    /// Whether any anchors have been opened or defined in the current document.
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Forget every anchor at the start of a document.
    pub(crate) fn clear(&mut self) {
        self.nodes.clear();
    }

    fn set(&mut self, anchor: &str, node: AnchorNode<T>) {
        match self.nodes.get_mut(anchor) {
            Some(entry) => *entry = node,
            None => {
                self.nodes.insert(anchor.to_owned(), node);
            }
        }
    }

    /// A collection with this anchor has started.
    pub(crate) fn open(&mut self, anchor: &str) {
        self.set(anchor, AnchorNode::Open);
    }

    /// A scalar with this anchor has been read.
    pub(crate) fn define(&mut self, anchor: &str, value: T) {
        self.set(anchor, AnchorNode::Finished(value));
    }

    /// A collection with this anchor has ended. A node inside it with the same anchor
    /// comes later in the document, so it stays the nearest: in `&x {k: &x v}`, `*x` is
    /// `v`.
    pub(crate) fn close(&mut self, anchor: &str, value: T) {
        if let Some(entry) = self.nodes.get_mut(anchor)
            && matches!(entry, AnchorNode::Open)
        {
            *entry = AnchorNode::Finished(value);
        }
    }

    /// The node an alias at this point refers to, or the code and message to reject it with.
    pub(crate) fn resolve(&self, anchor: &str) -> Result<&T, (ErrorCode, &'static str)> {
        match self.nodes.get(anchor) {
            Some(AnchorNode::Finished(value)) => Ok(value),
            Some(AnchorNode::Open) => Err((
                ErrorCode::CyclicAlias,
                "an alias cannot refer to a collection that contains it",
            )),
            None => Err((ErrorCode::UnknownAnchor, "unknown anchor")),
        }
    }
}

/// One step from a collection to a child: a sequence item, or a mapping
/// entry's key or value, by position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Item(usize),
    Key(usize),
    Value(usize),
}

/// Where an anchored node sits in the document being built, from the root
/// collection down, with its size estimate and nesting height.
#[derive(Clone, Debug)]
struct AnchorDef {
    path: Vec<Step>,
    size: usize,
    height: usize,
}

/// Maximum number of sequence items buffered in `StreamingComposer::scratch_items` before
/// spilling to an owned `Vec<Value>`, keeping small sequences exact-sized (1 allocation, 0 slack)
/// without temporarily doubling peak memory on huge sequences.
const SEQ_SCRATCH_LIMIT: usize = 64;

enum SeqBuf {
    Scratch { start: usize, len: usize },
    Owned(Vec<Value>),
}

impl SeqBuf {
    #[inline]
    fn len(&self) -> usize {
        match self {
            Self::Scratch { len, .. } => *len,
            Self::Owned(items) => items.len(),
        }
    }
}

enum MapBuf {
    Scratch { start: usize, len: u8 },
    Large(Mapping),
}

impl MapBuf {
    #[inline]
    fn len(&self) -> usize {
        match self {
            Self::Scratch { len, .. } => usize::from(*len),
            Self::Large(mapping) => mapping.len(),
        }
    }
}

enum Frame<'a> {
    Sequence {
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        items: SeqBuf,
        span: Span,
        size: usize,
        height: usize,
    },
    Mapping {
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        mapping: MapBuf,
        pending_key: Option<(Value, Span)>,
        span: Span,
        size: usize,
        height: usize,
    },
}

impl Frame<'_> {
    /// Where the next finished child goes.
    fn position(&self) -> Step {
        match self {
            Self::Sequence { items, .. } => Step::Item(items.len()),
            Self::Mapping {
                mapping,
                pending_key: None,
                ..
            } => Step::Key(mapping.len()),
            Self::Mapping { mapping, .. } => Step::Value(mapping.len()),
        }
    }

    /// A finished child.
    fn child<'b>(
        &'b self,
        step: Step,
        scratch_items: &'b [Value],
        scratch_entries: &'b [(Value, Value)],
    ) -> Option<&'b Value> {
        match (self, step) {
            (Self::Sequence { items, .. }, Step::Item(index)) => match items {
                SeqBuf::Scratch { start, len } if index < *len => scratch_items.get(*start + index),
                SeqBuf::Scratch { .. } => None,
                SeqBuf::Owned(items) => items.get(index),
            },
            (
                Self::Mapping {
                    mapping,
                    pending_key,
                    ..
                },
                Step::Key(index),
            ) => match mapping {
                MapBuf::Scratch { start, len } => {
                    let count = usize::from(*len);
                    match index.cmp(&count) {
                        std::cmp::Ordering::Less => {
                            scratch_entries.get(*start + index).map(|(key, _)| key)
                        }
                        std::cmp::Ordering::Equal => pending_key.as_ref().map(|(key, _)| key),
                        std::cmp::Ordering::Greater => None,
                    }
                }
                MapBuf::Large(mapping) => match mapping.get_index(index) {
                    Some((key, _)) => Some(key),
                    None if index == mapping.len() => pending_key.as_ref().map(|(key, _)| key),
                    None => None,
                },
            },
            (Self::Mapping { mapping, .. }, Step::Value(index)) => match mapping {
                MapBuf::Scratch { start, len } if index < usize::from(*len) => {
                    scratch_entries.get(*start + index).map(|(_, value)| value)
                }
                MapBuf::Scratch { .. } => None,
                MapBuf::Large(mapping) => mapping.get_index(index).map(|(_, value)| value),
            },
            _ => None,
        }
    }
}

/// A finished child of a finished collection, looking through tags.
fn child_of(mut value: &Value, step: Step) -> Option<&Value> {
    while let Value::Tagged(tagged) = value {
        value = &tagged.value;
    }
    match (value, step) {
        (Value::Sequence(items), Step::Item(index)) => items.get(index),
        (Value::Mapping(mapping), Step::Key(index)) => mapping.get_index(index).map(|(key, _)| key),
        (Value::Mapping(mapping), Step::Value(index)) => {
            mapping.get_index(index).map(|(_, value)| value)
        }
        _ => None,
    }
}

/// Resolve an anchor path against the active composer stack and scratch buffers without borrowing
/// `self.anchors`.
fn anchored_value<'b>(
    stack: &'b [Frame<'_>],
    scratch_items: &'b [Value],
    scratch_entries: &'b [(Value, Value)],
    path: &[Step],
) -> Option<&'b Value> {
    for (depth, step) in path.iter().enumerate() {
        let frame = stack.get(depth)?;
        if depth + 1 < stack.len() && frame.position() == *step {
            continue;
        }
        let mut value = frame.child(*step, scratch_items, scratch_entries)?;
        for step in &path[depth + 1..] {
            value = child_of(value, *step)?;
        }
        return Some(value);
    }
    None
}

/// Trim excess capacity on a finished root collection (looking through tags). Called only
/// after the parser's token arena and composer scratch buffers have been dropped so the
/// reallocation copy does not stack on top of the parse peak.
fn shrink_root_value(mut value: &mut Value) {
    while let Value::Tagged(tagged) = value {
        value = &mut tagged.value;
    }
    match value {
        Value::Sequence(items) => {
            if items.capacity() > items.len() {
                items.shrink_to_fit();
            }
        }
        Value::Mapping(mapping) => mapping.shrink_to_fit(),
        _ => {}
    }
}

/// Event-driven composer behind `parse_value`, `parse_documents`, and the
/// document cursor.
pub(crate) struct StreamingComposer<'a> {
    limits: Limits,
    input_len: usize,
    anchors: Anchors<AnchorDef>,
    documents: Vec<Value>,
    document: Option<Value>,
    /// Single-document mode avoids wrapping the root document in `self.documents`.
    single_document: bool,
    single_completed: bool,
    /// Optional entry-count hint from the parser for the next root flow collection.
    next_capacity: usize,
    stack: Vec<Frame<'a>>,
    scratch_items: Vec<Value>,
    scratch_entries: Vec<(Value, Value)>,
    budget: AliasBudget,
}

impl<'a> StreamingComposer<'a> {
    pub(crate) fn new(limits: Limits, input_len: usize) -> Self {
        Self::with_budget(limits, input_len, AliasBudget::default())
    }

    pub(crate) fn new_single(limits: Limits, input_len: usize) -> Self {
        let mut composer = Self::with_budget(limits, input_len, AliasBudget::default());
        composer.single_document = true;
        composer
    }

    pub(crate) fn with_budget(limits: Limits, input_len: usize, budget: AliasBudget) -> Self {
        Self {
            limits,
            input_len,
            anchors: Anchors::new(),
            documents: Vec::new(),
            document: None,
            single_document: false,
            single_completed: false,
            next_capacity: 0,
            stack: Vec::new(),
            scratch_items: Vec::new(),
            scratch_entries: Vec::new(),
            budget,
        }
    }

    /// Reserve room for the next collection when the parser already counted its entries.
    pub(crate) fn reserve_collection(&mut self, entries: usize) {
        self.next_capacity = entries.min(self.limits.max_collection_entries);
    }

    /// Fast path for an untagged, unanchored plain scalar directly from the parser.
    #[inline]
    pub(crate) fn plain_scalar(&mut self, value: &str, span: Span) -> Result<(), Box<Error>> {
        self.next_capacity = 0;
        let resolved = resolve_plain(value).unwrap_or_else(|| Value::String(value.to_owned()));
        let size = if self.anchors.is_empty() {
            0
        } else {
            estimate_size(&resolved, self.limits.node_cost_bytes)
        };
        self.feed(resolved, span, size, 0)
    }

    /// Direct scalar entry point bypassing `Event::Scalar` construction and unused `raw` slices.
    #[inline]
    pub(crate) fn scalar(
        &mut self,
        value: Cow<'a, str>,
        style: ScalarStyle,
        tag: Option<&Tag>,
        anchor: Option<&Anchor<'a>>,
        span: Span,
    ) -> Result<(), Box<Error>> {
        self.next_capacity = 0;
        let resolved = resolve_scalar_cow(value, style, tag)
            .map_err(|error| Box::new(error.with_span(span)))?;
        // Skip size estimation when no anchor is open or being defined in this document.
        // Scalar values always have collection nesting height 0.
        let size = if anchor.is_some() || !self.anchors.is_empty() {
            estimate_size(&resolved, self.limits.node_cost_bytes)
        } else {
            0
        };
        self.record_anchor(anchor, size, 0, Anchors::define);
        self.feed(resolved, span, size, 0)
    }

    pub(crate) fn event(&mut self, event: Event<'a>) -> Result<(), Box<Error>> {
        let reserved = std::mem::take(&mut self.next_capacity);
        match event {
            Event::StreamStart { .. } | Event::StreamEnd { .. } | Event::Directive { .. } => {}
            Event::DocumentStart { span, .. } => {
                if !self.stack.is_empty() {
                    return Err(Box::new(Error::new(
                        ErrorKind::Composition,
                        "document started inside a collection",
                    )));
                }
                if self.single_document && self.single_completed {
                    return Err(Box::new(
                        Error::coded(
                            ErrorKind::Parser,
                            ErrorCode::TrailingDocuments,
                            "expected a single YAML document, but found trailing documents",
                        )
                        .with_span(span),
                    ));
                }
                // Anchors are scoped to a document; an alias may not see a
                // definition from an earlier document.
                self.anchors.clear();
                self.document = None;
            }
            Event::DocumentEnd { span, .. } => {
                if !self.stack.is_empty() {
                    return Err(self.error_at(span, "document ended inside a collection"));
                }
                if self.single_document {
                    if self.document.is_none() {
                        self.document = Some(Value::Null);
                    }
                    self.single_completed = true;
                } else {
                    let value = self.document.take().unwrap_or(Value::Null);
                    self.documents.push(value);
                }
            }
            Event::Scalar {
                value,
                style,
                tag,
                anchor,
                span,
                ..
            } => {
                return self.scalar(value, style, tag.as_ref(), anchor.as_ref(), span);
            }
            Event::Alias { anchor, span } => {
                let (size, height, value) = {
                    let definition =
                        self.anchors
                            .resolve(anchor.as_str())
                            .map_err(|(code, message)| {
                                Box::new((*self.error_at(span, message)).with_code(code))
                            })?;
                    let size = definition.size;
                    let height = definition.height;
                    // Charge before cloning: the clone is the allocation this
                    // budget exists to bound.
                    self.budget
                        .charge(&self.limits, self.input_len, size)
                        .map_err(|error| Box::new(error.with_span(span)))?;
                    // The alias nests its target here, so the target's
                    // collections count toward the depth.
                    if height > 0 {
                        self.limits
                            .check_depth(self.stack.len() + height)
                            .map_err(|error| Box::new(Error::from(error).with_span(span)))?;
                    }
                    let value = anchored_value(
                        &self.stack,
                        &self.scratch_items,
                        &self.scratch_entries,
                        &definition.path,
                    )
                    .cloned()
                    .ok_or_else(|| {
                        Box::new(
                            (*self.error_at(span, "unknown anchor"))
                                .with_code(ErrorCode::UnknownAnchor),
                        )
                    })?;
                    (size, height, value)
                };
                self.feed(value, span, size, height)?;
            }
            Event::SequenceStart {
                tag, anchor, span, ..
            } => {
                self.limits
                    .check_depth(self.stack.len() + 1)
                    .map_err(|error| Box::new(Error::from(error).with_span(span)))?;
                if let Some(anchor) = &anchor {
                    self.anchors.open(anchor.as_str());
                }
                let items = if reserved > SEQ_SCRATCH_LIMIT {
                    SeqBuf::Owned(Vec::with_capacity(reserved))
                } else {
                    SeqBuf::Scratch {
                        start: self.scratch_items.len(),
                        len: 0,
                    }
                };
                self.stack.push(Frame::Sequence {
                    tag,
                    anchor,
                    items,
                    span,
                    size: self.limits.node_cost_bytes,
                    height: 0,
                });
            }
            Event::SequenceEnd { span } => self.finish_sequence(span)?,
            Event::MappingStart {
                tag, anchor, span, ..
            } => {
                self.limits
                    .check_depth(self.stack.len() + 1)
                    .map_err(|error| Box::new(Error::from(error).with_span(span)))?;
                if let Some(anchor) = &anchor {
                    self.anchors.open(anchor.as_str());
                }
                let mapping = if reserved > SMALL_LIMIT {
                    MapBuf::Large(Mapping::with_capacity(reserved))
                } else {
                    MapBuf::Scratch {
                        start: self.scratch_entries.len(),
                        len: 0,
                    }
                };
                self.stack.push(Frame::Mapping {
                    tag,
                    anchor,
                    mapping,
                    pending_key: None,
                    span,
                    size: self.limits.node_cost_bytes,
                    height: 0,
                });
            }
            Event::MappingEnd { span } => self.finish_mapping(span)?,
        }
        Ok(())
    }

    #[inline]
    fn finish_sequence(&mut self, span: Span) -> Result<(), Box<Error>> {
        let Some(Frame::Sequence {
            tag,
            anchor,
            items,
            span: start_span,
            size,
            height,
        }) = self.stack.pop()
        else {
            return Err(self.error_at(span, "unexpected sequence end"));
        };
        let items = match items {
            SeqBuf::Scratch { len: 0, .. } => Vec::new(),
            SeqBuf::Scratch { start, len } => {
                let mut exact = Vec::with_capacity(len);
                exact.extend(self.scratch_items.drain(start..));
                exact
            }
            SeqBuf::Owned(mut items) => {
                // Shrink small-to-medium nested sequences immediately; defer large or root
                // shrinks until after the parser drops its token arena so the copy does
                // not stack on top of the parse peak.
                if !self.stack.is_empty() && items.len() <= 256 && items.capacity() > items.len() {
                    items.shrink_to_fit();
                }
                items
            }
        };
        let (value, size) = self.finish_collection(tag.as_ref(), Value::Sequence(items), size);
        self.record_anchor(anchor.as_ref(), size, height + 1, Anchors::close);
        self.feed(value, start_span, size, height + 1)
    }

    #[inline]
    fn finish_mapping(&mut self, span: Span) -> Result<(), Box<Error>> {
        let Some(Frame::Mapping {
            tag,
            anchor,
            mapping,
            pending_key,
            span: start_span,
            size,
            height,
        }) = self.stack.pop()
        else {
            return Err(self.error_at(span, "unexpected mapping end"));
        };
        if let Some((key, key_span)) = pending_key {
            return Err(Box::new(
                Error::new(
                    ErrorKind::Composition,
                    format!("mapping key without value: {key:?}"),
                )
                .with_span(key_span),
            ));
        }
        let mapping = match mapping {
            MapBuf::Scratch { len: 0, .. } => Mapping::new(),
            MapBuf::Scratch { start, .. } => {
                Mapping::from_small_drain(self.scratch_entries.drain(start..))
            }
            MapBuf::Large(mut mapping) => {
                if !self.stack.is_empty() && mapping.len() <= 256 {
                    mapping.shrink_to_fit();
                }
                mapping
            }
        };
        let (value, size) = self.finish_collection(tag.as_ref(), Value::Mapping(mapping), size);
        self.record_anchor(anchor.as_ref(), size, height + 1, Anchors::close);
        self.feed(value, start_span, size, height + 1)
    }

    pub(crate) fn finish(self) -> Result<Vec<Value>, Error> {
        self.finish_with_budget().map(|(documents, _)| documents)
    }

    /// Finish a single-document parse without wrapping the root value in a `Vec`.
    pub(crate) fn finish_single(self) -> Result<Value, Error> {
        let Self {
            stack,
            document,
            scratch_items,
            scratch_entries,
            anchors,
            documents,
            ..
        } = self;
        if !stack.is_empty() {
            return Err(Error::new(
                ErrorKind::Composition,
                "unterminated collection at end of stream",
            ));
        }
        drop(stack);
        drop(scratch_items);
        drop(scratch_entries);
        drop(anchors);
        drop(documents);
        let mut value = document.unwrap_or(Value::Null);
        shrink_root_value(&mut value);
        Ok(value)
    }

    /// Finish the stream and hand back the alias budget it spent.
    pub(crate) fn finish_with_budget(self) -> Result<(Vec<Value>, AliasBudget), Error> {
        let Self {
            stack,
            mut documents,
            scratch_items,
            scratch_entries,
            anchors,
            document,
            budget,
            ..
        } = self;
        if !stack.is_empty() {
            return Err(Error::new(
                ErrorKind::Composition,
                "unterminated collection at end of stream",
            ));
        }
        drop(stack);
        drop(scratch_items);
        drop(scratch_entries);
        drop(anchors);
        drop(document);
        for doc in &mut documents {
            shrink_root_value(doc);
        }
        Ok((documents, budget))
    }

    /// Tag a finished collection; a tag adds one node and its text to the size estimate.
    fn finish_collection(&self, tag: Option<&Tag>, value: Value, size: usize) -> (Value, usize) {
        let value = apply_tag(tag, value);
        let size = match &value {
            Value::Tagged(tagged) => size
                .saturating_add(self.limits.node_cost_bytes)
                .saturating_add(tagged.tag.text_len()),
            _ => size,
        };
        (value, size)
    }

    fn feed(
        &mut self,
        value: Value,
        span: Span,
        size: usize,
        height: usize,
    ) -> Result<(), Box<Error>> {
        match self.stack.last_mut() {
            Some(Frame::Sequence {
                items,
                size: total,
                height: deepest,
                ..
            }) => {
                *total = total.saturating_add(size);
                *deepest = (*deepest).max(height);
                match items {
                    SeqBuf::Scratch { start, len } => {
                        if *len < SEQ_SCRATCH_LIMIT {
                            if self.scratch_items.capacity() == 0 {
                                self.scratch_items.reserve_exact(16);
                            }
                            self.scratch_items.push(value);
                            *len += 1;
                        } else {
                            let mut owned = Vec::with_capacity(SEQ_SCRATCH_LIMIT * 2);
                            owned.extend(self.scratch_items.drain(*start..));
                            owned.push(value);
                            *items = SeqBuf::Owned(owned);
                        }
                    }
                    SeqBuf::Owned(owned) => {
                        owned.push(value);
                    }
                }
            }
            Some(Frame::Mapping {
                mapping,
                pending_key,
                size: total,
                height: deepest,
                ..
            }) => {
                *total = total.saturating_add(size);
                *deepest = (*deepest).max(height);
                if let Some((key, key_span)) = pending_key.take() {
                    match mapping {
                        MapBuf::Scratch { start, len } => {
                            let existing = &self.scratch_entries[*start..];
                            if let Some((existing_key, _)) =
                                existing.iter().find(|(k, _)| k == &key)
                            {
                                return Err(Box::new(
                                    Error::new(
                                        ErrorKind::DuplicateKey,
                                        format!(
                                            "duplicate mapping key: {}",
                                            crate::value::key_text(existing_key)
                                        ),
                                    )
                                    .with_span(key_span),
                                ));
                            }
                            if usize::from(*len) < SMALL_LIMIT {
                                if self.scratch_entries.capacity() == 0 {
                                    self.scratch_entries.reserve_exact(SMALL_LIMIT);
                                }
                                self.scratch_entries.push((key, value));
                                *len += 1;
                            } else {
                                let mut large = Mapping::with_capacity(SMALL_LIMIT * 2);
                                for (k, v) in self.scratch_entries.drain(*start..) {
                                    drop(large.insert(k, v));
                                }
                                drop(large.insert(key, value));
                                *mapping = MapBuf::Large(large);
                            }
                        }
                        MapBuf::Large(large) => {
                            if let Err(duplicate) = large.insert_new(key, value) {
                                return Err(Box::new(
                                    Error::new(
                                        ErrorKind::DuplicateKey,
                                        format!("duplicate mapping key: {duplicate}"),
                                    )
                                    .with_span(key_span),
                                ));
                            }
                        }
                    }
                } else {
                    *pending_key = Some((value, span));
                }
            }
            None => {
                if self.document.is_some() {
                    return Err(self.error_at(span, "multiple root nodes in one document"));
                }
                self.document = Some(value);
            }
        }
        Ok(())
    }

    /// Note an anchored node about to be fed: its position is where each open collection
    /// will put its next child.
    fn record_anchor(
        &mut self,
        anchor: Option<&Anchor<'a>>,
        size: usize,
        height: usize,
        record: fn(&mut Anchors<AnchorDef>, &str, AnchorDef),
    ) {
        if let Some(anchor) = anchor {
            let path = self.stack.iter().map(Frame::position).collect();
            let definition = AnchorDef { path, size, height };
            record(&mut self.anchors, anchor.as_str(), definition);
        }
    }

    #[cold]
    #[inline(never)]
    #[allow(
        clippy::unused_self,
        clippy::unnecessary_box_returns,
        reason = "hot-path callers propagate Box<Error> to keep Result<_, Box<Error>> pointer-sized"
    )]
    fn error_at(&self, span: Span, message: impl Into<String>) -> Box<Error> {
        Box::new(Error::new(ErrorKind::Composition, message).with_span(span))
    }
}

#[cfg(test)]
mod tests {
    use super::compose_documents;
    use crate::error::ErrorKind;
    use crate::limits::Limits;
    use crate::parser::parse_events;
    use crate::value::Value;

    fn compose(input: &str) -> Value {
        let events = parse_events(input, &Limits::default()).expect("parse");
        compose_documents(&events, input.len(), &Limits::default())
            .expect("compose")
            .into_iter()
            .next()
            .expect("document")
    }

    #[test]
    fn composes_scalars_sequences_and_mappings() {
        let value = compose("name: ayaml\nitems:\n  - one\n  - two\n");
        assert_eq!(value["name"], Value::from("ayaml"));
        assert_eq!(value["items"][0], Value::from("one"));
        assert_eq!(value["items"][1], Value::from("two"));
    }

    fn fixture(name: &str) -> Option<String> {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat");
        std::fs::read_to_string(root.join(name)).ok()
    }

    #[test]
    fn compatibility_scalars_and_tags() {
        let Some(input) = fixture("scalar-tags-merges-aliases.yml") else {
            return;
        };
        let value = compose(&input);
        assert_eq!(value["plain_bool_word"], Value::from("yes"));
        assert_eq!(value["legacy_octal"], Value::from("0123"));
        assert_eq!(value["binary"].as_u64(), Some(3));
        assert_eq!(value["hex"].as_u64(), Some(31));
        assert_eq!(value["exponent"].as_f64(), Some(1000.0));
        assert!(value["null_value"].is_null());
        assert_eq!(value["core_string"], Value::from("123"));
        assert!(value["tagged"].is_tagged());
        assert_eq!(value["alias"], value["base"]);
        assert_eq!(value["merged"]["<<"], value["base"]);
    }

    #[test]
    fn duplicate_keys_are_rejected() {
        let input = "a: 1\na: 2\n";
        let events = parse_events(input, &Limits::default()).expect("parse");
        let error =
            compose_documents(&events, input.len(), &Limits::default()).expect_err("duplicate");
        assert_eq!(error.kind(), ErrorKind::DuplicateKey);
    }

    #[test]
    fn aliases_are_materialized_and_budgeted() {
        let input = "base: &b {x: 1}\nuse: *b\n";
        let value = compose(input);
        assert_eq!(value["use"], value["base"]);

        let events = parse_events(input, &Limits::default()).expect("parse");
        let limits = Limits {
            max_alias_expansion_bytes: 0,
            ..Limits::default()
        };
        let error = compose_documents(&events, input.len(), &limits).expect_err("budget");
        assert_eq!(error.kind(), ErrorKind::Limit);
    }

    #[test]
    fn anchors_are_scoped_to_their_document() {
        let input = "---\na: &x 1\n---\nb: *x\n";
        let limits = Limits {
            max_documents: 2,
            ..Limits::default()
        };
        let events = parse_events(input, &limits).expect("parse");
        let error = compose_documents(&events, input.len(), &limits).expect_err("cross-document");
        assert_eq!(error.kind(), ErrorKind::Composition);
        assert!(error.message().contains("unknown anchor"));
    }

    #[test]
    fn aliases_before_their_anchor_are_rejected() {
        let input = "a: *missing\n";
        let events = parse_events(input, &Limits::default()).expect("parse");
        let error = compose_documents(&events, input.len(), &Limits::default()).expect_err("alias");
        assert_eq!(error.kind(), ErrorKind::Composition);
        assert!(error.message().contains("unknown anchor"));
    }

    #[test]
    fn multiple_documents_compose_in_order() {
        let input = "a: 1\n---\nb: 2\n";
        let limits = Limits {
            max_documents: 2,
            ..Limits::default()
        };
        let events = parse_events(input, &limits).expect("parse");
        let documents = compose_documents(&events, input.len(), &limits).expect("compose");
        assert_eq!(documents.len(), 2);
        assert_eq!(documents[0]["a"].as_u64(), Some(1));
        assert_eq!(documents[1]["b"].as_u64(), Some(2));
    }

    #[test]
    fn toolchain_fixture_composes() {
        let Some(input) = fixture("toolchain-pins.yml") else {
            return;
        };
        let value = compose(&input);
        assert_eq!(value["version"].as_u64(), Some(1));
        assert!(value["facts"].is_sequence());
        assert!(value["rules"].is_sequence());
    }

    #[test]
    fn duplicate_key_diagnostic_matches_across_small_and_large_mappings() {
        let small_float = "0.0: 1\n-0.0: 2\n";
        let mut large_float = String::new();
        for idx in 0..8 {
            use std::fmt::Write as _;
            let _ = writeln!(large_float, "k{idx}: {idx}");
        }
        large_float.push_str(small_float);
        let small_err = crate::parse_value(small_float).expect_err("small duplicate");
        let large_err = crate::parse_value(&large_float).expect_err("large duplicate");
        assert_eq!(small_err.kind(), ErrorKind::DuplicateKey);
        assert_eq!(large_err.kind(), ErrorKind::DuplicateKey);
        assert_eq!(small_err.message(), large_err.message());

        let small_map = "? {a: 1, b: 2}\n: 1\n? {b: 2, a: 1}\n: 2\n";
        let mut large_map = String::new();
        for idx in 0..8 {
            use std::fmt::Write as _;
            let _ = writeln!(large_map, "k{idx}: {idx}");
        }
        large_map.push_str(small_map);
        let small_err = crate::parse_value(small_map).expect_err("small complex duplicate");
        let large_err = crate::parse_value(&large_map).expect_err("large complex duplicate");
        assert_eq!(small_err.message(), large_err.message());
    }

    #[test]
    fn aliases_resolve_across_scratch_and_spilled_collections_and_pending_keys() {
        // Sequence spilling past SEQ_SCRATCH_LIMIT (64) with aliases before and after the spill.
        let mut seq_yaml = String::from("- &first {a: 1, b: 2}\n- *first\n");
        for idx in 0..70 {
            use std::fmt::Write as _;
            let _ = writeln!(seq_yaml, "- {idx}");
        }
        seq_yaml.push_str("- *first\n");
        let seq = crate::parse_value(&seq_yaml).expect("spilled seq with aliases");
        let items = seq.as_sequence().expect("sequence");
        assert_eq!(items.len(), 73);
        assert_eq!(items[0], items[1]);
        assert_eq!(items[0], items[72]);

        // Mapping spilling past SMALL_LIMIT (8) with aliases before and after the spill.
        let mut map_yaml = String::from("a0: &m0 [1, 2]\na1: *m0\n");
        for idx in 2..12 {
            use std::fmt::Write as _;
            let _ = writeln!(map_yaml, "a{idx}: {idx}");
        }
        map_yaml.push_str("tail: *m0\n");
        let map = crate::parse_value(&map_yaml).expect("spilled map with aliases");
        assert_eq!(map["a0"], map["a1"]);
        assert_eq!(map["a0"], map["tail"]);

        // Alias targeting an anchored pending complex key while a nested mapping spills to Large.
        let mut complex_yaml = String::from("? &pk {k: [1, 2]}\n:\n");
        for idx in 0..10 {
            use std::fmt::Write as _;
            let _ = writeln!(complex_yaml, "  n{idx}: {idx}");
        }
        complex_yaml.push_str("  ref_key: *pk\n");
        let complex = crate::parse_value(&complex_yaml).expect("alias to pending complex key");
        let root_map = complex.as_mapping().expect("root mapping");
        let (key_val, val_map) = root_map.get_index(0).expect("first entry");
        assert_eq!(&val_map["ref_key"], key_val);
    }
}
