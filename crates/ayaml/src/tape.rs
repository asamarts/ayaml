//! Read-only flat-array YAML DOM (`Tape` and `TapeDocuments`).
//!
//! [`Tape`] stores a parsed YAML document in a contiguous flat vector of
//! compact nodes with borrowed `&'a str` slices for unescaped scalars and a
//! single bump buffer for decoded text, avoiding per-string heap allocations
//! while remaining 100% safe Rust.
//!
//! All container, handle, and iterator types in this module are opaque so the
//! internal node layout and indexing strategies can evolve across `1.x`
//! releases without breaking `SemVer`.
//!
//! # Quick start
//!
//! ```
//! use ayaml::{Tape, TapeKind};
//!
//! let tape = Tape::parse("name: example\nports:\n  - 80\n  - 443\n")?;
//! let root = tape.root();
//! assert_eq!(root.kind(), TapeKind::Mapping);
//! assert_eq!(root.get("name").and_then(|n| n.as_str()), Some("example"));
//! assert_eq!(
//!     root.get("ports")
//!         .and_then(|n| n.get(1))
//!         .and_then(|n| n.as_u64()),
//!     Some(443),
//! );
//! # Ok::<(), ayaml::Error>(())
//! ```

use std::fmt;
use std::iter::FusedIterator;

use crate::emitter::PathSegment;
use crate::error::Error;
use crate::mapping::Mapping;
use crate::number::Number;
use crate::options::ParseOptions;
use crate::scratch::ParserBuf;
use crate::span::Span;
use crate::tag::{Tag, TaggedValue};
use crate::value::Value;

pub(crate) mod builder;
mod cmp;
use self::cmp::{tape_eq_value, tape_key_text, tape_node_eq, tape_node_hash};
mod de;
#[cfg(kani)]
mod kani_proofs;
#[cfg(test)]
mod tests;

pub(crate) use builder::{RecycledTape, TapeBuilder, TapeScratch};

/// Sentinel 24-bit span length indicating the node's full [`Span`] is stored
/// in `TapeStorage::overflow_spans`.
pub(crate) const OVERFLOW_SPAN_LEN: u32 = 0x00FF_FFFF;
const SPAN_LEN_MASK: u32 = 0x00FF_FFFF;
const TAG_SHIFT: u32 = 24;
const TAG_MASK: u32 = 0x0F;
const FLAGS_SHIFT: u32 = 28;

/// Untagged plain scalar flag (allows float parsing of leading-zero spellings
/// like `01` and `01.5` in Serde float targets).
pub(crate) const FLAG_PLAIN_SCALAR: u8 = 1 << 0;
/// Radix integer flag (`0x`, `0o`, `0b`; rejected by Serde float targets).
pub(crate) const FLAG_RADIX_INT: u8 = 1 << 1;
/// Outermost node of a materialized alias copy.
pub(crate) const FLAG_ALIAS_COPY: u8 = 1 << 2;

/// Compact 4-bit internal discriminant for [`TapeNode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum NodeTag {
    EmptyNull = 0,
    ExplicitNull = 1,
    Bool = 2,
    PosInt = 3,
    NegInt = 4,
    Float = 5,
    WideInt = 6,
    BorrowedStr = 7,
    DecodedStr = 8,
    LargeStr = 9,
    Sequence = 10,
    Mapping = 11,
    Tagged = 12,
}

impl NodeTag {
    #[inline]
    pub(crate) const fn from_bits(bits: u8) -> Self {
        match bits {
            1 => Self::ExplicitNull,
            2 => Self::Bool,
            3 => Self::PosInt,
            4 => Self::NegInt,
            5 => Self::Float,
            6 => Self::WideInt,
            7 => Self::BorrowedStr,
            8 => Self::DecodedStr,
            9 => Self::LargeStr,
            10 => Self::Sequence,
            11 => Self::Mapping,
            12 => Self::Tagged,
            _ => Self::EmptyNull,
        }
    }
}

/// 16-byte flat-array node storing inline spans, kind/flags, and payload.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TapeNode {
    /// Byte offset of node start in `input`, or `u32::MAX` when overflowed.
    span_start: u32,
    /// Bits `0..24`: `span_len` (`0xFF_FFFF` => overflow lookup).
    /// Bits `24..28`: 4-bit [`NodeTag`].
    /// Bits `28..32`: 4 flag bits.
    span_len_and_tag: u32,
    /// 8-byte payload (numeric value, `(start, len)` slice, or `(len/tag, end)`).
    payload: u64,
}

const _: () = assert!(size_of::<TapeNode>() == 16);
const _: () = assert!(align_of::<TapeNode>() == 8);

/// Pack 24-bit span length, 4-bit node tag, and 4-bit flags into one `u32`.
#[inline]
pub(crate) const fn pack_span_tag(span_len_24: u32, tag: NodeTag, flags: u8) -> u32 {
    (span_len_24 & SPAN_LEN_MASK)
        | (((tag as u32) & TAG_MASK) << TAG_SHIFT)
        | (((flags as u32) & 0x0F) << FLAGS_SHIFT)
}

/// Pack two `u32` halves into a `u64` payload (`lo` in bits `0..32`, `hi` in bits `32..64`).
#[inline]
pub(crate) const fn pack_u32_pair(lo: u32, hi: u32) -> u64 {
    (lo as u64) | ((hi as u64) << 32)
}

/// Unpack a `u64` payload into `(lo, hi)`.
#[inline]
pub(crate) const fn unpack_u32_pair(payload: u64) -> (u32, u32) {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "intentional low/high 32-bit word extraction"
    )]
    let lo = payload as u32;
    let hi = (payload >> 32) as u32;
    (lo, hi)
}

/// Compute checked shifted `end` index when copying an anchored subtree.
#[inline]
pub(crate) const fn shift_alias_end(end: u32, delta: u32) -> Option<u32> {
    end.checked_add(delta)
}

impl TapeNode {
    #[inline]
    pub(crate) const fn new(
        span_start: u32,
        span_len_24: u32,
        tag: NodeTag,
        flags: u8,
        payload: u64,
    ) -> Self {
        Self {
            span_start,
            span_len_and_tag: pack_span_tag(span_len_24, tag, flags),
            payload,
        }
    }

    #[inline]
    pub(crate) const fn tag(self) -> NodeTag {
        NodeTag::from_bits(((self.span_len_and_tag >> TAG_SHIFT) & TAG_MASK) as u8)
    }

    #[inline]
    pub(crate) const fn flags(self) -> u8 {
        (self.span_len_and_tag >> FLAGS_SHIFT) as u8
    }

    #[inline]
    pub(crate) const fn has_flag(self, flag: u8) -> bool {
        (self.flags() & flag) != 0
    }

    #[inline]
    pub(crate) const fn span_len_bits(self) -> u32 {
        self.span_len_and_tag & SPAN_LEN_MASK
    }

    #[inline]
    pub(crate) const fn inline_span(self) -> Option<Span> {
        let len = self.span_len_bits();
        if len == OVERFLOW_SPAN_LEN {
            return None;
        }
        let start = self.span_start as usize;
        Some(Span::new(start, start + len as usize))
    }

    #[inline]
    pub(crate) const fn with_span_parts(self, span_start: u32, span_len_24: u32) -> Self {
        Self {
            span_start,
            span_len_and_tag: pack_span_tag(span_len_24, self.tag(), self.flags()),
            payload: self.payload,
        }
    }

    #[inline]
    pub(crate) const fn with_added_flag(self, flag: u8) -> Self {
        Self {
            span_start: self.span_start,
            span_len_and_tag: pack_span_tag(self.span_len_bits(), self.tag(), self.flags() | flag),
            payload: self.payload,
        }
    }

    #[inline]
    pub(crate) const fn payload(self) -> u64 {
        self.payload
    }

    #[inline]
    pub(crate) const fn payload_pair(self) -> (u32, u32) {
        unpack_u32_pair(self.payload)
    }

    #[inline]
    pub(crate) fn set_payload_pair(&mut self, lo: u32, hi: u32) {
        self.payload = pack_u32_pair(lo, hi);
    }

    #[inline]
    pub(crate) const fn is_container(self) -> bool {
        matches!(
            self.tag(),
            NodeTag::Sequence | NodeTag::Mapping | NodeTag::Tagged
        )
    }

    #[inline]
    pub(crate) const fn subtree_end(self, index: usize) -> usize {
        if self.is_container() {
            self.payload_pair().1 as usize
        } else {
            index + 1
        }
    }
}

/// Cold storage for integers wider than 64 bits (`i128` / `u128`).
#[derive(Clone, Copy, Debug)]
pub(crate) enum WideInt {
    Pos(u64, u64),
    Neg(u64, u64),
}

impl WideInt {
    #[inline]
    pub(crate) fn from_number(number: Number) -> Option<Self> {
        if let Some(u) = number.as_u128() {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "splitting 128-bit integer into two 64-bit limbs"
            )]
            return Some(Self::Pos((u >> 64) as u64, u as u64));
        }
        if let Some(i) = number.as_i128() {
            let bits = i.cast_unsigned();
            #[allow(
                clippy::cast_possible_truncation,
                reason = "splitting 128-bit integer into two 64-bit limbs"
            )]
            return Some(Self::Neg((bits >> 64) as u64, bits as u64));
        }
        None
    }

    #[inline]
    pub(crate) const fn to_number(self) -> Number {
        match self {
            Self::Pos(hi, lo) => Number::from_u128(((hi as u128) << 64) | (lo as u128)),
            Self::Neg(hi, lo) => {
                Number::from_i128((((hi as u128) << 64) | (lo as u128)).cast_signed())
            }
        }
    }
}

/// Cold storage for strings whose byte offset or length exceeds `u32::MAX`.
#[derive(Clone, Copy, Debug)]
pub(crate) enum LargeStr {
    Borrowed { start: usize, len: usize },
    Decoded { start: usize, len: usize },
}

/// Flat-array backing storage shared by [`Tape`] and [`TapeDocuments`].
#[derive(Clone)]
pub(crate) struct TapeStorage<'a> {
    pub(crate) input: &'a str,
    pub(crate) nodes: Vec<TapeNode>,
    pub(crate) decoded: String,
    pub(crate) tags: Vec<Tag>,
    pub(crate) wide_ints: Vec<WideInt>,
    pub(crate) large_strings: Vec<LargeStr>,
    pub(crate) overflow_spans: Vec<(u32, Span)>,
}

impl<'a> TapeStorage<'a> {
    #[inline]
    pub(crate) fn node(&self, index: usize) -> TapeNode {
        self.nodes
            .get(index)
            .copied()
            .unwrap_or_else(|| TapeNode::new(0, 0, NodeTag::EmptyNull, 0, 0))
    }

    #[inline]
    pub(crate) fn span_at(&self, index: usize) -> Span {
        let node = self.node(index);
        if let Some(span) = node.inline_span() {
            return span;
        }
        let target = u32::try_from(index).unwrap_or(u32::MAX);
        self.overflow_spans
            .binary_search_by_key(&target, |(idx, _)| *idx)
            .ok()
            .and_then(|pos| self.overflow_spans.get(pos))
            .map_or_else(|| Span::empty(0), |(_, span)| *span)
    }

    #[inline]
    pub(crate) fn str_at(&self, index: usize) -> Option<&str> {
        let node = self.node(index);
        match node.tag() {
            NodeTag::BorrowedStr => {
                let (start, len) = node.payload_pair();
                let start = start as usize;
                let end = start.checked_add(len as usize)?;
                self.input.get(start..end)
            }
            NodeTag::DecodedStr => {
                let (start, len) = node.payload_pair();
                let start = start as usize;
                let end = start.checked_add(len as usize)?;
                self.decoded.get(start..end)
            }
            NodeTag::LargeStr => {
                let entry = *self
                    .large_strings
                    .get(usize::try_from(node.payload()).ok()?)?;
                match entry {
                    LargeStr::Borrowed { start, len } => {
                        self.input.get(start..start.checked_add(len)?)
                    }
                    LargeStr::Decoded { start, len } => {
                        self.decoded.get(start..start.checked_add(len)?)
                    }
                }
            }
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn borrowed_str_at(&self, index: usize) -> Option<&'a str> {
        let node = self.node(index);
        match node.tag() {
            NodeTag::BorrowedStr => {
                let (start, len) = node.payload_pair();
                let start = start as usize;
                let end = start.checked_add(len as usize)?;
                self.input.get(start..end)
            }
            NodeTag::LargeStr => match *self
                .large_strings
                .get(usize::try_from(node.payload()).ok()?)?
            {
                LargeStr::Borrowed { start, len } => self.input.get(start..start.checked_add(len)?),
                LargeStr::Decoded { .. } => None,
            },
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn number_at(&self, index: usize) -> Option<Number> {
        let node = self.node(index);
        match node.tag() {
            NodeTag::PosInt => Some(Number::from_u64(node.payload())),
            NodeTag::NegInt => Some(Number::from_i64(node.payload().cast_signed())),
            NodeTag::Float => Some(Number::from_f64(f64::from_bits(node.payload()))),
            NodeTag::WideInt => self
                .wide_ints
                .get(usize::try_from(node.payload()).ok()?)
                .map(|wide| wide.to_number()),
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn tag_at(&self, index: usize) -> Option<&Tag> {
        let node = self.node(index);
        if node.tag() != NodeTag::Tagged {
            return None;
        }
        let (tag_idx, _) = node.payload_pair();
        self.tags.get(tag_idx as usize)
    }

    #[inline]
    pub(crate) fn untag_index(&self, mut index: usize) -> usize {
        while index < self.nodes.len() && self.node(index).tag() == NodeTag::Tagged {
            index += 1;
        }
        index
    }

    /// Convert the node at `root_index` to an owned [`Value`] iteratively
    /// without native recursion.
    pub(crate) fn to_value_at(&self, root_index: usize) -> Value {
        enum Frame {
            Seq {
                items: Vec<Value>,
                next: usize,
                end: usize,
            },
            Map {
                map: Mapping,
                pending_key: Option<Value>,
                next: usize,
                end: usize,
            },
            Tag {
                tag: Tag,
            },
        }

        let mut stack: Vec<Frame> = Vec::new();
        let mut cursor = root_index;

        let mut finished: Option<Value> = None;
        loop {
            let value = if let Some(val) = finished.take() {
                val
            } else {
                let node = self.node(cursor);
                match node.tag() {
                    NodeTag::EmptyNull | NodeTag::ExplicitNull => Value::Null,
                    NodeTag::Bool => Value::Bool(node.payload() != 0),
                    NodeTag::PosInt => Value::Number(Number::from_u64(node.payload())),
                    NodeTag::NegInt => {
                        Value::Number(Number::from_i64(node.payload().cast_signed()))
                    }
                    NodeTag::Float => {
                        Value::Number(Number::from_f64(f64::from_bits(node.payload())))
                    }
                    NodeTag::WideInt => Value::Number(
                        usize::try_from(node.payload())
                            .ok()
                            .and_then(|idx| self.wide_ints.get(idx))
                            .map_or_else(|| Number::from_u64(0), |wide| wide.to_number()),
                    ),
                    NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr => {
                        Value::String(self.str_at(cursor).unwrap_or("").to_owned())
                    }
                    NodeTag::Sequence => {
                        let (len, end) = node.payload_pair();
                        if len == 0 {
                            Value::Sequence(Vec::new())
                        } else {
                            let first = cursor + 1;
                            let next = self.node(first).subtree_end(first);
                            stack.push(Frame::Seq {
                                items: Vec::with_capacity(len as usize),
                                next,
                                end: end as usize,
                            });
                            cursor = first;
                            continue;
                        }
                    }
                    NodeTag::Mapping => {
                        let (len, end) = node.payload_pair();
                        if len == 0 {
                            Value::Mapping(Mapping::new())
                        } else {
                            let first = cursor + 1;
                            let next = self.node(first).subtree_end(first);
                            stack.push(Frame::Map {
                                map: Mapping::with_capacity(len as usize),
                                pending_key: None,
                                next,
                                end: end as usize,
                            });
                            cursor = first;
                            continue;
                        }
                    }
                    NodeTag::Tagged => {
                        let tag = self
                            .tag_at(cursor)
                            .cloned()
                            .unwrap_or_else(|| Tag::new("!", ""));
                        stack.push(Frame::Tag { tag });
                        cursor += 1;
                        continue;
                    }
                }
            };

            match stack.pop() {
                None => return value,
                Some(Frame::Tag { tag }) => {
                    finished = Some(Value::from(TaggedValue::new(tag, value)));
                }
                Some(Frame::Seq {
                    mut items,
                    next,
                    end,
                }) => {
                    items.push(value);
                    if next < end {
                        let after = self.node(next).subtree_end(next);
                        stack.push(Frame::Seq {
                            items,
                            next: after,
                            end,
                        });
                        cursor = next;
                    } else {
                        finished = Some(Value::Sequence(items));
                    }
                }
                Some(Frame::Map {
                    mut map,
                    pending_key,
                    next,
                    end,
                }) => {
                    if let Some(key) = pending_key {
                        drop(map.insert(key, value));
                        if next < end {
                            let after = self.node(next).subtree_end(next);
                            stack.push(Frame::Map {
                                map,
                                pending_key: None,
                                next: after,
                                end,
                            });
                            cursor = next;
                        } else {
                            finished = Some(Value::Mapping(map));
                        }
                    } else if next < end {
                        let after = self.node(next).subtree_end(next);
                        stack.push(Frame::Map {
                            map,
                            pending_key: Some(value),
                            next: after,
                            end,
                        });
                        cursor = next;
                    } else {
                        finished = Some(Value::Mapping(map));
                    }
                }
            }
        }
    }
}

/// Kind of node referenced by a [`TapeRef`].
///
/// This enum is `#[non_exhaustive]` so future `1.x` versions may introduce
/// additional node classifications without breaking `SemVer`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TapeKind {
    /// YAML null (`null`, `~`, or empty scalar).
    Null,
    /// YAML boolean (`true` or `false`).
    Bool,
    /// YAML integer or floating-point number.
    Number,
    /// YAML string scalar.
    String,
    /// YAML sequence.
    Sequence,
    /// YAML mapping.
    Mapping,
    /// Explicitly tagged node wrapping an inner payload.
    Tagged,
}

/// A read-only, flat-array YAML DOM for a single document.
///
/// `Tape<'a>` stores parsed nodes in a contiguous vector and borrows unescaped
/// scalars directly from the input `&'a str`, minimizing heap allocations and
/// improving cache locality compared to [`Value`].
///
/// Empty or comment-only inputs produce a single `Null` root, matching
/// [`crate::parse_value`]. Streams with more than one document are rejected
/// with [`crate::ErrorCode::TrailingDocuments`]; use [`TapeDocuments`] for
/// multi-document streams.
#[derive(Clone)]
pub struct Tape<'a> {
    storage: TapeStorage<'a>,
}

impl fmt::Debug for Tape<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tape").field("root", &self.root()).finish()
    }
}

impl<'a> Tape<'a> {
    /// Parse a single YAML document into a flat-array [`Tape`] with default limits.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax, resolution, duplicate-key, trailing-document,
    /// and limit failures as [`crate::parse_value`].
    pub fn parse(input: &'a str) -> Result<Self, Error> {
        Self::parse_with(input, &ParseOptions::new())
    }

    /// Parse a single YAML document into a flat-array [`Tape`] with explicit options.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax, resolution, duplicate-key, trailing-document,
    /// and limit failures as [`crate::parse_value_with`].
    pub fn parse_with(input: &'a str, options: &ParseOptions) -> Result<Self, Error> {
        builder::parse_tape(input, options, None)
    }

    /// Parse a single YAML document into a flat-array [`Tape`], reusing internal
    /// scratch buffers from `buf`.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax, resolution, duplicate-key, trailing-document,
    /// and limit failures as [`crate::parse_value`].
    pub fn parse_in(input: &'a str, buf: &mut ParserBuf) -> Result<Self, Error> {
        Self::parse_with_in(input, &ParseOptions::new(), buf)
    }

    /// Parse a single YAML document into a flat-array [`Tape`] with explicit options,
    /// reusing internal scratch buffers from `buf`.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax, resolution, duplicate-key, trailing-document,
    /// and limit failures as [`crate::parse_value_with`].
    pub fn parse_with_in(
        input: &'a str,
        options: &ParseOptions,
        buf: &mut ParserBuf,
    ) -> Result<Self, Error> {
        builder::parse_tape(input, options, Some(buf))
    }

    /// Return a read-only handle to the document's root node.
    #[inline]
    #[must_use]
    pub fn root(&self) -> TapeRef<'_, 'a> {
        TapeRef {
            storage: &self.storage,
            index: 0,
        }
    }

    /// Look up a direct child of the root node by sequence index or mapping key.
    #[inline]
    #[must_use]
    pub fn get<I: Index>(&self, index: I) -> Option<TapeRef<'_, 'a>> {
        self.root().get(index)
    }

    /// Resolve a path of [`PathSegment`] steps starting from the root node.
    #[must_use]
    pub fn lookup(&self, path: &[PathSegment]) -> Option<TapeRef<'_, 'a>> {
        self.root().lookup(path)
    }

    /// Materialize this tape into an owned [`Value`].
    #[must_use]
    pub fn to_value(&self) -> Value {
        self.root().to_value()
    }

    /// Deserialize this tape directly into a typed value `T`, borrowing unescaped
    /// strings from the original input slice when `T` supports `'a` borrowing.
    ///
    /// Unlike [`crate::from_str`], which preserves raw scalar spelling when coercing
    /// non-string plain scalars (such as `42`, `true`, or `null`) into a `String`
    /// target, `Tape::deserialize` enforces resolved YAML types strictly (matching
    /// [`crate::from_value`]) while still supporting [`crate::Spanned`] and
    /// empty-scalar defaults.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::ErrorKind::Serde`] error with source span and location
    /// when `T` does not match the tape's contents.
    pub fn deserialize<T: serde::Deserialize<'a>>(&self) -> Result<T, Error> {
        T::deserialize(self.root())
    }

    /// Deserialize this tape directly with a stateful [`serde::de::DeserializeSeed`].
    ///
    /// # Errors
    ///
    /// Returns a [`crate::ErrorKind::Serde`] error with source span and location
    /// when `seed` rejects the tape's contents.
    pub fn deserialize_seed<S: serde::de::DeserializeSeed<'a>>(
        &self,
        seed: S,
    ) -> Result<S::Value, Error> {
        seed.deserialize(self.root())
    }

    /// Return this tape's internal backing buffers to `buf` for reuse by future
    /// [`Tape::parse_in`] or [`TapeDocuments::parse_in`] calls.
    ///
    /// Buffers larger than `ParserBuf`'s retention ceiling are released instead
    /// of retained so an outlier document cannot pin memory indefinitely.
    pub fn recycle_into(self, buf: &mut ParserBuf) {
        buf.recycle_storage(self.storage, Vec::new());
    }
}

impl From<&Tape<'_>> for Value {
    fn from(tape: &Tape<'_>) -> Self {
        tape.to_value()
    }
}

/// A read-only, flat-array YAML DOM for a multi-document stream.
///
/// All documents in the stream share one contiguous backing tape.
#[derive(Clone)]
pub struct TapeDocuments<'a> {
    storage: TapeStorage<'a>,
    roots: Vec<u32>,
}

impl fmt::Debug for TapeDocuments<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<'a> TapeDocuments<'a> {
    /// Parse every YAML document in a stream into a flat-array [`TapeDocuments`]
    /// with default limits.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax, resolution, duplicate-key, and limit
    /// failures as [`crate::parse_documents`].
    pub fn parse(input: &'a str) -> Result<Self, Error> {
        Self::parse_with(input, &ParseOptions::new())
    }

    /// Parse every YAML document in a stream into a flat-array [`TapeDocuments`]
    /// with explicit options.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax, resolution, duplicate-key, and limit
    /// failures as [`crate::parse_documents_with`].
    pub fn parse_with(input: &'a str, options: &ParseOptions) -> Result<Self, Error> {
        builder::parse_tape_documents(input, options, None)
    }

    /// Parse every YAML document in a stream into a flat-array [`TapeDocuments`],
    /// reusing internal scratch buffers from `buf`.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax, resolution, duplicate-key, and limit
    /// failures as [`crate::parse_documents`].
    pub fn parse_in(input: &'a str, buf: &mut ParserBuf) -> Result<Self, Error> {
        Self::parse_with_in(input, &ParseOptions::new(), buf)
    }

    /// Parse every YAML document in a stream into a flat-array [`TapeDocuments`]
    /// with explicit options, reusing internal scratch buffers from `buf`.
    ///
    /// # Errors
    ///
    /// Returns an error for the same syntax, resolution, duplicate-key, and limit
    /// failures as [`crate::parse_documents_with`].
    pub fn parse_with_in(
        input: &'a str,
        options: &ParseOptions,
        buf: &mut ParserBuf,
    ) -> Result<Self, Error> {
        builder::parse_tape_documents(input, options, Some(buf))
    }

    /// Number of documents in this stream.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.roots.len()
    }

    /// Whether the stream contains no documents.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Return a read-only handle to document `index`, if present.
    #[inline]
    #[must_use]
    pub fn get(&self, index: usize) -> Option<TapeRef<'_, 'a>> {
        let root = *self.roots.get(index)?;
        Some(TapeRef {
            storage: &self.storage,
            index: root as usize,
        })
    }

    /// Iterate over every document root in stream order.
    #[inline]
    #[must_use]
    pub fn iter(&self) -> TapeDocumentsIter<'_, 'a> {
        TapeDocumentsIter {
            storage: &self.storage,
            roots: self.roots.iter(),
        }
    }

    /// Materialize every document into an owned `Vec<Value>`.
    #[must_use]
    pub fn to_values(&self) -> Vec<Value> {
        self.iter().map(|doc| doc.to_value()).collect()
    }

    /// Return this stream's internal backing buffers to `buf` for reuse by future
    /// [`Tape::parse_in`] or [`TapeDocuments::parse_in`] calls.
    pub fn recycle_into(self, buf: &mut ParserBuf) {
        buf.recycle_storage(self.storage, self.roots);
    }
}

impl<'t, 'a> IntoIterator for &'t TapeDocuments<'a> {
    type Item = TapeRef<'t, 'a>;
    type IntoIter = TapeDocumentsIter<'t, 'a>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over document roots in a [`TapeDocuments`] stream.
#[derive(Clone)]
pub struct TapeDocumentsIter<'t, 'a> {
    storage: &'t TapeStorage<'a>,
    roots: std::slice::Iter<'t, u32>,
}

impl fmt::Debug for TapeDocumentsIter<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

impl<'t, 'a> Iterator for TapeDocumentsIter<'t, 'a> {
    type Item = TapeRef<'t, 'a>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let &root = self.roots.next()?;
        Some(TapeRef {
            storage: self.storage,
            index: root as usize,
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.roots.size_hint()
    }
}

impl ExactSizeIterator for TapeDocumentsIter<'_, '_> {
    #[inline]
    fn len(&self) -> usize {
        self.roots.len()
    }
}

impl FusedIterator for TapeDocumentsIter<'_, '_> {}

/// Read-only `Copy` handle to one node inside a [`Tape`] or [`TapeDocuments`].
///
/// The lifetime `'t` borrows the owning tape storage, while `'a` borrows the
/// original YAML input slice so unescaped strings (and Serde deserialization
/// of borrowed `&'a str` types) can outlive the tape borrow.
///
/// Matching [`Value`], [`kind`](Self::kind), [`is_tagged`](Self::is_tagged),
/// [`tag`](Self::tag), and [`as_tagged`](Self::as_tagged) inspect an outer tag
/// wrapper directly, while typed accessors (`as_*`, `is_*`, `len`, `get`,
/// `lookup`) look through tags automatically via [`untag`](Self::untag).
#[derive(Clone, Copy)]
pub struct TapeRef<'t, 'a> {
    pub(crate) storage: &'t TapeStorage<'a>,
    pub(crate) index: usize,
}

impl fmt::Debug for TapeRef<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            TapeKind::Null => f.write_str("Null"),
            TapeKind::Bool => f
                .debug_tuple("Bool")
                .field(&self.as_bool().unwrap_or(false))
                .finish(),
            TapeKind::Number => match self.as_number() {
                Some(num) => f.debug_tuple("Number").field(&num).finish(),
                None => f.write_str("Number"),
            },
            TapeKind::String => f
                .debug_tuple("String")
                .field(&self.as_str().unwrap_or(""))
                .finish(),
            TapeKind::Sequence => {
                let mut list = f.debug_list();
                if let Some(iter) = self.as_sequence() {
                    list.entries(iter);
                }
                list.finish()
            }
            TapeKind::Mapping => {
                let mut map = f.debug_map();
                if let Some(iter) = self.as_mapping() {
                    map.entries(iter);
                }
                map.finish()
            }
            TapeKind::Tagged => {
                if let Some((tag, inner)) = self.as_tagged() {
                    f.debug_tuple("Tagged").field(tag).field(&inner).finish()
                } else {
                    f.write_str("Tagged")
                }
            }
        }
    }
}

impl<'t, 'a> TapeRef<'t, 'a> {
    #[inline]
    pub(crate) fn node(self) -> TapeNode {
        self.storage.node(self.index)
    }

    /// Return the direct [`TapeKind`] of this node (including [`TapeKind::Tagged`]
    /// if wrapped in an explicit tag).
    #[inline]
    #[must_use]
    pub fn kind(&self) -> TapeKind {
        match self.node().tag() {
            NodeTag::EmptyNull | NodeTag::ExplicitNull => TapeKind::Null,
            NodeTag::Bool => TapeKind::Bool,
            NodeTag::PosInt | NodeTag::NegInt | NodeTag::Float | NodeTag::WideInt => {
                TapeKind::Number
            }
            NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr => TapeKind::String,
            NodeTag::Sequence => TapeKind::Sequence,
            NodeTag::Mapping => TapeKind::Mapping,
            NodeTag::Tagged => TapeKind::Tagged,
        }
    }

    /// Strip any outer tag wrapper, returning the underlying payload node.
    #[inline]
    #[must_use]
    pub fn untag(&self) -> Self {
        Self {
            storage: self.storage,
            index: self.storage.untag_index(self.index),
        }
    }

    /// Whether this node carries an explicit tag wrapper.
    #[inline]
    #[must_use]
    pub fn is_tagged(&self) -> bool {
        self.node().tag() == NodeTag::Tagged
    }

    /// Return the outer [`Tag`] if this node is tagged.
    #[inline]
    #[must_use]
    pub fn tag(&self) -> Option<&'t Tag> {
        self.storage.tag_at(self.index)
    }

    /// Return `(tag, payload)` if this node is tagged.
    #[inline]
    #[must_use]
    pub fn as_tagged(&self) -> Option<(&'t Tag, Self)> {
        let tag = self.storage.tag_at(self.index)?;
        Some((
            tag,
            Self {
                storage: self.storage,
                index: self.index + 1,
            },
        ))
    }

    /// Whether this node (looking through tags) is null.
    #[inline]
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(
            self.untag().node().tag(),
            NodeTag::EmptyNull | NodeTag::ExplicitNull
        )
    }

    /// Whether this node (looking through tags) is a boolean.
    #[inline]
    #[must_use]
    pub fn is_bool(&self) -> bool {
        self.untag().node().tag() == NodeTag::Bool
    }

    /// Whether this node (looking through tags) is a number.
    #[inline]
    #[must_use]
    pub fn is_number(&self) -> bool {
        matches!(
            self.untag().node().tag(),
            NodeTag::PosInt | NodeTag::NegInt | NodeTag::Float | NodeTag::WideInt
        )
    }

    /// Whether this node (looking through tags) is an integer fitting in `i64`.
    #[inline]
    #[must_use]
    pub fn is_i64(&self) -> bool {
        self.as_number().is_some_and(|n| n.is_i64())
    }

    /// Whether this node (looking through tags) is a non-negative integer fitting in `u64`.
    #[inline]
    #[must_use]
    pub fn is_u64(&self) -> bool {
        self.as_number().is_some_and(|n| n.is_u64())
    }

    /// Whether this node (looking through tags) is a floating-point number.
    #[inline]
    #[must_use]
    pub fn is_f64(&self) -> bool {
        self.untag().node().tag() == NodeTag::Float
    }

    /// Whether this node (looking through tags) is a string.
    #[inline]
    #[must_use]
    pub fn is_string(&self) -> bool {
        matches!(
            self.untag().node().tag(),
            NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr
        )
    }

    /// Whether this node (looking through tags) is a sequence.
    #[inline]
    #[must_use]
    pub fn is_sequence(&self) -> bool {
        self.untag().node().tag() == NodeTag::Sequence
    }

    /// Whether this node (looking through tags) is a mapping.
    #[inline]
    #[must_use]
    pub fn is_mapping(&self) -> bool {
        self.untag().node().tag() == NodeTag::Mapping
    }

    /// Return `Some(())` if this node (looking through tags) is null.
    #[inline]
    #[must_use]
    pub fn as_null(&self) -> Option<()> {
        self.is_null().then_some(())
    }

    /// Return the boolean value if this node (looking through tags) is a boolean.
    #[inline]
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        let node = self.untag().node();
        (node.tag() == NodeTag::Bool).then_some(node.payload() != 0)
    }

    /// Return the [`Number`] value if this node (looking through tags) is a number.
    #[inline]
    #[must_use]
    pub fn as_number(&self) -> Option<Number> {
        let idx = self.storage.untag_index(self.index);
        self.storage.number_at(idx)
    }

    /// Return the `i64` value if this node is an integer in the `i64` range.
    #[inline]
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        self.as_number()?.as_i64()
    }

    /// Return the `u64` value if this node is a non-negative integer in the `u64` range.
    #[inline]
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        self.as_number()?.as_u64()
    }

    /// Return the `i128` value if this node is an integer in the `i128` range.
    #[inline]
    #[must_use]
    pub fn as_i128(&self) -> Option<i128> {
        self.as_number()?.as_i128()
    }

    /// Return the `u128` value if this node is a non-negative integer in the `u128` range.
    #[inline]
    #[must_use]
    pub fn as_u128(&self) -> Option<u128> {
        self.as_number()?.as_u128()
    }

    /// Return the `f64` value if this node is a number representable as `f64`,
    /// matching [`Value::as_f64`].
    #[inline]
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        self.as_number()?.as_f64()
    }

    /// Return the string slice if this node (looking through tags) is a string.
    ///
    /// The returned reference is tied to the tape borrow `'t` rather than `&self`,
    /// so chained expressions such as `tape.root().get("k")?.as_str()` compile cleanly.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> Option<&'t str> {
        let idx = self.storage.untag_index(self.index);
        self.storage.str_at(idx)
    }

    /// Return a zero-copy `&'a str` borrowed directly from the original input
    /// slice when this string required no escape or multiline decoding.
    ///
    /// Returns `None` for non-strings or strings stored in the tape's decoded
    /// bump buffer; use [`as_str`](Self::as_str) to read any string uniformly.
    #[inline]
    #[must_use]
    pub fn as_borrowed_str(&self) -> Option<&'a str> {
        let idx = self.storage.untag_index(self.index);
        self.storage.borrowed_str_at(idx)
    }

    /// Return an iterator over sequence elements if this node (looking through tags)
    /// is a sequence.
    #[inline]
    #[must_use]
    pub fn as_sequence(&self) -> Option<TapeSeqIter<'t, 'a>> {
        let idx = self.storage.untag_index(self.index);
        let node = self.storage.node(idx);
        if node.tag() != NodeTag::Sequence {
            return None;
        }
        let (len, end) = node.payload_pair();
        Some(TapeSeqIter {
            storage: self.storage,
            cursor: idx + 1,
            end: end as usize,
            remaining: len as usize,
        })
    }

    /// Return an iterator over `(key, value)` pairs in insertion order if this node
    /// (looking through tags) is a mapping.
    #[inline]
    #[must_use]
    pub fn as_mapping(&self) -> Option<TapeMapIter<'t, 'a>> {
        let idx = self.storage.untag_index(self.index);
        let node = self.storage.node(idx);
        if node.tag() != NodeTag::Mapping {
            return None;
        }
        let (len, end) = node.payload_pair();
        Some(TapeMapIter {
            storage: self.storage,
            cursor: idx + 1,
            end: end as usize,
            remaining: len as usize,
        })
    }

    /// Number of direct items in a sequence or key-value pairs in a mapping
    /// (looking through tags), or `None` for scalars.
    #[inline]
    #[must_use]
    pub fn len(&self) -> Option<usize> {
        let node = self.untag().node();
        match node.tag() {
            NodeTag::Sequence | NodeTag::Mapping => Some(node.payload_pair().0 as usize),
            _ => None,
        }
    }

    /// Whether a sequence or mapping (looking through tags) has zero entries,
    /// or `None` for scalars.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> Option<bool> {
        self.len().map(|len| len == 0)
    }

    /// Look up a child by sequence index or mapping key (looking through tags).
    #[inline]
    #[must_use]
    pub fn get<I: Index>(&self, index: I) -> Option<Self> {
        index.index_into(*self)
    }

    /// Look up the `index`-th item of a sequence (looking through tags).
    #[inline]
    #[must_use]
    pub fn get_index(&self, index: usize) -> Option<Self> {
        let mut iter = self.as_sequence()?;
        if index >= iter.len() {
            return None;
        }
        iter.nth(index)
    }

    /// Look up a string key in a mapping (looking through tags).
    #[inline]
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<Self> {
        let idx = self.storage.untag_index(self.index);
        let node = self.storage.node(idx);
        if node.tag() != NodeTag::Mapping {
            return None;
        }
        let (_, end) = node.payload_pair();
        let end = end as usize;
        let mut cursor = idx + 1;
        while cursor < end {
            let key_node = self.storage.node(cursor);
            let val_idx = key_node.subtree_end(cursor);
            if matches!(
                key_node.tag(),
                NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr
            ) && self.storage.str_at(cursor) == Some(key)
            {
                return Some(Self {
                    storage: self.storage,
                    index: val_idx,
                });
            }
            cursor = self.storage.node(val_idx).subtree_end(val_idx);
        }
        None
    }

    /// Look up an arbitrary [`Value`] key in a mapping (looking through tags).
    #[must_use]
    pub(crate) fn get_value(&self, key: &Value) -> Option<Self> {
        let idx = self.storage.untag_index(self.index);
        let node = self.storage.node(idx);
        if node.tag() != NodeTag::Mapping {
            return None;
        }
        let (_, end) = node.payload_pair();
        let end = end as usize;
        let mut cursor = idx + 1;
        while cursor < end {
            let key_node = self.storage.node(cursor);
            let val_idx = key_node.subtree_end(cursor);
            if tape_eq_value(self.storage, cursor, key) {
                return Some(Self {
                    storage: self.storage,
                    index: val_idx,
                });
            }
            cursor = self.storage.node(val_idx).subtree_end(val_idx);
        }
        None
    }

    /// Resolve a path of [`PathSegment`] steps starting from this node.
    #[must_use]
    pub fn lookup(&self, path: &[PathSegment]) -> Option<Self> {
        let mut current = *self;
        for segment in path {
            current = match segment {
                PathSegment::Index(index) => current.get_index(*index)?,
                PathSegment::Key(key) => current.get_value(key)?,
            };
        }
        Some(current)
    }

    /// Source byte [`Span`] of this node in the original input.
    ///
    /// Materialized alias roots carry their alias use-site span, while inner
    /// nodes copied from an anchored collection retain their definition spans.
    #[inline]
    #[must_use]
    pub fn span(&self) -> Span {
        self.storage.span_at(self.index)
    }

    /// Materialize this node and its subtree into an owned [`Value`].
    #[must_use]
    pub fn to_value(&self) -> Value {
        self.storage.to_value_at(self.index)
    }

    /// Deserialize this node into a typed value `T`, borrowing unescaped strings
    /// directly from the original `'a` input slice (even if the tape itself is
    /// later dropped).
    ///
    /// Non-string scalars (`42`, `true`, explicit `null`) are rejected for `String`
    /// targets rather than coerced from source spelling, matching [`crate::from_value`].
    ///
    /// # Errors
    ///
    /// Returns a [`crate::ErrorKind::Serde`] error with source span and location
    /// when `T` does not match this node.
    pub fn deserialize<T: serde::Deserialize<'a>>(self) -> Result<T, Error> {
        T::deserialize(self)
    }

    /// Deserialize this node with a stateful [`serde::de::DeserializeSeed`].
    ///
    /// # Errors
    ///
    /// Returns a [`crate::ErrorKind::Serde`] error with source span and location
    /// when `seed` rejects this node.
    pub fn deserialize_seed<S: serde::de::DeserializeSeed<'a>>(
        self,
        seed: S,
    ) -> Result<S::Value, Error> {
        seed.deserialize(self)
    }
}

impl From<TapeRef<'_, '_>> for Value {
    fn from(node: TapeRef<'_, '_>) -> Self {
        node.to_value()
    }
}

/// Forward iterator over elements of a YAML sequence in a [`Tape`].
///
/// Advances in O(1) per element via flat-array skip bounds.
#[derive(Clone)]
pub struct TapeSeqIter<'t, 'a> {
    storage: &'t TapeStorage<'a>,
    cursor: usize,
    end: usize,
    remaining: usize,
}

impl fmt::Debug for TapeSeqIter<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

impl<'t, 'a> Iterator for TapeSeqIter<'t, 'a> {
    type Item = TapeRef<'t, 'a>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 || self.cursor >= self.end {
            return None;
        }
        let idx = self.cursor;
        self.cursor = self.storage.node(idx).subtree_end(idx);
        self.remaining -= 1;
        Some(TapeRef {
            storage: self.storage,
            index: idx,
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for TapeSeqIter<'_, '_> {
    #[inline]
    fn len(&self) -> usize {
        self.remaining
    }
}

impl FusedIterator for TapeSeqIter<'_, '_> {}

/// Forward iterator over `(key, value)` pairs of a YAML mapping in a [`Tape`]
/// in insertion order.
///
/// Advances in O(1) per entry via flat-array skip bounds.
#[derive(Clone)]
pub struct TapeMapIter<'t, 'a> {
    storage: &'t TapeStorage<'a>,
    cursor: usize,
    end: usize,
    remaining: usize,
}

impl fmt::Debug for TapeMapIter<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.clone()).finish()
    }
}

impl<'t, 'a> Iterator for TapeMapIter<'t, 'a> {
    type Item = (TapeRef<'t, 'a>, TapeRef<'t, 'a>);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 || self.cursor >= self.end {
            return None;
        }
        let key_idx = self.cursor;
        let val_idx = self.storage.node(key_idx).subtree_end(key_idx);
        self.cursor = self.storage.node(val_idx).subtree_end(val_idx);
        self.remaining -= 1;
        Some((
            TapeRef {
                storage: self.storage,
                index: key_idx,
            },
            TapeRef {
                storage: self.storage,
                index: val_idx,
            },
        ))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for TapeMapIter<'_, '_> {
    #[inline]
    fn len(&self) -> usize {
        self.remaining
    }
}

impl FusedIterator for TapeMapIter<'_, '_> {}

mod private {
    pub trait Sealed {}
    impl Sealed for usize {}
    impl Sealed for str {}
    impl Sealed for String {}
    impl Sealed for crate::value::Value {}
    impl<T: ?Sized + Sealed> Sealed for &T {}
}

/// A type that can index into a [`Tape`] or [`TapeRef`] via [`TapeRef::get`].
///
/// This trait is sealed and cannot be implemented outside `ayaml`.
pub trait Index: private::Sealed {
    /// Look up `self` within `node`.
    #[doc(hidden)]
    fn index_into<'t, 'a>(&self, node: TapeRef<'t, 'a>) -> Option<TapeRef<'t, 'a>>;
}

impl Index for usize {
    #[inline]
    fn index_into<'t, 'a>(&self, node: TapeRef<'t, 'a>) -> Option<TapeRef<'t, 'a>> {
        node.get_index(*self)
            .or_else(|| node.get_value(&Value::from(*self)))
    }
}

impl Index for str {
    #[inline]
    fn index_into<'t, 'a>(&self, node: TapeRef<'t, 'a>) -> Option<TapeRef<'t, 'a>> {
        node.get_str(self)
    }
}

impl Index for String {
    #[inline]
    fn index_into<'t, 'a>(&self, node: TapeRef<'t, 'a>) -> Option<TapeRef<'t, 'a>> {
        node.get_str(self.as_str())
    }
}

impl Index for Value {
    #[inline]
    fn index_into<'t, 'a>(&self, node: TapeRef<'t, 'a>) -> Option<TapeRef<'t, 'a>> {
        node.get_value(self)
    }
}

impl<T: ?Sized + Index> Index for &T {
    #[inline]
    fn index_into<'t, 'a>(&self, node: TapeRef<'t, 'a>) -> Option<TapeRef<'t, 'a>> {
        (**self).index_into(node)
    }
}
