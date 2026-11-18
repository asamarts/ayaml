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
    /// Where the node's last character ends. A block collection's is its last entry's, which
    /// its span can pass.
    content_end: usize,
    /// The node, or a node inside it, defines an anchor.
    anchored: bool,
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

    /// The span that removes the entry at `path` and nothing else, or `None` when no span does.
    /// See [`SpannedNode::entry_span`] for which entries have one.
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

    /// The span that removes this node's entry from its parent, when deleting those bytes
    /// removes that entry and leaves the rest of the document as it was.
    ///
    /// A block mapping's entry and a block sequence's item have one when they begin their own
    /// line and nothing follows them on their last line but a comment. The span runs from the
    /// start of that line through the entry's last line break, so it takes the entry's
    /// indentation, a value on later lines, a block scalar, and a comment after the value. A
    /// byte-order mark before the first entry stays outside it.
    ///
    /// The other entries have none, since no span of theirs is exact: an entry of a flow
    /// collection, whose separator belongs to its neighbor as much as to it; the first key of a
    /// mapping that begins on a sequence's `-` line; an entry with an explicit `?`, or with a
    /// tag or an anchor before its key; an item with no value; and an entry that defines an
    /// anchor, which an alias elsewhere may name.
    ///
    /// Removing a block collection's only entry leaves it with none, and it then reads as an
    /// empty value, not as an empty collection.
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
    /// The collection has an anchor of its own.
    anchored: bool,
    kind: FrameKind<'a>,
}

enum FrameKind<'a> {
    Sequence(Vec<SpannedNode<'a>>),
    Mapping {
        entries: Vec<Entry<'a>>,
        /// The key that waits for its value. Boxed, so a sequence's frame isn't as large.
        key: Option<Box<SpannedNode<'a>>>,
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
                anchor,
                span,
            } => {
                let node = SpannedNode {
                    span,
                    entry_span: None,
                    content_end: span.end(),
                    anchored: anchor.is_some(),
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
                    content_end: span.end(),
                    anchored: false,
                    kind: Kind::Alias,
                };
                self.add(node);
            }
            Event::SequenceStart {
                span,
                style,
                anchor,
                ..
            } => {
                self.stack.push(Frame {
                    open: span,
                    style,
                    anchored: anchor.is_some(),
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
                let flow = frame.style == crate::event::CollectionStyle::Flow;
                let node = SpannedNode {
                    span: Span::new(frame.open.start(), span.end()),
                    entry_span: None,
                    content_end: match items.last() {
                        Some(last) if !flow => last.content_end,
                        _ => span.end(),
                    },
                    anchored: frame.anchored || items.iter().any(|item| item.anchored),
                    kind: Kind::Sequence(items),
                };
                self.add(node);
            }
            Event::MappingStart {
                span,
                style,
                anchor,
                ..
            } => {
                self.stack.push(Frame {
                    open: span,
                    style,
                    anchored: anchor.is_some(),
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
                let flow = frame.style == crate::event::CollectionStyle::Flow;
                let node = SpannedNode {
                    span: Span::new(frame.open.start(), span.end()),
                    entry_span: None,
                    content_end: match entries.last() {
                        Some(last) if !flow => last.value.content_end,
                        _ => span.end(),
                    },
                    anchored: frame.anchored
                        || entries
                            .iter()
                            .any(|entry| entry.key.anchored || entry.value.anchored),
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
                node.entry_span = block_entry_span(
                    self.input,
                    frame.style,
                    Lead::Item,
                    node.span.start(),
                    node.content_end,
                    node.anchored,
                );
                items.push(node);
            }
            FrameKind::Mapping { entries, key, seen } => {
                let Some(key_node) = key.take() else {
                    node.entry_span = None;
                    *key = Some(Box::new(node));
                    return;
                };
                let mut key_node = *key_node;
                key_node.entry_span = None;
                if node.is_missing_value() {
                    // The parser places a value that isn't there at the next token. It stands
                    // after its key, where a value would be written.
                    let at = after_key(self.input, key_node.content_end);
                    node.span = Span::empty(at);
                    node.content_end = at;
                }
                let span = block_entry_span(
                    self.input,
                    frame.style,
                    Lead::Key,
                    key_node.span.start(),
                    node.content_end,
                    key_node.anchored || node.anchored,
                );
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
                node.entry_span = span;
                entries.push(Entry {
                    key: key_node,
                    value: node,
                    ambiguous,
                });
            }
        }
    }
}

impl SpannedNode<'_> {
    /// Whether this is the empty scalar that stands for a value the source doesn't write.
    fn is_missing_value(&self) -> bool {
        self.span.is_empty()
            && !self.anchored
            && matches!(
                &self.kind,
                Kind::Scalar {
                    raw,
                    style: ScalarStyle::Plain,
                    tag: None,
                    ..
                } if raw.is_empty()
            )
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

/// What stands before a block entry on its first line.
#[derive(Clone, Copy)]
enum Lead {
    /// A mapping's key: indentation alone.
    Key,
    /// A sequence's item: indentation, its `-`, and spaces.
    Item,
}

/// The span whose removal removes one block entry and nothing else, or `None` when the entry
/// has no such span (see [`SpannedNode::entry_span`]). The entry begins at `first`, its last
/// character ends at `content_end`, and `anchored` says whether it defines an anchor.
fn block_entry_span(
    input: &str,
    style: crate::event::CollectionStyle,
    lead: Lead,
    first: usize,
    content_end: usize,
    anchored: bool,
) -> Option<Span> {
    if style == crate::event::CollectionStyle::Flow || anchored {
        return None;
    }
    let mut start = line_start(input, first);
    // A byte-order mark opens the input, not the first entry.
    if start == 0 && input.starts_with('\u{feff}') {
        start = '\u{feff}'.len_utf8();
    }
    let before = input.get(start..first)?;
    let begins_its_line = match lead {
        Lead::Key => before.bytes().all(|byte| byte == b' '),
        Lead::Item => before
            .trim_start_matches(' ')
            .strip_prefix('-')
            .is_some_and(|blanks| !blanks.is_empty() && blanks.bytes().all(|byte| byte == b' ')),
    };
    if !begins_its_line {
        return None;
    }
    let end = entry_end(input, content_end)?;
    (end > start).then(|| Span::new(start, end))
}

/// Where an entry whose last character ends at `content_end` ends: after its line break, or
/// `None` when more than a comment follows it on that line.
fn entry_end(input: &str, content_end: usize) -> Option<usize> {
    let (before, rest) = input.split_at_checked(content_end)?;
    // A block scalar's text runs through its last line break.
    if before.ends_with(['\n', '\r']) {
        return Some(content_end);
    }
    let line = rest.split(['\n', '\r']).next().unwrap_or(rest);
    let after = line.trim_start_matches([' ', '\t']);
    // A comment needs a blank before its `#`.
    let comment = after.starts_with('#') && after.len() < line.len();
    (after.is_empty() || comment).then(|| line_end(input, content_end))
}

/// Where a value that the source doesn't write stands: after the `:` that follows its key on
/// the key's line, or after the key when none does.
fn after_key(input: &str, key_end: usize) -> usize {
    let Some(rest) = input.get(key_end..) else {
        return key_end;
    };
    let blanks = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    if rest[blanks..].starts_with(':') {
        key_end + blanks + 1
    } else {
        key_end
    }
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
