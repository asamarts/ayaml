//! A read-only tree with byte spans and path lookup.
//!
//! The tree is built from the same spanned events as the value readers. It is
//! intentionally conservative around aliases, merge keys, duplicate keys, and
//! multiple documents: a lookup that would have to guess returns `None`.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::PathSegment;
use crate::error::{Error, ErrorCode, ErrorKind};
use crate::event::{Event, ScalarStyle};
use crate::limits::Limits;
use crate::mapping::Mapping;
use crate::options::ParseOptions;
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
        Self::parse_with(input, &ParseOptions::new())
    }

    /// Parse one default-profile stream into a spanned tree with explicit
    /// parse options.
    ///
    /// When `options.merge_keys` is enabled, merged entries keep the source [`Span`] of the
    /// originating entry in the merged operand and [`SpannedNode::entry_span`] returns `None`
    /// conservatively so deleting a merged entry can never mutate its anchored source mapping.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax and limit failures as
    /// [`crate::parse_events_with`].
    pub fn parse_with(input: &'a str, options: &ParseOptions) -> Result<Self, Error> {
        let events = crate::parse_events_with(input, options)?;
        build(input, events, options.merge_keys)
    }

    /// Parse one default-profile stream with explicit limits.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax and limit failures as
    /// [`crate::parse_events_with`].
    #[deprecated(
        note = "use `SpannedTree::parse_with` with `ParseOptions`; deprecated through 0.x and will be removed in 1.0"
    )]
    pub fn parse_with_limits(input: &'a str, limits: &Limits) -> Result<Self, Error> {
        Self::parse_with(input, &ParseOptions::new().with_limits(limits.clone()))
    }

    /// Parse one compatibility-profile stream into a spanned tree.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax and limit failures as
    /// [`crate::compat::parse_events`].
    #[cfg(feature = "libyaml-compat")]
    #[cfg_attr(docsrs, doc(cfg(feature = "libyaml-compat")))]
    pub fn parse_compat(input: &'a str) -> Result<Self, Error> {
        Self::parse_compat_with(input, &crate::compat::ParseOptions::new())
    }

    /// Parse one compatibility-profile stream into a spanned tree with
    /// explicit compatibility options.
    ///
    /// Applies `options.limits` and preserves syntax tags as written.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax and limit failures as
    /// [`crate::compat::parse_events_with`].
    #[cfg(feature = "libyaml-compat")]
    #[cfg_attr(docsrs, doc(cfg(feature = "libyaml-compat")))]
    pub fn parse_compat_with(
        input: &'a str,
        options: &crate::compat::ParseOptions,
    ) -> Result<Self, Error> {
        let events = crate::compat::parse_events_with(input, options)?;
        build(input, events, false)
    }

    /// Parse one compatibility-profile stream with explicit limits.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax and limit failures as
    /// [`crate::compat::parse_events_with`].
    #[cfg(feature = "libyaml-compat")]
    #[cfg_attr(docsrs, doc(cfg(feature = "libyaml-compat")))]
    #[deprecated(
        note = "use `SpannedTree::parse_compat_with` with `compat::ParseOptions`; deprecated through 0.x and will be removed in 1.0"
    )]
    pub fn parse_compat_with_limits(input: &'a str, limits: &Limits) -> Result<Self, Error> {
        Self::parse_compat_with(
            input,
            &crate::compat::ParseOptions::new().with_limits(limits.clone()),
        )
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
    /// `None` too: a key that the mapping holds twice, a merge key, any key of a mapping that has
    /// an alias among its keys or a key that holds an alias or a merge key (since that key may be
    /// any other key of the mapping again), and any path that steps through an alias value. An
    /// alias value itself is found at its own path, with [`SpannedNode::is_alias`] true.
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
    /// A block mapping's entry and a block sequence's item can have one. Usually the span runs
    /// from the start of the entry's first line through its last line break, so it takes the
    /// entry's indentation, a value on later lines, a block scalar, and a comment after the value.
    /// For the first key of a block mapping that begins on a sequence item's `-` line, the span
    /// runs from the key's start to the next key's start, leaving the `-` and moving the next key
    /// into its column when that is exact. The byte-order marks that open the input stay outside
    /// the first entry's.
    ///
    /// `None` is conservative: it means that no span is known to be exact. These have none:
    ///
    /// - an entry of a flow collection, whose separator belongs to its neighbor as much as to
    ///   it;
    /// - an entry that doesn't begin its own line (other than the first key of a mapping on a
    ///   sequence's `-` line whose next key can move into its place), and one with more than a
    ///   comment after it on its last line;
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
    /// - every entry of a tree parsed with `ParseOptions::with_merge_keys(true)`, where expanded
    ///   entries can originate from anchored source mappings outside the target mapping;
    /// - an entry after a block scalar when a following blank or comment line could be absorbed
    ///   by the scalar (for example, an empty or explicitly indented scalar, a keep-chomping
    ///   scalar followed by a blank line, a blank line with spaces, a tab-indented blank or
    ///   comment line, or a comment indented to the scalar's content column).
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
    let Some((first, rest)) = path.split_first() else {
        return Some(node);
    };
    if node.is_alias() {
        return None;
    }
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

/// Compact encoding of whether the last written node was a block scalar, and if so, its content
/// indentation and keep-chomping flag. Fits in 4 bytes to avoid growing parser frames.
///
/// Encoding:
/// - `0`: not after a block scalar
/// - `1`: after an empty or explicitly indented block scalar (conservative)
/// - `>= 2`: `indent = ((raw - 2) >> 1) as usize`, `keep = (raw - 2) & 1 != 0`
#[derive(Clone, Copy, Default)]
struct AfterBlockScalar(u32);

impl AfterBlockScalar {
    const NONE: Self = Self(0);
    const UNKNOWN: Self = Self(1);

    fn is_some(self) -> bool {
        self.0 != 0
    }

    fn known(self) -> Option<(usize, bool)> {
        (self.0 >= 2).then(|| {
            let packed = self.0 - 2;
            ((packed >> 1) as usize, (packed & 1) != 0)
        })
    }

    fn from_raw(raw: &str) -> Self {
        let (header, rest) = split_first_line(raw);
        let header = header.split('#').next().unwrap_or(header);
        let keep = header.contains('+');
        if header.bytes().any(|byte| byte.is_ascii_digit()) {
            return Self::UNKNOWN;
        }
        let mut cursor = rest;
        while !cursor.is_empty() {
            let (line, next) = split_first_line(cursor);
            let spaces = line.len() - line.trim_start_matches(' ').len();
            if spaces < line.len() {
                return u32::try_from(spaces)
                    .ok()
                    .and_then(|s| s.checked_shl(1))
                    .and_then(|s| s.checked_add(2 + u32::from(keep)))
                    .map_or(Self::UNKNOWN, Self);
            }
            cursor = next;
        }
        Self::UNKNOWN
    }
}

fn split_first_line(text: &str) -> (&str, &str) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            return (&text[..i], &text[i + 1..]);
        }
        if bytes[i] == b'\r' {
            let next = if bytes.get(i + 1) == Some(&b'\n') {
                i + 2
            } else {
                i + 1
            };
            return (&text[..i], &text[next..]);
        }
        i += 1;
    }
    (text, "")
}

fn build<'a>(
    input: &'a str,
    events: impl Iterator<Item = Result<Event<'a>, Error>>,
    suppress_entry_spans: bool,
) -> Result<SpannedTree<'a>, Error> {
    let mut builder = Builder {
        input,
        documents: Vec::new(),
        stack: Vec::new(),
        root: None,
        after_block_scalar: AfterBlockScalar::NONE,
        // A run of marks opens the input as one mark does.
        marked: input.trim_start_matches('\u{feff}').contains('\u{feff}'),
        suppress_entry_spans,
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
    after_block_scalar: AfterBlockScalar,
    /// The input holds a byte-order mark past the marks that open it. The compatibility reader
    /// counts one at a line's start toward that line's indentation, and takes one after a line
    /// break for a comment's or a blank line's start, unless it opens the input. So what such
    /// a mark means depends on the lines before it, and no entry's removal is known to be
    /// exact.
    marked: bool,
    /// Suppress all `entry_span`s conservatively (for example, when merge-key expansion replays
    /// entries whose source spans point into anchored source mappings).
    suppress_entry_spans: bool,
}

struct Frame<'a> {
    open: Span,
    style: crate::event::CollectionStyle,
    /// The collection has an anchor of its own.
    anchored: bool,
    /// What [`Builder::after_block_scalar`] was when the collection began.
    after_block_scalar: AfterBlockScalar,
    /// The collection is a mapping key (or nested inside one), so the builder assembles its
    /// resolved [`Value`] bottom-up in linear time instead of re-walking nested subtrees.
    in_key: bool,
    kind: FrameKind<'a>,
}

enum KeyTracker {
    /// Duplicate-key table for regular mappings or complex-key mappings that became unresolvable.
    Seen(HashMap<Value, usize>),
    /// Bottom-up resolved mapping for an `in_key` mapping whose entries are all unique and
    /// resolvable so far; entries align 1:1 with `FrameKind::Mapping::entries`.
    Resolved(Box<Mapping>),
}

struct PendingKey<'a> {
    node: SpannedNode<'a>,
    resolved: Option<Box<Value>>,
    after_block_scalar: AfterBlockScalar,
}

enum FrameKind<'a> {
    Sequence {
        items: Vec<SpannedNode<'a>>,
        resolved: Option<Vec<Value>>,
    },
    Mapping {
        entries: Vec<Entry<'a>>,
        /// The key that waits for its value, its bottom-up resolved value (when the key was a
        /// collection), and what [`Builder::after_block_scalar`] was when the key began. Boxed,
        /// so a sequence's frame isn't as large.
        key: Option<Box<PendingKey<'a>>>,
        keys: KeyTracker,
        /// A key here has no value of its own to compare: an alias, or a collection that holds
        /// one. It may be any other key of the mapping again, so a lookup would have to guess.
        guess: bool,
    },
}

impl<'a> Builder<'a> {
    /// Whether a new collection at the current position belongs to a mapping key (or is nested
    /// inside one).
    fn in_key_position(&self) -> bool {
        self.stack.last().is_some_and(|frame| {
            frame.in_key || matches!(frame.kind, FrameKind::Mapping { key: None, .. })
        })
    }

    /// Whether the current open collection itself tracks bottom-up resolved values.
    fn tracking_resolved(&self) -> bool {
        self.stack.last().is_some_and(|frame| frame.in_key)
    }

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
                        if matches!(style, ScalarStyle::Literal | ScalarStyle::Folded) {
                            AfterBlockScalar::from_raw(&raw)
                        } else {
                            AfterBlockScalar::NONE
                        };
                }
                let resolved = self
                    .tracking_resolved()
                    .then(|| {
                        resolve_scalar(&value, style, tag.as_ref())
                            .ok()
                            .map(Box::new)
                    })
                    .flatten();
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
                self.add(node, resolved, before);
            }
            Event::Alias { span, .. } => {
                let before =
                    std::mem::replace(&mut self.after_block_scalar, AfterBlockScalar::NONE);
                let node = SpannedNode {
                    span,
                    entry_span: None,
                    content_end: Some(span.end()),
                    anchored: false,
                    kind: Kind::Alias,
                };
                self.add(node, None, before);
            }
            Event::SequenceStart {
                span,
                style,
                anchor,
                ..
            } => {
                let in_key = self.in_key_position();
                self.stack.push(Frame {
                    open: span,
                    style,
                    anchored: anchor.is_some(),
                    after_block_scalar: self.after_block_scalar,
                    in_key,
                    kind: FrameKind::Sequence {
                        items: Vec::new(),
                        resolved: in_key.then(Vec::new),
                    },
                });
            }
            Event::SequenceEnd { span } => self.finish_sequence(span)?,
            Event::MappingStart {
                span,
                style,
                anchor,
                ..
            } => {
                let in_key = self.in_key_position();
                let keys = if in_key {
                    KeyTracker::Resolved(Box::new(Mapping::new()))
                } else {
                    KeyTracker::Seen(HashMap::new())
                };
                self.stack.push(Frame {
                    open: span,
                    style,
                    anchored: anchor.is_some(),
                    after_block_scalar: self.after_block_scalar,
                    in_key,
                    kind: FrameKind::Mapping {
                        entries: Vec::new(),
                        key: None,
                        keys,
                        guess: false,
                    },
                });
            }
            Event::MappingEnd { span } => self.finish_mapping(span)?,
        }
        Ok(())
    }

    fn finish_sequence(&mut self, span: Span) -> Result<(), Error> {
        let Some(frame) = self.stack.pop() else {
            return Err(Error::coded(
                ErrorKind::Parser,
                ErrorCode::MalformedEventStream,
                "unexpected sequence end",
            ));
        };
        let FrameKind::Sequence {
            mut items,
            resolved,
        } = frame.kind
        else {
            return Err(Error::coded(
                ErrorKind::Parser,
                ErrorCode::MalformedEventStream,
                "sequence end for a mapping",
            ));
        };
        let flow = frame.style == crate::event::CollectionStyle::Flow;
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
            self.after_block_scalar = AfterBlockScalar::NONE;
        }
        let resolved_val = resolved.map(|vals| Box::new(Value::Sequence(vals)));
        self.add(node, resolved_val, frame.after_block_scalar);
        Ok(())
    }

    fn finish_mapping(&mut self, span: Span) -> Result<(), Error> {
        let Some(frame) = self.stack.pop() else {
            return Err(Error::coded(
                ErrorKind::Parser,
                ErrorCode::MalformedEventStream,
                "unexpected mapping end",
            ));
        };
        let FrameKind::Mapping {
            mut entries,
            keys,
            guess,
            ..
        } = frame.kind
        else {
            return Err(Error::coded(
                ErrorKind::Parser,
                ErrorCode::MalformedEventStream,
                "mapping end for a sequence",
            ));
        };
        let flow = frame.style == crate::event::CollectionStyle::Flow;
        if let [only] = entries.as_mut_slice() {
            only.value.entry_span = None;
        }
        if guess {
            for entry in &mut entries {
                entry.ambiguous = true;
                entry.value.entry_span = None;
            }
        } else if !flow
            && !self.marked
            && !self.suppress_entry_spans
            && !frame.anchored
            && !self.stack.iter().any(|outer| outer.anchored)
            && entries.len() >= 2
            && !entries[0].ambiguous
            && entries[0].value.entry_span.is_none()
            && !entries[0].key.anchored
            && !entries[0].value.anchored
        {
            entries[0].value.entry_span =
                compact_first_entry_span(self.input, &entries[0], &entries[1]);
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
            self.after_block_scalar = AfterBlockScalar::NONE;
        }
        let resolved_value = match keys {
            KeyTracker::Resolved(map) if !guess => Some(Box::new(Value::Mapping(*map))),
            _ => None,
        };
        self.add(node, resolved_value, frame.after_block_scalar);
        Ok(())
    }

    /// Add a finished node to the collection that is open. `after_block_scalar` says whether
    /// the last thing the source wrote before the node was a block scalar.
    fn add(
        &mut self,
        mut node: SpannedNode<'a>,
        node_resolved: Option<Box<Value>>,
        after_block_scalar: AfterBlockScalar,
    ) {
        // An alias to a collection reads the entries inside it, so removing one of them changes
        // what the alias stands for.
        let marked = self.marked;
        let aliased =
            marked || self.suppress_entry_spans || self.stack.iter().any(|frame| frame.anchored);
        let Some(frame) = self.stack.last_mut() else {
            self.root = Some(node);
            return;
        };
        match &mut frame.kind {
            FrameKind::Sequence { items, resolved } => {
                if let Some(values) = resolved {
                    if let Some(value) = node_resolved {
                        values.push(*value);
                    } else {
                        *resolved = None;
                    }
                }
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
                keys,
                guess,
            } => {
                let Some(waiting) = key.take() else {
                    node.entry_span = None;
                    *key = Some(Box::new(PendingKey {
                        node,
                        resolved: node_resolved,
                        after_block_scalar,
                    }));
                    return;
                };
                let PendingKey {
                    node: mut key_node,
                    resolved: key_resolved,
                    after_block_scalar,
                } = *waiting;
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
                // Nodes inside `in_key` and collection keys already built their resolved value
                // bottom-up; scalar keys outside `in_key` resolve in O(1) directly from the node.
                let key_value = if frame.in_key {
                    key_resolved.map(|val| *val)
                } else if key_node.is_scalar() {
                    key_node.resolved_value()
                } else {
                    key_resolved.map(|val| *val)
                };
                *guess |= key_value.is_none();
                let mut ambiguous = is_merge_key(&key_node);
                match keys {
                    KeyTracker::Resolved(map) => {
                        if let Some(kv) = key_value {
                            if let Some(previous) = map.get_index_of(&kv) {
                                if let Some(entry) = entries.get_mut(previous) {
                                    entry.ambiguous = true;
                                }
                                ambiguous = true;
                                if let KeyTracker::Resolved(existing) =
                                    std::mem::replace(keys, KeyTracker::Seen(HashMap::new()))
                                    && let KeyTracker::Seen(seen) = keys
                                {
                                    for (idx, (k, _)) in existing.into_iter().enumerate() {
                                        seen.insert(k, idx);
                                    }
                                }
                            } else if !ambiguous && let Some(vv) = node_resolved {
                                map.insert(kv, *vv);
                            } else if let KeyTracker::Resolved(existing) =
                                std::mem::replace(keys, KeyTracker::Seen(HashMap::new()))
                                && let KeyTracker::Seen(seen) = keys
                            {
                                for (idx, (k, _)) in existing.into_iter().enumerate() {
                                    seen.insert(k, idx);
                                }
                                let index = entries.len();
                                seen.entry(kv).or_insert(index);
                            }
                        } else {
                            *keys = KeyTracker::Seen(HashMap::new());
                        }
                    }
                    KeyTracker::Seen(seen) => {
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
                    }
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
/// Whether `before` is indentation followed by one or more block-sequence `- ` indicators, as
/// precedes the first key of a compact mapping (`- a: 1` or `- - a: 1`).
fn is_compact_item_prefix(before: &str) -> bool {
    let mut rest = before.trim_start_matches(' ');
    let mut seen = false;
    while let Some(after) = rest.strip_prefix('-') {
        let spaces = after.len() - after.trim_start_matches(' ').len();
        if spaces == 0 {
            return false;
        }
        rest = &after[spaces..];
        seen = true;
    }
    seen && rest.is_empty()
}

/// The span that removes the first entry of a block mapping starting on a sequence's `-` line
/// (`- a: 1\n  b: 2\n`), leaving `- ` and shifting `next` into `first`'s column (`- b: 2\n`).
fn compact_first_entry_span(
    input: &str,
    first_entry: &Entry<'_>,
    next_entry: &Entry<'_>,
) -> Option<Span> {
    let first = first_entry.key.span.start();
    let key_end = first_entry.key.content_end?;
    let content_end = first_entry.value.content_end?;
    let mut line0 = line_start(input, first);
    if line0 == 0 {
        line0 = input.len() - input.trim_start_matches('\u{feff}').len();
    }
    let before = input.get(line0..first)?;
    if !is_compact_item_prefix(before) {
        return None;
    }
    let colon = after_key(input, key_end);
    if colon <= key_end || input.get(first..colon)?.contains(['\n', '\r']) {
        return None;
    }
    let end = entry_end(input, content_end)?;
    let next_first = next_entry.key.span.start();
    let next_key_end = next_entry.key.content_end?;
    let next_line = line_start(input, next_first);
    if end > next_line {
        return None;
    }
    let next_before = input.get(next_line..next_first)?;
    if !next_before.bytes().all(|byte| byte == b' ') || next_before.len() != before.len() {
        return None;
    }
    let next_colon = after_key(input, next_key_end);
    if next_colon <= next_key_end || input.get(next_first..next_colon)?.contains(['\n', '\r']) {
        return None;
    }
    if end < next_line {
        let between = input.get(end..next_line)?;
        // Refuse if any comment or non-whitespace line stands between the first entry and the
        // next key; deleting up to `next_first` would silently delete that intervening comment.
        if !between
            .bytes()
            .all(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }
        if value_indicator_follows(input, end) {
            return None;
        }
    }
    (next_first > first).then(|| Span::new(first, next_first))
}

fn block_entry_span(
    input: &str,
    style: crate::event::CollectionStyle,
    lead: Lead,
    first: usize,
    content_end: Option<usize>,
    anchored: bool,
    after_block_scalar: AfterBlockScalar,
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
    // begins. With the entry gone it could absorb blank or comment lines that follow unless
    // content follows or every intervening blank/comment line is outside the scalar.
    if after_block_scalar.is_some() && !block_scalar_ignores_after(input, end, after_block_scalar) {
        return None;
    }
    (end > start).then(|| Span::new(start, end))
}

/// Whether the preceding block scalar ignores all blank and comment lines starting at `from`
/// until the next content line (or EOF).
fn block_scalar_ignores_after(input: &str, from: usize, scalar: AfterBlockScalar) -> bool {
    if content_follows(input, from) {
        return true;
    }
    let Some((indent, keep)) = scalar.known() else {
        return false;
    };
    let mut cursor = input.get(from..).unwrap_or_default();
    let mut seen_comment = false;
    while !cursor.is_empty() {
        let (line, next) = split_first_line(cursor);
        let spaces = line.len() - line.trim_start_matches(' ').len();
        let after = &line[spaces..];
        if after.is_empty() {
            if spaces > 0 || (keep && !seen_comment) {
                return false;
            }
        } else if after.starts_with('\t') {
            let trimmed = after.trim_start_matches([' ', '\t']);
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return false;
            }
            return true;
        } else if after.starts_with('#') {
            if spaces >= indent {
                return false;
            }
            seen_comment = true;
        } else {
            return true;
        }
        cursor = next;
    }
    true
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
    let bytes = input.as_bytes();
    let mut i = index;
    while i > 0 && !matches!(bytes[i - 1], b'\n' | b'\r') {
        i -= 1;
    }
    i
}

fn line_end(input: &str, index: usize) -> usize {
    let bytes = input.as_bytes();
    let mut end = index;
    while end < bytes.len() && !matches!(bytes[end], b'\n' | b'\r') {
        end += 1;
    }
    if end < bytes.len() {
        if bytes[end] == b'\r' && bytes.get(end + 1) == Some(&b'\n') {
            end += 2;
        } else {
            end += 1;
        }
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complex_keys_detect_duplicates_and_ambiguity_in_linear_time() {
        // Duplicate mapping keys make lookup return None, while unique complex keys resolve.
        let unique = "? {a: 1, b: [2, 3]}\n: ok\nother: yes\n";
        let tree = SpannedTree::parse(unique).expect("parse unique complex key");
        let mut expected_key = Mapping::new();
        expected_key.insert(Value::String("a".into()), Value::from(1_i64));
        expected_key.insert(
            Value::String("b".into()),
            Value::Sequence(vec![Value::from(2_i64), Value::from(3_i64)]),
        );
        let found = tree
            .lookup(&[PathSegment::Key(Value::Mapping(expected_key.clone()))])
            .expect("unique complex key should be found");
        assert_eq!(found.scalar_text(), Some("ok"));
        assert_eq!(
            tree.lookup(&[PathSegment::Key(Value::String("other".into()))])
                .and_then(SpannedNode::scalar_text),
            Some("yes")
        );

        let dup = "? {a: 1, b: [2, 3]}\n: first\n? {b: [2, 3], a: 1}\n: second\nother: yes\n";
        let dup_tree = SpannedTree::parse(dup).expect("parse duplicate complex keys");
        assert!(
            dup_tree
                .lookup(&[PathSegment::Key(Value::Mapping(expected_key))])
                .is_none(),
            "duplicate complex keys must be marked ambiguous"
        );
        assert_eq!(
            dup_tree
                .lookup(&[PathSegment::Key(Value::String("other".into()))])
                .and_then(SpannedNode::scalar_text),
            Some("yes")
        );

        // An inner duplicate, merge key, or alias inside a complex key marks the enclosing
        // mapping ambiguous so lookups do not guess.
        for ambiguous_input in [
            "? {a: 1, a: 2}\n: v\nother: yes\n",
            "? {<<: {a: 1}}\n: v\nother: yes\n",
            "anchor: &x 1\n? {a: *x}\n: v\nother: yes\n",
            "? [*x]\n: v\nother: yes\n",
        ] {
            let t = SpannedTree::parse(ambiguous_input).expect("parse ambiguous complex key");
            assert!(
                t.lookup(&[PathSegment::Key(Value::String("other".into()))])
                    .is_none(),
                "enclosing mapping must not guess for {ambiguous_input:?}"
            );
        }

        // Deeply nested complex keys stay linear.
        let mut nested = "{k0: 0, k1: 1, k2: 2}".to_owned();
        for _ in 0..48 {
            nested = format!("{{? {nested}: null}}");
        }
        nested.push('\n');
        assert!(SpannedTree::parse(&nested).is_ok());
    }
}
