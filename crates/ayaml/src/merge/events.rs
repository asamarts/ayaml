#![allow(
    clippy::mutable_key_type,
    reason = "Value's hash cache is interior-immutable after construction and never changes equality"
)]

//! Opt-in YAML 1.1 merge-key expansion at the spanned event layer (AY-147).
//!
//! Expands plain `<<` (and `!!merge <<`) mapping entries while keeping quoted `"<<"`, `'<<'`,
//! block `<<`, and `!!str <<` keys literal. Every expanded entry retains its original source
//! [`Span`] and borrowed text slices, and all resource limits (`max_depth`,
//! `max_collection_entries`, `max_events`, `max_anchors`, `max_alias_dereferences`, and
//! `max_alias_expansion_bytes`) are enforced before allocation.

use std::borrow::Cow;
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use crate::composer::Anchors;
use crate::error::{Error, ErrorCode, ErrorKind};
use crate::event::{Anchor, CollectionStyle, Event, ScalarStyle};
use crate::limits::{AliasBudget, Limits};
use crate::parser::EventCursor;
use crate::resolver::resolve_scalar;
use crate::span::Span;
use crate::tag::Tag;
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
        Ok(Self {
            cursor,
            limits: limits.clone(),
            input,
            budget: AliasBudget::default(),
            queue: VecDeque::new(),
            stream_started: false,
            stream_ended: false,
            emitted_events: 0,
        })
    }

    pub(crate) fn next_event(&mut self) -> Result<Option<Event<'a>>, Error> {
        if let Some(event) = self.queue.pop_front() {
            return Ok(Some(event));
        }
        if self.stream_ended {
            return Ok(None);
        }
        if !self.stream_started {
            self.stream_started = true;
            self.emitted_events += 1;
            self.limits.check_events(self.emitted_events).map_err(|e| {
                Error::from(e)
                    .with_span(Span::empty(0))
                    .with_input_location(self.input, false)
            })?;
            return Ok(Some(Event::StreamStart {
                span: Span::empty(0),
            }));
        }
        if let Some(events) = self.next_document()? {
            self.queue = VecDeque::from(events);
            return Ok(self.queue.pop_front());
        }
        self.stream_ended = true;
        let span = Span::empty(self.input.len());
        self.emitted_events += 1;
        self.limits.check_events(self.emitted_events).map_err(|e| {
            Error::from(e)
                .with_span(span)
                .with_input_location(self.input, false)
        })?;
        Ok(Some(Event::StreamEnd { span }))
    }

    pub(crate) fn next_document(&mut self) -> Result<Option<Vec<Event<'a>>>, Error> {
        let Some(raw_events) = self.cursor.next_document()? else {
            return Ok(None);
        };
        let expanded = expand_document_events(
            raw_events,
            self.input.len(),
            &self.limits,
            &mut self.budget,
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

/// Expand a single-document input with merge keys enabled, rejecting trailing documents.
pub(crate) fn expand_single_document<'a>(
    input: &'a str,
    limits: &Limits,
) -> Result<(Vec<Event<'a>>, AliasBudget), Error> {
    let mut stream = MergedEventStream::new(input, limits)?;
    let first = stream.next_document()?;
    if let Some(second) = stream.next_document()? {
        let span = second
            .iter()
            .find_map(|event| match event {
                Event::DocumentStart { span, .. } => Some(*span),
                _ => None,
            })
            .unwrap_or_else(|| Span::empty(input.len()));
        return Err(Error::coded(
            ErrorKind::Parser,
            ErrorCode::TrailingDocuments,
            "expected a single YAML document, but found trailing documents",
        )
        .with_span(span)
        .with_input_location(input, false));
    }
    Ok((first.unwrap_or_default(), stream.budget))
}

#[derive(Clone, Debug)]
struct EventNode<'a> {
    kind: EventNodeKind<'a>,
    span: Span,
    /// Estimated byte cost for alias/merge expansion budgeting.
    size: usize,
    /// Nesting height (0 for scalars/aliases, 1+ for collections).
    height: usize,
}

#[derive(Clone, Debug)]
enum EventNodeKind<'a> {
    Scalar {
        value: Cow<'a, str>,
        raw: Cow<'a, str>,
        style: ScalarStyle,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
    },
    Alias {
        name: Anchor<'a>,
    },
    Sequence {
        style: CollectionStyle,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        end_span: Span,
        items: Vec<EventNode<'a>>,
    },
    Mapping {
        style: CollectionStyle,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        end_span: Span,
        entries: Vec<(EventNode<'a>, EventNode<'a>)>,
    },
}

#[derive(Clone)]
struct AnchoredNode<'a> {
    node: Arc<EventNode<'a>>,
}

enum Frame<'a> {
    Sequence {
        style: CollectionStyle,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        span: Span,
        items: Vec<EventNode<'a>>,
        size: usize,
        height: usize,
    },
    Mapping {
        style: CollectionStyle,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        span: Span,
        entries: Vec<(EventNode<'a>, EventNode<'a>)>,
        key: Option<Box<EventNode<'a>>>,
        merge_operand: Option<Box<(Span, EventNode<'a>)>>,
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

fn node_key_value(
    node: &EventNode<'_>,
    anchors: &Anchors<AnchoredNode<'_>>,
    remaining_depth: usize,
) -> Option<Value> {
    match &node.kind {
        EventNodeKind::Scalar {
            value, style, tag, ..
        } => resolve_scalar(value, *style, tag.as_ref()).ok(),
        EventNodeKind::Sequence { items, .. } => {
            let next_depth = remaining_depth.checked_sub(1)?;
            Some(Value::Sequence(
                items
                    .iter()
                    .map(|item| node_key_value(item, anchors, next_depth))
                    .collect::<Option<_>>()?,
            ))
        }
        EventNodeKind::Mapping { entries, .. } => {
            let next_depth = remaining_depth.checked_sub(1)?;
            let mut mapping = crate::Mapping::new();
            for (k, v) in entries {
                mapping.insert(
                    node_key_value(k, anchors, next_depth)?,
                    node_key_value(v, anchors, next_depth)?,
                );
            }
            Some(Value::Mapping(mapping))
        }
        EventNodeKind::Alias { name } => {
            let next_depth = remaining_depth.checked_sub(1)?;
            let target = anchors.resolve(name.as_str()).ok()?;
            node_key_value(&target.node, anchors, next_depth)
        }
    }
}

fn strip_anchors(node: &mut EventNode<'_>) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        match &mut current.kind {
            EventNodeKind::Scalar { anchor, .. } => *anchor = None,
            EventNodeKind::Alias { .. } => {}
            EventNodeKind::Sequence { anchor, items, .. } => {
                *anchor = None;
                for item in items.iter_mut() {
                    stack.push(item);
                }
            }
            EventNodeKind::Mapping {
                anchor, entries, ..
            } => {
                *anchor = None;
                for (k, v) in entries.iter_mut() {
                    stack.push(k);
                    stack.push(v);
                }
            }
        }
    }
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
    let mut root: Option<EventNode<'a>> = None;
    let mut anchors: Anchors<AnchoredNode<'a>> = Anchors::new();

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
                let node = EventNode {
                    kind: EventNodeKind::Scalar {
                        value,
                        raw,
                        style,
                        tag,
                        anchor: anchor.clone(),
                    },
                    span,
                    size,
                    height: 0,
                };
                if let Some(anchor) = &anchor {
                    anchors.define(
                        anchor.as_str(),
                        AnchoredNode {
                            node: Arc::new(node.clone()),
                        },
                    );
                }
                push_built(&mut stack, &mut root, node, limits)?;
            }
            Event::Alias { anchor, span } => {
                let node = EventNode {
                    kind: EventNodeKind::Alias { name: anchor },
                    span,
                    size: limits.node_cost_bytes,
                    height: 0,
                };
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
                if let Some(anchor) = &anchor {
                    anchors.open(anchor.as_str());
                }
                stack.push(Frame::Sequence {
                    style,
                    tag,
                    anchor,
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
                if let Some(anchor) = &anchor {
                    anchors.open(anchor.as_str());
                }
                stack.push(Frame::Mapping {
                    style,
                    tag,
                    anchor,
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
        emit_node(node, &mut out, limits, emitted_events)?;
    }
    if let Some(event) = doc_end {
        push_event(&mut out, event, limits, emitted_events)?;
    }
    Ok(out)
}

fn finish_sequence_frame<'a>(
    stack: &mut Vec<Frame<'a>>,
    root: &mut Option<EventNode<'a>>,
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
    let node = EventNode {
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
    };
    if let Some(anchor) = &anchor {
        anchors.close(
            anchor.as_str(),
            AnchoredNode {
                node: Arc::new(node.clone()),
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
    root: &mut Option<EventNode<'a>>,
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
    if let Some(boxed) = merge_operand {
        let (merge_span, operand) = *boxed;
        apply_event_merge(
            operand,
            merge_span,
            &mut entries,
            &mut size,
            &mut height,
            stack.len() + 1,
            anchors,
            input_len,
            limits,
            budget,
        )?;
    }
    if let Some(tag) = &tag {
        size = size.saturating_add(tag.text_len());
    }
    let node = EventNode {
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
    };
    if let Some(anchor) = &anchor {
        anchors.close(
            anchor.as_str(),
            AnchoredNode {
                node: Arc::new(node.clone()),
            },
        );
    }
    push_built(stack, root, node, limits)
}

fn push_built<'a>(
    stack: &mut [Frame<'a>],
    root: &mut Option<EventNode<'a>>,
    node: EventNode<'a>,
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
            None => *key = Some(Box::new(node)),
            Some(entry_key) => {
                let entry_key = *entry_key;
                if is_merge_key(&entry_key) {
                    if merge_operand.is_some() {
                        return Err(Error::coded(
                            ErrorKind::DuplicateKey,
                            ErrorCode::DuplicateKey,
                            "duplicate mapping key: <<",
                        )
                        .with_span(entry_key.span));
                    }
                    *merge_operand = Some(Box::new((node.span, node)));
                } else {
                    limits
                        .check_collection_entries(entries.len() + 1)
                        .map_err(|e| Error::from(e).with_span(entry_key.span))?;
                    *size = size
                        .saturating_add(entry_key.size)
                        .saturating_add(node.size);
                    *height = (*height).max(entry_key.height).max(node.height);
                    entries.push((entry_key, node));
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
    operand: EventNode<'a>,
    merge_span: Span,
    entries: &mut Vec<(EventNode<'a>, EventNode<'a>)>,
    size: &mut usize,
    height: &mut usize,
    current_depth: usize,
    anchors: &Anchors<AnchoredNode<'a>>,
    input_len: usize,
    limits: &Limits,
    budget: &mut AliasBudget,
) -> Result<(), Error> {
    let mut seen: HashSet<Value> = HashSet::with_capacity(entries.len());
    for (k, _) in entries.iter() {
        if let Some(val) = node_key_value(k, anchors, limits.max_depth) {
            seen.insert(val);
        }
    }

    let (resolved, operand_from_alias) =
        resolve_merge_node(operand, anchors, input_len, limits, budget)?;
    match &resolved.kind {
        EventNodeKind::Mapping {
            entries: src_entries,
            ..
        } => merge_entries(
            src_entries,
            operand_from_alias,
            merge_span,
            entries,
            &mut seen,
            size,
            height,
            current_depth,
            anchors,
            input_len,
            limits,
            budget,
        ),
        EventNodeKind::Sequence { items, .. } => {
            for item in items {
                let item_span = item.span;
                let (resolved_item, item_from_alias) =
                    resolve_merge_node(item.clone(), anchors, input_len, limits, budget)?;
                let EventNodeKind::Mapping {
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
                merge_entries(
                    src_entries,
                    operand_from_alias || item_from_alias,
                    item_span,
                    entries,
                    &mut seen,
                    size,
                    height,
                    current_depth,
                    anchors,
                    input_len,
                    limits,
                    budget,
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
    node: EventNode<'a>,
    anchors: &Anchors<AnchoredNode<'a>>,
    input_len: usize,
    limits: &Limits,
    budget: &mut AliasBudget,
) -> Result<(EventNode<'a>, bool), Error> {
    let EventNodeKind::Alias { name } = &node.kind else {
        return Ok((node, false));
    };
    let span = node.span;
    let target = anchors
        .resolve(name.as_str())
        .map_err(|(code, message)| Error::coded(ErrorKind::Alias, code, message).with_span(span))?;
    budget
        .charge(limits, input_len, target.node.size)
        .map_err(|error| error.with_span(span))?;
    Ok(((*target.node).clone(), true))
}

#[allow(
    clippy::too_many_arguments,
    reason = "internal merge helper threading bounded state across one mapping"
)]
fn merge_entries<'a>(
    src_entries: &[(EventNode<'a>, EventNode<'a>)],
    strip_copied_anchors: bool,
    merge_span: Span,
    entries: &mut Vec<(EventNode<'a>, EventNode<'a>)>,
    seen: &mut HashSet<Value>,
    size: &mut usize,
    height: &mut usize,
    current_depth: usize,
    anchors: &Anchors<AnchoredNode<'a>>,
    input_len: usize,
    limits: &Limits,
    budget: &mut AliasBudget,
) -> Result<(), Error> {
    for (src_key, src_val) in src_entries {
        if let Some(key_val) = node_key_value(src_key, anchors, limits.max_depth)
            && !seen.insert(key_val)
        {
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
        budget
            .charge(limits, input_len, entry_cost)
            .map_err(|error| error.with_span(merge_span))?;

        let mut k = src_key.clone();
        let mut v = src_val.clone();
        if strip_copied_anchors {
            strip_anchors(&mut k);
            strip_anchors(&mut v);
        }
        *size = size.saturating_add(entry_cost);
        *height = (*height).max(entry_height);
        entries.push((k, v));
    }
    Ok(())
}

enum EmitStep<'a> {
    Node(EventNode<'a>),
    Event(Event<'a>),
}

fn emit_node<'a>(
    root: EventNode<'a>,
    out: &mut Vec<Event<'a>>,
    limits: &Limits,
    emitted_events: &mut usize,
) -> Result<(), Error> {
    let mut stack = vec![EmitStep::Node(root)];
    while let Some(step) = stack.pop() {
        match step {
            EmitStep::Event(event) => push_event(out, event, limits, emitted_events)?,
            EmitStep::Node(node) => {
                let span = node.span;
                match node.kind {
                    EventNodeKind::Scalar {
                        value,
                        raw,
                        style,
                        tag,
                        anchor,
                    } => {
                        push_event(
                            out,
                            Event::Scalar {
                                value,
                                raw,
                                style,
                                tag,
                                anchor,
                                span,
                            },
                            limits,
                            emitted_events,
                        )?;
                    }
                    EventNodeKind::Alias { name } => {
                        push_event(
                            out,
                            Event::Alias { anchor: name, span },
                            limits,
                            emitted_events,
                        )?;
                    }
                    EventNodeKind::Sequence {
                        style,
                        tag,
                        anchor,
                        end_span,
                        items,
                    } => {
                        push_event(
                            out,
                            Event::SequenceStart {
                                style,
                                tag,
                                anchor,
                                span,
                            },
                            limits,
                            emitted_events,
                        )?;
                        stack.push(EmitStep::Event(Event::SequenceEnd { span: end_span }));
                        for item in items.into_iter().rev() {
                            stack.push(EmitStep::Node(item));
                        }
                    }
                    EventNodeKind::Mapping {
                        style,
                        tag,
                        anchor,
                        end_span,
                        entries,
                    } => {
                        push_event(
                            out,
                            Event::MappingStart {
                                style,
                                tag,
                                anchor,
                                span,
                            },
                            limits,
                            emitted_events,
                        )?;
                        stack.push(EmitStep::Event(Event::MappingEnd { span: end_span }));
                        for (k, v) in entries.into_iter().rev() {
                            stack.push(EmitStep::Node(v));
                            stack.push(EmitStep::Node(k));
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
