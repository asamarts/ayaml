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
use crate::mapping::{GlobalHasher, Mapping, SMALL_LIMIT};
use crate::options::ParseOptions;
use crate::resolver::{resolve_plain, resolve_scalar};
use crate::span::Span;
use crate::tag::Tag;
use crate::value::Value;

mod span_scan;

use span_scan::{
    Lead, block_entry_span, compact_first_entry_span, compute_entry_end, is_merge_key,
    missing_value_place, pair_end, scan_blank_run,
};

/// Compact `Option<Span>` encoding using `usize::MAX` as `None` sentinel (no valid byte offset in
/// any Rust slice can reach `usize::MAX`), saving 8 bytes per [`SpannedNode`].
#[derive(Clone, Copy, Debug)]
struct PackedOptionSpan {
    start: usize,
    end: usize,
}

impl PackedOptionSpan {
    const NONE: Self = Self {
        start: usize::MAX,
        end: 0,
    };

    #[inline]
    const fn from_option(span: Option<Span>) -> Self {
        match span {
            Some(span) => Self {
                start: span.start(),
                end: span.end(),
            },
            None => Self::NONE,
        }
    }

    #[inline]
    const fn get(self) -> Option<Span> {
        if self.start == usize::MAX {
            None
        } else {
            Some(Span::new(self.start, self.end))
        }
    }

    #[inline]
    const fn is_none(self) -> bool {
        self.start == usize::MAX
    }
}

/// A parsed YAML stream with source spans.
#[derive(Clone, Debug)]
pub struct SpannedTree<'a> {
    documents: Vec<SpannedNode<'a>>,
}

/// One node in a [`SpannedTree`].
#[derive(Clone, Debug)]
pub struct SpannedNode<'a> {
    span: Span,
    entry_span: PackedOptionSpan,
    kind: Kind<'a>,
}

/// Full metadata for quoted, block, multi-line folded, owned, or tagged scalars.
#[derive(Clone, Debug)]
struct ScalarData<'a> {
    value: Cow<'a, str>,
    raw: Cow<'a, str>,
    style: ScalarStyle,
    tag: Option<Tag>,
}

#[derive(Clone, Debug)]
enum Kind<'a> {
    /// Unquoted, untagged single-line plain scalar borrowed directly from input (`value == raw`).
    PlainScalar(&'a str),
    /// Quoted, block, multi-line folded, owned, or tagged scalar.
    Scalar(Box<ScalarData<'a>>),
    Sequence(Box<[SpannedNode<'a>]>),
    Mapping(Box<[Entry<'a>]>),
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
        options.limits.validate()?;
        options.limits.check_input(input.len()).map_err(|error| {
            Error::from(error)
                .with_span(Span::empty(0))
                .with_location(crate::Location::new(1, 1))
        })?;
        if options.merge_keys {
            let events = crate::parse_events_with(input, options)?;
            return build(input, events, true);
        }
        crate::parser::parse_spanned(input, &options.limits, false)
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
        crate::parser::parse_spanned(input, &options.limits, true)
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
        self.entry_span.get()
    }

    /// Whether this is a scalar node.
    #[must_use]
    pub const fn is_scalar(&self) -> bool {
        matches!(self.kind, Kind::PlainScalar(_) | Kind::Scalar(_))
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
            Kind::PlainScalar(raw) => Some(raw),
            Kind::Scalar(data) => Some(&data.raw),
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
                if entry.key.matches_key(key) {
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

impl<'a> SpannedNode<'a> {
    /// Zero-allocation fast path for checking whether a mapping key node equals `target`.
    fn matches_key(&self, target: &Value) -> bool {
        if let Value::String(target_str) = target {
            match &self.kind {
                Kind::PlainScalar(text) => {
                    return *text == target_str.as_str() && resolve_plain(text).is_none();
                }
                Kind::Scalar(data) if data.tag.is_none() => {
                    return if data.style == ScalarStyle::Plain {
                        data.value.as_ref() == target_str.as_str()
                            && resolve_plain(&data.value).is_none()
                    } else {
                        data.value.as_ref() == target_str.as_str()
                    };
                }
                _ => {}
            }
        }
        self.resolved_value().as_ref() == Some(target)
    }

    /// Resolve a scalar key into a [`SpannedKey`] without allocating a `Value::String` for
    /// borrowed plain, quoted, or block string keys.
    fn spanned_scalar_key(&self) -> Option<SpannedKey<'a>> {
        match &self.kind {
            Kind::PlainScalar(text) => Some(match resolve_plain(text) {
                None => SpannedKey::Str(Cow::Borrowed(text)),
                Some(value) => SpannedKey::Other(value),
            }),
            Kind::Scalar(data) => match &data.tag {
                None if data.style != ScalarStyle::Plain => {
                    Some(SpannedKey::Str(data.value.clone()))
                }
                None => Some(match resolve_plain(&data.value) {
                    None => SpannedKey::Str(data.value.clone()),
                    Some(value) => SpannedKey::Other(value),
                }),
                Some(tag) => resolve_scalar(&data.value, data.style, Some(tag))
                    .ok()
                    .map(SpannedKey::from_value),
            },
            _ => None,
        }
    }

    fn resolved_value(&self) -> Option<Value> {
        match &self.kind {
            Kind::PlainScalar(text) => {
                Some(resolve_plain(text).unwrap_or_else(|| Value::String((*text).to_owned())))
            }
            Kind::Scalar(data) => resolve_scalar(&data.value, data.style, data.tag.as_ref()).ok(),
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
    let mut builder = Builder::new(input, suppress_entry_spans);
    for event in events {
        builder.event(event?)?;
    }
    Ok(builder.finish())
}

/// Memoized summary of a contiguous run of blank and comment lines starting at line boundary
/// `from` and ending at the first content line `next_content` (or `input.len()`).
#[derive(Clone, Copy)]
struct BlankRunSummary {
    from: usize,
    next_content: usize,
    next_is_value_indicator: bool,
    next_content_indent: usize,
    /// True when any blank line has leading spaces or any blank/comment line is tab-indented,
    /// which prevents any preceding block scalar from safely ignoring the run.
    blocks_any_scalar: bool,
    has_blank_before_comment: bool,
    max_comment_indent: Option<usize>,
    #[cfg(test)]
    lines_scanned: usize,
}

impl BlankRunSummary {
    fn block_scalar_ignores(self, scalar: AfterBlockScalar) -> bool {
        if self.from == self.next_content {
            return true;
        }
        let Some((indent, keep)) = scalar.known() else {
            return false;
        };
        if self.blocks_any_scalar {
            return false;
        }
        if keep && self.has_blank_before_comment {
            return false;
        }
        self.max_comment_indent.is_none_or(|col| col < indent)
    }
}

/// Linear-time line and trailing-blank scanner with single-entry memoization so deeply nested
/// block closings sharing the same `content_end` or trailing blank/comment run scan each byte
/// at most once.
struct LineScanCache<'a> {
    input: &'a str,
    last_entry_end: Option<(usize, Option<usize>)>,
    last_blank_run: Option<BlankRunSummary>,
    #[cfg(test)]
    blank_run_lines_scanned: usize,
}

impl<'a> LineScanCache<'a> {
    const fn new(input: &'a str) -> Self {
        Self {
            input,
            last_entry_end: None,
            last_blank_run: None,
            #[cfg(test)]
            blank_run_lines_scanned: 0,
        }
    }

    fn entry_end(&mut self, content_end: usize) -> Option<usize> {
        if let Some((cached_at, cached)) = self.last_entry_end
            && cached_at == content_end
        {
            return cached;
        }
        let result = compute_entry_end(self.input, content_end);
        // Preserve the unwinding frontier: backward lookbacks must not evict a newer offset.
        if self
            .last_entry_end
            .is_none_or(|(cached_at, _)| content_end >= cached_at)
        {
            self.last_entry_end = Some((content_end, result));
        }
        result
    }

    fn blank_run(&mut self, from: usize) -> BlankRunSummary {
        let from = from.min(self.input.len());
        if let Some(cached) = self.last_blank_run
            && cached.from == from
        {
            return cached;
        }
        let summary = scan_blank_run(self.input, from);
        #[cfg(test)]
        {
            self.blank_run_lines_scanned += summary.lines_scanned;
        }
        if self.last_blank_run.is_none_or(|cached| from >= cached.from) {
            self.last_blank_run = Some(summary);
        }
        summary
    }

    fn value_indicator_follows(&mut self, from: usize) -> bool {
        if let Some(cached) = self.last_blank_run
            && from >= cached.from
            && from <= cached.next_content
        {
            return cached.next_is_value_indicator;
        }
        self.blank_run(from).next_is_value_indicator
    }

    fn block_scalar_ignores_after(&mut self, from: usize, scalar: AfterBlockScalar) -> bool {
        self.blank_run(from).block_scalar_ignores(scalar)
    }
}

/// Maximum items/entries buffered in `Builder`'s shared scratch stacks before spilling to an owned
/// `Vec`, keeping small collections exact-sized (`Box<[_]>`, zero slack) without doubling peak
/// memory on huge flat collections.
const SCRATCH_LIMIT: usize = 64;

/// Build-time metadata for a completed node; never stored in the final [`SpannedTree`].
#[derive(Clone, Copy)]
struct NodeMeta {
    content_end: Option<usize>,
    anchored: bool,
}

/// Borrowed-first duplicate-key representation so plain, quoted, block, and `!!str` mapping keys
/// never allocate a `Value::String`.
///
/// Invariant: `Other` never wraps `Value::String(_)`, so `Str` and `Other` are disjoint.
#[derive(PartialEq, Eq, Hash)]
enum SpannedKey<'a> {
    Str(Cow<'a, str>),
    Other(Value),
}

impl SpannedKey<'_> {
    fn from_value(value: Value) -> Self {
        match value {
            Value::String(text) => Self::Str(Cow::Owned(text)),
            other => Self::Other(other),
        }
    }
}

enum SeqStorage<'a> {
    Scratch { start: usize, len: usize },
    Owned(Vec<SpannedNode<'a>>),
}

enum EntryStorage<'a> {
    Scratch { start: usize, len: usize },
    Owned(Vec<Entry<'a>>),
}

impl<'a> EntryStorage<'a> {
    #[inline]
    fn len(&self) -> usize {
        match self {
            Self::Scratch { len, .. } => *len,
            Self::Owned(entries) => entries.len(),
        }
    }

    #[inline]
    fn get_mut<'b>(
        &'b mut self,
        scratch: &'b mut [Entry<'a>],
        index: usize,
    ) -> Option<&'b mut Entry<'a>> {
        match self {
            Self::Scratch { start, len } if index < *len => scratch.get_mut(*start + index),
            Self::Scratch { .. } => None,
            Self::Owned(entries) => entries.get_mut(index),
        }
    }
}

/// Build-time metadata for the first two and last entries of an open mapping, supplying
/// `compact_first_entry_span`, `pair_end`, and parent `content_end` without keeping build-only
/// fields on every [`SpannedNode`].
#[derive(Clone, Copy, Default)]
struct MappingMeta {
    first_key_end: Option<usize>,
    first_val_end: Option<usize>,
    first_anchored: bool,
    second_key_end: Option<usize>,
    last_key_end: Option<usize>,
    last_val_end: Option<usize>,
    any_anchored: bool,
}

pub(crate) struct Builder<'a> {
    scan_cache: LineScanCache<'a>,
    documents: Vec<SpannedNode<'a>>,
    stack: Vec<Frame<'a>>,
    /// Reusable stack for pending mapping keys (1:1 LIFO with open mappings that have a key),
    /// eliminating a `Box<PendingKey>` heap allocation on every mapping entry.
    pending_keys: Vec<PendingKey<'a>>,
    /// Reusable flat stack of seen `(key, entry_index)` pairs for small mappings (`<= SMALL_LIMIT`).
    seen_keys: Vec<(SpannedKey<'a>, usize)>,
    /// Reusable scratch stacks for small sequences and mappings (`<= SCRATCH_LIMIT`).
    scratch_items: Vec<SpannedNode<'a>>,
    scratch_entries: Vec<Entry<'a>>,
    pending_capacity: Option<usize>,
    root: Option<SpannedNode<'a>>,
    /// The last scalar or alias that the source writes was a block scalar. One takes the blank
    /// and more indented lines after it as its own, until other content ends it.
    after_block_scalar: AfterBlockScalar,
    /// Number of open collection frames on `stack` that define an anchor of their own.
    anchored_depth: usize,
    /// The input holds a byte-order mark past the marks that open it. The compatibility reader
    /// counts one at a line's start toward that line's indentation, and takes one after a line
    /// break for a comment's or a blank line's start, unless it opens the input. So what such
    /// a mark means depends on the lines before it, and no entry's removal is known to be
    /// exact.
    marked: bool,
    /// Suppress all `entry_span`s conservatively (for example, when merge-key expansion replays
    /// entries whose source spans point into anchored source mappings).
    suppress_entry_spans: bool,
    /// When `suppress_entry_spans` is active (merge-key expansion), memoizes originating
    /// `missing_value_place` results by `(key_start, key_end)` so sibling and ancestor merge
    /// replays reuse the exact originating span in O(1) without rescanning blank runs.
    merged_missing_places: Option<HashMap<(usize, usize), Option<usize>, GlobalHasher>>,
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

enum KeyTracker<'a> {
    /// Zero-allocation linear scan on `Builder::seen_keys[start..]` for mappings with `<= SMALL_LIMIT` keys.
    Small { start: usize },
    /// Promoted SipHash-1-3 duplicate-key table for mappings exceeding `SMALL_LIMIT` keys or
    /// complex-key mappings that became unresolvable.
    Large(HashMap<SpannedKey<'a>, usize, GlobalHasher>),
    /// Bottom-up resolved mapping for an `in_key` mapping whose entries are all unique and
    /// resolvable so far; entries align 1:1 with `FrameKind::Mapping::entries`.
    Resolved(Box<Mapping>),
}

struct PendingKey<'a> {
    node: SpannedNode<'a>,
    meta: NodeMeta,
    resolved: Option<Box<Value>>,
    after_block_scalar: AfterBlockScalar,
}

enum FrameKind<'a> {
    Sequence {
        items: SeqStorage<'a>,
        last_content_end: Option<usize>,
        any_anchored: bool,
        resolved: Option<Vec<Value>>,
    },
    Mapping {
        entries: EntryStorage<'a>,
        /// Whether this mapping has a pending key waiting on top of `Builder::pending_keys`.
        has_key: bool,
        keys: KeyTracker<'a>,
        meta: MappingMeta,
        /// Cached starting column of this block mapping, computed lazily on the first empty-value
        /// entry instead of rescanning from line start on every entry.
        column: Option<usize>,
        /// A key here has no value of its own to compare: an alias, or a collection that holds
        /// one. It may be any other key of the mapping again, so a lookup would have to guess.
        guess: bool,
    },
}

impl<'a> Builder<'a> {
    pub(crate) fn new(input: &'a str, suppress_entry_spans: bool) -> Self {
        Self {
            scan_cache: LineScanCache::new(input),
            documents: Vec::new(),
            stack: Vec::new(),
            pending_keys: Vec::new(),
            seen_keys: Vec::new(),
            scratch_items: Vec::new(),
            scratch_entries: Vec::new(),
            pending_capacity: None,
            root: None,
            after_block_scalar: AfterBlockScalar::NONE,
            anchored_depth: 0,
            // A run of marks opens the input as one mark does.
            marked: input.trim_start_matches('\u{feff}').contains('\u{feff}'),
            suppress_entry_spans,
            merged_missing_places: suppress_entry_spans.then(|| HashMap::with_hasher(GlobalHasher)),
        }
    }

    pub(crate) fn reserve_collection(&mut self, entries: usize) {
        self.pending_capacity = Some(entries);
    }

    pub(crate) fn finish(self) -> SpannedTree<'a> {
        SpannedTree {
            documents: self.documents,
        }
    }

    /// Whether a new collection at the current position belongs to a mapping key (or is nested
    /// inside one).
    fn in_key_position(&self) -> bool {
        self.stack.last().is_some_and(|frame| {
            frame.in_key || matches!(frame.kind, FrameKind::Mapping { has_key: false, .. })
        })
    }

    /// Whether the current open collection itself tracks bottom-up resolved values.
    fn tracking_resolved(&self) -> bool {
        self.stack.last().is_some_and(|frame| frame.in_key)
    }

    /// Specialized hot-path entry point for untagged, unanchored single-line plain scalars.
    #[inline]
    pub(crate) fn push_plain_scalar(&mut self, value: &'a str, span: Span) {
        self.pending_capacity = None;
        let before = self.after_block_scalar;
        if !self.suppress_entry_spans && !self.marked && !span.is_empty() {
            self.after_block_scalar = AfterBlockScalar::NONE;
        }
        let resolved = self.tracking_resolved().then(|| {
            Box::new(resolve_plain(value).unwrap_or_else(|| Value::String(value.to_owned())))
        });
        let node = SpannedNode {
            span,
            entry_span: PackedOptionSpan::NONE,
            kind: Kind::PlainScalar(value),
        };
        let meta = NodeMeta {
            content_end: (!span.is_empty()).then(|| span.end()),
            anchored: false,
        };
        self.add(node, meta, resolved, before);
    }

    /// Specialized hot-path entry point for general scalars directly from [`crate::parser`].
    #[inline]
    pub(crate) fn push_scalar(
        &mut self,
        value: Cow<'a, str>,
        raw: &'a str,
        style: ScalarStyle,
        tag: Option<Tag>,
        anchored: bool,
        span: Span,
    ) {
        self.push_scalar_inner(value, Cow::Borrowed(raw), style, tag, anchored, span);
    }

    fn push_scalar_inner(
        &mut self,
        value: Cow<'a, str>,
        raw: Cow<'a, str>,
        style: ScalarStyle,
        tag: Option<Tag>,
        anchored: bool,
        span: Span,
    ) {
        self.pending_capacity = None;
        let before = self.after_block_scalar;
        // A scalar with no text isn't written, so it ends no block scalar before it.
        // Skip scanning block-scalar raw headers when entry spans are suppressed or marked.
        if !self.suppress_entry_spans && !self.marked && !span.is_empty() {
            self.after_block_scalar = if matches!(style, ScalarStyle::Literal | ScalarStyle::Folded)
            {
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
        let kind = match (style, tag, value, raw) {
            (ScalarStyle::Plain, None, Cow::Borrowed(v), Cow::Borrowed(r)) if v == r => {
                Kind::PlainScalar(v)
            }
            (style, tag, value, raw) => Kind::Scalar(Box::new(ScalarData {
                value,
                raw,
                style,
                tag,
            })),
        };
        let node = SpannedNode {
            span,
            entry_span: PackedOptionSpan::NONE,
            kind,
        };
        let meta = NodeMeta {
            content_end: (!span.is_empty()).then(|| span.end()),
            anchored,
        };
        self.add(node, meta, resolved, before);
    }

    pub(crate) fn event(&mut self, event: Event<'a>) -> Result<(), Error> {
        match event {
            Event::StreamStart { .. } | Event::StreamEnd { .. } | Event::Directive { .. } => {}
            Event::DocumentStart { .. } => {
                self.root = None;
                self.pending_capacity = None;
                if let Some(cache) = self.merged_missing_places.as_mut() {
                    cache.clear();
                }
            }
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
                self.push_scalar_inner(value, raw, style, tag, anchor.is_some(), span);
            }
            Event::Alias { span, .. } => {
                self.pending_capacity = None;
                let before =
                    std::mem::replace(&mut self.after_block_scalar, AfterBlockScalar::NONE);
                let node = SpannedNode {
                    span,
                    entry_span: PackedOptionSpan::NONE,
                    kind: Kind::Alias,
                };
                let meta = NodeMeta {
                    content_end: Some(span.end()),
                    anchored: false,
                };
                self.add(node, meta, None, before);
            }
            Event::SequenceStart {
                span,
                style,
                anchor,
                ..
            } => {
                let capacity = self.pending_capacity.take().unwrap_or(0);
                let in_key = self.in_key_position();
                let anchored = anchor.is_some();
                if anchored {
                    self.anchored_depth += 1;
                }
                let items = if capacity > SCRATCH_LIMIT {
                    SeqStorage::Owned(Vec::with_capacity(capacity))
                } else {
                    SeqStorage::Scratch {
                        start: self.scratch_items.len(),
                        len: 0,
                    }
                };
                self.stack.push(Frame {
                    open: span,
                    style,
                    anchored,
                    after_block_scalar: self.after_block_scalar,
                    in_key,
                    kind: FrameKind::Sequence {
                        items,
                        last_content_end: None,
                        any_anchored: false,
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
                let capacity = self.pending_capacity.take().unwrap_or(0);
                let in_key = self.in_key_position();
                let anchored = anchor.is_some();
                if anchored {
                    self.anchored_depth += 1;
                }
                let entries = if capacity > SCRATCH_LIMIT {
                    EntryStorage::Owned(Vec::with_capacity(capacity))
                } else {
                    EntryStorage::Scratch {
                        start: self.scratch_entries.len(),
                        len: 0,
                    }
                };
                let keys = if in_key {
                    KeyTracker::Resolved(Box::new(Mapping::new()))
                } else {
                    KeyTracker::Small {
                        start: self.seen_keys.len(),
                    }
                };
                self.stack.push(Frame {
                    open: span,
                    style,
                    anchored,
                    after_block_scalar: self.after_block_scalar,
                    in_key,
                    kind: FrameKind::Mapping {
                        entries,
                        has_key: false,
                        keys,
                        meta: MappingMeta::default(),
                        column: None,
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
        if frame.anchored {
            self.anchored_depth -= 1;
        }
        let FrameKind::Sequence {
            items,
            last_content_end,
            any_anchored,
            resolved,
        } = frame.kind
        else {
            return Err(Error::coded(
                ErrorKind::Parser,
                ErrorCode::MalformedEventStream,
                "sequence end for a mapping",
            ));
        };
        let mut items: Box<[SpannedNode<'a>]> = match items {
            SeqStorage::Scratch { start, len } => {
                if len == 0 {
                    Box::default()
                } else {
                    self.scratch_items.drain(start..).collect()
                }
            }
            SeqStorage::Owned(items) => items.into_boxed_slice(),
        };
        let flow = frame.style == crate::event::CollectionStyle::Flow;
        // Without its only item a collection is no collection: what is left of it reads
        // as an empty value, under whatever tag stood before it.
        if let [only] = items.as_mut() {
            only.entry_span = PackedOptionSpan::NONE;
        }
        let content_end = if !flow && !items.is_empty() {
            last_content_end
        } else {
            Some(span.end())
        };
        let node = SpannedNode {
            span: Span::new(frame.open.start(), span.end()),
            entry_span: PackedOptionSpan::NONE,
            kind: Kind::Sequence(items),
        };
        let meta = NodeMeta {
            content_end,
            anchored: frame.anchored || any_anchored,
        };
        if flow {
            self.after_block_scalar = AfterBlockScalar::NONE;
        }
        let resolved_val = resolved.map(|vals| Box::new(Value::Sequence(vals)));
        self.add(node, meta, resolved_val, frame.after_block_scalar);
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
        if frame.anchored {
            self.anchored_depth -= 1;
        }
        let FrameKind::Mapping {
            entries,
            has_key,
            keys,
            meta: map_meta,
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
        if has_key {
            self.pending_keys.pop();
        }
        // Release the duplicate-key HashMap before shrinking `entries` into a boxed slice so both
        // buffers are not simultaneously live on large flat mappings.
        let resolved_value = match keys {
            KeyTracker::Small { start } => {
                self.seen_keys.truncate(start);
                None
            }
            KeyTracker::Large(seen) => {
                drop(seen);
                None
            }
            KeyTracker::Resolved(map) => (!guess).then(|| Box::new(Value::Mapping(*map))),
        };
        let mut entries: Box<[Entry<'a>]> = match entries {
            EntryStorage::Scratch { start, len } => {
                if len == 0 {
                    Box::default()
                } else {
                    self.scratch_entries.drain(start..).collect()
                }
            }
            EntryStorage::Owned(entries) => entries.into_boxed_slice(),
        };
        let flow = frame.style == crate::event::CollectionStyle::Flow;
        if let [only] = entries.as_mut() {
            only.value.entry_span = PackedOptionSpan::NONE;
        }
        if guess {
            for entry in &mut entries {
                entry.ambiguous = true;
                entry.value.entry_span = PackedOptionSpan::NONE;
            }
        } else if !flow
            && !self.marked
            && !self.suppress_entry_spans
            && !frame.anchored
            && self.anchored_depth == 0
            && entries.len() >= 2
            && !entries[0].ambiguous
            && entries[0].value.entry_span.is_none()
            && !map_meta.first_anchored
        {
            entries[0].value.entry_span = PackedOptionSpan::from_option(compact_first_entry_span(
                self.scan_cache.input,
                &entries[0],
                &entries[1],
                map_meta.first_key_end,
                map_meta.first_val_end,
                map_meta.second_key_end,
            ));
        }
        // A pair in a flow sequence, as in `[a: b]`, is a mapping with no braces. Its
        // end event stands at the token after it, past blanks and a comment, where a
        // mapping with braces has its `}`. The pair ends where what it writes does.
        let braced = self
            .scan_cache
            .input
            .get(span.start()..)
            .is_some_and(|text| text.starts_with('}'));
        let end = if !entries.is_empty() && flow && !braced {
            pair_end(
                self.scan_cache.input,
                frame.open.start(),
                map_meta.last_key_end,
                map_meta.last_val_end,
                span.start(),
            )
        } else {
            span.end()
        };
        let content_end = if !flow && !entries.is_empty() {
            map_meta.last_val_end
        } else {
            Some(end)
        };
        let node = SpannedNode {
            span: Span::new(frame.open.start(), end),
            entry_span: PackedOptionSpan::NONE,
            kind: Kind::Mapping(entries),
        };
        let node_meta = NodeMeta {
            content_end,
            anchored: frame.anchored || map_meta.any_anchored,
        };
        if flow {
            self.after_block_scalar = AfterBlockScalar::NONE;
        }
        self.add(node, node_meta, resolved_value, frame.after_block_scalar);
        Ok(())
    }

    /// Add a finished node to the collection that is open. `after_block_scalar` says whether
    /// the last thing the source wrote before the node was a block scalar.
    fn add(
        &mut self,
        mut node: SpannedNode<'a>,
        mut meta: NodeMeta,
        node_resolved: Option<Box<Value>>,
        after_block_scalar: AfterBlockScalar,
    ) {
        // An alias to a collection reads the entries inside it, so removing one of them changes
        // what the alias stands for.
        let marked = self.marked;
        let aliased = marked || self.suppress_entry_spans || self.anchored_depth > 0;
        let Some(frame) = self.stack.last_mut() else {
            self.root = Some(node);
            return;
        };
        match &mut frame.kind {
            FrameKind::Sequence {
                items,
                last_content_end,
                any_anchored,
                resolved,
            } => {
                if let Some(values) = resolved {
                    if let Some(value) = node_resolved {
                        values.push(*value);
                    } else {
                        *resolved = None;
                    }
                }
                node.entry_span = PackedOptionSpan::from_option(block_entry_span(
                    &mut self.scan_cache,
                    frame.style,
                    Lead::Item,
                    node.span.start(),
                    meta.content_end,
                    aliased || meta.anchored,
                    after_block_scalar,
                ));
                *last_content_end = meta.content_end;
                *any_anchored |= meta.anchored;
                match items {
                    SeqStorage::Scratch { start, len } => {
                        if *len < SCRATCH_LIMIT {
                            self.scratch_items.push(node);
                            *len += 1;
                        } else {
                            let mut owned = Vec::with_capacity(SCRATCH_LIMIT * 2);
                            owned.extend(self.scratch_items.drain(*start..));
                            owned.push(node);
                            *items = SeqStorage::Owned(owned);
                        }
                    }
                    SeqStorage::Owned(vec) => vec.push(node),
                }
            }
            FrameKind::Mapping {
                entries,
                has_key,
                keys,
                meta: map_meta,
                column,
                guess,
            } => {
                if !*has_key {
                    node.entry_span = PackedOptionSpan::NONE;
                    self.pending_keys.push(PendingKey {
                        node,
                        meta,
                        resolved: node_resolved,
                        after_block_scalar,
                    });
                    *has_key = true;
                    return;
                }
                *has_key = false;
                let Some(PendingKey {
                    node: mut key_node,
                    meta: key_meta,
                    resolved: key_resolved,
                    after_block_scalar,
                }) = self.pending_keys.pop()
                else {
                    return;
                };
                key_node.entry_span = PackedOptionSpan::NONE;
                // In an input with a byte-order mark past its start, what a line's indentation
                // is depends on the reader, so the value stays where the parser places it.
                // Under merge-key expansion (`merged_missing_places.is_some()`), memoize each
                // originating `(key_start, key_end)` placement once so sibling and ancestor merge
                // replays reuse the originating span in O(1) without rescanning blank runs.
                if node.is_missing_value(meta.anchored)
                    && !marked
                    && let Some(key_end) = key_meta.content_end
                {
                    let place = if let Some(cache) = self.merged_missing_places.as_mut() {
                        let key_id = (key_node.span.start(), key_end);
                        if let Some(&cached) = cache.get(&key_id) {
                            cached
                        } else if key_end >= frame.open.start() {
                            let computed = missing_value_place(
                                &mut self.scan_cache,
                                frame.open.start(),
                                column,
                                key_end,
                                frame.style,
                            );
                            cache.insert(key_id, computed);
                            computed
                        } else {
                            None
                        }
                    } else {
                        missing_value_place(
                            &mut self.scan_cache,
                            frame.open.start(),
                            column,
                            key_end,
                            frame.style,
                        )
                    };
                    if let Some(at) = place {
                        // The parser places a value that isn't there at the next token. It stands
                        // after its key's `:`, where a value would be written.
                        node.span = Span::empty(at);
                        meta.content_end = Some(at);
                    }
                }
                let span = block_entry_span(
                    &mut self.scan_cache,
                    frame.style,
                    Lead::Key {
                        end: key_meta.content_end,
                    },
                    key_node.span.start(),
                    meta.content_end,
                    aliased || key_meta.anchored || meta.anchored,
                    after_block_scalar,
                );
                let entry_index = entries.len();
                let ambiguous = record_mapping_key(
                    keys,
                    entries,
                    &mut self.seen_keys,
                    &mut self.scratch_entries,
                    guess,
                    MappingKeyInput {
                        in_key: frame.in_key,
                        key_node: &key_node,
                        key_resolved,
                        node_resolved,
                        entry_index,
                    },
                );
                if entry_index == 0 {
                    map_meta.first_key_end = key_meta.content_end;
                    map_meta.first_val_end = meta.content_end;
                    map_meta.first_anchored = key_meta.anchored || meta.anchored;
                } else if entry_index == 1 {
                    map_meta.second_key_end = key_meta.content_end;
                }
                map_meta.last_key_end = key_meta.content_end;
                map_meta.last_val_end = meta.content_end;
                map_meta.any_anchored |= key_meta.anchored || meta.anchored;
                node.entry_span = PackedOptionSpan::from_option(span);
                let entry = Entry {
                    key: key_node,
                    value: node,
                    ambiguous,
                };
                match entries {
                    EntryStorage::Scratch { start, len } => {
                        if *len < SCRATCH_LIMIT {
                            self.scratch_entries.push(entry);
                            *len += 1;
                        } else {
                            let mut owned = Vec::with_capacity(SCRATCH_LIMIT * 2);
                            owned.extend(self.scratch_entries.drain(*start..));
                            owned.push(entry);
                            *entries = EntryStorage::Owned(owned);
                        }
                    }
                    EntryStorage::Owned(vec) => vec.push(entry),
                }
            }
        }
    }
}

struct MappingKeyInput<'a, 'k> {
    in_key: bool,
    key_node: &'k SpannedNode<'a>,
    key_resolved: Option<Box<Value>>,
    node_resolved: Option<Box<Value>>,
    entry_index: usize,
}

#[allow(
    clippy::mutable_key_type,
    reason = "SpannedKey is immutable after insertion; Mapping's cached hash never changes equality"
)]
fn record_mapping_key<'a>(
    keys: &mut KeyTracker<'a>,
    entries: &mut EntryStorage<'a>,
    seen_keys: &mut Vec<(SpannedKey<'a>, usize)>,
    scratch_entries: &mut [Entry<'a>],
    guess: &mut bool,
    input: MappingKeyInput<'a, '_>,
) -> bool {
    let MappingKeyInput {
        in_key,
        key_node,
        key_resolved,
        node_resolved,
        entry_index,
    } = input;
    let mut ambiguous = is_merge_key(key_node);
    match keys {
        KeyTracker::Small { start } => {
            let key_val = if key_node.is_scalar() {
                key_node.spanned_scalar_key()
            } else {
                key_resolved.map(|val| SpannedKey::from_value(*val))
            };
            *guess |= key_val.is_none();
            if let Some(kv) = key_val {
                let slice = &seen_keys[*start..];
                if let Some(&(_, previous)) = slice.iter().find(|(k, _)| k == &kv) {
                    if let Some(entry) = entries.get_mut(scratch_entries, previous) {
                        entry.ambiguous = true;
                    }
                    ambiguous = true;
                } else if slice.len() < SMALL_LIMIT {
                    seen_keys.push((kv, entry_index));
                } else {
                    let mut map = HashMap::with_capacity_and_hasher(SMALL_LIMIT * 2, GlobalHasher);
                    for (k, idx) in seen_keys.drain(*start..) {
                        map.entry(k).or_insert(idx);
                    }
                    map.entry(kv).or_insert(entry_index);
                    *keys = KeyTracker::Large(map);
                }
            }
        }
        KeyTracker::Large(seen) => {
            let key_val = if in_key {
                key_resolved.map(|val| SpannedKey::from_value(*val))
            } else if key_node.is_scalar() {
                key_node.spanned_scalar_key()
            } else {
                key_resolved.map(|val| SpannedKey::from_value(*val))
            };
            *guess |= key_val.is_none();
            if let Some(kv) = key_val {
                if let Some(&previous) = seen.get(&kv) {
                    if let Some(entry) = entries.get_mut(scratch_entries, previous) {
                        entry.ambiguous = true;
                    }
                    ambiguous = true;
                } else {
                    seen.insert(kv, entry_index);
                }
            }
        }
        KeyTracker::Resolved(map) => {
            let key_value = key_resolved.map(|val| *val);
            *guess |= key_value.is_none();
            if let Some(kv) = key_value {
                if let Some(previous) = map.get_index_of(&kv) {
                    if let Some(entry) = entries.get_mut(scratch_entries, previous) {
                        entry.ambiguous = true;
                    }
                    ambiguous = true;
                    let placeholder = KeyTracker::Small {
                        start: seen_keys.len(),
                    };
                    if let KeyTracker::Resolved(existing) = std::mem::replace(keys, placeholder) {
                        let mut seen =
                            HashMap::with_capacity_and_hasher(existing.len(), GlobalHasher);
                        for (idx, (k, _)) in existing.into_iter().enumerate() {
                            seen.insert(SpannedKey::from_value(k), idx);
                        }
                        *keys = KeyTracker::Large(seen);
                    }
                } else if !ambiguous && let Some(vv) = node_resolved {
                    map.insert(kv, *vv);
                } else {
                    let placeholder = KeyTracker::Small {
                        start: seen_keys.len(),
                    };
                    if let KeyTracker::Resolved(existing) = std::mem::replace(keys, placeholder) {
                        let mut seen =
                            HashMap::with_capacity_and_hasher(existing.len() + 1, GlobalHasher);
                        for (idx, (k, _)) in existing.into_iter().enumerate() {
                            seen.insert(SpannedKey::from_value(k), idx);
                        }
                        seen.entry(SpannedKey::from_value(kv))
                            .or_insert(entry_index);
                        *keys = KeyTracker::Large(seen);
                    }
                }
            } else {
                *keys = KeyTracker::Large(HashMap::with_hasher(GlobalHasher));
            }
        }
    }
    ambiguous
}

impl SpannedNode<'_> {
    /// Whether this is the empty scalar that stands for a value the source doesn't write.
    fn is_missing_value(&self, anchored: bool) -> bool {
        self.span.is_empty()
            && !anchored
            && (matches!(&self.kind, Kind::PlainScalar(""))
                || matches!(
                    &self.kind,
                    Kind::Scalar(data)
                        if data.style == ScalarStyle::Plain
                            && data.tag.is_none()
                            && data.raw.is_empty()
                ))
    }
}

#[cfg(test)]
mod tests;
