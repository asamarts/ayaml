//! A read-only tree with byte spans and path lookup.
//!
//! The tree is built from the same spanned events as the value readers. It is
//! intentionally conservative around aliases, merge keys, duplicate keys, and
//! multiple documents: a lookup that would have to guess returns `None`.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::PathSegment;
use crate::error::{Error, ErrorKind};
use crate::event::{Event, ScalarStyle};
use crate::limits::Limits;
use crate::mapping::Mapping;
use crate::resolver::resolve_scalar;
use crate::span::Span;
use crate::tag::Tag;
use crate::value::Value;

/// A parsed YAML stream with source spans.
#[derive(Clone, Debug)]
pub struct SpannedTree<'a> {
    documents: Vec<SpannedNode<'a>>,
}

/// One node in a [`SpannedTree`].
#[derive(Clone, Debug)]
pub struct SpannedNode<'a> {
    span: Span,
    entry_span: Option<Span>,
    kind: Kind<'a>,
}

#[derive(Clone, Debug)]
enum Kind<'a> {
    Scalar {
        value: Cow<'a, str>,
        raw: Cow<'a, str>,
        style: ScalarStyle,
        tag: Option<Tag>,
    },
    Sequence(Vec<SpannedNode<'a>>),
    Mapping(Vec<Entry<'a>>),
    Alias,
}

#[derive(Clone, Debug)]
struct Entry<'a> {
    key: SpannedNode<'a>,
    value: SpannedNode<'a>,
    ambiguous: bool,
}

impl<'a> SpannedTree<'a> {
    /// Parse one default-profile stream into a spanned tree.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax and limit failures as [`crate::parse_events`].
    pub fn parse(input: &'a str) -> Result<Self, Error> {
        Self::parse_with_limits(input, &Limits::default())
    }

    /// Parse one default-profile stream with explicit limits.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax and limit failures as
    /// [`crate::parse_events_with_limits`].
    pub fn parse_with_limits(input: &'a str, limits: &Limits) -> Result<Self, Error> {
        let events = crate::parse_events_with_limits(input, limits)?;
        build(input, events)
    }

    /// Parse one compatibility-profile stream into a spanned tree.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax and limit failures as
    /// [`crate::compat::parse_events`].
    #[cfg(feature = "libyaml-compat")]
    pub fn parse_compat(input: &'a str) -> Result<Self, Error> {
        Self::parse_compat_with_limits(input, &Limits::default())
    }

    /// Parse one compatibility-profile stream with explicit limits.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax and limit failures as
    /// [`crate::compat::parse_events_with_limits`].
    #[cfg(feature = "libyaml-compat")]
    pub fn parse_compat_with_limits(input: &'a str, limits: &Limits) -> Result<Self, Error> {
        let events = crate::compat::parse_events_with_limits(input, limits)?;
        build(input, events)
    }

    /// The documents in source order.
    #[must_use]
    pub fn documents(&self) -> &[SpannedNode<'a>] {
        &self.documents
    }

    /// The document at `index`.
    #[must_use]
    pub fn document(&self, index: usize) -> Option<&SpannedNode<'a>> {
        self.documents.get(index)
    }

    /// Look up a node by a path. A multi-document stream has no unambiguous root, so this returns
    /// `None` unless it holds exactly one document.
    #[must_use]
    pub fn lookup(&self, path: &[PathSegment]) -> Option<&SpannedNode<'a>> {
        let [document] = self.documents.as_slice() else {
            return None;
        };
        find(document, path)
    }

    /// The byte span of the value at `path`.
    #[must_use]
    pub fn span_at(&self, path: &[PathSegment]) -> Option<Span> {
        self.lookup(path).map(SpannedNode::span)
    }

    /// The span that removes the entry at `path`. For a block entry this includes its
    /// indentation and ending line break; for a flow entry it covers the key and value.
    #[must_use]
    pub fn entry_span_at(&self, path: &[PathSegment]) -> Option<Span> {
        self.lookup(path).and_then(SpannedNode::entry_span)
    }

    /// The source text of the scalar at `path`, or `None` for a collection, alias, ambiguous
    /// entry, or path outside the tree.
    #[must_use]
    pub fn scalar_text_at(&self, path: &[PathSegment]) -> Option<&str> {
        self.lookup(path).and_then(SpannedNode::scalar_text)
    }

    /// The original scalar text at `path`. Replacing its span with this text preserves the
    /// value in that position.
    #[must_use]
    pub fn scalar_encoding_at(&self, path: &[PathSegment]) -> Option<&str> {
        self.scalar_text_at(path)
    }
}

impl SpannedNode<'_> {
    /// The node's byte span.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// The span that removes this node from its parent entry, when it has one.
    #[must_use]
    pub const fn entry_span(&self) -> Option<Span> {
        self.entry_span
    }

    /// Whether this is a scalar node.
    #[must_use]
    pub const fn is_scalar(&self) -> bool {
        matches!(self.kind, Kind::Scalar { .. })
    }

    /// Whether this is a sequence node.
    #[must_use]
    pub const fn is_sequence(&self) -> bool {
        matches!(self.kind, Kind::Sequence(_))
    }

    /// Whether this is a mapping node.
    #[must_use]
    pub const fn is_mapping(&self) -> bool {
        matches!(self.kind, Kind::Mapping(_))
    }

    /// Whether this node is an alias, whose content this tree does not expand.
    #[must_use]
    pub const fn is_alias(&self) -> bool {
        matches!(self.kind, Kind::Alias)
    }

    /// The source text of a scalar node.
    #[must_use]
    pub fn scalar_text(&self) -> Option<&str> {
        match &self.kind {
            Kind::Scalar { raw, .. } => Some(raw),
            _ => None,
        }
    }
}

fn find<'a, 'b>(node: &'b SpannedNode<'a>, path: &[PathSegment]) -> Option<&'b SpannedNode<'a>> {
    if node.is_alias() {
        return None;
    }
    let Some((first, rest)) = path.split_first() else {
        return Some(node);
    };
    match (&node.kind, first) {
        (Kind::Sequence(items), PathSegment::Index(index)) => find(items.get(*index)?, rest),
        (Kind::Mapping(entries), PathSegment::Key(key)) => {
            let mut found = None;
            for entry in entries {
                if entry.ambiguous || entry.key.is_alias() {
                    continue;
                }
                if entry.key.resolved_value().as_ref() == Some(key) {
                    if found.is_some() {
                        return None;
                    }
                    found = Some(&entry.value);
                }
            }
            find(found?, rest)
        }
        _ => None,
    }
}

impl SpannedNode<'_> {
    fn resolved_value(&self) -> Option<Value> {
        match &self.kind {
            Kind::Scalar {
                value, style, tag, ..
            } => resolve_scalar(value, *style, tag.as_ref()).ok(),
            Kind::Sequence(items) => Some(Value::Sequence(
                items
                    .iter()
                    .map(Self::resolved_value)
                    .collect::<Option<_>>()?,
            )),
            Kind::Mapping(entries) => {
                let mut mapping = Mapping::new();
                for entry in entries {
                    if entry.ambiguous {
                        return None;
                    }
                    mapping.insert(entry.key.resolved_value()?, entry.value.resolved_value()?);
                }
                Some(Value::Mapping(mapping))
            }
            Kind::Alias => None,
        }
    }
}

fn build<'a>(
    input: &'a str,
    events: impl Iterator<Item = Result<Event<'a>, Error>>,
) -> Result<SpannedTree<'a>, Error> {
    let mut builder = Builder {
        input,
        documents: Vec::new(),
        stack: Vec::new(),
        root: None,
    };
    for event in events {
        builder.event(event?)?;
    }
    Ok(SpannedTree {
        documents: builder.documents,
    })
}

struct Builder<'a> {
    input: &'a str,
    documents: Vec<SpannedNode<'a>>,
    stack: Vec<Frame<'a>>,
    root: Option<SpannedNode<'a>>,
}

struct Frame<'a> {
    open: Span,
    style: crate::event::CollectionStyle,
    kind: FrameKind<'a>,
}

enum FrameKind<'a> {
    Sequence(Vec<SpannedNode<'a>>),
    Mapping {
        entries: Vec<Entry<'a>>,
        key: Option<SpannedNode<'a>>,
        seen: HashMap<Value, usize>,
    },
}

impl<'a> Builder<'a> {
    fn event(&mut self, event: Event<'a>) -> Result<(), Error> {
        match event {
            Event::StreamStart { .. } | Event::StreamEnd { .. } | Event::Directive { .. } => {}
            Event::DocumentStart { .. } => self.root = None,
            Event::DocumentEnd { .. } => {
                if let Some(root) = self.root.take() {
                    self.documents.push(root);
                }
            }
            Event::Scalar {
                value,
                raw,
                style,
                tag,
                span,
                ..
            } => {
                let node = SpannedNode {
                    span,
                    entry_span: None,
                    kind: Kind::Scalar {
                        value,
                        raw,
                        style,
                        tag,
                    },
                };
                self.add(node);
            }
            Event::Alias { span, .. } => {
                let node = SpannedNode {
                    span,
                    entry_span: None,
                    kind: Kind::Alias,
                };
                self.add(node);
            }
            Event::SequenceStart { span, style, .. } => {
                self.stack.push(Frame {
                    open: span,
                    style,
                    kind: FrameKind::Sequence(Vec::new()),
                });
            }
            Event::SequenceEnd { span } => {
                let Some(frame) = self.stack.pop() else {
                    return Err(Error::new(ErrorKind::Parser, "unexpected sequence end"));
                };
                let FrameKind::Sequence(items) = frame.kind else {
                    return Err(Error::new(ErrorKind::Parser, "sequence end for a mapping"));
                };
                let node = SpannedNode {
                    span: Span::new(frame.open.start(), span.end()),
                    entry_span: None,
                    kind: Kind::Sequence(items),
                };
                self.add(node);
            }
            Event::MappingStart { span, style, .. } => {
                self.stack.push(Frame {
                    open: span,
                    style,
                    kind: FrameKind::Mapping {
                        entries: Vec::new(),
                        key: None,
                        seen: HashMap::new(),
                    },
                });
            }
            Event::MappingEnd { span } => {
                let Some(frame) = self.stack.pop() else {
                    return Err(Error::new(ErrorKind::Parser, "unexpected mapping end"));
                };
                let FrameKind::Mapping { entries, .. } = frame.kind else {
                    return Err(Error::new(ErrorKind::Parser, "mapping end for a sequence"));
                };
                let node = SpannedNode {
                    span: Span::new(frame.open.start(), span.end()),
                    entry_span: None,
                    kind: Kind::Mapping(entries),
                };
                self.add(node);
            }
        }
        Ok(())
    }

    fn add(&mut self, mut node: SpannedNode<'a>) {
        let Some(frame) = self.stack.last_mut() else {
            self.root = Some(node);
            return;
        };
        match &mut frame.kind {
            FrameKind::Sequence(items) => {
                node.entry_span = Some(entry_span(self.input, node.span, frame.style));
                items.push(node);
            }
            FrameKind::Mapping { entries, key, seen } => {
                let Some(mut key_node) = key.take() else {
                    node.entry_span = None;
                    *key = Some(node);
                    return;
                };
                key_node.entry_span = None;
                let span = entry_span(self.input, key_node.span, frame.style);
                let key_value = key_node.resolved_value();
                let mut ambiguous = is_merge_key(&key_node);
                if let Some(value) = &key_value
                    && let Some(previous) = seen.get(value)
                {
                    if let Some(entry) = entries.get_mut(*previous) {
                        entry.ambiguous = true;
                    }
                    ambiguous = true;
                }
                if let Some(value) = key_value {
                    let index = entries.len();
                    seen.entry(value).or_insert(index);
                }
                node.entry_span = Some(span);
                entries.push(Entry {
                    key: key_node,
                    value: node,
                    ambiguous,
                });
            }
        }
    }
}

fn is_merge_key(node: &SpannedNode<'_>) -> bool {
    matches!(
        &node.kind,
        Kind::Scalar {
            value,
            style: ScalarStyle::Plain,
            ..
        } if value == "<<"
    )
}

fn entry_span(input: &str, node: Span, style: crate::event::CollectionStyle) -> Span {
    if style == crate::event::CollectionStyle::Flow {
        return node;
    }
    Span::new(line_start(input, node.start()), line_end(input, node.end()))
}

fn line_start(input: &str, index: usize) -> usize {
    input[..index]
        .rfind(['\n', '\r'])
        .map_or(0, |break_at| break_at + 1)
}

fn line_end(input: &str, index: usize) -> usize {
    let mut end = input[index..]
        .find(['\n', '\r'])
        .map_or(input.len(), |offset| index + offset);
    if end < input.len() {
        let mut chars = input[end..].chars();
        if let Some(ch) = chars.next() {
            end += ch.len_utf8();
            if ch == '\r' && input[end..].starts_with('\n') {
                end += 1;
            }
        }
    }
    end
}
