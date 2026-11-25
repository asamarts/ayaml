#![allow(
    clippy::mutable_key_type,
    reason = "Value's hash cache is interior-immutable after construction and never changes equality"
)]

//! Opt-in YAML 1.1 merge-key expansion at the spanned event layer (AY-147).
//!
//! Expands plain `<<` (and `!!merge <<`) mapping entries while keeping quoted `"<<"`, `'<<'`,
//! block `<<`, and `!!str <<` keys literal. Every expanded entry retains its original source
//! [`Span`] and borrowed text slices, preserves lexical anchor/alias scoping, shares anchored
//! subtrees via `Arc` without deep-cloning, and enforces all resource limits (`max_depth`,
//! `max_collection_entries`, `max_events`, `max_anchors`, `max_alias_dereferences`, and
//! `max_alias_expansion_bytes`) before allocation.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::composer::Anchors;
use crate::error::{Error, ErrorCode, ErrorKind};
use crate::event::{Anchor, CollectionStyle, Event, ScalarStyle};
use crate::limits::{AliasBudget, Limits};
use crate::parser::EventCursor;
use crate::resolver::resolve_scalar;
use crate::span::Span;
use crate::tag::{Tag, TaggedValue};
use crate::value::Value;

/// Pull-based event stream that expands YAML 1.1 `<<` merge keys one document at a time.
#[derive(Debug)]
pub(crate) struct MergedEventStream<'a> {
    cursor: EventCursor<'a>,
    limits: Limits,
    input: &'a str,
    budget: AliasBudget,
    queue: VecDeque<Event<'a>>,
    stream_started: bool,
    stream_ended: bool,
    emitted_events: usize,
}

impl<'a> MergedEventStream<'a> {
    pub(crate) fn new(input: &'a str, limits: &Limits) -> Result<Self, Error> {
        let cursor = EventCursor::new(input, limits)?;
        Ok(Self::from_cursor(cursor, input, limits))
    }

    fn new_single_document(input: &'a str, limits: &Limits) -> Result<Self, Error> {
        let cursor = EventCursor::new_single_document(input, limits)?;
        Ok(Self::from_cursor(cursor, input, limits))
    }

    fn from_cursor(cursor: EventCursor<'a>, input: &'a str, limits: &Limits) -> Self {
        Self {
            cursor,
            limits: limits.clone(),
            input,
            budget: AliasBudget::default(),
            queue: VecDeque::new(),
            stream_started: false,
            stream_ended: false,
            emitted_events: 0,
        }
    }

    fn start_stream(&mut self) -> Result<Span, Error> {
        let span = self.cursor.take_stream_start_span()?;
        self.stream_started = true;
        self.emitted_events += 1;
        self.limits.check_events(self.emitted_events).map_err(|e| {
            Error::from(e)
                .with_span(span)
                .with_input_location(self.input, false)
        })?;
        Ok(span)
    }

    fn end_stream(&mut self) -> Result<Span, Error> {
        self.stream_ended = true;
        let span = Span::empty(self.input.len());
        self.emitted_events += 1;
        self.limits.check_events(self.emitted_events).map_err(|e| {
            Error::from(e)
                .with_span(span)
                .with_input_location(self.input, false)
        })?;
        Ok(span)
    }

    pub(crate) fn next_event(&mut self) -> Result<Option<Event<'a>>, Error> {
        let result = self.next_event_inner();
        if result.is_err() {
            self.stream_ended = true;
            self.queue.clear();
        }
        result
    }

    fn next_event_inner(&mut self) -> Result<Option<Event<'a>>, Error> {
        if let Some(event) = self.queue.pop_front() {
            return Ok(Some(event));
        }
        if self.stream_ended {
            return Ok(None);
        }
        if !self.stream_started {
            let span = self.start_stream()?;
            return Ok(Some(Event::StreamStart { span }));
        }
        if let Some(events) = self.next_document_inner()? {
            self.queue = VecDeque::from(events);
            return Ok(self.queue.pop_front());
        }
        let span = self.end_stream()?;
        Ok(Some(Event::StreamEnd { span }))
    }

    pub(crate) fn next_document_with_budget(
        &mut self,
        budget: &mut AliasBudget,
    ) -> Result<Option<Vec<Event<'a>>>, Error> {
        if self.stream_ended {
            return Ok(None);
        }
        let result = self.next_document_using_budget(budget);
        if result.is_err() {
            self.stream_ended = true;
            self.queue.clear();
        }
        result
    }

    fn next_document_inner(&mut self) -> Result<Option<Vec<Event<'a>>>, Error> {
        let mut budget = self.budget;
        let result = self.next_document_using_budget(&mut budget);
        self.budget = budget;
        result
    }

    fn next_document_using_budget(
        &mut self,
        budget: &mut AliasBudget,
    ) -> Result<Option<Vec<Event<'a>>>, Error> {
        if !self.stream_started {
            let _span = self.start_stream()?;
        }
        let Some(raw_events) = self.cursor.next_document()? else {
            let _span = self.end_stream()?;
            return Ok(None);
        };
        let expanded = expand_document_events(
            raw_events,
            self.input.len(),
            &self.limits,
            budget,
            &mut self.emitted_events,
        )
        .map_err(|error| error.with_input_location(self.input, false))?;
        Ok(Some(expanded))
    }
}

/// Expand every document in a stream with merge keys enabled, returning the combined events and
/// the alias budget already consumed during merge expansion.
pub(crate) fn expand_all_documents<'a>(
    input: &'a str,
    limits: &Limits,
) -> Result<(Vec<Event<'a>>, AliasBudget), Error> {
    let mut stream = MergedEventStream::new(input, limits)?;
    let mut events = Vec::new();
    while let Some(event) = stream.next_event()? {
        events.push(event);
    }
    Ok((events, stream.budget))
}

/// Expand a single-document input with merge keys enabled, rejecting trailing documents before
/// expanding any second document and keeping `max_events` parity with `parse_events_with`.
pub(crate) fn expand_single_document<'a>(
    input: &'a str,
    limits: &Limits,
) -> Result<(Vec<Event<'a>>, AliasBudget), Error> {
    let mut stream = MergedEventStream::new_single_document(input, limits)?;
    let _start = stream.start_stream()?;
    let first = match stream.cursor.next_document()? {
        Some(raw_events) => Some(
            expand_document_events(
                raw_events,
                input.len(),
                limits,
                &mut stream.budget,
                &mut stream.emitted_events,
            )
            .map_err(|error| error.with_input_location(input, false))?,
        ),
        None => None,
    };
    if stream.cursor.next_document()?.is_some() {
        return Err(Error::coded(
            ErrorKind::Parser,
            ErrorCode::TrailingDocuments,
            "expected a single YAML document, but found trailing documents",
        )
        .with_span(Span::empty(input.len()))
        .with_input_location(input, false));
    }
    let _end = stream.end_stream()?;
    Ok((first.unwrap_or_default(), stream.budget))
}

#[derive(Debug)]
struct EventNode<'a> {
    kind: EventNodeKind<'a>,
    span: Span,
    /// Estimated byte cost for alias/merge expansion budgeting.
    size: usize,
    /// Nesting height (0 for scalars/aliases, 1+ for collections).
    height: usize,
}

#[derive(Debug)]
enum AliasBinding<'a> {
    Resolved {
        epoch: u64,
        node: Arc<EventNode<'a>>,
    },
    Cyclic,
    Unknown,
}

#[derive(Debug)]
enum EventNodeKind<'a> {
    Scalar {
        value: Cow<'a, str>,
        raw: Cow<'a, str>,
        style: ScalarStyle,
        tag: Option<Tag>,
        anchor: Option<(Anchor<'a>, u64)>,
    },
    Alias {
        name: Anchor<'a>,
        binding: AliasBinding<'a>,
    },
    Sequence {
        style: CollectionStyle,
        tag: Option<Tag>,
        anchor: Option<(Anchor<'a>, u64)>,
        end_span: Span,
        items: Vec<Arc<EventNode<'a>>>,
    },
    Mapping {
        style: CollectionStyle,
        tag: Option<Tag>,
        anchor: Option<(Anchor<'a>, u64)>,
        end_span: Span,
        /// `(key, value, strip_anchors_on_emit)`
        entries: Vec<(Arc<EventNode<'a>>, Arc<EventNode<'a>>, bool)>,
    },
}

#[derive(Clone)]
struct AnchoredNode<'a> {
    epoch: u64,
    node: Arc<EventNode<'a>>,
}

enum Frame<'a> {
    Sequence {
        style: CollectionStyle,
        tag: Option<Tag>,
        anchor: Option<(Anchor<'a>, u64)>,
        span: Span,
        items: Vec<Arc<EventNode<'a>>>,
        size: usize,
        height: usize,
    },
    Mapping {
        style: CollectionStyle,
        tag: Option<Tag>,
        anchor: Option<(Anchor<'a>, u64)>,
        span: Span,
        entries: Vec<(Arc<EventNode<'a>>, Arc<EventNode<'a>>, bool)>,
        key: Option<Arc<EventNode<'a>>>,
        merge_operand: Option<(Span, Arc<EventNode<'a>>)>,
        size: usize,
        height: usize,
    },
}

fn is_merge_key(node: &EventNode<'_>) -> bool {
    match &node.kind {
        EventNodeKind::Scalar {
            value, style, tag, ..
        } => {
            if value != "<<" {
                return false;
            }
            match tag {
                None => *style == ScalarStyle::Plain,
                Some(tag) => tag.is_core() && tag.suffix() == "merge",
            }
        }
        _ => false,
    }
}

fn is_core_collection_tag(tag: &Tag, expected_suffix: &str) -> bool {
    tag.is_core() && tag.suffix() == expected_suffix
}

fn wrap_collection_tag(tag: Option<&Tag>, value: Value) -> Value {
    match tag {
        Some(tag) if !is_dropped_core_collection_tag(tag) => {
            Value::Tagged(Box::new(TaggedValue::new(tag.clone(), value)))
        }
        _ => value,
    }
}

fn is_dropped_core_collection_tag(tag: &Tag) -> bool {
    tag.is_core()
        && matches!(
            tag.suffix(),
            "null" | "bool" | "int" | "float" | "str" | "seq" | "map"
        )
}

fn node_key_value(
    node: &EventNode<'_>,
    remaining_depth: usize,
    in_alias: bool,
    key_budget: &mut AliasBudget,
    input_len: usize,
    limits: &Limits,
) -> Result<Option<Value>, Error> {
    match &node.kind {
        EventNodeKind::Scalar {
            value, style, tag, ..
        } => {
            if in_alias {
                let mut cost = limits.node_cost_bytes.saturating_add(value.len());
                if let Some(tag) = tag {
                    cost = cost.saturating_add(tag.text_len());
                }
                key_budget
                    .charge_bytes(limits, input_len, cost)
                    .map_err(|e| e.with_span(node.span))?;
            }
            Ok(resolve_scalar(value, *style, tag.as_ref()).ok())
        }
        EventNodeKind::Sequence { items, tag, .. } => {
            let Some(next_depth) = remaining_depth.checked_sub(1) else {
                return Ok(None);
            };
            if in_alias {
                let mut cost = limits.node_cost_bytes;
                if let Some(tag) = tag {
                    cost = cost.saturating_add(tag.text_len());
                }
                key_budget
                    .charge_bytes(limits, input_len, cost)
                    .map_err(|e| e.with_span(node.span))?;
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let Some(val) =
                    node_key_value(item, next_depth, in_alias, key_budget, input_len, limits)?
                else {
                    return Ok(None);
                };
                out.push(val);
            }
            Ok(Some(wrap_collection_tag(
                tag.as_ref(),
                Value::Sequence(out),
            )))
        }
        EventNodeKind::Mapping { entries, tag, .. } => {
            let Some(next_depth) = remaining_depth.checked_sub(1) else {
                return Ok(None);
            };
            if in_alias {
                let mut cost = limits.node_cost_bytes;
                if let Some(tag) = tag {
                    cost = cost.saturating_add(tag.text_len());
                }
                key_budget
                    .charge_bytes(limits, input_len, cost)
                    .map_err(|e| e.with_span(node.span))?;
            }
            let mut mapping = crate::Mapping::with_capacity(entries.len());
            for (k, v, _) in entries {
                let Some(kv) =
                    node_key_value(k, next_depth, in_alias, key_budget, input_len, limits)?
                else {
                    return Ok(None);
                };
                let Some(vv) =
                    node_key_value(v, next_depth, in_alias, key_budget, input_len, limits)?
                else {
                    return Ok(None);
                };
                mapping.insert(kv, vv);
            }
            Ok(Some(wrap_collection_tag(
                tag.as_ref(),
                Value::Mapping(mapping),
            )))
        }
        EventNodeKind::Alias { binding, .. } => match binding {
            AliasBinding::Resolved { node: target, .. } => {
                // Aliases are acyclic (checked at binding time) and add no collection nesting
                // depth; charge the alias budget before expanding to prevent alias-bomb keys.
                key_budget
                    .charge(limits, input_len, limits.node_cost_bytes)
                    .map_err(|e| e.with_span(node.span))?;
                node_key_value(target, remaining_depth, true, key_budget, input_len, limits)
            }
            AliasBinding::Cyclic | AliasBinding::Unknown => Ok(None),
        },
    }
}

/// Validate a dropped inline merge entry so invalid core-schema scalars or unresolved/cyclic
/// aliases inside an overridden inline merge operand are not silently swallowed.
fn validate_dropped_node(root: &EventNode<'_>) -> Result<(), Error> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match &node.kind {
            EventNodeKind::Scalar {
                value, style, tag, ..
            } => {
                if let Some(tag) = tag {
                    resolve_scalar(value, *style, Some(tag))
                        .map_err(|err| err.with_span(node.span))?;
                }
            }
            EventNodeKind::Alias { name, binding } => match binding {
                AliasBinding::Resolved { .. } => {}
                AliasBinding::Cyclic => {
                    return Err(Error::coded(
                        ErrorKind::Composition,
                        ErrorCode::CyclicAlias,
                        format!("cyclic alias `*{}`", name.as_str()),
                    )
                    .with_span(node.span));
                }
                AliasBinding::Unknown => {
                    return Err(Error::coded(
                        ErrorKind::Composition,
                        ErrorCode::UnknownAnchor,
                        format!("unknown anchor `{}`", name.as_str()),
                    )
                    .with_span(node.span));
                }
            },
            EventNodeKind::Sequence { items, .. } => {
                for item in items {
                    stack.push(item);
                }
            }
            EventNodeKind::Mapping { entries, .. } => {
                for (k, v, _) in entries {
                    stack.push(k);
                    stack.push(v);
                }
            }
        }
    }
    Ok(())
}

fn expand_document_events<'a>(
    raw_events: Vec<Event<'a>>,
    input_len: usize,
    limits: &Limits,
    budget: &mut AliasBudget,
    emitted_events: &mut usize,
) -> Result<Vec<Event<'a>>, Error> {
    let mut prefix: Vec<Event<'a>> = Vec::new();
    let mut doc_start: Option<Event<'a>> = None;
    let mut doc_end: Option<Event<'a>> = None;
    let mut stack: Vec<Frame<'a>> = Vec::new();
    let mut root: Option<Arc<EventNode<'a>>> = None;
    let mut anchors: Anchors<AnchoredNode<'a>> = Anchors::new();
    let mut next_epoch: u64 = 1;

    for event in raw_events {
        match event {
            Event::StreamStart { .. } | Event::StreamEnd { .. } => {}
            Event::Directive { .. } => prefix.push(event),
            Event::DocumentStart { .. } => doc_start = Some(event),
            Event::DocumentEnd { .. } => doc_end = Some(event),
            Event::Scalar {
                value,
                raw,
                style,
                tag,
                anchor,
                span,
            } => {
                let mut size = limits.node_cost_bytes.saturating_add(value.len());
                if let Some(tag) = &tag {
                    size = size.saturating_add(tag.text_len());
                }
                let anchor_with_epoch = anchor.map(|a| {
                    let epoch = next_epoch;
                    next_epoch = next_epoch.wrapping_add(1);
                    (a, epoch)
                });
                let node = Arc::new(EventNode {
                    kind: EventNodeKind::Scalar {
                        value,
                        raw,
                        style,
                        tag,
                        anchor: anchor_with_epoch.clone(),
                    },
                    span,
                    size,
                    height: 0,
                });
                if let Some((anchor, epoch)) = anchor_with_epoch {
                    anchors.define(
                        anchor.as_str(),
                        AnchoredNode {
                            epoch,
                            node: Arc::clone(&node),
                        },
                    );
                }
                push_built(&mut stack, &mut root, node, limits)?;
            }
            Event::Alias { anchor, span } => {
                let binding = match anchors.resolve(anchor.as_str()) {
                    Ok(anchored) => AliasBinding::Resolved {
                        epoch: anchored.epoch,
                        node: Arc::clone(&anchored.node),
                    },
                    Err((ErrorCode::CyclicAlias, _)) => AliasBinding::Cyclic,
                    Err(_) => AliasBinding::Unknown,
                };
                let node = Arc::new(EventNode {
                    kind: EventNodeKind::Alias {
                        name: anchor,
                        binding,
                    },
                    span,
                    size: limits.node_cost_bytes,
                    height: 0,
                });
                push_built(&mut stack, &mut root, node, limits)?;
            }
            Event::SequenceStart {
                style,
                tag,
                anchor,
                span,
            } => {
                limits
                    .check_depth(stack.len() + 1)
                    .map_err(|e| Error::from(e).with_span(span))?;
                let anchor_with_epoch = anchor.map(|a| {
                    let epoch = next_epoch;
                    next_epoch = next_epoch.wrapping_add(1);
                    anchors.open(a.as_str());
                    (a, epoch)
                });
                stack.push(Frame::Sequence {
                    style,
                    tag,
                    anchor: anchor_with_epoch,
                    span,
                    items: Vec::new(),
                    size: limits.node_cost_bytes,
                    height: 0,
                });
            }
            Event::MappingStart {
                style,
                tag,
                anchor,
                span,
            } => {
                limits
                    .check_depth(stack.len() + 1)
                    .map_err(|e| Error::from(e).with_span(span))?;
                let anchor_with_epoch = anchor.map(|a| {
                    let epoch = next_epoch;
                    next_epoch = next_epoch.wrapping_add(1);
                    anchors.open(a.as_str());
                    (a, epoch)
                });
                stack.push(Frame::Mapping {
                    style,
                    tag,
                    anchor: anchor_with_epoch,
                    span,
                    entries: Vec::new(),
                    key: None,
                    merge_operand: None,
                    size: limits.node_cost_bytes,
                    height: 0,
                });
            }
            Event::SequenceEnd { span: end_span } => {
                finish_sequence_frame(&mut stack, &mut root, &mut anchors, end_span, limits)?;
            }
            Event::MappingEnd { span: end_span } => {
                finish_mapping_frame(
                    &mut stack,
                    &mut root,
                    &mut anchors,
                    end_span,
                    input_len,
                    limits,
                    budget,
                )?;
            }
        }
    }

    let mut out = Vec::new();
    for event in prefix {
        push_event(&mut out, event, limits, emitted_events)?;
    }
    if let Some(event) = doc_start {
        push_event(&mut out, event, limits, emitted_events)?;
    }
    if let Some(node) = root {
        emit_node(node, &mut out, input_len, limits, budget, emitted_events)?;
    }
    if let Some(event) = doc_end {
        push_event(&mut out, event, limits, emitted_events)?;
    }
    Ok(out)
}

fn finish_sequence_frame<'a>(
    stack: &mut Vec<Frame<'a>>,
    root: &mut Option<Arc<EventNode<'a>>>,
    anchors: &mut Anchors<AnchoredNode<'a>>,
    end_span: Span,
    limits: &Limits,
) -> Result<(), Error> {
    let Some(Frame::Sequence {
        style,
        tag,
        anchor,
        span,
        items,
        size,
        height,
    }) = stack.pop()
    else {
        return Err(Error::coded(
            ErrorKind::Composition,
            ErrorCode::MalformedEventStream,
            "unexpected sequence end",
        )
        .with_span(end_span));
    };
    let mut total_size = size;
    if let Some(tag) = &tag {
        total_size = total_size.saturating_add(tag.text_len());
    }
    let node = Arc::new(EventNode {
        kind: EventNodeKind::Sequence {
            style,
            tag,
            anchor: anchor.clone(),
            end_span,
            items,
        },
        span,
        size: total_size,
        height: height + 1,
    });
    if let Some((anchor, epoch)) = anchor {
        anchors.close(
            anchor.as_str(),
            AnchoredNode {
                epoch,
                node: Arc::clone(&node),
            },
        );
    }
    push_built(stack, root, node, limits)
}

#[allow(
    clippy::too_many_arguments,
    reason = "internal frame-closing helper threading bounded document state"
)]
fn finish_mapping_frame<'a>(
    stack: &mut Vec<Frame<'a>>,
    root: &mut Option<Arc<EventNode<'a>>>,
    anchors: &mut Anchors<AnchoredNode<'a>>,
    end_span: Span,
    input_len: usize,
    limits: &Limits,
    budget: &mut AliasBudget,
) -> Result<(), Error> {
    let Some(Frame::Mapping {
        style,
        tag,
        anchor,
        span,
        mut entries,
        key: None,
        merge_operand,
        mut size,
        mut height,
    }) = stack.pop()
    else {
        return Err(Error::coded(
            ErrorKind::Composition,
            ErrorCode::MalformedEventStream,
            "unexpected mapping end",
        )
        .with_span(end_span));
    };
    if let Some((merge_span, operand)) = merge_operand {
        apply_event_merge(
            operand,
            merge_span,
            &mut entries,
            &mut size,
            &mut height,
            stack.len() + 1,
            input_len,
            limits,
            budget,
        )?;
    }
    if let Some(tag) = &tag {
        size = size.saturating_add(tag.text_len());
    }
    let node = Arc::new(EventNode {
        kind: EventNodeKind::Mapping {
            style,
            tag,
            anchor: anchor.clone(),
            end_span,
            entries,
        },
        span,
        size,
        height: height + 1,
    });
    if let Some((anchor, epoch)) = anchor {
        anchors.close(
            anchor.as_str(),
            AnchoredNode {
                epoch,
                node: Arc::clone(&node),
            },
        );
    }
    push_built(stack, root, node, limits)
}

fn push_built<'a>(
    stack: &mut [Frame<'a>],
    root: &mut Option<Arc<EventNode<'a>>>,
    node: Arc<EventNode<'a>>,
    limits: &Limits,
) -> Result<(), Error> {
    match stack.last_mut() {
        None => *root = Some(node),
        Some(Frame::Sequence {
            items,
            size,
            height,
            ..
        }) => {
            limits
                .check_collection_entries(items.len() + 1)
                .map_err(|e| Error::from(e).with_span(node.span))?;
            *size = size.saturating_add(node.size);
            *height = (*height).max(node.height);
            items.push(node);
        }
        Some(Frame::Mapping {
            entries,
            key,
            merge_operand,
            size,
            height,
            ..
        }) => match key.take() {
            None => *key = Some(node),
            Some(entry_key) => {
                if is_merge_key(&entry_key) {
                    if merge_operand.is_some() {
                        return Err(Error::coded(
                            ErrorKind::DuplicateKey,
                            ErrorCode::DuplicateKey,
                            "duplicate mapping key: <<",
                        )
                        .with_span(entry_key.span));
                    }
                    *merge_operand = Some((node.span, node));
                } else {
                    limits
                        .check_collection_entries(entries.len() + 1)
                        .map_err(|e| Error::from(e).with_span(entry_key.span))?;
                    *size = size
                        .saturating_add(entry_key.size)
                        .saturating_add(node.size);
                    *height = (*height).max(entry_key.height).max(node.height);
                    entries.push((entry_key, node, false));
                }
            }
        },
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "internal merge helper threading bounded state across one mapping"
)]
fn apply_event_merge<'a>(
    operand: Arc<EventNode<'a>>,
    merge_span: Span,
    entries: &mut Vec<(Arc<EventNode<'a>>, Arc<EventNode<'a>>, bool)>,
    size: &mut usize,
    height: &mut usize,
    current_depth: usize,
    input_len: usize,
    limits: &Limits,
    budget: &mut AliasBudget,
) -> Result<(), Error> {
    let mut key_budget = *budget;
    let mut seen: HashSet<Value> = HashSet::with_capacity(entries.len());
    for (k, _, _) in entries.iter() {
        if let Some(val) = node_key_value(
            k,
            limits.max_depth,
            false,
            &mut key_budget,
            input_len,
            limits,
        )? {
            seen.insert(val);
        }
    }

    let (resolved, operand_from_alias) = resolve_merge_node(operand, input_len, limits, budget)?;
    match &resolved.kind {
        EventNodeKind::Mapping {
            tag,
            entries: src_entries,
            ..
        } => {
            if let Some(tag) = tag
                && !is_core_collection_tag(tag, "map")
            {
                return Err(Error::coded(
                    ErrorKind::Composition,
                    ErrorCode::InvalidMergeOperand,
                    "merge operand must be a mapping or a sequence of mappings",
                )
                .with_span(merge_span));
            }
            merge_entries(
                src_entries,
                operand_from_alias,
                merge_span,
                entries,
                &mut seen,
                size,
                height,
                current_depth,
                &mut key_budget,
                input_len,
                limits,
            )
        }
        EventNodeKind::Sequence { tag, items, .. } => {
            if let Some(tag) = tag
                && !is_core_collection_tag(tag, "seq")
            {
                return Err(Error::coded(
                    ErrorKind::Composition,
                    ErrorCode::InvalidMergeOperand,
                    "merge operand must be a mapping or a sequence of mappings",
                )
                .with_span(merge_span));
            }
            for item in items {
                let item_span = item.span;
                let (resolved_item, item_from_alias) =
                    resolve_merge_node(Arc::clone(item), input_len, limits, budget)?;
                let EventNodeKind::Mapping {
                    tag: item_tag,
                    entries: src_entries,
                    ..
                } = &resolved_item.kind
                else {
                    return Err(Error::coded(
                        ErrorKind::Composition,
                        ErrorCode::InvalidMergeOperand,
                        "merge sequence operands must be mappings",
                    )
                    .with_span(item_span));
                };
                if let Some(tag) = item_tag
                    && !is_core_collection_tag(tag, "map")
                {
                    return Err(Error::coded(
                        ErrorKind::Composition,
                        ErrorCode::InvalidMergeOperand,
                        "merge sequence operands must be mappings",
                    )
                    .with_span(item_span));
                }
                merge_entries(
                    src_entries,
                    operand_from_alias || item_from_alias,
                    item_span,
                    entries,
                    &mut seen,
                    size,
                    height,
                    current_depth,
                    &mut key_budget,
                    input_len,
                    limits,
                )?;
            }
            Ok(())
        }
        _ => Err(Error::coded(
            ErrorKind::Composition,
            ErrorCode::InvalidMergeOperand,
            "merge operand must be a mapping or a sequence of mappings",
        )
        .with_span(merge_span)),
    }
}

fn resolve_merge_node<'a>(
    node: Arc<EventNode<'a>>,
    input_len: usize,
    limits: &Limits,
    budget: &mut AliasBudget,
) -> Result<(Arc<EventNode<'a>>, bool), Error> {
    let EventNodeKind::Alias { name, binding } = &node.kind else {
        return Ok((node, false));
    };
    let span = node.span;
    let target = match binding {
        AliasBinding::Resolved { node: target, .. } => target,
        AliasBinding::Cyclic => {
            return Err(Error::coded(
                ErrorKind::Composition,
                ErrorCode::CyclicAlias,
                format!("cyclic alias `*{}`", name.as_str()),
            )
            .with_span(span));
        }
        AliasBinding::Unknown => {
            return Err(Error::coded(
                ErrorKind::Composition,
                ErrorCode::UnknownAnchor,
                format!("unknown anchor `{}`", name.as_str()),
            )
            .with_span(span));
        }
    };
    budget
        .charge(limits, input_len, target.size)
        .map_err(|error| error.with_span(span))?;
    Ok((Arc::clone(target), true))
}

#[allow(
    clippy::too_many_arguments,
    reason = "internal merge helper threading bounded state across one mapping"
)]
fn merge_entries<'a>(
    src_entries: &[(Arc<EventNode<'a>>, Arc<EventNode<'a>>, bool)],
    from_alias: bool,
    merge_span: Span,
    entries: &mut Vec<(Arc<EventNode<'a>>, Arc<EventNode<'a>>, bool)>,
    seen: &mut HashSet<Value>,
    size: &mut usize,
    height: &mut usize,
    current_depth: usize,
    key_budget: &mut AliasBudget,
    input_len: usize,
    limits: &Limits,
) -> Result<(), Error> {
    let mut inline_seen = (!from_alias).then(|| HashSet::with_capacity(src_entries.len()));
    for (src_key, src_val, src_strip) in src_entries {
        let key_val = node_key_value(
            src_key,
            limits.max_depth,
            false,
            key_budget,
            input_len,
            limits,
        )?;
        if let Some(ref mut local) = inline_seen
            && let Some(ref kv) = key_val
            && !local.insert(kv.clone())
        {
            return Err(Error::coded(
                ErrorKind::DuplicateKey,
                ErrorCode::DuplicateKey,
                format!("duplicate mapping key: {}", crate::value::key_text(kv)),
            )
            .with_span(src_key.span));
        }
        if let Some(kv) = key_val
            && !seen.insert(kv)
        {
            if !from_alias {
                validate_dropped_node(src_key)?;
                validate_dropped_node(src_val)?;
            }
            continue;
        }
        limits
            .check_collection_entries(entries.len() + 1)
            .map_err(|e| Error::from(e).with_span(merge_span))?;
        let entry_height = src_key.height.max(src_val.height);
        limits
            .check_depth(current_depth.saturating_add(entry_height))
            .map_err(|e| Error::from(e).with_span(merge_span))?;
        let entry_cost = src_key.size.saturating_add(src_val.size);
        *size = size.saturating_add(entry_cost);
        *height = (*height).max(entry_height);
        entries.push((
            Arc::clone(src_key),
            Arc::clone(src_val),
            from_alias || *src_strip,
        ));
    }
    Ok(())
}

enum EmitStep<'a> {
    Node {
        node: Arc<EventNode<'a>>,
        strip_anchors: bool,
        depth: usize,
    },
    Event(Event<'a>),
    CloseAnchor(Anchor<'a>),
}

fn emit_node<'a>(
    root: Arc<EventNode<'a>>,
    out: &mut Vec<Event<'a>>,
    input_len: usize,
    limits: &Limits,
    budget: &mut AliasBudget,
    emitted_events: &mut usize,
) -> Result<(), Error> {
    let mut emitted_anchors: HashMap<String, u64> = HashMap::new();
    let mut open_anchors: HashMap<String, usize> = HashMap::new();
    let mut stack = vec![EmitStep::Node {
        node: root,
        strip_anchors: false,
        depth: 0,
    }];
    while let Some(step) = stack.pop() {
        match step {
            EmitStep::Event(event) => push_event(out, event, limits, emitted_events)?,
            EmitStep::CloseAnchor(anchor) => {
                if let Some(count) = open_anchors.get_mut(anchor.as_str()) {
                    *count -= 1;
                    if *count == 0 {
                        open_anchors.remove(anchor.as_str());
                    }
                }
            }
            EmitStep::Node {
                node,
                strip_anchors,
                depth,
            } => {
                let span = node.span;
                match &node.kind {
                    EventNodeKind::Scalar {
                        value,
                        raw,
                        style,
                        tag,
                        anchor,
                    } => {
                        let emit_anchor = if strip_anchors {
                            None
                        } else {
                            anchor.as_ref().map(|(a, epoch)| {
                                emitted_anchors.insert(a.as_str().to_owned(), *epoch);
                                a.clone()
                            })
                        };
                        push_event(
                            out,
                            Event::Scalar {
                                value: value.clone(),
                                raw: raw.clone(),
                                style: *style,
                                tag: tag.clone(),
                                anchor: emit_anchor,
                                span,
                            },
                            limits,
                            emitted_events,
                        )?;
                    }
                    EventNodeKind::Alias { name, binding } => match binding {
                        AliasBinding::Resolved {
                            epoch,
                            node: target,
                        } => {
                            let same_live = emitted_anchors.get(name.as_str()) == Some(epoch)
                                && !open_anchors.contains_key(name.as_str());
                            if same_live {
                                push_event(
                                    out,
                                    Event::Alias {
                                        anchor: name.clone(),
                                        span,
                                    },
                                    limits,
                                    emitted_events,
                                )?;
                            } else {
                                // The lexical target was redefined, is an active ancestor anchor,
                                // or lived inside a dropped/reordered inline merge operand.
                                // Expand the exact lexical target inline under the alias budget.
                                limits
                                    .check_depth(depth.saturating_add(target.height))
                                    .map_err(|e| Error::from(e).with_span(span))?;
                                budget
                                    .charge(limits, input_len, target.size)
                                    .map_err(|e| e.with_span(span))?;
                                stack.push(EmitStep::Node {
                                    node: Arc::clone(target),
                                    strip_anchors: true,
                                    depth,
                                });
                            }
                        }
                        AliasBinding::Cyclic | AliasBinding::Unknown => {
                            push_event(
                                out,
                                Event::Alias {
                                    anchor: name.clone(),
                                    span,
                                },
                                limits,
                                emitted_events,
                            )?;
                        }
                    },
                    EventNodeKind::Sequence {
                        style,
                        tag,
                        anchor,
                        end_span,
                        items,
                    } => {
                        let next_depth = depth.saturating_add(1);
                        limits
                            .check_depth(next_depth)
                            .map_err(|e| Error::from(e).with_span(span))?;
                        let emit_anchor = if strip_anchors {
                            None
                        } else {
                            anchor.as_ref().map(|(a, epoch)| {
                                emitted_anchors.insert(a.as_str().to_owned(), *epoch);
                                *open_anchors.entry(a.as_str().to_owned()).or_insert(0) += 1;
                                a.clone()
                            })
                        };
                        push_event(
                            out,
                            Event::SequenceStart {
                                style: *style,
                                tag: tag.clone(),
                                anchor: emit_anchor.clone(),
                                span,
                            },
                            limits,
                            emitted_events,
                        )?;
                        stack.push(EmitStep::Event(Event::SequenceEnd { span: *end_span }));
                        if let Some(a) = emit_anchor {
                            stack.push(EmitStep::CloseAnchor(a));
                        }
                        for item in items.iter().rev() {
                            stack.push(EmitStep::Node {
                                node: Arc::clone(item),
                                strip_anchors,
                                depth: next_depth,
                            });
                        }
                    }
                    EventNodeKind::Mapping {
                        style,
                        tag,
                        anchor,
                        end_span,
                        entries,
                    } => {
                        let next_depth = depth.saturating_add(1);
                        limits
                            .check_depth(next_depth)
                            .map_err(|e| Error::from(e).with_span(span))?;
                        let emit_anchor = if strip_anchors {
                            None
                        } else {
                            anchor.as_ref().map(|(a, epoch)| {
                                emitted_anchors.insert(a.as_str().to_owned(), *epoch);
                                *open_anchors.entry(a.as_str().to_owned()).or_insert(0) += 1;
                                a.clone()
                            })
                        };
                        push_event(
                            out,
                            Event::MappingStart {
                                style: *style,
                                tag: tag.clone(),
                                anchor: emit_anchor.clone(),
                                span,
                            },
                            limits,
                            emitted_events,
                        )?;
                        stack.push(EmitStep::Event(Event::MappingEnd { span: *end_span }));
                        if let Some(a) = emit_anchor {
                            stack.push(EmitStep::CloseAnchor(a));
                        }
                        for (k, v, entry_strip) in entries.iter().rev() {
                            let child_strip = strip_anchors || *entry_strip;
                            stack.push(EmitStep::Node {
                                node: Arc::clone(v),
                                strip_anchors: child_strip,
                                depth: next_depth,
                            });
                            stack.push(EmitStep::Node {
                                node: Arc::clone(k),
                                strip_anchors: child_strip,
                                depth: next_depth,
                            });
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn push_event<'a>(
    out: &mut Vec<Event<'a>>,
    event: Event<'a>,
    limits: &Limits,
    emitted_events: &mut usize,
) -> Result<(), Error> {
    let span = event.span();
    *emitted_events += 1;
    limits
        .check_events(*emitted_events)
        .map_err(|e| Error::from(e).with_span(span))?;
    out.push(event);
    Ok(())
}
