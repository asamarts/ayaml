//! Reusable `'static` scratch buffers for streaming typed reads.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasher, BuildHasherDefault, Hasher};

use crate::mapping::GlobalHasher;
use crate::parser::{Cont, QueuedEvent, WindowScratch};
use crate::scanner::TokenText;
use crate::scratch::clamp_retained_capacity;
use crate::span::Span;

/// Pass-through hasher for keys that are already cryptographic `SipHash-1-3` digests from
/// [`GlobalHasher`], avoiding double `SipHash` on every large-mapping lookup.
#[derive(Default)]
pub(crate) struct PrehashedU64Hasher(u64);

impl Hasher for PrehashedU64Hasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = self.0.wrapping_mul(0x100).wrapping_add(u64::from(b));
        }
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.0 = i;
    }
}

pub(crate) type PrehashedMap<V> = HashMap<u64, V, BuildHasherDefault<PrehashedU64Hasher>>;

/// Hash bucket entry for promoted large-mapping duplicate detection in `Stream`.
#[derive(Clone, Debug)]
pub(super) enum StreamKeySlot {
    One(TokenText),
    Many(Vec<TokenText>),
}

/// Active promoted duplicate-key table paired with its owning `Stream::open` depth.
pub(super) type StreamSeenLarge = (usize, PrehashedMap<StreamKeySlot>);

/// Compute SipHash-1-3 of a scalar key's UTF-8 bytes for [`StreamSeenLarge`].
#[inline]
pub(super) fn hash_scalar_key_bytes(bytes: &[u8]) -> u64 {
    GlobalHasher.hash_one(bytes)
}

/// Convert a borrowed or owned scalar key into `'static` [`TokenText`] without allocating
/// whenever `text` matches the source span (or quote-stripped inner span).
#[inline]
pub(super) fn cow_to_key_text(text: Cow<'_, str>, span: Span, input: &str) -> TokenText {
    match text {
        Cow::Borrowed(s) => {
            if s.is_empty() {
                return TokenText::EMPTY;
            }
            let s_bytes = s.as_bytes();
            if span.len() == s_bytes.len() && span.bytes(input) == s_bytes {
                return TokenText::Slice(span);
            }
            if span.len() == s_bytes.len() + 2 {
                let inner = Span::new(span.start() + 1, span.end() - 1);
                if inner.bytes(input) == s_bytes {
                    return TokenText::Slice(inner);
                }
            }
            TokenText::Owned(s.to_owned())
        }
        Cow::Owned(owned) => TokenText::Owned(owned),
    }
}

/// Insert `key` into a promoted hash map if absent. Returns `Ok(())` if inserted, or `Err(key)` if
/// a matching key was already present.
#[inline]
pub(super) fn insert_large_key_map(
    map: &mut PrehashedMap<StreamKeySlot>,
    key: TokenText,
    input: &str,
) -> Result<(), TokenText> {
    let bytes = key.as_bytes(input);
    let hash = hash_scalar_key_bytes(bytes);
    match map.get_mut(&hash) {
        None => {
            map.insert(hash, StreamKeySlot::One(key));
            Ok(())
        }
        Some(StreamKeySlot::One(existing)) => {
            if existing.as_bytes(input) == bytes {
                return Err(key);
            }
            let first = std::mem::replace(existing, TokenText::EMPTY);
            map.insert(hash, StreamKeySlot::Many(vec![first, key]));
            Ok(())
        }
        Some(StreamKeySlot::Many(list)) => {
            if list
                .iter()
                .any(|existing| existing.as_bytes(input) == bytes)
            {
                return Err(key);
            }
            list.push(key);
            Ok(())
        }
    }
}

/// Promote a small mapping's inline keys from `seen_keys[seen_start..]` into a SipHash-1-3 table
/// and insert `new_key`. Reuses an idle warm table when available.
#[cold]
#[inline(never)]
#[allow(
    clippy::too_many_arguments,
    reason = "cold promotion helper avoids borrowing all of Stream mutably at the call site"
)]
pub(super) fn promote_seen_large(
    seen_keys: &mut Vec<TokenText>,
    seen_start: usize,
    top: &mut Option<StreamSeenLarge>,
    rest: &mut Vec<StreamSeenLarge>,
    depth: usize,
    promoted_cap: usize,
    new_key: TokenText,
    input: &str,
) -> Result<(), TokenText> {
    let mut map = match top.take() {
        Some((usize::MAX, mut warm)) => {
            warm.clear();
            warm
        }
        Some(active) => {
            rest.push(active);
            PrehashedMap::with_capacity_and_hasher(promoted_cap, BuildHasherDefault::default())
        }
        None => PrehashedMap::with_capacity_and_hasher(promoted_cap, BuildHasherDefault::default()),
    };
    for existing in seen_keys.drain(seen_start..) {
        drop(insert_large_key_map(&mut map, existing, input));
    }
    let result = insert_large_key_map(&mut map, new_key, input);
    *top = Some((depth, map));
    result
}

/// Release the innermost promoted duplicate-key table, retaining one warm idle table when small.
#[inline]
pub(super) fn pop_seen_large(
    top: &mut Option<StreamSeenLarge>,
    rest: &mut Vec<StreamSeenLarge>,
    depth: usize,
) {
    if let Some((d, mut map)) = top.take() {
        debug_assert_eq!(d, depth);
        let _ = d;
        map.clear();
        if let Some(prev) = rest.pop() {
            *top = Some(prev);
        } else if clamp_retained_capacity(map.capacity(), 512) {
            *top = Some((usize::MAX, map));
        }
    }
}

/// Reusable `'static` buffers for streaming typed Serde decoding (`from_str_in` / `from_str_seed_in`).
#[derive(Default)]
pub(crate) struct StreamScratch {
    pub(super) window: WindowScratch,
    pub(super) queue: VecDeque<QueuedEvent>,
    pub(super) cont_overflow: Vec<Cont>,
    pub(super) seen_keys: Vec<TokenText>,
    pub(super) seen_large_top: Option<StreamSeenLarge>,
    pub(super) seen_large_rest: Vec<StreamSeenLarge>,
}

impl StreamScratch {
    pub(crate) fn clear_and_clamp(&mut self) {
        self.window.clear_and_clamp();
        self.queue.clear();
        if !clamp_retained_capacity(self.queue.capacity(), 256) {
            self.queue = VecDeque::new();
        }
        self.cont_overflow.clear();
        if !clamp_retained_capacity(self.cont_overflow.capacity(), 256) {
            self.cont_overflow = Vec::new();
        }
        self.seen_keys.clear();
        if !clamp_retained_capacity(self.seen_keys.capacity(), 512) {
            self.seen_keys = Vec::new();
        }
        if let Some((_, mut map)) = self.seen_large_top.take() {
            map.clear();
            if clamp_retained_capacity(map.capacity(), 512) {
                self.seen_large_top = Some((usize::MAX, map));
            }
        }
        self.seen_large_rest.clear();
        if !clamp_retained_capacity(self.seen_large_rest.capacity(), 16) {
            self.seen_large_rest = Vec::new();
        }
    }

    pub(crate) fn release_all(&mut self) {
        self.window.release_all();
        self.queue = VecDeque::new();
        self.cont_overflow = Vec::new();
        self.seen_keys = Vec::new();
        self.seen_large_top = None;
        self.seen_large_rest = Vec::new();
    }
}
