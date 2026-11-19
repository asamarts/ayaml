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
    /// Where the node's last character ends, when its span says. A block collection's is its
    /// last entry's, which its span can pass. A scalar with no text has none: the parser places
    /// it at the token after it, which can be lines away.
    content_end: Option<usize>,
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
    /// `None` unless it holds exactly one document. A lookup that would have to guess returns
    /// `None` too: a key that the mapping holds twice, a merge key, and any key of a mapping
    /// that has an alias among its keys, or a key that holds an alias or a merge key, since
    /// that key may be any other key of the mapping again.
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

    /// The span that removes the entry at `path` and changes nothing else, or `None` when no
    /// span is known to. See [`SpannedNode::entry_span`] for which entries have one.
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
    /// The node's byte span: a scalar as the source spells it, and a collection from its first
    /// byte to its last. A pair in a flow sequence, as in `[a: b]`, is a mapping with no braces,
    /// and ends where its value does, or where the last thing it writes does if it writes no
    /// value: its key, a `?` or `:`, a tag, or an anchor.
    ///
    /// A scalar that the source doesn't write has an empty span. A value that is plainly
    /// missing after its key's `:` stands right after that `:`. Any other stands where the
    /// parser places it, at the next token, which can lie past the end of the collection it is
    /// in: an item with nothing after its `-`, the value of a key with a `?`, a value that
    /// only a tag or an anchor stands for, and every such value of an input that holds a
    /// byte-order mark past the marks that open it.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// The span that removes this node's entry from its parent, when deleting those bytes is
    /// known to leave a document that reads as before without that entry, and with nothing
    /// else changed.
    ///
    /// A block mapping's entry and a block sequence's item can have one. The span runs from the
    /// start of the entry's first line through its last line break, so it takes the entry's
    /// indentation, a value on later lines, a block scalar, and a comment after the value.
    /// The byte-order marks that open the input stay outside the first entry's.
    ///
    /// `None` is conservative: it means that no span is known to be exact. These have none:
    ///
    /// - an entry of a flow collection, whose separator belongs to its neighbor as much as to
    ///   it;
    /// - an entry that doesn't begin its own line, such as the first key of a mapping on a
    ///   sequence's `-` line, and one with more than a comment after it on its last line;
    /// - a mapping entry with an explicit `?`, with a tag or an anchor before its key, or with
    ///   a key that has no text;
    /// - an item with no value, an item whose value begins on a later line, and an entry that
    ///   ends in such an item or in a value that a tag stands for;
    /// - an entry that defines an anchor, or stands inside a collection that has one: an alias
    ///   reads what it names;
    /// - the only entry of its collection, which would be no collection without it;
    /// - an entry before a line that begins with `:`, which would give its value to a key
    ///   before the entry;
    /// - every entry of an input that holds a byte-order mark past the marks that open it, be
    ///   it in a quoted scalar: what a mark that begins a line means depends on the lines
    ///   before it;
    /// - an entry after a block scalar, unless content follows the entry directly: the scalar
    ///   would take the blank and comment lines after it.
    ///
    /// The lines around an entry decide most of these, so the same entry can have a span in one
    /// document and none in another.
    ///
    /// The span is exact for what the document reads as. It can't answer for two things
    /// outside that. A limit that a document keeps within only by its length can be passed
    /// once it is shorter: the alias allowance grows with the input. And where merge keys are
    /// applied to the value afterwards, removing an entry that overrides a merged one brings
    /// the merged one back.
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
        after_block_scalar: false,
        // A run of marks opens the input as one mark does.
        marked: input.trim_start_matches('\u{feff}').contains('\u{feff}'),
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
    /// The last scalar or alias that the source writes was a block scalar. One takes the blank
    /// and more indented lines after it as its own, until other content ends it.
    after_block_scalar: bool,
    /// The input holds a byte-order mark past the marks that open it. The compatibility reader
    /// counts one at a line's start toward that line's indentation, and takes one after a line
    /// break for a comment's or a blank line's start, unless it opens the input. So what such
    /// a mark means depends on the lines before it, and no entry's removal is known to be
    /// exact.
    marked: bool,
}

struct Frame<'a> {
    open: Span,
    style: crate::event::CollectionStyle,
    /// The collection has an anchor of its own.
    anchored: bool,
    /// What [`Builder::after_block_scalar`] was when the collection began.
    after_block_scalar: bool,
    kind: FrameKind<'a>,
}

enum FrameKind<'a> {
    Sequence(Vec<SpannedNode<'a>>),
    Mapping {
        entries: Vec<Entry<'a>>,
        /// The key that waits for its value, and what [`Builder::after_block_scalar`] was
        /// when the key began. Boxed, so a sequence's frame isn't as large.
        key: Option<Box<(SpannedNode<'a>, bool)>>,
        seen: HashMap<Value, usize>,
        /// A key here has no value of its own to compare: an alias, or a collection that holds
        /// one. It may be any other key of the mapping again, so a lookup would have to guess.
        guess: bool,
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
                let before = self.after_block_scalar;
                // A scalar with no text isn't written, so it ends no block scalar before it.
                if !span.is_empty() {
                    self.after_block_scalar =
                        matches!(style, ScalarStyle::Literal | ScalarStyle::Folded);
                }
                let node = SpannedNode {
                    span,
                    entry_span: None,
                    content_end: (!span.is_empty()).then(|| span.end()),
                    anchored: anchor.is_some(),
                    kind: Kind::Scalar {
                        value,
                        raw,
                        style,
                        tag,
                    },
                };
                self.add(node, before);
            }
            Event::Alias { span, .. } => {
                let before = std::mem::replace(&mut self.after_block_scalar, false);
                let node = SpannedNode {
                    span,
                    entry_span: None,
                    content_end: Some(span.end()),
                    anchored: false,
                    kind: Kind::Alias,
                };
                self.add(node, before);
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
                    after_block_scalar: self.after_block_scalar,
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
                let mut items = items;
                // Without its only item a collection is no collection: what is left of it reads
                // as an empty value, under whatever tag stood before it.
                if let [only] = items.as_mut_slice() {
                    only.entry_span = None;
                }
                let node = SpannedNode {
                    span: Span::new(frame.open.start(), span.end()),
                    entry_span: None,
                    content_end: match items.last() {
                        Some(last) if !flow => last.content_end,
                        _ => Some(span.end()),
                    },
                    anchored: frame.anchored || items.iter().any(|item| item.anchored),
                    kind: Kind::Sequence(items),
                };
                if flow {
                    self.after_block_scalar = false;
                }
                self.add(node, frame.after_block_scalar);
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
                    after_block_scalar: self.after_block_scalar,
                    kind: FrameKind::Mapping {
                        entries: Vec::new(),
                        key: None,
                        seen: HashMap::new(),
                        guess: false,
                    },
                });
            }
            Event::MappingEnd { span } => {
                let Some(frame) = self.stack.pop() else {
                    return Err(Error::new(ErrorKind::Parser, "unexpected mapping end"));
                };
                let FrameKind::Mapping { entries, guess, .. } = frame.kind else {
                    return Err(Error::new(ErrorKind::Parser, "mapping end for a sequence"));
                };
                let flow = frame.style == crate::event::CollectionStyle::Flow;
                let mut entries = entries;
                if let [only] = entries.as_mut_slice() {
                    only.value.entry_span = None;
                }
                if guess {
                    for entry in &mut entries {
                        entry.ambiguous = true;
                        entry.value.entry_span = None;
                    }
                }
                // A pair in a flow sequence, as in `[a: b]`, is a mapping with no braces. Its
                // end event stands at the token after it, past blanks and a comment, where a
                // mapping with braces has its `}`. The pair ends where what it writes does.
                let braced = self
                    .input
                    .get(span.start()..)
                    .is_some_and(|text| text.starts_with('}'));
                let end = match entries.last() {
                    Some(last) if flow && !braced => {
                        pair_end(self.input, frame.open.start(), last, span.start())
                    }
                    _ => span.end(),
                };
                let node = SpannedNode {
                    span: Span::new(frame.open.start(), end),
                    entry_span: None,
                    content_end: match entries.last() {
                        Some(last) if !flow => last.value.content_end,
                        _ => Some(end),
                    },
                    anchored: frame.anchored
                        || entries
                            .iter()
                            .any(|entry| entry.key.anchored || entry.value.anchored),
                    kind: Kind::Mapping(entries),
                };
                if flow {
                    self.after_block_scalar = false;
                }
                self.add(node, frame.after_block_scalar);
            }
        }
        Ok(())
    }

    /// Add a finished node to the collection that is open. `after_block_scalar` says whether
    /// the last thing the source wrote before the node was a block scalar.
    fn add(&mut self, mut node: SpannedNode<'a>, after_block_scalar: bool) {
        // An alias to a collection reads the entries inside it, so removing one of them changes
        // what the alias stands for.
        let marked = self.marked;
        let aliased = marked || self.stack.iter().any(|frame| frame.anchored);
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
                    aliased || node.anchored,
                    after_block_scalar,
                );
                items.push(node);
            }
            FrameKind::Mapping {
                entries,
                key,
                seen,
                guess,
            } => {
                let Some(waiting) = key.take() else {
                    node.entry_span = None;
                    *key = Some(Box::new((node, after_block_scalar)));
                    return;
                };
                let (mut key_node, after_block_scalar) = *waiting;
                key_node.entry_span = None;
                // In an input with a byte-order mark past its start, what a line's indentation
                // is depends on the reader, so the value stays where the parser places it.
                if node.is_missing_value()
                    && !marked
                    && let Some(key_end) = key_node.content_end
                    && let Some(at) =
                        missing_value_place(self.input, frame.open.start(), key_end, frame.style)
                {
                    // The parser places a value that isn't there at the next token. It stands
                    // after its key's `:`, where a value would be written.
                    node.span = Span::empty(at);
                    node.content_end = Some(at);
                }
                let span = block_entry_span(
                    self.input,
                    frame.style,
                    Lead::Key {
                        end: key_node.content_end,
                    },
                    key_node.span.start(),
                    node.content_end,
                    aliased || key_node.anchored || node.anchored,
                    after_block_scalar,
                );
                let key_value = key_node.resolved_value();
                *guess |= key_value.is_none();
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

/// Where a pair with no braces ends, which starts at `start` and has the token after it at
/// `next`: with its value, where the source writes one. Where it doesn't, the pair ends with the
/// last thing written before `next`: its key, a `?` or `:`, a tag, or an anchor. Blanks, line
/// breaks, and comments after that are not the pair's.
fn pair_end(input: &str, start: usize, entry: &Entry<'_>, next: usize) -> usize {
    if let Some(end) = entry.value.content_end {
        return end;
    }
    let from = entry.key.content_end.unwrap_or(start).min(next);
    let bytes = input.as_bytes();
    let blank = |at: usize| matches!(bytes.get(at), Some(b' ' | b'\t' | b'\n' | b'\r'));
    // The compatibility reader reads past a byte-order mark that begins a line, so one is no
    // more written text than the line's spaces are, and a comment may follow it. A mark
    // anywhere else is text to it, which would be the pair's value, so one that stands here,
    // in a pair with no value, begins its line, after the break that separates what follows.
    let mark = '\u{feff}';
    // Whether a `#` here would start a comment: something separates it from the pair's text.
    let (mut end, mut at, mut separated) = (from, from, false);
    while at < next {
        if blank(at) {
            at += 1;
            separated = true;
        } else if input[at..].starts_with(mark) {
            at += mark.len_utf8();
        } else if bytes[at] == b'#' && separated {
            while at < next && !matches!(bytes[at], b'\n' | b'\r') {
                at += 1;
            }
        } else {
            while at < next && !blank(at) {
                at += 1;
            }
            end = at;
            separated = false;
        }
    }
    end
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
    /// A mapping's key, which ends at `end`: indentation alone before it, and the key and its
    /// `:` on that one line. A key that follows a `?`, on that line or an earlier one, has its
    /// `:` on a later line or has none. A key with no text has no known end, whether a `?`
    /// came before it or not.
    Key { end: Option<usize> },
    /// A sequence's item: indentation, its `-`, and spaces.
    Item,
}

/// The span whose removal removes one block entry and nothing else, or `None` when the entry
/// has no such span (see [`SpannedNode::entry_span`]). The entry begins at `first`, its last
/// character ends at `content_end`, when that is known, `anchored` says whether it defines an
/// anchor, stands inside a collection that has one, or is in an input with a byte-order mark
/// past the marks that open it, and `after_block_scalar` whether the last thing written before
/// it is a block scalar.
fn block_entry_span(
    input: &str,
    style: crate::event::CollectionStyle,
    lead: Lead,
    first: usize,
    content_end: Option<usize>,
    anchored: bool,
    after_block_scalar: bool,
) -> Option<Span> {
    if style == crate::event::CollectionStyle::Flow || anchored {
        return None;
    }
    let content_end = content_end?;
    let mut start = line_start(input, first);
    // The byte-order marks that open the input are no part of the first entry.
    if start == 0 {
        start = input.len() - input.trim_start_matches('\u{feff}').len();
    }
    let before = input.get(start..first)?;
    let begins_its_line = match lead {
        Lead::Key { end } => {
            let end = end?;
            let colon = after_key(input, end);
            before.bytes().all(|byte| byte == b' ')
                && colon > end
                && !input.get(first..colon)?.contains(['\n', '\r'])
        }
        Lead::Item => before
            .trim_start_matches(' ')
            .strip_prefix('-')
            .is_some_and(|blanks| !blanks.is_empty() && blanks.bytes().all(|byte| byte == b' ')),
    };
    if !begins_its_line {
        return None;
    }
    let end = entry_end(input, content_end)?;
    // A line that begins with `:` gives a value to the key before it. With this entry gone,
    // that could be an explicit key from before the entry.
    if value_indicator_follows(input, end) {
        return None;
    }
    // A block scalar that is the last thing written before this entry ends where the entry
    // begins. With the entry gone it would run on through the blank and comment lines that
    // follow, unless content follows.
    if after_block_scalar && !content_follows(input, end) {
        return None;
    }
    (end > start).then(|| Span::new(start, end))
}

/// Whether the input ends at `from`, or the line there holds more than blanks or a comment.
fn content_follows(input: &str, from: usize) -> bool {
    let rest = input.get(from..).unwrap_or_default();
    let line = rest.split(['\n', '\r']).next().unwrap_or(rest);
    let content = line.trim_start_matches([' ', '\t']);
    rest.is_empty() || !(content.is_empty() || content.starts_with('#'))
}

/// Whether the first line from `from` on that holds more than blanks or a comment begins with a
/// `:` indicator.
fn value_indicator_follows(input: &str, from: usize) -> bool {
    let rest = input.get(from..).unwrap_or_default();
    for line in rest.split(['\n', '\r']) {
        let content = line.trim_start_matches([' ', '\t']);
        if content.is_empty() || content.starts_with('#') {
            continue;
        }
        return content
            .strip_prefix(':')
            .is_some_and(|after| after.is_empty() || after.starts_with([' ', '\t']));
    }
    false
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

/// Where a value that the source doesn't write stands, when the source shows that it wrote
/// none: after the `:` that follows its key on the key's line. `None` when no `:` follows the
/// key there, as after an explicit key, and when what comes next could be the value: anything
/// but a comment on the key's line, or a line indented past the mapping, which begins at
/// `mapping_start`. A tag that names the kind of a collection stands there with no event to
/// show for it. The mapping's column is the one to compare with, and not the key's: a tag or an
/// anchor before a key puts the key itself further right.
fn missing_value_place(
    input: &str,
    mapping_start: usize,
    key_end: usize,
    style: crate::event::CollectionStyle,
) -> Option<usize> {
    let at = after_key(input, key_end);
    if at == key_end {
        return None;
    }
    let flow = style == crate::event::CollectionStyle::Flow;
    // The column counts characters, and a byte-order mark that opens the input is none.
    let key_column = input
        .get(line_start(input, mapping_start)..mapping_start)?
        .chars()
        .filter(|ch| *ch != '\u{feff}')
        .count();
    for (number, line) in input.get(at..)?.split(['\n', '\r']).enumerate() {
        let content = line.trim_start_matches(' ');
        let blank = content.trim_start_matches('\t').is_empty();
        // A comment needs a blank before its `#`, which a line's start is.
        if blank || content.starts_with('#') || content.trim_start_matches('\t').starts_with('#') {
            continue;
        }
        let ends_the_entry = if flow {
            content.starts_with([',', '}', ']'])
        } else {
            number > 0 && line.len() - content.len() <= key_column
        };
        return ends_the_entry.then_some(at);
    }
    Some(at)
}

/// The offset after the `:` that follows a key on the key's line, or the key's end when none
/// does. A key that is a block scalar ends with its last line, so a `:` after it stands on
/// another line, and is another entry's.
fn after_key(input: &str, key_end: usize) -> usize {
    let Some((before, rest)) = input.split_at_checked(key_end) else {
        return key_end;
    };
    if before.ends_with(['\n', '\r']) {
        return key_end;
    }
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
