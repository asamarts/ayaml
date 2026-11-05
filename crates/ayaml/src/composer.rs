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

use std::collections::HashMap;

use crate::error::{Error, ErrorKind};
use crate::event::{Anchor, Event};
use crate::limits::{AliasBudget, Limits};
use crate::mapping::Mapping;
use crate::resolver::resolve_scalar;
use crate::span::Span;
use crate::tag::{Tag, TaggedValue};
use crate::value::Value;

/// Compose every document in an event stream.
pub(crate) fn compose_documents<'a>(
    events: &[Event<'a>],
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
        composer.event(event)?;
    }
    let (documents, spent) = composer.finish_with_budget()?;
    *budget = spent;
    Ok(documents)
}

/// Tag a collection. A core collection tag restates the node kind and is
/// dropped.
fn apply_tag(tag: Option<&Tag>, value: Value) -> Value {
    match tag {
        Some(tag) if !tag.is_core_collection() => Value::from(TaggedValue::new(tag.clone(), value)),
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
        Value::Tagged(tagged) => node_cost.saturating_add(estimate_size(&tagged.value, node_cost)),
    }
}

/// Record `value` as the definition of `anchor` for a node whose span is
/// `span`. An alias refers to the most recent anchor before it in the
/// document. A collection is registered when it ends, after any anchor
/// inside it, so it must not replace a definition that starts at or after
/// its own start: in `&x {k: &x v}`, `*x` is `v`.
pub(crate) fn define_anchor<T>(
    anchors: &mut HashMap<String, (usize, T)>,
    anchor: &str,
    span: Span,
    value: impl FnOnce() -> T,
) {
    let start = span.start().index();
    match anchors.get_mut(anchor) {
        Some((defined, _)) if *defined >= start => {}
        Some(entry) => *entry = (start, value()),
        None => {
            anchors.insert(anchor.to_owned(), (start, value()));
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

enum Frame<'a> {
    Sequence {
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        items: Vec<Value>,
        span: Span,
        size: usize,
        height: usize,
    },
    Mapping {
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        mapping: Mapping,
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
    fn child(&self, step: Step) -> Option<&Value> {
        match (self, step) {
            (Self::Sequence { items, .. }, Step::Item(index)) => items.get(index),
            (
                Self::Mapping {
                    mapping,
                    pending_key,
                    ..
                },
                Step::Key(index),
            ) => match mapping.get_index(index) {
                Some((key, _)) => Some(key),
                None if index == mapping.len() => pending_key.as_ref().map(|(key, _)| key),
                None => None,
            },
            (Self::Mapping { mapping, .. }, Step::Value(index)) => {
                mapping.get_index(index).map(|(_, value)| value)
            }
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

/// The value nesting height: the collections on its deepest path.
fn value_height(value: &Value) -> usize {
    match value {
        Value::Sequence(items) => 1 + items.iter().map(value_height).max().unwrap_or(0),
        Value::Mapping(mapping) => {
            1 + mapping
                .iter()
                .map(|(key, value)| value_height(key).max(value_height(value)))
                .max()
                .unwrap_or(0)
        }
        Value::Tagged(tagged) => value_height(&tagged.value),
        _ => 0,
    }
}

/// Event-driven composer behind `parse_value`, `parse_documents`, and the
/// document cursor.
pub(crate) struct StreamingComposer<'a> {
    limits: Limits,
    input_len: usize,
    anchors: HashMap<String, (usize, AnchorDef)>,
    documents: Vec<Value>,
    document: Option<Value>,
    stack: Vec<Frame<'a>>,
    budget: AliasBudget,
}

impl<'a> StreamingComposer<'a> {
    pub(crate) fn new(limits: Limits, input_len: usize) -> Self {
        Self::with_budget(limits, input_len, AliasBudget::default())
    }

    pub(crate) fn with_budget(limits: Limits, input_len: usize, budget: AliasBudget) -> Self {
        Self {
            limits,
            input_len,
            anchors: HashMap::new(),
            documents: Vec::new(),
            document: None,
            stack: Vec::new(),
            budget,
        }
    }

    pub(crate) fn event(&mut self, event: Event<'a>) -> Result<(), Error> {
        match event {
            Event::StreamStart { .. } | Event::StreamEnd { .. } | Event::Directive { .. } => {}
            Event::DocumentStart { .. } => {
                if !self.stack.is_empty() {
                    return Err(Error::new(
                        ErrorKind::Composition,
                        "document started inside a collection",
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
                let value = self.document.take().unwrap_or(Value::Null);
                self.documents.push(value);
            }
            Event::Scalar {
                value,
                style,
                tag,
                anchor,
                span,
                ..
            } => {
                let resolved = resolve_scalar(value.as_ref(), style, tag.as_ref())
                    .map_err(|error| error.with_span(span))?;
                let size = estimate_size(&resolved, self.limits.node_cost_bytes);
                let height = value_height(&resolved);
                self.register_anchor(anchor.as_ref(), span, size, height);
                self.feed(resolved, span, size, height)?;
            }
            Event::Alias { anchor, span } => {
                let definition = self
                    .anchors
                    .get(anchor.as_str())
                    .map(|(_, definition)| definition.clone())
                    .ok_or_else(|| self.error_at(span, "unknown anchor"))?;
                // Charge before cloning: the clone is the allocation this
                // budget exists to bound.
                self.budget
                    .charge(&self.limits, self.input_len, definition.size)
                    .map_err(|error| error.with_span(span))?;
                // The alias nests its target here, so the target's
                // collections count toward the depth.
                if definition.height > 0 {
                    self.limits
                        .check_depth(self.stack.len() + definition.height)
                        .map_err(|error| Error::from(error).with_span(span))?;
                }
                let value = self
                    .anchored(&definition.path)
                    .cloned()
                    .ok_or_else(|| self.error_at(span, "unknown anchor"))?;
                self.feed(value, span, definition.size, definition.height)?;
            }
            Event::SequenceStart {
                tag, anchor, span, ..
            } => {
                self.limits
                    .check_depth(self.stack.len() + 1)
                    .map_err(Error::from)?;
                self.stack.push(Frame::Sequence {
                    tag,
                    anchor,
                    items: Vec::new(),
                    span,
                    size: self.limits.node_cost_bytes,
                    height: 0,
                });
            }
            Event::SequenceEnd { span } => {
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
                let (value, size) =
                    self.finish_collection(tag.as_ref(), Value::Sequence(items), size);
                self.register_anchor(anchor.as_ref(), start_span, size, height + 1);
                self.feed(value, start_span, size, height + 1)?;
            }
            Event::MappingStart {
                tag, anchor, span, ..
            } => {
                self.limits
                    .check_depth(self.stack.len() + 1)
                    .map_err(Error::from)?;
                self.stack.push(Frame::Mapping {
                    tag,
                    anchor,
                    mapping: Mapping::new(),
                    pending_key: None,
                    span,
                    size: self.limits.node_cost_bytes,
                    height: 0,
                });
            }
            Event::MappingEnd { span } => {
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
                    return Err(Error::new(
                        ErrorKind::Composition,
                        format!("mapping key without value: {key:?}"),
                    )
                    .with_span(key_span));
                }
                let (value, size) =
                    self.finish_collection(tag.as_ref(), Value::Mapping(mapping), size);
                self.register_anchor(anchor.as_ref(), start_span, size, height + 1);
                self.feed(value, start_span, size, height + 1)?;
            }
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<Vec<Value>, Error> {
        self.finish_with_budget().map(|(documents, _)| documents)
    }

    /// Finish the stream and hand back the alias budget it spent.
    pub(crate) fn finish_with_budget(self) -> Result<(Vec<Value>, AliasBudget), Error> {
        if !self.stack.is_empty() {
            return Err(Error::new(
                ErrorKind::Composition,
                "unterminated collection at end of stream",
            ));
        }
        Ok((self.documents, self.budget))
    }

    /// Tag a finished collection; a tag adds one node to its size estimate.
    fn finish_collection(&self, tag: Option<&Tag>, value: Value, size: usize) -> (Value, usize) {
        let value = apply_tag(tag, value);
        let size = if value.is_tagged() {
            size.saturating_add(self.limits.node_cost_bytes)
        } else {
            size
        };
        (value, size)
    }

    fn feed(&mut self, value: Value, span: Span, size: usize, height: usize) -> Result<(), Error> {
        match self.stack.last_mut() {
            Some(Frame::Sequence {
                items,
                size: total,
                height: deepest,
                ..
            }) => {
                *total = total.saturating_add(size);
                *deepest = (*deepest).max(height);
                items.push(value);
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
                    if mapping.contains_key(&key) {
                        return Err(Error::new(
                            ErrorKind::DuplicateKey,
                            format!("duplicate mapping key: {}", crate::value::key_text(&key)),
                        )
                        .with_span(key_span));
                    }
                    mapping.insert(key, value);
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

    /// Record an anchor for the node about to be fed: its position is where
    /// each open collection will put its next child.
    fn register_anchor(
        &mut self,
        anchor: Option<&Anchor<'a>>,
        span: Span,
        size: usize,
        height: usize,
    ) {
        if let Some(anchor) = anchor {
            let stack = &self.stack;
            define_anchor(&mut self.anchors, anchor.as_str(), span, || AnchorDef {
                path: stack.iter().map(Frame::position).collect(),
                size,
                height,
            });
        }
    }

    /// The finished node at `path`. The path's first steps run through the
    /// open collections; once a step leaves the open child, it reaches a
    /// finished value, and the rest of the path descends inside it.
    fn anchored(&self, path: &[Step]) -> Option<&Value> {
        for (depth, step) in path.iter().enumerate() {
            let frame = self.stack.get(depth)?;
            if depth + 1 < self.stack.len() && frame.position() == *step {
                continue;
            }
            let mut value = frame.child(*step)?;
            for step in &path[depth + 1..] {
                value = child_of(value, *step)?;
            }
            return Some(value);
        }
        None
    }

    fn error_at(&self, span: Span, message: impl Into<String>) -> Error {
        Error::new(ErrorKind::Composition, message).with_span(span)
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

    #[test]
    fn compatibility_scalars_and_tags() {
        let input = include_str!("../../../tests/fixtures/compat/scalar-tags-merges-aliases.yml");
        let value = compose(input);
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
        let input = include_str!("../../../tests/fixtures/compat/toolchain-pins.yml");
        let value = compose(input);
        assert_eq!(value["version"].as_u64(), Some(1));
        assert!(value["facts"].is_sequence());
        assert!(value["rules"].is_sequence());
    }
}
