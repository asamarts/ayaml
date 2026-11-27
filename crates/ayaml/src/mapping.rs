//! An insertion-ordered YAML mapping.
//!
//! Small mappings (up to 8 entries) are stored inline in a single exact-sized or compact slice
//! allocation with linear lookup and no hash table. Larger mappings promote transparently to an
//! `indexmap`-backed table. No type of `indexmap` is part of this crate's public API: a mapping
//! iterates through [`Iter`], [`IterMut`], and [`IntoIter`]. Through 0.1.x the `IntoIterator`
//! implementations named `indexmap`'s own iterator types, which made that crate's major version part
//! of this one's.

use std::fmt;
use std::hash::{BuildHasher, Hash, Hasher, RandomState};
use std::iter::FusedIterator;
use std::ops::Index as OpsIndex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use indexmap::{Equivalent, IndexMap};

pub(crate) use crate::limits::GlobalHasher;
use crate::value::{NULL_VALUE, Value};

/// Maximum number of entries stored in the compact linear small-map representation before
/// promoting to a hash-indexed `IndexMap`.
pub(crate) const SMALL_LIMIT: usize = 8;

/// A borrowed string key that hashes and compares as `Value::String` does.
struct StringKey<'a>(&'a str);

impl Hash for StringKey<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Match the derived `Hash` for `Value::String`: its discriminant, then the text.
        std::mem::discriminant(&Value::String(String::new())).hash(state);
        self.0.hash(state);
    }
}

impl Equivalent<Value> for StringKey<'_> {
    fn equivalent(&self, key: &Value) -> bool {
        matches!(key, Value::String(text) if text == self.0)
    }
}

/// Single-allocation storage for small mappings (`1..=SMALL_LIMIT` entries).
///
/// The unsized trailing slice allows constructing exact capacities `1..=SMALL_LIMIT` via safe
/// array-to-slice unsized coercion, keeping the heap header at 8 bytes (`AtomicU64` cached hash)
/// with no extra `len` word (stored inline in `MappingRepr::Small`).
struct SmallSlice<T: ?Sized> {
    /// Cached order-independent entry hash (`0` means uncached).
    hash: AtomicU64,
    entries: T,
}

/// Heap storage for mappings that exceed [`SMALL_LIMIT`] entries.
struct LargeMapping {
    map: IndexMap<Value, Value, GlobalHasher>,
    /// Cached order-independent entry hash (`0` means uncached).
    hash: AtomicU64,
}

impl Clone for LargeMapping {
    fn clone(&self) -> Self {
        Self {
            map: self.map.clone(),
            hash: AtomicU64::new(self.hash.load(Ordering::Relaxed)),
        }
    }
}

#[derive(Default)]
enum MappingRepr {
    #[default]
    Empty,
    Small {
        len: u8,
        buf: Box<SmallSlice<[(Value, Value)]>>,
    },
    Large(Box<LargeMapping>),
}

/// Allocate an empty small-map buffer of capacity `1..=SMALL_LIMIT` in one allocation.
fn alloc_small_empty(cap: usize) -> Box<SmallSlice<[(Value, Value)]>> {
    macro_rules! make {
        ($n:literal) => {
            Box::new(SmallSlice {
                hash: AtomicU64::new(0),
                entries: [const { (Value::Null, Value::Null) }; $n],
            })
        };
    }
    match cap {
        1 => make!(1),
        2 => make!(2),
        3 => make!(3),
        4 => make!(4),
        5 => make!(5),
        6 => make!(6),
        7 => make!(7),
        8 => make!(8),
        _ => unreachable!("small mapping capacity must be in 1..=SMALL_LIMIT"),
    }
}

/// Allocate an exact-sized small-map buffer populated from `iter` (`1..=SMALL_LIMIT` items).
fn alloc_small_exact<I>(mut iter: I, hash: u64) -> Box<SmallSlice<[(Value, Value)]>>
where
    I: ExactSizeIterator<Item = (Value, Value)>,
{
    let len = iter.len();
    let mut next = || iter.next().unwrap_or((Value::Null, Value::Null));
    match len {
        1 => Box::new(SmallSlice {
            hash: AtomicU64::new(hash),
            entries: [next()],
        }),
        2 => Box::new(SmallSlice {
            hash: AtomicU64::new(hash),
            entries: [next(), next()],
        }),
        3 => Box::new(SmallSlice {
            hash: AtomicU64::new(hash),
            entries: [next(), next(), next()],
        }),
        4 => Box::new(SmallSlice {
            hash: AtomicU64::new(hash),
            entries: [next(), next(), next(), next()],
        }),
        5 => Box::new(SmallSlice {
            hash: AtomicU64::new(hash),
            entries: [next(), next(), next(), next(), next()],
        }),
        6 => Box::new(SmallSlice {
            hash: AtomicU64::new(hash),
            entries: [next(), next(), next(), next(), next(), next()],
        }),
        7 => Box::new(SmallSlice {
            hash: AtomicU64::new(hash),
            entries: [next(), next(), next(), next(), next(), next(), next()],
        }),
        8 => Box::new(SmallSlice {
            hash: AtomicU64::new(hash),
            entries: [
                next(),
                next(),
                next(),
                next(),
                next(),
                next(),
                next(),
                next(),
            ],
        }),
        _ => unreachable!("small mapping length must be in 1..=SMALL_LIMIT"),
    }
}

/// A YAML mapping with insertion-order iteration and swap-remove semantics.
#[derive(Default)]
pub struct Mapping {
    inner: MappingRepr,
}

impl Clone for Mapping {
    fn clone(&self) -> Self {
        let inner = match &self.inner {
            MappingRepr::Empty => MappingRepr::Empty,
            MappingRepr::Small { len, buf } => {
                let count = usize::from(*len);
                if count == 0 {
                    MappingRepr::Empty
                } else {
                    let hash = buf.hash.load(Ordering::Relaxed);
                    let cloned = alloc_small_exact(buf.entries[..count].iter().cloned(), hash);
                    MappingRepr::Small {
                        len: *len,
                        buf: cloned,
                    }
                }
            }
            MappingRepr::Large(large) => MappingRepr::Large(large.clone()),
        };
        Self { inner }
    }
}

impl PartialEq for Mapping {
    fn eq(&self, other: &Self) -> bool {
        if self.len() != other.len() {
            return false;
        }
        match (&self.inner, &other.inner) {
            (MappingRepr::Empty, MappingRepr::Empty) => true,
            (MappingRepr::Large(left), MappingRepr::Large(right)) => left.map == right.map,
            _ => self
                .iter()
                .all(|(key, value)| other.get(key) == Some(value)),
        }
    }
}

impl Eq for Mapping {}

/// A borrowed or owned key accepted by [`Mapping`] lookup operations.
///
/// This is sealed by the set of implementations below: YAML keys may be full
/// [`Value`]s, while string keys are the common ergonomic case. References to
/// either form are accepted without forcing callers to match one ownership
/// convention.
pub trait Index: private::Sealed {
    // Each method that hands out or removes a value forgets the mapping's cached hash itself:
    // the methods are hidden, not private, so a caller can reach one without `Mapping`'s own.
    #[doc(hidden)]
    fn contains_key(&self, mapping: &Mapping) -> bool;
    #[doc(hidden)]
    fn get<'a>(&self, mapping: &'a Mapping) -> Option<&'a Value>;
    #[doc(hidden)]
    fn get_mut<'a>(&self, mapping: &'a mut Mapping) -> Option<&'a mut Value>;
    #[doc(hidden)]
    fn swap_remove(&self, mapping: &mut Mapping) -> Option<Value>;
    #[doc(hidden)]
    fn shift_remove(&self, mapping: &mut Mapping) -> Option<Value>;
}

impl Mapping {
    /// Create an empty mapping.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an empty mapping with capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let inner = if capacity == 0 {
            MappingRepr::Empty
        } else if capacity <= SMALL_LIMIT {
            MappingRepr::Small {
                len: 0,
                buf: alloc_small_empty(capacity),
            }
        } else {
            MappingRepr::Large(Box::new(LargeMapping {
                map: IndexMap::with_capacity_and_hasher(capacity, GlobalHasher),
                hash: AtomicU64::new(0),
            }))
        };
        Self { inner }
    }

    /// Build a mapping from at most [`SMALL_LIMIT`] already-deduplicated entries in one exact
    /// allocation.
    pub(crate) fn from_small_drain<I>(iter: I) -> Self
    where
        I: ExactSizeIterator<Item = (Value, Value)>,
    {
        let len = iter.len();
        assert!(
            len <= SMALL_LIMIT,
            "from_small_drain requires len <= SMALL_LIMIT"
        );
        if len == 0 {
            return Self::new();
        }
        let buf = alloc_small_exact(iter, 0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "len <= SMALL_LIMIT (8) always fits in u8"
        )]
        Self {
            inner: MappingRepr::Small {
                len: len as u8,
                buf,
            },
        }
    }

    #[inline]
    fn invalidate_hash(&mut self) {
        match &mut self.inner {
            MappingRepr::Empty => {}
            MappingRepr::Small { buf, .. } => buf.hash.store(0, Ordering::Relaxed),
            MappingRepr::Large(large) => large.hash.store(0, Ordering::Relaxed),
        }
    }

    /// Give back the room that the entries don't need. A mapping that grew as its entries
    /// arrived can hold room for nearly twice as many.
    pub fn shrink_to_fit(&mut self) {
        match &mut self.inner {
            MappingRepr::Empty => {}
            MappingRepr::Small { len, buf } => {
                let count = usize::from(*len);
                if count == 0 {
                    self.inner = MappingRepr::Empty;
                } else if buf.entries.len() > count {
                    let hash = buf.hash.load(Ordering::Relaxed);
                    let mut src = buf.entries[..count]
                        .iter_mut()
                        .map(|slot| std::mem::replace(slot, (Value::Null, Value::Null)));
                    let shrunk = alloc_small_exact(&mut src, hash);
                    *buf = shrunk;
                }
            }
            MappingRepr::Large(large) => {
                let count = large.map.len();
                if count == 0 {
                    self.inner = MappingRepr::Empty;
                } else if count <= SMALL_LIMIT {
                    let hash = large.hash.load(Ordering::Relaxed);
                    let MappingRepr::Large(large) =
                        std::mem::replace(&mut self.inner, MappingRepr::Empty)
                    else {
                        unreachable!();
                    };
                    let buf = alloc_small_exact(large.map.into_iter(), hash);
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "count <= SMALL_LIMIT (8) always fits in u8"
                    )]
                    {
                        self.inner = MappingRepr::Small {
                            len: count as u8,
                            buf,
                        };
                    }
                } else {
                    large.map.shrink_to_fit();
                }
            }
        }
    }

    /// How many entries the mapping has room for.
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        match &self.inner {
            MappingRepr::Empty => 0,
            MappingRepr::Small { buf, .. } => buf.entries.len(),
            MappingRepr::Large(large) => large.map.capacity(),
        }
    }

    /// Position of `key` in insertion order, if present.
    pub(crate) fn get_index_of(&self, key: &Value) -> Option<usize> {
        match &self.inner {
            MappingRepr::Empty => None,
            MappingRepr::Small { len, buf } => buf.entries[..usize::from(*len)]
                .iter()
                .position(|(k, _)| k == key),
            MappingRepr::Large(large) => large.map.get_index_of(key),
        }
    }

    #[inline]
    fn small_find_str(entries: &[(Value, Value)], key: &str) -> Option<usize> {
        entries
            .iter()
            .position(|(k, _)| matches!(k, Value::String(text) if text == key))
    }

    /// Number of entries.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        match &self.inner {
            MappingRepr::Empty => 0,
            MappingRepr::Small { len, .. } => usize::from(*len),
            MappingRepr::Large(large) => large.map.len(),
        }
    }

    /// Whether the mapping is empty.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Remove all entries.
    pub fn clear(&mut self) {
        match &mut self.inner {
            MappingRepr::Empty => {}
            MappingRepr::Small { len, buf } => {
                for slot in &mut buf.entries[..usize::from(*len)] {
                    *slot = (Value::Null, Value::Null);
                }
                *len = 0;
                buf.hash.store(0, Ordering::Relaxed);
            }
            MappingRepr::Large(large) => {
                large.map.clear();
                large.hash.store(0, Ordering::Relaxed);
            }
        }
    }

    /// Promote a `Small` mapping into a `Large` `IndexMap` with room for at least `min_cap`
    /// entries, returning a mutable reference to the `LargeMapping`.
    #[cold]
    fn promote_to_large(&mut self, min_cap: usize) -> &mut LargeMapping {
        let prev = std::mem::replace(&mut self.inner, MappingRepr::Empty);
        debug_assert!(!matches!(prev, MappingRepr::Large(_)));
        let mut map = IndexMap::with_capacity_and_hasher(min_cap, GlobalHasher);
        if let MappingRepr::Small { len, mut buf } = prev {
            for slot in &mut buf.entries[..usize::from(len)] {
                let (k, v) = std::mem::replace(slot, (Value::Null, Value::Null));
                map.insert(k, v);
            }
        }
        self.inner = MappingRepr::Large(Box::new(LargeMapping {
            map,
            hash: AtomicU64::new(0),
        }));
        let MappingRepr::Large(large) = &mut self.inner else {
            unreachable!();
        };
        large
    }

    /// Append a key known not to exist in a non-`Large` mapping, growing or promoting as needed,
    /// and return a mutable reference to the inserted value slot.
    fn push_absent_small(&mut self, key: Value, value: Value) -> &mut Value {
        let needs_promote = matches!(
            &self.inner,
            MappingRepr::Small { len, .. } if usize::from(*len) >= SMALL_LIMIT
        );
        if needs_promote {
            let large = self.promote_to_large(SMALL_LIMIT * 2);
            return large.map.entry(key).or_insert(value);
        }
        if matches!(self.inner, MappingRepr::Empty) {
            let mut buf = alloc_small_empty(2);
            buf.entries[0] = (key, value);
            self.inner = MappingRepr::Small { len: 1, buf };
            let MappingRepr::Small { buf, .. } = &mut self.inner else {
                unreachable!();
            };
            return &mut buf.entries[0].1;
        }
        match &mut self.inner {
            MappingRepr::Small { len, buf } => {
                let count = usize::from(*len);
                buf.hash.store(0, Ordering::Relaxed);
                if count >= buf.entries.len() {
                    let new_cap = (buf.entries.len() * 2).min(SMALL_LIMIT);
                    let mut grown = alloc_small_empty(new_cap);
                    for (dst, src) in grown.entries[..count]
                        .iter_mut()
                        .zip(&mut buf.entries[..count])
                    {
                        *dst = std::mem::replace(src, (Value::Null, Value::Null));
                    }
                    *buf = grown;
                }
                buf.entries[count] = (key, value);
                *len += 1;
                &mut buf.entries[count].1
            }
            MappingRepr::Large(large) => {
                large.hash.store(0, Ordering::Relaxed);
                large.map.entry(key).or_insert(value)
            }
            MappingRepr::Empty => unreachable!(),
        }
    }

    /// Insert or replace a value, returning the previous value.
    pub fn insert(&mut self, key: Value, value: Value) -> Option<Value> {
        match &mut self.inner {
            MappingRepr::Empty => {
                let _ = self.push_absent_small(key, value);
                None
            }
            MappingRepr::Small { len, buf } => {
                let count = usize::from(*len);
                buf.hash.store(0, Ordering::Relaxed);
                if let Some((_, slot)) = buf.entries[..count].iter_mut().find(|(k, _)| k == &key) {
                    return Some(std::mem::replace(slot, value));
                }
                let _ = self.push_absent_small(key, value);
                None
            }
            MappingRepr::Large(large) => {
                large.hash.store(0, Ordering::Relaxed);
                large.map.insert(key, value)
            }
        }
    }

    /// Insert a key that the mapping doesn't hold yet. For a key that it holds, return the key's
    /// text and leave the mapping as it was.
    pub(crate) fn insert_new(&mut self, key: Value, value: Value) -> Result<(), String> {
        match &mut self.inner {
            MappingRepr::Empty => {
                let _ = self.push_absent_small(key, value);
                Ok(())
            }
            MappingRepr::Small { len, buf } => {
                let count = usize::from(*len);
                if let Some((existing, _)) = buf.entries[..count].iter().find(|(k, _)| k == &key) {
                    return Err(crate::value::key_text(existing));
                }
                let _ = self.push_absent_small(key, value);
                Ok(())
            }
            MappingRepr::Large(large) => {
                large.hash.store(0, Ordering::Relaxed);
                match large.map.entry(key) {
                    indexmap::map::Entry::Vacant(entry) => {
                        entry.insert(value);
                        Ok(())
                    }
                    indexmap::map::Entry::Occupied(entry) => {
                        Err(crate::value::key_text(entry.key()))
                    }
                }
            }
        }
    }

    /// Insert a key only when the mapping does not already hold it, checking
    /// `limits.max_collection_entries` before growing the mapping.
    pub(crate) fn insert_if_absent_with_limit(
        &mut self,
        key: Value,
        value: Value,
        limits: &crate::limits::Limits,
    ) -> Result<bool, crate::Error> {
        let next_len = self.len().saturating_add(1);
        match &mut self.inner {
            MappingRepr::Empty => {
                limits.check_collection_entries(next_len)?;
                let _ = self.push_absent_small(key, value);
                Ok(true)
            }
            MappingRepr::Small { len, buf } => {
                let count = usize::from(*len);
                if buf.entries[..count].iter().any(|(k, _)| k == &key) {
                    return Ok(false);
                }
                limits.check_collection_entries(next_len)?;
                let _ = self.push_absent_small(key, value);
                Ok(true)
            }
            MappingRepr::Large(large) => match large.map.entry(key) {
                indexmap::map::Entry::Vacant(entry) => {
                    limits.check_collection_entries(next_len)?;
                    entry.insert(value);
                    large.hash.store(0, Ordering::Relaxed);
                    Ok(true)
                }
                indexmap::map::Entry::Occupied(_) => Ok(false),
            },
        }
    }

    /// Get a mutable entry for `key`, inserting lazily through [`Entry`].
    pub fn entry(&mut self, key: Value) -> Entry<'_> {
        let small_pos = match &self.inner {
            MappingRepr::Large(_) => return self.entry_large(key),
            MappingRepr::Empty => None,
            MappingRepr::Small { len, buf } => buf.entries[..usize::from(*len)]
                .iter()
                .position(|(k, _)| k == &key),
        };
        match small_pos {
            Some(index) => Entry {
                inner: EntryInner::SmallOccupied {
                    mapping: self,
                    index,
                },
            },
            None => Entry {
                inner: EntryInner::SmallVacant { mapping: self, key },
            },
        }
    }

    fn entry_large(&mut self, key: Value) -> Entry<'_> {
        let MappingRepr::Large(large) = &mut self.inner else {
            unreachable!();
        };
        let LargeMapping { map, hash } = large.as_mut();
        Entry {
            inner: EntryInner::Large {
                entry: map.entry(key),
                hash,
            },
        }
    }

    /// Look up a value by key.
    #[must_use]
    pub fn get<I: Index>(&self, key: I) -> Option<&Value> {
        Index::get(&key, self)
    }

    /// Look up a value by string key.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&Value> {
        match &self.inner {
            MappingRepr::Empty => None,
            MappingRepr::Small { len, buf } => {
                let entries = &buf.entries[..usize::from(*len)];
                Self::small_find_str(entries, key).map(|idx| &entries[idx].1)
            }
            MappingRepr::Large(large) => large.map.get(&StringKey(key)),
        }
    }

    /// Mutably look up a value by key.
    #[must_use]
    pub fn get_mut<I: Index>(&mut self, key: I) -> Option<&mut Value> {
        Index::get_mut(&key, self)
    }

    /// Mutably look up a value by string key.
    #[must_use]
    pub fn get_str_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.invalidate_hash();
        match &mut self.inner {
            MappingRepr::Empty => None,
            MappingRepr::Small { len, buf } => {
                let entries = &mut buf.entries[..usize::from(*len)];
                Self::small_find_str(entries, key).map(|idx| &mut entries[idx].1)
            }
            MappingRepr::Large(large) => large.map.get_mut(&StringKey(key)),
        }
    }

    /// Whether the mapping contains a key.
    #[must_use]
    pub fn contains_key<I: Index>(&self, key: I) -> bool {
        Index::contains_key(&key, self)
    }

    /// Remove a key, swapping the last entry into its place.
    pub fn remove<I: Index>(&mut self, key: I) -> Option<Value> {
        Index::swap_remove(&key, self)
    }

    /// Remove a key, preserving the relative order of the remaining entries.
    pub fn shift_remove<I: Index>(&mut self, key: I) -> Option<Value> {
        Index::shift_remove(&key, self)
    }

    fn small_swap_remove_at(
        len: &mut u8,
        buf: &mut SmallSlice<[(Value, Value)]>,
        index: usize,
    ) -> Value {
        let last = usize::from(*len) - 1;
        buf.hash.store(0, Ordering::Relaxed);
        if index != last {
            buf.entries.swap(index, last);
        }
        let (_, removed) = std::mem::replace(&mut buf.entries[last], (Value::Null, Value::Null));
        *len -= 1;
        removed
    }

    fn small_shift_remove_at(
        len: &mut u8,
        buf: &mut SmallSlice<[(Value, Value)]>,
        index: usize,
    ) -> Value {
        let count = usize::from(*len);
        buf.hash.store(0, Ordering::Relaxed);
        let (_, removed) = std::mem::replace(&mut buf.entries[index], (Value::Null, Value::Null));
        buf.entries[index..count].rotate_left(1);
        *len -= 1;
        removed
    }

    /// Iterate over entries in insertion order.
    #[must_use]
    #[inline]
    pub fn iter(&self) -> Iter<'_> {
        let inner = match &self.inner {
            MappingRepr::Empty => IterInner::Slice([].iter()),
            MappingRepr::Small { len, buf } => {
                IterInner::Slice(buf.entries[..usize::from(*len)].iter())
            }
            MappingRepr::Large(large) => IterInner::Large(large.map.iter()),
        };
        Iter { inner }
    }

    /// Mutably iterate over entries in insertion order.
    #[inline]
    pub fn iter_mut(&mut self) -> IterMut<'_> {
        self.invalidate_hash();
        let inner = match &mut self.inner {
            MappingRepr::Empty => IterMutInner::Slice([].iter_mut()),
            MappingRepr::Small { len, buf } => {
                IterMutInner::Slice(buf.entries[..usize::from(*len)].iter_mut())
            }
            MappingRepr::Large(large) => IterMutInner::Large(large.map.iter_mut()),
        };
        IterMut { inner }
    }

    /// Iterate over keys in insertion order.
    pub fn keys(&self) -> impl Iterator<Item = &Value> + '_ {
        self.iter().map(|(key, _)| key)
    }

    /// Iterate over values in insertion order.
    pub fn values(&self) -> impl Iterator<Item = &Value> + '_ {
        self.iter().map(|(_, value)| value)
    }

    /// Mutably iterate over values in insertion order.
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Value> + '_ {
        self.iter_mut().map(|(_, value)| value)
    }

    /// Get an entry by insertion index.
    #[must_use]
    pub fn get_index(&self, index: usize) -> Option<(&Value, &Value)> {
        match &self.inner {
            MappingRepr::Empty => None,
            MappingRepr::Small { len, buf } => buf.entries[..usize::from(*len)]
                .get(index)
                .map(|(k, v)| (k, v)),
            MappingRepr::Large(large) => large.map.get_index(index),
        }
    }
}

enum EntryInner<'a> {
    SmallOccupied {
        mapping: &'a mut Mapping,
        index: usize,
    },
    SmallVacant {
        mapping: &'a mut Mapping,
        key: Value,
    },
    Large {
        entry: indexmap::map::Entry<'a, Value, Value>,
        hash: &'a mut AtomicU64,
    },
}

impl fmt::Debug for EntryInner<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct OccupiedDebug<'a>(&'a Value, &'a Value);
        impl fmt::Debug for OccupiedDebug<'_> {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct("OccupiedEntry")
                    .field("key", self.0)
                    .field("value", self.1)
                    .finish()
            }
        }
        struct VacantDebug<'a>(&'a Value);
        impl fmt::Debug for VacantDebug<'_> {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct("VacantEntry")
                    .field("key", self.0)
                    .finish()
            }
        }
        match self {
            Self::SmallOccupied { mapping, index } => {
                let (key, value) = mapping
                    .get_index(*index)
                    .unwrap_or((&NULL_VALUE, &NULL_VALUE));
                formatter
                    .debug_tuple("Occupied")
                    .field(&OccupiedDebug(key, value))
                    .finish()
            }
            Self::SmallVacant { key, .. } => formatter
                .debug_tuple("Vacant")
                .field(&VacantDebug(key))
                .finish(),
            Self::Large { entry, .. } => fmt::Debug::fmt(entry, formatter),
        }
    }
}

/// A view into one mapping slot, mirroring the `indexmap` entry API.
#[derive(Debug)]
pub struct Entry<'a> {
    inner: EntryInner<'a>,
}

impl<'a> Entry<'a> {
    /// Insert `default` if the key is absent, and return the slot.
    #[must_use]
    pub fn or_insert(self, default: Value) -> &'a mut Value {
        match self.inner {
            EntryInner::SmallOccupied { mapping, index } => {
                mapping.invalidate_hash();
                let MappingRepr::Small { buf, .. } = &mut mapping.inner else {
                    unreachable!();
                };
                &mut buf.entries[index].1
            }
            EntryInner::SmallVacant { mapping, key } => mapping.push_absent_small(key, default),
            EntryInner::Large { entry, hash } => {
                hash.store(0, Ordering::Relaxed);
                entry.or_insert(default)
            }
        }
    }

    /// Insert the result of `default` if the key is absent, and return the slot.
    pub fn or_insert_with<F: FnOnce() -> Value>(self, default: F) -> &'a mut Value {
        match self.inner {
            EntryInner::SmallOccupied { mapping, index } => {
                mapping.invalidate_hash();
                let MappingRepr::Small { buf, .. } = &mut mapping.inner else {
                    unreachable!();
                };
                &mut buf.entries[index].1
            }
            EntryInner::SmallVacant { mapping, key } => mapping.push_absent_small(key, default()),
            EntryInner::Large { entry, hash } => {
                hash.store(0, Ordering::Relaxed);
                entry.or_insert_with(default)
            }
        }
    }

    /// Mutate the value if the key is present, and return the entry.
    #[must_use]
    pub fn and_modify<F: FnOnce(&mut Value)>(mut self, f: F) -> Self {
        match &mut self.inner {
            EntryInner::SmallOccupied { mapping, index } => {
                mapping.invalidate_hash();
                let MappingRepr::Small { buf, .. } = &mut mapping.inner else {
                    unreachable!();
                };
                f(&mut buf.entries[*index].1);
            }
            EntryInner::SmallVacant { .. } => {}
            EntryInner::Large { entry, hash } => {
                hash.store(0, Ordering::Relaxed);
                if let indexmap::map::Entry::Occupied(entry) = entry {
                    f(entry.get_mut());
                }
            }
        }
        self
    }
}

impl fmt::Debug for Mapping {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Mapping ")?;
        formatter.debug_map().entries(self.iter()).finish()
    }
}

impl Hash for Mapping {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self.inner {
            MappingRepr::Empty => 0_u64.hash(state),
            MappingRepr::Small { len, buf } => {
                let cached = buf.hash.load(Ordering::Relaxed);
                if cached != 0 {
                    cached.hash(state);
                    return;
                }
                let hash =
                    hash_of_entries(buf.entries[..usize::from(*len)].iter().map(|(k, v)| (k, v)));
                if hash != 0 {
                    buf.hash.store(hash, Ordering::Relaxed);
                }
                hash.hash(state);
            }
            MappingRepr::Large(large) => {
                let cached = large.hash.load(Ordering::Relaxed);
                if cached != 0 {
                    cached.hash(state);
                    return;
                }
                let hash = hash_of_entries(large.map.iter());
                if hash != 0 {
                    large.hash.store(hash, Ordering::Relaxed);
                }
                hash.hash(state);
            }
        }
    }
}

/// A hash of a mapping's entries that doesn't depend on their order, as Eq doesn't. XOR gives that,
/// but whoever can compute an entry's hash can solve for entries that cancel out, and so write many
/// mappings with one hash, as keys that take quadratic time. So the key is random. Two entries can
/// still cancel if they write the same bytes, so what a key and value write has to tell them apart.
pub(crate) fn hash_of_entries<'a, K, V>(entries: impl Iterator<Item = (&'a K, &'a V)>) -> u64
where
    K: Hash + 'a,
    V: Hash + 'a,
{
    static ENTRY_HASH: OnceLock<RandomState> = OnceLock::new();
    let entry_hash = ENTRY_HASH.get_or_init(RandomState::new);
    let mut xor = 0_u64;
    for (key, value) in entries {
        let mut hasher = entry_hash.build_hasher();
        key.hash(&mut hasher);
        value.hash(&mut hasher);
        xor ^= hasher.finish();
    }
    xor
}

impl OpsIndex<&str> for Mapping {
    type Output = Value;

    fn index(&self, index: &str) -> &Self::Output {
        self.get_str(index).unwrap_or(&NULL_VALUE)
    }
}

impl OpsIndex<String> for Mapping {
    type Output = Value;

    fn index(&self, index: String) -> &Self::Output {
        self.get_str(&index).unwrap_or(&NULL_VALUE)
    }
}

impl OpsIndex<&String> for Mapping {
    type Output = Value;

    fn index(&self, index: &String) -> &Self::Output {
        self.get_str(index).unwrap_or(&NULL_VALUE)
    }
}

impl OpsIndex<&Value> for Mapping {
    type Output = Value;

    fn index(&self, index: &Value) -> &Self::Output {
        self.get(index).unwrap_or(&NULL_VALUE)
    }
}

mod private {
    pub trait Sealed {}
}

impl private::Sealed for Value {}

impl Index for Value {
    fn contains_key(&self, mapping: &Mapping) -> bool {
        match &mapping.inner {
            MappingRepr::Empty => false,
            MappingRepr::Small { len, buf } => buf.entries[..usize::from(*len)]
                .iter()
                .any(|(k, _)| k == self),
            MappingRepr::Large(large) => large.map.contains_key(self),
        }
    }

    fn get<'a>(&self, mapping: &'a Mapping) -> Option<&'a Value> {
        match &mapping.inner {
            MappingRepr::Empty => None,
            MappingRepr::Small { len, buf } => buf.entries[..usize::from(*len)]
                .iter()
                .find(|(k, _)| k == self)
                .map(|(_, v)| v),
            MappingRepr::Large(large) => large.map.get(self),
        }
    }

    fn get_mut<'a>(&self, mapping: &'a mut Mapping) -> Option<&'a mut Value> {
        mapping.invalidate_hash();
        match &mut mapping.inner {
            MappingRepr::Empty => None,
            MappingRepr::Small { len, buf } => buf.entries[..usize::from(*len)]
                .iter_mut()
                .find(|(k, _)| k == self)
                .map(|(_, v)| v),
            MappingRepr::Large(large) => large.map.get_mut(self),
        }
    }

    fn swap_remove(&self, mapping: &mut Mapping) -> Option<Value> {
        match &mut mapping.inner {
            MappingRepr::Empty => None,
            MappingRepr::Small { len, buf } => {
                let idx = buf.entries[..usize::from(*len)]
                    .iter()
                    .position(|(k, _)| k == self)?;
                Some(Mapping::small_swap_remove_at(len, buf, idx))
            }
            MappingRepr::Large(large) => {
                large.hash.store(0, Ordering::Relaxed);
                large.map.swap_remove(self)
            }
        }
    }

    fn shift_remove(&self, mapping: &mut Mapping) -> Option<Value> {
        match &mut mapping.inner {
            MappingRepr::Empty => None,
            MappingRepr::Small { len, buf } => {
                let idx = buf.entries[..usize::from(*len)]
                    .iter()
                    .position(|(k, _)| k == self)?;
                Some(Mapping::small_shift_remove_at(len, buf, idx))
            }
            MappingRepr::Large(large) => {
                large.hash.store(0, Ordering::Relaxed);
                large.map.shift_remove(self)
            }
        }
    }
}

impl private::Sealed for str {}

impl Index for str {
    fn contains_key(&self, mapping: &Mapping) -> bool {
        match &mapping.inner {
            MappingRepr::Empty => false,
            MappingRepr::Small { len, buf } => {
                Mapping::small_find_str(&buf.entries[..usize::from(*len)], self).is_some()
            }
            MappingRepr::Large(large) => large.map.contains_key(&StringKey(self)),
        }
    }

    fn get<'a>(&self, mapping: &'a Mapping) -> Option<&'a Value> {
        mapping.get_str(self)
    }

    fn get_mut<'a>(&self, mapping: &'a mut Mapping) -> Option<&'a mut Value> {
        mapping.get_str_mut(self)
    }

    fn swap_remove(&self, mapping: &mut Mapping) -> Option<Value> {
        match &mut mapping.inner {
            MappingRepr::Empty => None,
            MappingRepr::Small { len, buf } => {
                let idx = Mapping::small_find_str(&buf.entries[..usize::from(*len)], self)?;
                Some(Mapping::small_swap_remove_at(len, buf, idx))
            }
            MappingRepr::Large(large) => {
                large.hash.store(0, Ordering::Relaxed);
                large.map.swap_remove(&StringKey(self))
            }
        }
    }

    fn shift_remove(&self, mapping: &mut Mapping) -> Option<Value> {
        match &mut mapping.inner {
            MappingRepr::Empty => None,
            MappingRepr::Small { len, buf } => {
                let idx = Mapping::small_find_str(&buf.entries[..usize::from(*len)], self)?;
                Some(Mapping::small_shift_remove_at(len, buf, idx))
            }
            MappingRepr::Large(large) => {
                large.hash.store(0, Ordering::Relaxed);
                large.map.shift_remove(&StringKey(self))
            }
        }
    }
}

impl private::Sealed for String {}

impl Index for String {
    fn contains_key(&self, mapping: &Mapping) -> bool {
        <str as Index>::contains_key(self.as_str(), mapping)
    }

    fn get<'a>(&self, mapping: &'a Mapping) -> Option<&'a Value> {
        <str as Index>::get(self.as_str(), mapping)
    }

    fn get_mut<'a>(&self, mapping: &'a mut Mapping) -> Option<&'a mut Value> {
        <str as Index>::get_mut(self.as_str(), mapping)
    }

    fn swap_remove(&self, mapping: &mut Mapping) -> Option<Value> {
        <str as Index>::swap_remove(self.as_str(), mapping)
    }

    fn shift_remove(&self, mapping: &mut Mapping) -> Option<Value> {
        <str as Index>::shift_remove(self.as_str(), mapping)
    }
}

impl<T> private::Sealed for &T where T: ?Sized + Index {}

impl<T> Index for &T
where
    T: ?Sized + Index,
{
    fn contains_key(&self, mapping: &Mapping) -> bool {
        T::contains_key(*self, mapping)
    }

    fn get<'a>(&self, mapping: &'a Mapping) -> Option<&'a Value> {
        T::get(*self, mapping)
    }

    fn get_mut<'a>(&self, mapping: &'a mut Mapping) -> Option<&'a mut Value> {
        T::get_mut(*self, mapping)
    }

    fn swap_remove(&self, mapping: &mut Mapping) -> Option<Value> {
        T::swap_remove(*self, mapping)
    }

    fn shift_remove(&self, mapping: &mut Mapping) -> Option<Value> {
        T::shift_remove(*self, mapping)
    }
}

impl FromIterator<(Value, Value)> for Mapping {
    fn from_iter<T: IntoIterator<Item = (Value, Value)>>(iter: T) -> Self {
        let iter = iter.into_iter();
        let (lower, _) = iter.size_hint();
        let mut mapping = Self::with_capacity(lower);
        mapping.extend(iter);
        mapping
    }
}

impl Extend<(Value, Value)> for Mapping {
    fn extend<T: IntoIterator<Item = (Value, Value)>>(&mut self, iter: T) {
        let iter = iter.into_iter();
        let (lower, _) = iter.size_hint();
        if self.len().saturating_add(lower) > SMALL_LIMIT
            && !matches!(self.inner, MappingRepr::Large(_))
        {
            let target_cap = self.len().saturating_add(lower);
            let _ = self.promote_to_large(target_cap);
        }
        match &mut self.inner {
            MappingRepr::Large(large) => {
                large.hash.store(0, Ordering::Relaxed);
                large.map.extend(iter);
            }
            _ => {
                for (key, value) in iter {
                    drop(self.insert(key, value));
                }
            }
        }
    }
}

#[derive(Clone)]
enum IterInner<'a> {
    Slice(std::slice::Iter<'a, (Value, Value)>),
    Large(indexmap::map::Iter<'a, Value, Value>),
}

impl Default for IterInner<'_> {
    fn default() -> Self {
        Self::Slice([].iter())
    }
}

/// An iterator over a mapping's entries in insertion order, by reference. `&Mapping` and
/// [`Mapping::iter`] give one.
#[derive(Clone, Default)]
pub struct Iter<'a> {
    inner: IterInner<'a>,
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a Value, &'a Value);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IterInner::Slice(iter) => iter.next().map(|(k, v)| (k, v)),
            IterInner::Large(iter) => iter.next(),
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.inner {
            IterInner::Slice(iter) => iter.size_hint(),
            IterInner::Large(iter) => iter.size_hint(),
        }
    }

    #[inline]
    fn count(self) -> usize {
        self.len()
    }

    #[inline]
    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        match &mut self.inner {
            IterInner::Slice(iter) => iter.nth(n).map(|(k, v)| (k, v)),
            IterInner::Large(iter) => iter.nth(n),
        }
    }

    #[inline]
    fn last(mut self) -> Option<Self::Item> {
        self.next_back()
    }

    #[inline]
    fn fold<B, F>(self, init: B, mut f: F) -> B
    where
        F: FnMut(B, Self::Item) -> B,
    {
        match self.inner {
            IterInner::Slice(iter) => iter.fold(init, |acc, (k, v)| f(acc, (k, v))),
            IterInner::Large(iter) => iter.fold(init, f),
        }
    }

    #[inline]
    fn collect<C: FromIterator<Self::Item>>(self) -> C {
        match self.inner {
            IterInner::Slice(iter) => iter.map(|(k, v)| (k, v)).collect(),
            IterInner::Large(iter) => iter.collect(),
        }
    }
}

#[allow(
    clippy::elidable_lifetime_names,
    reason = "preserves the exact v0.2.3 public-api impl<'a> signature"
)]
impl<'a> DoubleEndedIterator for Iter<'a> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IterInner::Slice(iter) => iter.next_back().map(|(k, v)| (k, v)),
            IterInner::Large(iter) => iter.next_back(),
        }
    }

    #[inline]
    fn nth_back(&mut self, n: usize) -> Option<Self::Item> {
        match &mut self.inner {
            IterInner::Slice(iter) => iter.nth_back(n).map(|(k, v)| (k, v)),
            IterInner::Large(iter) => iter.nth_back(n),
        }
    }

    #[inline]
    fn rfold<B, F>(self, init: B, mut f: F) -> B
    where
        F: FnMut(B, Self::Item) -> B,
    {
        match self.inner {
            IterInner::Slice(iter) => iter.rfold(init, |acc, (k, v)| f(acc, (k, v))),
            IterInner::Large(iter) => iter.rfold(init, f),
        }
    }
}

#[allow(
    clippy::elidable_lifetime_names,
    reason = "preserves the exact v0.2.3 public-api impl<'a> signature"
)]
impl<'a> ExactSizeIterator for Iter<'a> {
    #[inline]
    fn len(&self) -> usize {
        match &self.inner {
            IterInner::Slice(iter) => iter.len(),
            IterInner::Large(iter) => iter.len(),
        }
    }
}

#[allow(
    clippy::elidable_lifetime_names,
    reason = "preserves the exact v0.2.3 public-api impl<'a> signature"
)]
impl<'a> FusedIterator for Iter<'a> {}

#[allow(
    clippy::elidable_lifetime_names,
    reason = "preserves the exact v0.2.3 public-api impl<'a> signature"
)]
impl<'a> fmt::Debug for Iter<'a> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner {
            IterInner::Slice(iter) => formatter
                .debug_list()
                .entries(iter.as_slice().iter().map(|(k, v)| (k, v)))
                .finish(),
            IterInner::Large(iter) => fmt::Debug::fmt(iter, formatter),
        }
    }
}

enum IterMutInner<'a> {
    Slice(std::slice::IterMut<'a, (Value, Value)>),
    Large(indexmap::map::IterMut<'a, Value, Value>),
}

impl Default for IterMutInner<'_> {
    fn default() -> Self {
        Self::Slice([].iter_mut())
    }
}

/// An iterator over a mapping's entries in insertion order, with each value mutable.
/// `&mut Mapping` and [`Mapping::iter_mut`] give one.
#[derive(Default)]
pub struct IterMut<'a> {
    inner: IterMutInner<'a>,
}

impl<'a> Iterator for IterMut<'a> {
    type Item = (&'a Value, &'a mut Value);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IterMutInner::Slice(iter) => iter.next().map(|(k, v)| (&*k, v)),
            IterMutInner::Large(iter) => iter.next(),
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.inner {
            IterMutInner::Slice(iter) => iter.size_hint(),
            IterMutInner::Large(iter) => iter.size_hint(),
        }
    }

    #[inline]
    fn count(self) -> usize {
        self.len()
    }

    #[inline]
    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        match &mut self.inner {
            IterMutInner::Slice(iter) => iter.nth(n).map(|(k, v)| (&*k, v)),
            IterMutInner::Large(iter) => iter.nth(n),
        }
    }

    #[inline]
    fn last(mut self) -> Option<Self::Item> {
        self.next_back()
    }

    #[inline]
    fn fold<B, F>(self, init: B, mut f: F) -> B
    where
        F: FnMut(B, Self::Item) -> B,
    {
        match self.inner {
            IterMutInner::Slice(iter) => iter.fold(init, |acc, (k, v)| f(acc, (&*k, v))),
            IterMutInner::Large(iter) => iter.fold(init, f),
        }
    }

    #[inline]
    fn collect<C: FromIterator<Self::Item>>(self) -> C {
        match self.inner {
            IterMutInner::Slice(iter) => iter.map(|(k, v)| (&*k, v)).collect(),
            IterMutInner::Large(iter) => iter.collect(),
        }
    }
}

#[allow(
    clippy::elidable_lifetime_names,
    reason = "preserves the exact v0.2.3 public-api impl<'a> signature"
)]
impl<'a> DoubleEndedIterator for IterMut<'a> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IterMutInner::Slice(iter) => iter.next_back().map(|(k, v)| (&*k, v)),
            IterMutInner::Large(iter) => iter.next_back(),
        }
    }

    #[inline]
    fn nth_back(&mut self, n: usize) -> Option<Self::Item> {
        match &mut self.inner {
            IterMutInner::Slice(iter) => iter.nth_back(n).map(|(k, v)| (&*k, v)),
            IterMutInner::Large(iter) => iter.nth_back(n),
        }
    }

    #[inline]
    fn rfold<B, F>(self, init: B, mut f: F) -> B
    where
        F: FnMut(B, Self::Item) -> B,
    {
        match self.inner {
            IterMutInner::Slice(iter) => iter.rfold(init, |acc, (k, v)| f(acc, (&*k, v))),
            IterMutInner::Large(iter) => iter.rfold(init, f),
        }
    }
}

#[allow(
    clippy::elidable_lifetime_names,
    reason = "preserves the exact v0.2.3 public-api impl<'a> signature"
)]
impl<'a> ExactSizeIterator for IterMut<'a> {
    #[inline]
    fn len(&self) -> usize {
        match &self.inner {
            IterMutInner::Slice(iter) => iter.len(),
            IterMutInner::Large(iter) => iter.len(),
        }
    }
}

#[allow(
    clippy::elidable_lifetime_names,
    reason = "preserves the exact v0.2.3 public-api impl<'a> signature"
)]
impl<'a> FusedIterator for IterMut<'a> {}

#[allow(
    clippy::elidable_lifetime_names,
    reason = "preserves the exact v0.2.3 public-api impl<'a> signature"
)]
impl<'a> fmt::Debug for IterMut<'a> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner {
            IterMutInner::Slice(iter) => formatter
                .debug_list()
                .entries(iter.as_slice().iter().map(|(k, v)| (k, v)))
                .finish(),
            IterMutInner::Large(iter) => fmt::Debug::fmt(iter, formatter),
        }
    }
}

#[derive(Default)]
enum IntoIterInner {
    #[default]
    Empty,
    Small {
        buf: Box<SmallSlice<[(Value, Value)]>>,
        front: usize,
        back: usize,
    },
    Large(indexmap::map::IntoIter<Value, Value>),
}

impl Clone for IntoIterInner {
    fn clone(&self) -> Self {
        match self {
            Self::Empty => Self::Empty,
            Self::Small { buf, front, back } => {
                if front >= back {
                    Self::Empty
                } else {
                    let slice = &buf.entries[*front..*back];
                    let cloned = alloc_small_exact(slice.iter().cloned(), 0);
                    Self::Small {
                        buf: cloned,
                        front: 0,
                        back: slice.len(),
                    }
                }
            }
            Self::Large(iter) => Self::Large(iter.clone()),
        }
    }
}

/// An iterator that takes a mapping's entries in insertion order. `Mapping` gives one.
#[derive(Clone, Default)]
pub struct IntoIter {
    inner: IntoIterInner,
}

impl Iterator for IntoIter {
    type Item = (Value, Value);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IntoIterInner::Empty => None,
            IntoIterInner::Small { buf, front, back } => {
                if *front < *back {
                    let item =
                        std::mem::replace(&mut buf.entries[*front], (Value::Null, Value::Null));
                    *front += 1;
                    Some(item)
                } else {
                    None
                }
            }
            IntoIterInner::Large(iter) => iter.next(),
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.len();
        (remaining, Some(remaining))
    }

    #[inline]
    fn count(self) -> usize {
        self.len()
    }

    #[inline]
    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        match &mut self.inner {
            IntoIterInner::Empty => None,
            IntoIterInner::Small { buf, front, back } => {
                let remaining = back.saturating_sub(*front);
                if n >= remaining {
                    for slot in &mut buf.entries[*front..*back] {
                        *slot = (Value::Null, Value::Null);
                    }
                    *front = *back;
                    return None;
                }
                for slot in &mut buf.entries[*front..*front + n] {
                    *slot = (Value::Null, Value::Null);
                }
                *front += n;
                self.next()
            }
            IntoIterInner::Large(iter) => iter.nth(n),
        }
    }

    #[inline]
    fn last(mut self) -> Option<Self::Item> {
        self.next_back()
    }

    #[inline]
    fn collect<C: FromIterator<Self::Item>>(self) -> C {
        match self.inner {
            IntoIterInner::Empty => std::iter::empty().collect(),
            IntoIterInner::Small {
                mut buf,
                front,
                back,
            } => buf.entries[front..back]
                .iter_mut()
                .map(|slot| std::mem::replace(slot, (Value::Null, Value::Null)))
                .collect(),
            IntoIterInner::Large(iter) => iter.collect(),
        }
    }
}

impl DoubleEndedIterator for IntoIter {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IntoIterInner::Empty => None,
            IntoIterInner::Small { buf, front, back } => {
                if *front < *back {
                    *back -= 1;
                    Some(std::mem::replace(
                        &mut buf.entries[*back],
                        (Value::Null, Value::Null),
                    ))
                } else {
                    None
                }
            }
            IntoIterInner::Large(iter) => iter.next_back(),
        }
    }

    #[inline]
    fn nth_back(&mut self, n: usize) -> Option<Self::Item> {
        match &mut self.inner {
            IntoIterInner::Empty => None,
            IntoIterInner::Small { buf, front, back } => {
                let remaining = back.saturating_sub(*front);
                if n >= remaining {
                    for slot in &mut buf.entries[*front..*back] {
                        *slot = (Value::Null, Value::Null);
                    }
                    *back = *front;
                    return None;
                }
                let new_back = *back - n;
                for slot in &mut buf.entries[new_back..*back] {
                    *slot = (Value::Null, Value::Null);
                }
                *back = new_back;
                self.next_back()
            }
            IntoIterInner::Large(iter) => iter.nth_back(n),
        }
    }
}

impl ExactSizeIterator for IntoIter {
    #[inline]
    fn len(&self) -> usize {
        match &self.inner {
            IntoIterInner::Empty => 0,
            IntoIterInner::Small { front, back, .. } => back.saturating_sub(*front),
            IntoIterInner::Large(iter) => iter.len(),
        }
    }
}

impl FusedIterator for IntoIter {}

impl fmt::Debug for IntoIter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner {
            IntoIterInner::Empty => formatter.debug_list().finish(),
            IntoIterInner::Small { buf, front, back } => formatter
                .debug_list()
                .entries(buf.entries[*front..*back].iter())
                .finish(),
            IntoIterInner::Large(iter) => fmt::Debug::fmt(iter, formatter),
        }
    }
}

impl IntoIterator for Mapping {
    type Item = (Value, Value);
    type IntoIter = IntoIter;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        let inner = match self.inner {
            MappingRepr::Empty | MappingRepr::Small { len: 0, .. } => IntoIterInner::Empty,
            MappingRepr::Small { len, buf } => IntoIterInner::Small {
                buf,
                front: 0,
                back: usize::from(len),
            },
            MappingRepr::Large(large) => IntoIterInner::Large(large.map.into_iter()),
        };
        IntoIter { inner }
    }
}

impl<'a> IntoIterator for &'a Mapping {
    type Item = (&'a Value, &'a Value);
    type IntoIter = Iter<'a>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut Mapping {
    type Item = (&'a Value, &'a mut Value);
    type IntoIter = IterMut<'a>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

#[cfg(test)]
mod tests;
