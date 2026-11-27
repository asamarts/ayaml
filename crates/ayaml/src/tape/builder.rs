//! Streaming flat-array [`Tape`] and [`TapeDocuments`] builder.

use std::borrow::Cow;
use std::hash::BuildHasherDefault;

use crate::composer::Anchors;
use crate::error::{Error, ErrorCode, ErrorKind};
use crate::event::{Anchor, Event, ScalarStyle};
use crate::limits::{AliasBudget, LimitExceeded, LimitKind, Limits};
use crate::number::Number;
use crate::options::ParseOptions;
use crate::resolver::{
    is_null, is_radix_int, parse_bool, parse_float_with, parse_int_with, resolve_plain,
};
use crate::scratch::ParserBuf;
use crate::span::Span;
use crate::tag::Tag;
use crate::typed::PrehashedMap;

use super::{
    FLAG_ALIAS_COPY, FLAG_PLAIN_SCALAR, FLAG_RADIX_INT, LargeStr, NodeTag, OVERFLOW_SPAN_LEN, Tape,
    TapeDocuments, TapeNode, TapeStorage, WideInt, pack_u32_pair, shift_alias_end, tape_key_text,
    tape_node_eq, tape_node_hash,
};

/// Maximum keys per mapping checked linearly on the inline/flat `seen_keys` stack
/// before promoting to a SipHash-1-3 hash table.
const TAPE_SEEN_SMALL_LIMIT: usize = 12;
/// Inline frame stack capacity (0 heap allocs for nesting depth <= 8).
const INLINE_FRAMES: usize = 8;
/// Inline flat seen-key buffer capacity (0 heap allocs for shallow/typical mappings).
const INLINE_SEEN_KEYS: usize = 32;

/// Estimate initial `nodes: Vec<TapeNode>` capacity from input byte length,
/// bounded strictly by `max_events` so hostile limits cannot over-reserve.
pub(crate) const fn initial_node_capacity(input_len: usize, max_events: usize) -> usize {
    if max_events == 0 {
        return 0;
    }
    let est = if input_len <= 128 {
        16
    } else if input_len <= 4_096 {
        let approx = input_len / 10;
        if approx < 32 { 32 } else { approx }
    } else {
        let approx = input_len / 12;
        if approx > 24_576 { 24_576 } else { approx }
    };
    if est > max_events { max_events } else { est }
}

/// Hash bucket entry for promoted large-mapping duplicate detection.
#[derive(Clone, Debug)]
pub(crate) enum TapeKeySlot {
    One(u32),
    Many(Vec<u32>),
}

/// Active promoted duplicate-key table paired with its owning collection depth.
pub(crate) type TapeSeenLarge = (usize, PrehashedMap<TapeKeySlot>);

/// Anchored subtree metadata stored in `Anchors<TapeAnchorDef>`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TapeAnchorDef {
    start: u32,
    end: u32,
    size: usize,
    height: usize,
}

/// Open collection frame while streaming events into the flat tape.
#[derive(Clone, Debug)]
pub(crate) struct TapeFrame {
    is_mapping: bool,
    /// Index of the `Sequence` or `Mapping` node in `nodes`.
    node_idx: u32,
    /// Optional index of the immediately preceding `Tagged` wrapper node.
    tag_node_idx: Option<u32>,
    /// Optional anchor name registered on `SequenceStart`/`MappingStart`.
    anchor: Option<String>,
    /// Opening byte span.
    start_span: Span,
    /// Direct child count (sequence items or completed mapping pairs).
    len: u32,
    /// Mapping only: waiting for the value of the key at `(key_node_idx, key_span)`.
    pending_key: Option<(u32, Span)>,
    /// Start offset into `seen_keys` for small-mapping duplicate checks (`usize::MAX` if promoted).
    seen_start: usize,
    /// Accumulated alias-expansion size estimate.
    size: usize,
    /// Maximum child collection nesting height.
    height: usize,
}

/// Reusable `'static` builder scratch stored inside [`ParserBuf`].
#[derive(Default)]
pub(crate) struct TapeScratch {
    pub(crate) frames_overflow: Vec<TapeFrame>,
    pub(crate) seen_keys_overflow: Vec<u32>,
    pub(crate) seen_large_top: Option<TapeSeenLarge>,
    pub(crate) seen_large_rest: Vec<TapeSeenLarge>,
    pub(crate) anchors: Option<Anchors<TapeAnchorDef>>,
}

impl TapeScratch {
    pub(crate) fn clear_and_clamp(&mut self) {
        self.frames_overflow.clear();
        if self.frames_overflow.capacity() > 128 {
            self.frames_overflow = Vec::new();
        }
        self.seen_keys_overflow.clear();
        if self.seen_keys_overflow.capacity() > 512 {
            self.seen_keys_overflow = Vec::new();
        }
        if let Some((_, mut map)) = self.seen_large_top.take() {
            map.clear();
            if crate::scratch::clamp_retained_capacity(map.capacity(), 512) {
                self.seen_large_top = Some((0, map));
            }
        }
        self.seen_large_rest.clear();
        if self.seen_large_rest.capacity() > 16 {
            self.seen_large_rest = Vec::new();
        }
        if let Some(ref mut anchors) = self.anchors {
            anchors.clear();
            if anchors.capacity() > 256 {
                self.anchors = None;
            }
        }
    }

    pub(crate) fn release_all(&mut self) {
        self.frames_overflow = Vec::new();
        self.seen_keys_overflow = Vec::new();
        self.seen_large_top = None;
        self.seen_large_rest = Vec::new();
        self.anchors = None;
    }
}

/// Optional recycled backing buffers returned via `Tape::recycle_into`.
#[derive(Default)]
pub(crate) struct RecycledTape {
    pub(crate) nodes: Vec<TapeNode>,
    pub(crate) decoded: String,
    pub(crate) tags: Vec<Tag>,
    pub(crate) wide_ints: Vec<WideInt>,
    pub(crate) large_strings: Vec<LargeStr>,
    pub(crate) overflow_spans: Vec<(u32, Span)>,
    pub(crate) roots: Vec<u32>,
}

impl RecycledTape {
    pub(crate) fn accept_storage(&mut self, mut storage: TapeStorage<'_>, mut roots: Vec<u32>) {
        storage.nodes.clear();
        if storage.nodes.capacity() <= 32_768 {
            self.nodes = storage.nodes;
        }
        storage.decoded.clear();
        if storage.decoded.capacity() <= 64 * 1024 {
            self.decoded = storage.decoded;
        }
        storage.tags.clear();
        if storage.tags.capacity() <= 256 {
            self.tags = storage.tags;
        }
        storage.wide_ints.clear();
        if storage.wide_ints.capacity() <= 128 {
            self.wide_ints = storage.wide_ints;
        }
        storage.large_strings.clear();
        if storage.large_strings.capacity() <= 64 {
            self.large_strings = storage.large_strings;
        }
        storage.overflow_spans.clear();
        if storage.overflow_spans.capacity() <= 128 {
            self.overflow_spans = storage.overflow_spans;
        }
        roots.clear();
        if roots.capacity() <= 256 {
            self.roots = roots;
        }
    }

    pub(crate) fn release_all(&mut self) {
        *self = Self::default();
    }
}

/// Inline-plus-overflow stack for open [`TapeFrame`]s (0 heap allocs for depth <= 8).
struct FrameStack {
    small: [Option<TapeFrame>; INLINE_FRAMES],
    small_len: usize,
    overflow: Vec<TapeFrame>,
}

impl FrameStack {
    fn new(overflow: Vec<TapeFrame>) -> Self {
        Self {
            small: [const { None }; INLINE_FRAMES],
            small_len: 0,
            overflow,
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.small_len + self.overflow.len()
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.small_len == 0 && self.overflow.is_empty()
    }

    #[inline]
    fn push(&mut self, frame: TapeFrame) {
        if self.small_len < INLINE_FRAMES {
            self.small[self.small_len] = Some(frame);
            self.small_len += 1;
        } else {
            self.overflow.push(frame);
        }
    }

    #[inline]
    fn pop(&mut self) -> Option<TapeFrame> {
        if let Some(frame) = self.overflow.pop() {
            return Some(frame);
        }
        if self.small_len == 0 {
            return None;
        }
        self.small_len -= 1;
        self.small[self.small_len].take()
    }

    #[inline]
    fn last_mut(&mut self) -> Option<&mut TapeFrame> {
        if let Some(frame) = self.overflow.last_mut() {
            return Some(frame);
        }
        if self.small_len == 0 {
            return None;
        }
        self.small[self.small_len - 1].as_mut()
    }

    fn take_overflow(&mut self) -> Vec<TapeFrame> {
        for slot in &mut self.small[..self.small_len] {
            *slot = None;
        }
        self.small_len = 0;
        self.overflow.clear();
        std::mem::take(&mut self.overflow)
    }
}

/// Inline-plus-overflow flat stack for small-mapping key node indices.
struct SeenKeyStack {
    small: [u32; INLINE_SEEN_KEYS],
    len: usize,
    overflow: Vec<u32>,
}

impl SeenKeyStack {
    fn new(mut overflow: Vec<u32>) -> Self {
        overflow.clear();
        Self {
            small: [0; INLINE_SEEN_KEYS],
            len: 0,
            overflow,
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.len
    }

    #[inline]
    fn push(&mut self, key_idx: u32) {
        if self.len < INLINE_SEEN_KEYS {
            self.small[self.len] = key_idx;
        } else {
            self.overflow.push(key_idx);
        }
        self.len += 1;
    }

    #[inline]
    fn get(&self, index: usize) -> Option<u32> {
        if index >= self.len {
            return None;
        }
        if index < INLINE_SEEN_KEYS {
            Some(self.small[index])
        } else {
            self.overflow.get(index - INLINE_SEEN_KEYS).copied()
        }
    }

    #[inline]
    fn truncate(&mut self, new_len: usize) {
        if new_len < self.len {
            if new_len <= INLINE_SEEN_KEYS {
                self.overflow.clear();
            } else {
                self.overflow.truncate(new_len - INLINE_SEEN_KEYS);
            }
            self.len = new_len;
        }
    }

    fn take_overflow(&mut self) -> Vec<u32> {
        self.len = 0;
        self.overflow.clear();
        std::mem::take(&mut self.overflow)
    }
}

/// Document multiplicity state for [`TapeBuilder`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum DocMode {
    Multi,
    SinglePending,
    SingleDone,
}

/// Streaming builder turning parser events directly into flat-array [`TapeStorage`].
pub(crate) struct TapeBuilder<'a> {
    input: &'a str,
    limits: Limits,
    mode: DocMode,
    document_started: bool,
    has_root: bool,
    storage: TapeStorage<'a>,
    roots: Vec<u32>,
    frames: FrameStack,
    seen_keys: SeenKeyStack,
    seen_large_top: Option<TapeSeenLarge>,
    seen_large_rest: Vec<TapeSeenLarge>,
    anchors: Option<Anchors<TapeAnchorDef>>,
    budget: AliasBudget,
}

impl<'a> TapeBuilder<'a> {
    pub(crate) fn new(
        input: &'a str,
        limits: &Limits,
        single_document: bool,
        budget: AliasBudget,
        scratch: TapeScratch,
        recycled: RecycledTape,
    ) -> Self {
        let mut nodes = recycled.nodes;
        nodes.clear();
        let target_cap = initial_node_capacity(input.len(), limits.max_events);
        if nodes.capacity() < target_cap {
            nodes.reserve_exact(target_cap);
        }
        let mut decoded = recycled.decoded;
        decoded.clear();
        let mut tags = recycled.tags;
        tags.clear();
        let mut wide_ints = recycled.wide_ints;
        wide_ints.clear();
        let mut large_strings = recycled.large_strings;
        large_strings.clear();
        let mut overflow_spans = recycled.overflow_spans;
        overflow_spans.clear();
        let mut roots = recycled.roots;
        roots.clear();

        Self {
            input,
            limits: limits.clone(),
            mode: if single_document {
                DocMode::SinglePending
            } else {
                DocMode::Multi
            },
            document_started: false,
            has_root: false,
            storage: TapeStorage {
                input,
                nodes,
                decoded,
                tags,
                wide_ints,
                large_strings,
                overflow_spans,
            },
            roots,
            frames: FrameStack::new(scratch.frames_overflow),
            seen_keys: SeenKeyStack::new(scratch.seen_keys_overflow),
            seen_large_top: scratch.seen_large_top,
            seen_large_rest: scratch.seen_large_rest,
            anchors: scratch.anchors,
            budget,
        }
    }

    pub(crate) fn take_scratch(&mut self) -> TapeScratch {
        let mut scratch = TapeScratch {
            frames_overflow: self.frames.take_overflow(),
            seen_keys_overflow: self.seen_keys.take_overflow(),
            seen_large_top: self.seen_large_top.take(),
            seen_large_rest: std::mem::take(&mut self.seen_large_rest),
            anchors: self.anchors.take(),
        };
        scratch.clear_and_clamp();
        scratch
    }

    pub(crate) fn reserve_collection(&mut self, entries: usize) {
        let bounded = entries.min(self.limits.max_collection_entries);
        // Reserve room for `bounded` direct scalar children (avoiding reallocation on flat flow
        // roots without over-reserving past `max_events`).
        let want = bounded.min(self.limits.max_events);
        if self
            .storage
            .nodes
            .capacity()
            .saturating_sub(self.storage.nodes.len())
            < want
        {
            let _ = self.storage.nodes.try_reserve(want);
        }
    }

    #[inline]
    fn has_anchors(&self) -> bool {
        self.anchors.as_ref().is_some_and(|a| !a.is_empty())
    }

    #[inline]
    fn anchors_mut(&mut self) -> &mut Anchors<TapeAnchorDef> {
        self.anchors.get_or_insert_with(Anchors::new)
    }

    #[inline]
    fn checked_node_index(index: usize, span: Span) -> Result<u32, Box<Error>> {
        u32::try_from(index).map_err(|_| {
            Box::new(
                Error::from(LimitExceeded::new(
                    LimitKind::Events,
                    u64::from(u32::MAX),
                    u64::try_from(index).unwrap_or(u64::MAX),
                ))
                .with_span(span),
            )
        })
    }

    #[inline]
    fn encode_span_parts(&mut self, node_idx: u32, span: Span) -> (u32, u32) {
        let start = span.start();
        let len = span.len();
        if let Ok(start_u32) = u32::try_from(start)
            && let Ok(len_u32) = u32::try_from(len)
            && len_u32 < OVERFLOW_SPAN_LEN
        {
            return (start_u32, len_u32);
        }
        self.record_overflow_span(node_idx, span);
        (u32::MAX, OVERFLOW_SPAN_LEN)
    }

    fn record_overflow_span(&mut self, node_idx: u32, span: Span) {
        if let Some(last) = self.storage.overflow_spans.last_mut() {
            if last.0 == node_idx {
                last.1 = span;
                return;
            }
            if last.0 < node_idx {
                self.storage.overflow_spans.push((node_idx, span));
                return;
            }
        }
        match self
            .storage
            .overflow_spans
            .binary_search_by_key(&node_idx, |(idx, _)| *idx)
        {
            Ok(pos) => self.storage.overflow_spans[pos].1 = span,
            Err(pos) => self.storage.overflow_spans.insert(pos, (node_idx, span)),
        }
    }

    #[inline]
    fn push_node(
        &mut self,
        span: Span,
        tag: NodeTag,
        flags: u8,
        payload: u64,
    ) -> Result<u32, Box<Error>> {
        let idx = Self::checked_node_index(self.storage.nodes.len(), span)?;
        let (span_start, span_len_24) = self.encode_span_parts(idx, span);
        if self.storage.nodes.len() == self.storage.nodes.capacity() {
            let cap = self.storage.nodes.capacity();
            if cap >= 4_096 {
                // Grow by 1.5x instead of 2x past 4096 nodes to keep realloc peaks well below `Value`.
                self.storage.nodes.reserve_exact(cap / 2);
            }
        }
        self.storage
            .nodes
            .push(TapeNode::new(span_start, span_len_24, tag, flags, payload));
        Ok(idx)
    }

    #[inline]
    fn push_number_node(
        &mut self,
        span: Span,
        number: Number,
        flags: u8,
    ) -> Result<u32, Box<Error>> {
        if number.is_f64() {
            let bits = number.as_f64().unwrap_or(f64::NAN).to_bits();
            self.push_node(span, NodeTag::Float, flags, bits)
        } else if let Some(u) = number.as_u64() {
            self.push_node(span, NodeTag::PosInt, flags, u)
        } else if let Some(i) = number.as_i64() {
            self.push_node(span, NodeTag::NegInt, flags, i.cast_unsigned())
        } else if let Some(wide) = WideInt::from_number(number) {
            let wide_idx = self.storage.wide_ints.len();
            self.storage.wide_ints.push(wide);
            self.push_node(
                span,
                NodeTag::WideInt,
                flags,
                u64::try_from(wide_idx).unwrap_or(u64::MAX),
            )
        } else {
            self.push_node(span, NodeTag::PosInt, flags, 0)
        }
    }

    /// Push a string scalar node, borrowing zero-copy from `self.input` whenever `slice_span`
    /// matches `text`, or appending into the single `self.storage.decoded` bump buffer otherwise.
    #[inline]
    fn push_str_node(
        &mut self,
        span: Span,
        text: &str,
        borrowed_span: Option<Span>,
        flags: u8,
    ) -> Result<u32, Box<Error>> {
        if let Some(slice) = borrowed_span {
            let start = slice.start();
            let len = slice.len();
            if let (Ok(s32), Ok(l32)) = (u32::try_from(start), u32::try_from(len)) {
                return self.push_node(span, NodeTag::BorrowedStr, flags, pack_u32_pair(s32, l32));
            }
            let large_idx = self.storage.large_strings.len();
            self.storage
                .large_strings
                .push(LargeStr::Borrowed { start, len });
            return self.push_node(
                span,
                NodeTag::LargeStr,
                flags,
                u64::try_from(large_idx).unwrap_or(u64::MAX),
            );
        }
        let start = self.storage.decoded.len();
        self.storage.decoded.push_str(text);
        let len = text.len();
        if let (Ok(s32), Ok(l32)) = (u32::try_from(start), u32::try_from(len)) {
            return self.push_node(span, NodeTag::DecodedStr, flags, pack_u32_pair(s32, l32));
        }
        let large_idx = self.storage.large_strings.len();
        self.storage
            .large_strings
            .push(LargeStr::Decoded { start, len });
        self.push_node(
            span,
            NodeTag::LargeStr,
            flags,
            u64::try_from(large_idx).unwrap_or(u64::MAX),
        )
    }

    fn push_tag_wrapper(&mut self, span: Span, tag: Tag) -> Result<u32, Box<Error>> {
        let tag_idx = Self::checked_node_index(self.storage.tags.len(), span)?;
        self.storage.tags.push(tag);
        // `end` will be patched once the wrapped payload completes.
        self.push_node(span, NodeTag::Tagged, 0, pack_u32_pair(tag_idx, 0))
    }

    /// Resolve an untagged or tagged scalar directly into `self.storage.nodes` without
    /// constructing any intermediate `Value::String`.
    fn push_resolved_scalar(
        &mut self,
        text: &str,
        borrowed_span: Option<Span>,
        style: ScalarStyle,
        tag: Option<Tag>,
        span: Span,
    ) -> Result<(u32, usize), Box<Error>> {
        let node_cost = self.limits.node_cost_bytes;
        let track_size = self.has_anchors();

        if let Some(tag) = tag {
            if tag.is_core() {
                let idx = self.push_explicit_core_scalar(text, borrowed_span, style, tag, span)?;
                let size = if track_size {
                    self.scalar_size(idx, node_cost)
                } else {
                    0
                };
                return Ok((idx, size));
            }
            let tag_text_len = tag.text_len();
            let non_specific = tag.is_non_specific();
            let tag_node = self.push_tag_wrapper(span, tag)?;
            let payload_idx = if non_specific {
                self.push_str_node(span, text, borrowed_span, 0)?
            } else {
                self.push_untagged_scalar(text, borrowed_span, style, span)?
            };
            let end = Self::checked_node_index(self.storage.nodes.len(), span)?;
            let (tag_idx, _) = self.storage.nodes[tag_node as usize].payload_pair();
            self.storage.nodes[tag_node as usize].set_payload_pair(tag_idx, end);
            let size = if track_size {
                node_cost
                    .saturating_add(tag_text_len)
                    .saturating_add(self.scalar_size(payload_idx, node_cost))
            } else {
                0
            };
            return Ok((tag_node, size));
        }

        let idx = self.push_untagged_scalar(text, borrowed_span, style, span)?;
        let size = if track_size {
            self.scalar_size(idx, node_cost)
        } else {
            0
        };
        Ok((idx, size))
    }

    #[inline]
    fn push_untagged_scalar(
        &mut self,
        text: &str,
        borrowed_span: Option<Span>,
        style: ScalarStyle,
        span: Span,
    ) -> Result<u32, Box<Error>> {
        if style != ScalarStyle::Plain {
            return self.push_str_node(span, text, borrowed_span, 0);
        }
        if text.is_empty() {
            return self.push_node(span, NodeTag::EmptyNull, FLAG_PLAIN_SCALAR, 0);
        }
        if let Some(value) = resolve_plain(text) {
            return match value {
                crate::value::Value::Null => {
                    self.push_node(span, NodeTag::ExplicitNull, FLAG_PLAIN_SCALAR, 0)
                }
                crate::value::Value::Bool(b) => {
                    self.push_node(span, NodeTag::Bool, FLAG_PLAIN_SCALAR, u64::from(b))
                }
                crate::value::Value::Number(num) => {
                    let flags = if is_radix_int(text) {
                        FLAG_PLAIN_SCALAR | FLAG_RADIX_INT
                    } else {
                        FLAG_PLAIN_SCALAR
                    };
                    self.push_number_node(span, num, flags)
                }
                _ => self.push_str_node(span, text, borrowed_span, FLAG_PLAIN_SCALAR),
            };
        }
        self.push_str_node(span, text, borrowed_span, FLAG_PLAIN_SCALAR)
    }

    fn push_explicit_core_scalar(
        &mut self,
        text: &str,
        borrowed_span: Option<Span>,
        style: ScalarStyle,
        tag: Tag,
        span: Span,
    ) -> Result<u32, Box<Error>> {
        match tag.suffix() {
            "str" => self.push_str_node(span, text, borrowed_span, 0),
            "int" => {
                let num = parse_int_with(text, true).ok_or_else(|| {
                    Box::new(
                        Error::coded(
                            ErrorKind::Resolution,
                            ErrorCode::InvalidIntScalar,
                            "invalid !!int scalar",
                        )
                        .with_span(span),
                    )
                })?;
                let flags = if style == ScalarStyle::Plain && is_radix_int(text) {
                    FLAG_RADIX_INT
                } else {
                    0
                };
                self.push_number_node(span, num, flags)
            }
            "float" => {
                let num = parse_float_with(text, true).ok_or_else(|| {
                    Box::new(
                        Error::coded(
                            ErrorKind::Resolution,
                            ErrorCode::InvalidFloatScalar,
                            "invalid !!float scalar",
                        )
                        .with_span(span),
                    )
                })?;
                self.push_number_node(span, num, 0)
            }
            "bool" => {
                let b = parse_bool(text).ok_or_else(|| {
                    Box::new(
                        Error::coded(
                            ErrorKind::Resolution,
                            ErrorCode::InvalidBoolScalar,
                            "invalid !!bool scalar",
                        )
                        .with_span(span),
                    )
                })?;
                self.push_node(span, NodeTag::Bool, 0, u64::from(b))
            }
            "null" => {
                if !is_null(text) {
                    return Err(Box::new(
                        Error::coded(
                            ErrorKind::Resolution,
                            ErrorCode::InvalidNullScalar,
                            "invalid !!null scalar",
                        )
                        .with_span(span),
                    ));
                }
                let (node_tag, flags) = if text.is_empty() {
                    (NodeTag::EmptyNull, FLAG_PLAIN_SCALAR)
                } else {
                    (NodeTag::ExplicitNull, 0)
                };
                self.push_node(span, node_tag, flags, 0)
            }
            // Unknown core tags (e.g. `!!timestamp`) wrap a string payload, matching `resolver.rs`.
            _ => {
                let tag_node = self.push_tag_wrapper(span, tag)?;
                let _ = self.push_str_node(span, text, borrowed_span, 0)?;
                let end = Self::checked_node_index(self.storage.nodes.len(), span)?;
                let (tag_idx, _) = self.storage.nodes[tag_node as usize].payload_pair();
                self.storage.nodes[tag_node as usize].set_payload_pair(tag_idx, end);
                Ok(tag_node)
            }
        }
    }

    #[inline]
    fn scalar_size(&self, idx: u32, node_cost: usize) -> usize {
        let node = self.storage.node(idx as usize);
        match node.tag() {
            NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr => {
                node_cost.saturating_add(self.storage.str_at(idx as usize).map_or(0, str::len))
            }
            NodeTag::Tagged => {
                let tag_len = self.storage.tag_at(idx as usize).map_or(0, Tag::text_len);
                node_cost
                    .saturating_add(tag_len)
                    .saturating_add(self.scalar_size(idx + 1, node_cost))
            }
            _ => node_cost,
        }
    }

    /// Determine whether `text` matches a direct slice of `self.input` without pointer arithmetic.
    #[inline]
    fn borrowed_slice_span(
        &self,
        text: &str,
        is_borrowed: bool,
        style: ScalarStyle,
        span: Span,
    ) -> Option<Span> {
        if !is_borrowed {
            return None;
        }
        if text.is_empty() {
            return Some(Span::empty(span.start().min(self.input.len())));
        }
        // Check exact span first (plain scalars), trimmed plain before comments, then quote-stripped.
        if let Some(raw) = span.slice(self.input) {
            if raw == text {
                return Some(span);
            }
            if style == ScalarStyle::Plain {
                let trimmed = raw.trim_end_matches([' ', '\t']);
                if trimmed == text {
                    return Some(Span::new(span.start(), span.start() + trimmed.len()));
                }
            }
            if matches!(style, ScalarStyle::SingleQuoted | ScalarStyle::DoubleQuoted)
                && span.len() >= 2
            {
                let inner = Span::new(span.start() + 1, span.end() - 1);
                if inner.slice(self.input) == Some(text) {
                    return Some(inner);
                }
            }
        }
        None
    }

    /// Hot-path callback for untagged, unanchored plain scalars directly from `Parser`.
    #[inline]
    pub(crate) fn plain_scalar(&mut self, value: &'a str, span: Span) -> Result<(), Box<Error>> {
        let borrowed_span = if value.is_empty() {
            Some(Span::empty(span.start().min(self.input.len())))
        } else {
            Some(span)
        };
        let node_idx = self.push_untagged_scalar(value, borrowed_span, ScalarStyle::Plain, span)?;
        let size = if self.has_anchors() {
            self.scalar_size(node_idx, self.limits.node_cost_bytes)
        } else {
            0
        };
        self.feed(node_idx, span, size, 0)
    }

    /// Hot-path callback for any scalar directly from `Parser`.
    #[inline]
    pub(crate) fn scalar(
        &mut self,
        text: &str,
        is_borrowed: bool,
        style: ScalarStyle,
        tag: Option<Tag>,
        anchor: Option<&Anchor<'a>>,
        span: Span,
    ) -> Result<(), Box<Error>> {
        let borrowed_span = self.borrowed_slice_span(text, is_borrowed, style, span);
        let (node_idx, size) = self.push_resolved_scalar(text, borrowed_span, style, tag, span)?;
        let size = if anchor.is_some() && size == 0 {
            self.scalar_size(node_idx, self.limits.node_cost_bytes)
        } else {
            size
        };
        if let Some(anchor) = anchor {
            let end = Self::checked_node_index(self.storage.nodes.len(), span)?;
            let def = TapeAnchorDef {
                start: node_idx,
                end,
                size,
                height: 0,
            };
            self.anchors_mut().define(anchor.as_str(), def);
        }
        self.feed(node_idx, span, size, 0)
    }

    /// Process any parser [`Event`].
    pub(crate) fn event(&mut self, event: Event<'a>) -> Result<(), Box<Error>> {
        match event {
            Event::StreamStart { .. } | Event::StreamEnd { .. } | Event::Directive { .. } => {}
            Event::DocumentStart { span, .. } => {
                if !self.frames.is_empty() {
                    return Err(Box::new(Error::new(
                        ErrorKind::Composition,
                        "document started inside a collection",
                    )));
                }
                if self.mode == DocMode::SingleDone {
                    return Err(Box::new(
                        Error::coded(
                            ErrorKind::Parser,
                            ErrorCode::TrailingDocuments,
                            "expected a single YAML document, but found trailing documents",
                        )
                        .with_span(span),
                    ));
                }
                if let Some(ref mut anchors) = self.anchors {
                    anchors.clear();
                }
                self.document_started = true;
                self.has_root = false;
            }
            Event::DocumentEnd { span, .. } => {
                if !self.frames.is_empty() {
                    return Err(self.error_at(span, "document ended inside a collection"));
                }
                if !self.has_root {
                    let root_idx = self.push_node(span, NodeTag::EmptyNull, 0, 0)?;
                    if self.mode == DocMode::Multi {
                        self.roots.push(root_idx);
                    }
                }
                if self.mode == DocMode::SinglePending {
                    self.mode = DocMode::SingleDone;
                }
                self.document_started = false;
                self.has_root = false;
            }
            Event::Scalar {
                value,
                style,
                tag,
                anchor,
                span,
                ..
            } => {
                return self.scalar(
                    value.as_ref(),
                    matches!(value, Cow::Borrowed(_)),
                    style,
                    tag,
                    anchor.as_ref(),
                    span,
                );
            }
            Event::Alias { anchor, span } => {
                self.handle_alias(anchor.as_str(), span)?;
            }
            Event::SequenceStart {
                tag, anchor, span, ..
            } => {
                self.start_collection(false, tag, anchor, span)?;
            }
            Event::SequenceEnd { span } => {
                self.finish_collection(false, span)?;
            }
            Event::MappingStart {
                tag, anchor, span, ..
            } => {
                self.start_collection(true, tag, anchor, span)?;
            }
            Event::MappingEnd { span } => {
                self.finish_collection(true, span)?;
            }
        }
        Ok(())
    }

    fn start_collection(
        &mut self,
        is_mapping: bool,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        span: Span,
    ) -> Result<(), Box<Error>> {
        self.limits
            .check_depth(self.frames.len() + 1)
            .map_err(|error| Box::new(Error::from(error).with_span(span)))?;
        let anchor_name = if let Some(anchor) = anchor {
            let owned = anchor.as_str().to_owned();
            self.anchors_mut().open(&owned);
            Some(owned)
        } else {
            None
        };
        let keep_tag = tag.filter(|t| !t.is_core_collection() && !t.is_core_scalar());
        let tag_node_idx = if let Some(tag) = keep_tag {
            Some(self.push_tag_wrapper(span, tag)?)
        } else {
            None
        };
        let node_tag = if is_mapping {
            NodeTag::Mapping
        } else {
            NodeTag::Sequence
        };
        let node_idx = self.push_node(span, node_tag, 0, 0)?;
        let seen_start = if is_mapping { self.seen_keys.len() } else { 0 };
        self.frames.push(TapeFrame {
            is_mapping,
            node_idx,
            tag_node_idx,
            anchor: anchor_name,
            start_span: span,
            len: 0,
            pending_key: None,
            seen_start,
            size: self.limits.node_cost_bytes,
            height: 0,
        });
        Ok(())
    }

    fn finish_collection(&mut self, is_mapping: bool, end_span: Span) -> Result<(), Box<Error>> {
        let depth = self.frames.len();
        let Some(frame) = self.frames.pop() else {
            let msg = if is_mapping {
                "unexpected mapping end"
            } else {
                "unexpected sequence end"
            };
            return Err(self.error_at(end_span, msg));
        };
        if frame.is_mapping != is_mapping {
            let msg = if is_mapping {
                "unexpected mapping end"
            } else {
                "unexpected sequence end"
            };
            return Err(self.error_at(end_span, msg));
        }
        if let Some((key_idx, key_span)) = frame.pending_key {
            let key_desc = tape_key_text(&self.storage, key_idx as usize);
            return Err(Box::new(
                Error::new(
                    ErrorKind::Composition,
                    format!("mapping key without value: {key_desc}"),
                )
                .with_span(key_span),
            ));
        }

        // Release duplicate-key tracking for this mapping.
        if is_mapping {
            if frame.seen_start == usize::MAX {
                if let Some((d, _)) = &self.seen_large_top
                    && *d == depth
                    && let Some((_, mut map)) = self.seen_large_top.take()
                {
                    map.clear();
                    if let Some(prev) = self.seen_large_rest.pop() {
                        self.seen_large_top = Some(prev);
                    } else if map.capacity() <= 512 {
                        // Keep one warm empty table (depth 0 sentinel) for future large maps.
                        self.seen_large_top = Some((0, map));
                    }
                }
            } else {
                self.seen_keys.truncate(frame.seen_start);
            }
        }

        let end = Self::checked_node_index(self.storage.nodes.len(), end_span)?;
        let full_span = Span::new(
            frame.start_span.start(),
            end_span.end().max(frame.start_span.end()),
        );
        let (span_start, span_len_24) = self.encode_span_parts(frame.node_idx, full_span);
        let coll = &mut self.storage.nodes[frame.node_idx as usize];
        *coll = coll.with_span_parts(span_start, span_len_24);
        coll.set_payload_pair(frame.len, end);

        let (root_idx, size) = if let Some(tag_node_idx) = frame.tag_node_idx {
            let (ts, tl) = self.encode_span_parts(tag_node_idx, full_span);
            let tag_node = &mut self.storage.nodes[tag_node_idx as usize];
            *tag_node = tag_node.with_span_parts(ts, tl);
            let (tag_idx, _) = tag_node.payload_pair();
            tag_node.set_payload_pair(tag_idx, end);
            let tag_text_len = self
                .storage
                .tags
                .get(tag_idx as usize)
                .map_or(0, Tag::text_len);
            let total_size = frame
                .size
                .saturating_add(self.limits.node_cost_bytes)
                .saturating_add(tag_text_len);
            (tag_node_idx, total_size)
        } else {
            (frame.node_idx, frame.size)
        };

        let height = frame.height + 1;
        if let Some(anchor) = frame.anchor {
            let def = TapeAnchorDef {
                start: root_idx,
                end,
                size,
                height,
            };
            self.anchors_mut().close(&anchor, def);
        }
        self.feed(root_idx, frame.start_span, size, height)
    }

    fn handle_alias(&mut self, anchor: &str, span: Span) -> Result<(), Box<Error>> {
        let def = match self.anchors.as_ref() {
            Some(anchors) => *anchors
                .resolve(anchor)
                .map_err(|(code, msg)| Box::new((*self.error_at(span, msg)).with_code(code)))?,
            None => {
                return Err(Box::new(
                    (*self.error_at(span, "unknown anchor")).with_code(ErrorCode::UnknownAnchor),
                ));
            }
        };
        // Charge alias budget and depth BEFORE copying nodes into the flat tape.
        self.budget
            .charge(&self.limits, self.input.len(), def.size)
            .map_err(|error| Box::new(error.with_span(span)))?;
        if def.height > 0 {
            self.limits
                .check_depth(self.frames.len() + def.height)
                .map_err(|error| Box::new(Error::from(error).with_span(span)))?;
        }

        let dst_start = Self::checked_node_index(self.storage.nodes.len(), span)?;
        let count = (def.end - def.start) as usize;
        let delta = dst_start.checked_sub(def.start).ok_or_else(|| {
            Box::new(Error::new(
                ErrorKind::Composition,
                "invalid alias subtree offset",
            ))
        })?;
        // Verify final index fits in `u32`.
        let _ = shift_alias_end(def.end, delta).ok_or_else(|| {
            Box::new(
                Error::from(LimitExceeded::new(
                    LimitKind::Events,
                    u64::from(u32::MAX),
                    u64::from(def.end) + u64::from(delta),
                ))
                .with_span(span),
            )
        })?;

        self.storage.nodes.try_reserve(count).map_err(|_| {
            Box::new(
                Error::from(LimitExceeded::new(
                    LimitKind::Events,
                    u64::try_from(self.limits.max_events).unwrap_or(u64::MAX),
                    u64::try_from(self.storage.nodes.len().saturating_add(count))
                        .unwrap_or(u64::MAX),
                ))
                .with_span(span),
            )
        })?;

        // Copy matching overflow spans for interior nodes (skipping root, which takes alias span).
        if !self.storage.overflow_spans.is_empty() {
            let start_pos = self
                .storage
                .overflow_spans
                .partition_point(|(idx, _)| *idx <= def.start);
            let end_pos = self
                .storage
                .overflow_spans
                .partition_point(|(idx, _)| *idx < def.end);
            if start_pos < end_pos {
                let mut extra = Vec::with_capacity(end_pos - start_pos);
                for &(orig_idx, orig_span) in &self.storage.overflow_spans[start_pos..end_pos] {
                    if let Some(shifted) = shift_alias_end(orig_idx, delta) {
                        extra.push((shifted, orig_span));
                    }
                }
                self.storage.overflow_spans.extend(extra);
            }
        }

        // Linear flat-array copy with O(1) per-node container `end` pointer adjustment.
        let src_start = def.start as usize;
        let src_end = def.end as usize;
        for i in src_start..src_end {
            let mut copied = self.storage.nodes[i];
            if copied.is_container() {
                let (lo, hi) = copied.payload_pair();
                let shifted_end = shift_alias_end(hi, delta).ok_or_else(|| {
                    Box::new(
                        Error::from(LimitExceeded::new(
                            LimitKind::Events,
                            u64::from(u32::MAX),
                            u64::from(hi) + u64::from(delta),
                        ))
                        .with_span(span),
                    )
                })?;
                copied.set_payload_pair(lo, shifted_end);
            }
            self.storage.nodes.push(copied);
        }

        // Set the copied root node's span to the alias use-site span and mark `FLAG_ALIAS_COPY`.
        let (span_start, span_len_24) = self.encode_span_parts(dst_start, span);
        let root_node = &mut self.storage.nodes[dst_start as usize];
        *root_node = root_node
            .with_span_parts(span_start, span_len_24)
            .with_added_flag(FLAG_ALIAS_COPY);

        self.feed(dst_start, span, def.size, def.height)
    }

    fn feed(
        &mut self,
        node_idx: u32,
        span: Span,
        size: usize,
        height: usize,
    ) -> Result<(), Box<Error>> {
        let depth = self.frames.len();
        let Some(frame) = self.frames.last_mut() else {
            if self.has_root {
                return Err(self.error_at(span, "multiple root nodes in one document"));
            }
            self.has_root = true;
            if self.mode == DocMode::Multi {
                self.roots.push(node_idx);
            }
            return Ok(());
        };
        frame.size = frame.size.saturating_add(size);
        frame.height = frame.height.max(height);

        if !frame.is_mapping {
            frame.len = frame.len.saturating_add(1);
            return Ok(());
        }

        if let Some((key_idx, key_span)) = frame.pending_key.take() {
            let seen_start = frame.seen_start;
            let current_len = frame.len as usize;
            frame.len = frame.len.saturating_add(1);

            if seen_start == usize::MAX {
                self.insert_large_key(depth, key_idx, key_span)?;
            } else if current_len < TAPE_SEEN_SMALL_LIMIT {
                let end = self.seen_keys.len();
                for pos in seen_start..end {
                    if let Some(existing) = self.seen_keys.get(pos)
                        && tape_node_eq(&self.storage, existing as usize, key_idx as usize)
                    {
                        return Err(self.duplicate_key_error(existing, key_span));
                    }
                }
                self.seen_keys.push(key_idx);
            } else {
                // Promote this mapping from linear scan to SipHash-1-3 hash table.
                self.promote_seen_keys(depth, seen_start, key_idx, key_span)?;
                if let Some(frame) = self.frames.last_mut() {
                    frame.seen_start = usize::MAX;
                }
            }
        } else {
            frame.pending_key = Some((node_idx, span));
        }
        Ok(())
    }

    #[cold]
    #[inline(never)]
    fn promote_seen_keys(
        &mut self,
        depth: usize,
        seen_start: usize,
        new_key: u32,
        key_span: Span,
    ) -> Result<(), Box<Error>> {
        let mut table = match self.seen_large_top.take() {
            Some((0, mut warm)) => {
                warm.clear();
                warm
            }
            Some(active) => {
                self.seen_large_rest.push(active);
                PrehashedMap::with_capacity_and_hasher(
                    TAPE_SEEN_SMALL_LIMIT * 2,
                    BuildHasherDefault::default(),
                )
            }
            None => PrehashedMap::with_capacity_and_hasher(
                TAPE_SEEN_SMALL_LIMIT * 2,
                BuildHasherDefault::default(),
            ),
        };
        let end = self.seen_keys.len();
        for pos in seen_start..end {
            if let Some(existing) = self.seen_keys.get(pos) {
                let hash = tape_node_hash(&self.storage, existing as usize);
                match table.get_mut(&hash) {
                    None => {
                        table.insert(hash, TapeKeySlot::One(existing));
                    }
                    Some(TapeKeySlot::One(first)) => {
                        let first = *first;
                        table.insert(hash, TapeKeySlot::Many(vec![first, existing]));
                    }
                    Some(TapeKeySlot::Many(list)) => {
                        list.push(existing);
                    }
                }
            }
        }
        self.seen_keys.truncate(seen_start);
        self.seen_large_top = Some((depth, table));
        self.insert_large_key(depth, new_key, key_span)
    }

    fn insert_large_key(
        &mut self,
        depth: usize,
        key_idx: u32,
        key_span: Span,
    ) -> Result<(), Box<Error>> {
        let hash = tape_node_hash(&self.storage, key_idx as usize);
        let duplicate = {
            let Some((active_depth, table)) = self.seen_large_top.as_mut() else {
                return Ok(());
            };
            debug_assert_eq!(*active_depth, depth);
            let _ = active_depth;
            match table.get_mut(&hash) {
                None => {
                    table.insert(hash, TapeKeySlot::One(key_idx));
                    None
                }
                Some(TapeKeySlot::One(existing)) => {
                    let existing = *existing;
                    if tape_node_eq(&self.storage, existing as usize, key_idx as usize) {
                        Some(existing)
                    } else {
                        table.insert(hash, TapeKeySlot::Many(vec![existing, key_idx]));
                        None
                    }
                }
                Some(TapeKeySlot::Many(list)) => {
                    let mut dup = None;
                    for &existing in list.iter() {
                        if tape_node_eq(&self.storage, existing as usize, key_idx as usize) {
                            dup = Some(existing);
                            break;
                        }
                    }
                    if dup.is_none() {
                        list.push(key_idx);
                    }
                    dup
                }
            }
        };
        if let Some(existing) = duplicate {
            return Err(self.duplicate_key_error(existing, key_span));
        }
        Ok(())
    }

    #[cold]
    #[inline(never)]
    #[allow(
        clippy::unnecessary_box_returns,
        reason = "hot-path callers propagate Box<Error> to keep Result<_, Box<Error>> pointer-sized"
    )]
    fn duplicate_key_error(&self, existing: u32, key_span: Span) -> Box<Error> {
        Box::new(
            Error::new(
                ErrorKind::DuplicateKey,
                format!(
                    "duplicate mapping key: {}",
                    tape_key_text(&self.storage, existing as usize)
                ),
            )
            .with_span(key_span),
        )
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

    pub(crate) fn finish_single(mut self) -> (Result<Tape<'a>, Error>, TapeScratch) {
        let unterminated = !self.frames.is_empty();
        let scratch = self.take_scratch();
        if unterminated {
            return (
                Err(Error::new(
                    ErrorKind::Composition,
                    "unterminated collection at end of stream",
                )),
                scratch,
            );
        }
        if self.storage.nodes.is_empty()
            && let Err(error) =
                self.push_node(Span::empty(self.input.len()), NodeTag::EmptyNull, 0, 0)
        {
            return (Err(*error), scratch);
        }
        (
            Ok(Tape {
                storage: self.storage,
            }),
            scratch,
        )
    }

    pub(crate) fn finish_documents(mut self) -> (Result<TapeDocuments<'a>, Error>, TapeScratch) {
        let unterminated = !self.frames.is_empty();
        let scratch = self.take_scratch();
        if unterminated {
            return (
                Err(Error::new(
                    ErrorKind::Composition,
                    "unterminated collection at end of stream",
                )),
                scratch,
            );
        }
        (
            Ok(TapeDocuments {
                storage: self.storage,
                roots: self.roots,
            }),
            scratch,
        )
    }
}

/// Parse a single document into [`Tape<'a>`], optionally reusing `buf`.
pub(crate) fn parse_tape<'a>(
    input: &'a str,
    options: &ParseOptions,
    buf: Option<&mut ParserBuf>,
) -> Result<Tape<'a>, Error> {
    options.limits.validate()?;
    if options.merge_keys {
        let (events, budget) =
            crate::merge::events::expand_single_document(input, &options.limits)?;
        let (scratch, recycled, buf_slot) = match buf {
            Some(b) => (b.take_tape_scratch(), b.take_recycled_tape(), Some(b)),
            None => (TapeScratch::default(), RecycledTape::default(), None),
        };
        let mut guard = TapeScratchGuard {
            builder: Some(TapeBuilder::new(
                input,
                &options.limits,
                true,
                budget,
                scratch,
                recycled,
            )),
            buf: buf_slot,
        };
        for event in events {
            if let Some(builder) = guard.builder.as_mut() {
                builder
                    .event(event)
                    .map_err(|e| (*e).with_input_location(input, false))?;
            }
        }
        let tape = guard
            .finish_single()
            .map_err(|e| e.with_input_location(input, false))?;
        return Ok(tape);
    }
    crate::parser::parse_tape(input, &options.limits, buf)
}

/// Parse all documents in a stream into [`TapeDocuments<'a>`], optionally reusing `buf`.
pub(crate) fn parse_tape_documents<'a>(
    input: &'a str,
    options: &ParseOptions,
    buf: Option<&mut ParserBuf>,
) -> Result<TapeDocuments<'a>, Error> {
    options.limits.validate()?;
    if options.merge_keys {
        let (events, budget) = crate::merge::events::expand_all_documents(input, &options.limits)?;
        let (scratch, recycled, buf_slot) = match buf {
            Some(b) => (b.take_tape_scratch(), b.take_recycled_tape(), Some(b)),
            None => (TapeScratch::default(), RecycledTape::default(), None),
        };
        let mut guard = TapeScratchGuard {
            builder: Some(TapeBuilder::new(
                input,
                &options.limits,
                false,
                budget,
                scratch,
                recycled,
            )),
            buf: buf_slot,
        };
        for event in events {
            if let Some(builder) = guard.builder.as_mut() {
                builder
                    .event(event)
                    .map_err(|e| (*e).with_input_location(input, false))?;
            }
        }
        let docs = guard
            .finish_documents()
            .map_err(|e| e.with_input_location(input, false))?;
        return Ok(docs);
    }
    crate::parser::parse_tape_documents(input, &options.limits, buf)
}

/// Panic-safe guard returning `TapeScratch` to `ParserBuf` even on error or unwind.
struct TapeScratchGuard<'a, 'b> {
    builder: Option<TapeBuilder<'a>>,
    buf: Option<&'b mut ParserBuf>,
}

impl<'a> TapeScratchGuard<'a, '_> {
    fn finish_single(mut self) -> Result<Tape<'a>, Error> {
        let Some(builder) = self.builder.take() else {
            return Err(Error::new(
                ErrorKind::Composition,
                "tape builder already consumed",
            ));
        };
        let (result, scratch) = builder.finish_single();
        if let Some(buf) = self.buf.take() {
            buf.put_tape_scratch(scratch);
        }
        result
    }

    fn finish_documents(mut self) -> Result<TapeDocuments<'a>, Error> {
        let Some(builder) = self.builder.take() else {
            return Err(Error::new(
                ErrorKind::Composition,
                "tape builder already consumed",
            ));
        };
        let (result, scratch) = builder.finish_documents();
        if let Some(buf) = self.buf.take() {
            buf.put_tape_scratch(scratch);
        }
        result
    }
}

impl Drop for TapeScratchGuard<'_, '_> {
    fn drop(&mut self) {
        if let Some(buf) = self.buf.take()
            && let Some(mut builder) = self.builder.take()
        {
            let scratch = builder.take_scratch();
            buf.put_tape_scratch(scratch);
        }
    }
}
