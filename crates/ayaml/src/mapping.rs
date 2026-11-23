//! An insertion-ordered YAML mapping.
//!
//! The mapping is backed by `indexmap`, and no type of that crate is part of this one's API:
//! a mapping iterates through [`Iter`], [`IterMut`], and [`IntoIter`]. Through 0.1.x the
//! `IntoIterator` implementations named `indexmap`'s own iterator types, which made that
//! crate's major version part of this one's.

use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{BuildHasher, Hash, Hasher, RandomState};
use std::iter::FusedIterator;
use std::ops::Index as OpsIndex;
use std::sync::OnceLock;

use indexmap::{Equivalent, IndexMap};

/// A zero-sized hasher that uses one process-wide random seed, leaving room for the hash cache.
#[derive(Clone, Default)]
struct GlobalHasher;

impl BuildHasher for GlobalHasher {
    type Hasher = DefaultHasher;

    fn build_hasher(&self) -> Self::Hasher {
        static STATE: OnceLock<RandomState> = OnceLock::new();
        STATE.get_or_init(RandomState::new).build_hasher()
    }
}

use crate::value::{NULL_VALUE, Value};

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

/// A YAML mapping with insertion-order iteration and swap-remove semantics.
#[derive(Clone, Default)]
pub struct Mapping {
    map: IndexMap<Value, Value, GlobalHasher>,
    /// The order-independent hash, cached until the mapping changes. A mapping used as a key
    /// would otherwise hash every nested entry at each duplicate check.
    hash: OnceLock<u64>,
}

impl PartialEq for Mapping {
    fn eq(&self, other: &Self) -> bool {
        self.map == other.map
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
        Self {
            map: IndexMap::with_capacity_and_hasher(capacity, GlobalHasher),
            hash: OnceLock::new(),
        }
    }

    /// Give back the room that the entries don't need. A mapping that grew as its entries
    /// arrived can hold room for nearly twice as many.
    pub fn shrink_to_fit(&mut self) {
        self.map.shrink_to_fit();
    }

    /// How many entries the mapping has room for.
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.map.capacity()
    }

    /// Position of `key` in insertion order, if present.
    pub(crate) fn get_index_of(&self, key: &Value) -> Option<usize> {
        self.map.get_index_of(key)
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the mapping is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Remove all entries.
    pub fn clear(&mut self) {
        self.map.clear();
        self.hash.take();
    }

    /// Insert or replace a value, returning the previous value.
    pub fn insert(&mut self, key: Value, value: Value) -> Option<Value> {
        self.hash.take();
        self.map.insert(key, value)
    }

    /// Insert a key that the mapping doesn't hold yet. For a key that it holds, return the key's
    /// text and leave the mapping as it was.
    pub(crate) fn insert_new(&mut self, key: Value, value: Value) -> Result<(), String> {
        self.hash.take();
        match self.map.entry(key) {
            indexmap::map::Entry::Vacant(entry) => {
                entry.insert(value);
                Ok(())
            }
            indexmap::map::Entry::Occupied(entry) => Err(crate::value::key_text(entry.key())),
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
        let next_len = self.map.len().saturating_add(1);
        match self.map.entry(key) {
            indexmap::map::Entry::Vacant(entry) => {
                limits.check_collection_entries(next_len)?;
                entry.insert(value);
                self.hash.take();
                Ok(true)
            }
            indexmap::map::Entry::Occupied(_) => Ok(false),
        }
    }

    /// Get a mutable entry for `key`, inserting lazily through [`Entry`].
    pub fn entry(&mut self, key: Value) -> Entry<'_> {
        let Self { map, hash } = self;
        Entry {
            inner: map.entry(key),
            hash,
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
        self.map.get(&StringKey(key))
    }

    /// Mutably look up a value by key.
    #[must_use]
    pub fn get_mut<I: Index>(&mut self, key: I) -> Option<&mut Value> {
        Index::get_mut(&key, self)
    }

    /// Mutably look up a value by string key.
    #[must_use]
    pub fn get_str_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.hash.take();
        self.map.get_mut(&StringKey(key))
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

    /// Iterate over entries in insertion order.
    #[inline]
    pub fn iter(&self) -> Iter<'_> {
        Iter {
            inner: self.map.iter(),
        }
    }

    /// Mutably iterate over entries in insertion order.
    #[inline]
    pub fn iter_mut(&mut self) -> IterMut<'_> {
        self.hash.take();
        IterMut {
            inner: self.map.iter_mut(),
        }
    }

    /// Iterate over keys in insertion order.
    pub fn keys(&self) -> impl Iterator<Item = &Value> + '_ {
        self.map.keys()
    }

    /// Iterate over values in insertion order.
    pub fn values(&self) -> impl Iterator<Item = &Value> + '_ {
        self.map.values()
    }

    /// Mutably iterate over values in insertion order.
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Value> + '_ {
        self.hash.take();
        self.map.values_mut()
    }

    /// Get an entry by insertion index.
    #[must_use]
    pub fn get_index(&self, index: usize) -> Option<(&Value, &Value)> {
        self.map.get_index(index)
    }
}

/// A view into one mapping slot, mirroring the `indexmap` entry API.
#[derive(Debug)]
pub struct Entry<'a> {
    inner: indexmap::map::Entry<'a, Value, Value>,
    hash: &'a mut OnceLock<u64>,
}

impl<'a> Entry<'a> {
    /// Insert `default` if the key is absent, and return the slot.
    #[must_use]
    pub fn or_insert(self, default: Value) -> &'a mut Value {
        self.hash.take();
        self.inner.or_insert(default)
    }

    /// Insert the result of `default` if the key is absent, and return the slot.
    pub fn or_insert_with<F: FnOnce() -> Value>(self, default: F) -> &'a mut Value {
        self.hash.take();
        self.inner.or_insert_with(default)
    }

    /// Mutate the value if the key is present, and return the entry.
    #[must_use]
    pub fn and_modify<F: FnOnce(&mut Value)>(mut self, f: F) -> Self {
        self.hash.take();
        if let indexmap::map::Entry::Occupied(entry) = &mut self.inner {
            f(entry.get_mut());
        }
        self
    }
}

impl fmt::Debug for Mapping {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Mapping ")?;
        formatter.debug_map().entries(self.map.iter()).finish()
    }
}

impl Hash for Mapping {
    fn hash<H: Hasher>(&self, state: &mut H) {
        if let Some(hash) = self.hash.get() {
            hash.hash(state);
            return;
        }
        let hash = hash_of_entries(self.map.iter());
        let _ = self.hash.set(hash);
        hash.hash(state);
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
        mapping.map.contains_key(self)
    }

    fn get<'a>(&self, mapping: &'a Mapping) -> Option<&'a Value> {
        mapping.map.get(self)
    }

    fn get_mut<'a>(&self, mapping: &'a mut Mapping) -> Option<&'a mut Value> {
        mapping.hash.take();
        mapping.map.get_mut(self)
    }

    fn swap_remove(&self, mapping: &mut Mapping) -> Option<Value> {
        mapping.hash.take();
        mapping.map.swap_remove(self)
    }

    fn shift_remove(&self, mapping: &mut Mapping) -> Option<Value> {
        mapping.hash.take();
        mapping.map.shift_remove(self)
    }
}

impl private::Sealed for str {}

impl Index for str {
    fn contains_key(&self, mapping: &Mapping) -> bool {
        mapping.map.contains_key(&StringKey(self))
    }

    fn get<'a>(&self, mapping: &'a Mapping) -> Option<&'a Value> {
        mapping.map.get(&StringKey(self))
    }

    fn get_mut<'a>(&self, mapping: &'a mut Mapping) -> Option<&'a mut Value> {
        mapping.hash.take();
        mapping.map.get_mut(&StringKey(self))
    }

    fn swap_remove(&self, mapping: &mut Mapping) -> Option<Value> {
        mapping.hash.take();
        mapping.map.swap_remove(&StringKey(self))
    }

    fn shift_remove(&self, mapping: &mut Mapping) -> Option<Value> {
        mapping.hash.take();
        mapping.map.shift_remove(&StringKey(self))
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
        Self {
            map: iter.into_iter().collect(),
            hash: OnceLock::new(),
        }
    }
}

impl Extend<(Value, Value)> for Mapping {
    fn extend<T: IntoIterator<Item = (Value, Value)>>(&mut self, iter: T) {
        self.hash.take();
        self.map.extend(iter);
    }
}

/// An iterator over a mapping's entries in insertion order, by reference. `&Mapping` and
/// [`Mapping::iter`] give one.
#[derive(Clone, Default)]
pub struct Iter<'a> {
    inner: indexmap::map::Iter<'a, Value, Value>,
}

/// An iterator over a mapping's entries in insertion order, with each value mutable.
/// `&mut Mapping` and [`Mapping::iter_mut`] give one.
#[derive(Default)]
pub struct IterMut<'a> {
    inner: indexmap::map::IterMut<'a, Value, Value>,
}

/// An iterator that takes a mapping's entries in insertion order. `Mapping` gives one.
#[derive(Clone, Default)]
pub struct IntoIter {
    inner: indexmap::map::IntoIter<Value, Value>,
}

/// Give a mapping iterator what the iterator inside it has: both ends, an exact length, an end
/// that stays an end, and the entries it has left to print. Each can also be made empty, with
/// `Default`, and the two that don't lend a value mutably can be cloned.
///
/// Each method only hands on to the iterator inside, and is marked `#[inline]` so that a loop
/// in another crate costs what a loop over that iterator costs: these types have no type
/// parameter, so without the mark their methods are compiled once, here, and called. `collect`
/// is handed on too, for the same reason and because the inner iterator has a faster one.
macro_rules! mapping_iterator {
    ($name:ident $(<$lifetime:lifetime>)?, $item:ty) => {
        impl$(<$lifetime>)? Iterator for $name$(<$lifetime>)? {
            type Item = $item;

            #[inline]
            fn next(&mut self) -> Option<Self::Item> {
                self.inner.next()
            }

            #[inline]
            fn size_hint(&self) -> (usize, Option<usize>) {
                self.inner.size_hint()
            }

            #[inline]
            fn count(self) -> usize {
                self.inner.len()
            }

            #[inline]
            fn nth(&mut self, n: usize) -> Option<Self::Item> {
                self.inner.nth(n)
            }

            #[inline]
            fn last(mut self) -> Option<Self::Item> {
                self.inner.next_back()
            }

            #[inline]
            fn collect<C: FromIterator<Self::Item>>(self) -> C {
                self.inner.collect()
            }
        }

        impl$(<$lifetime>)? DoubleEndedIterator for $name$(<$lifetime>)? {
            #[inline]
            fn next_back(&mut self) -> Option<Self::Item> {
                self.inner.next_back()
            }

            #[inline]
            fn nth_back(&mut self, n: usize) -> Option<Self::Item> {
                self.inner.nth_back(n)
            }
        }

        impl$(<$lifetime>)? ExactSizeIterator for $name$(<$lifetime>)? {
            #[inline]
            fn len(&self) -> usize {
                self.inner.len()
            }
        }

        impl$(<$lifetime>)? FusedIterator for $name$(<$lifetime>)? {}

        impl$(<$lifetime>)? fmt::Debug for $name$(<$lifetime>)? {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Debug::fmt(&self.inner, formatter)
            }
        }
    };
}

mapping_iterator!(Iter<'a>, (&'a Value, &'a Value));
mapping_iterator!(IterMut<'a>, (&'a Value, &'a mut Value));
mapping_iterator!(IntoIter, (Value, Value));

impl IntoIterator for Mapping {
    type Item = (Value, Value);
    type IntoIter = IntoIter;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        IntoIter {
            inner: self.map.into_iter(),
        }
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
mod tests {
    use super::Mapping;
    use crate::value::Value;

    fn key(name: &str) -> Value {
        Value::String(name.to_owned())
    }

    #[test]
    fn insertion_order_is_preserved() {
        let mut map = Mapping::new();
        map.insert(key("a"), Value::from(1_u64));
        map.insert(key("b"), Value::from(2_u64));
        map.insert(key("c"), Value::from(3_u64));
        let keys: Vec<_> = map.keys().cloned().collect();
        assert_eq!(keys, vec![key("a"), key("b"), key("c")]);
    }

    #[test]
    fn replacement_does_not_move_the_key() {
        let mut map = Mapping::new();
        map.insert(key("a"), Value::from(1_u64));
        map.insert(key("b"), Value::from(2_u64));
        assert_eq!(
            map.insert(key("a"), Value::from(3_u64)),
            Some(Value::from(1_u64))
        );
        let keys: Vec<_> = map.keys().cloned().collect();
        assert_eq!(keys, vec![key("a"), key("b")]);
    }

    #[test]
    fn borrowed_string_lookups_match_value_keys() {
        let mut map = Mapping::new();
        map.insert(key("a"), Value::from(1_u64));
        map.insert(key("b"), Value::from(2_u64));
        assert!(map.contains_key("a"));
        assert!(map.get("a").is_some());
        assert_eq!(map.get("b"), Some(&Value::from(2_u64)));
        assert_eq!(map.get_str("a"), Some(&Value::from(1_u64)));
        *map.get_str_mut("a").expect("mutable lookup") = Value::from(3_u64);
        *map.get_mut("b").expect("generic mutable lookup") = Value::from(4_u64);
        assert_eq!(map.remove("a"), Some(Value::from(3_u64)));
        assert_eq!(map.shift_remove("b"), Some(Value::from(4_u64)));
        assert!(map.is_empty());
    }

    #[test]
    fn remove_uses_swap_remove() {
        let mut map = Mapping::new();
        for name in ["a", "b", "c", "d"] {
            map.insert(key(name), Value::from(name.len() as u64));
        }
        map.remove(key("b"));
        let keys: Vec<_> = map.keys().cloned().collect();
        assert_eq!(keys, vec![key("a"), key("d"), key("c")]);
    }

    #[test]
    fn shift_remove_preserves_order() {
        let mut map = Mapping::new();
        for name in ["a", "b", "c"] {
            map.insert(key(name), Value::Null);
        }
        map.shift_remove(key("b"));
        let keys: Vec<_> = map.keys().cloned().collect();
        assert_eq!(keys, vec![key("a"), key("c")]);
    }

    #[test]
    fn mapping_equality_and_hash_are_order_insensitive() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut left = Mapping::new();
        left.insert(key("a"), Value::from(1_u64));
        left.insert(key("b"), Value::from(2_u64));

        let mut right = Mapping::new();
        right.insert(key("b"), Value::from(2_u64));
        right.insert(key("a"), Value::from(1_u64));

        assert_eq!(left, right);
        let mut left_hash = DefaultHasher::new();
        let mut right_hash = DefaultHasher::new();
        left.hash(&mut left_hash);
        right.hash(&mut right_hash);
        assert_eq!(left_hash.finish(), right_hash.finish());
    }

    #[test]
    fn entry_inserts_lazily_and_mutates_existing() {
        let mut map = Mapping::new();
        let _slot = map.entry(key("a")).or_insert(Value::from(1_u64));
        let _slot = map
            .entry(key("a"))
            .and_modify(|value| *value = Value::from(2_u64));
        let _slot = map.entry(key("b")).or_insert_with(|| Value::from(3_u64));
        assert_eq!(map.get("a"), Some(&Value::from(2_u64)));
        assert_eq!(map.get("b"), Some(&Value::from(3_u64)));
    }

    #[test]
    fn entry_implements_debug() {
        let mut map = Mapping::new();
        let entry = map.entry(key("a"));
        let rendered = format!("{entry:?}");
        assert_ne!(rendered, "");
    }

    #[test]
    fn debug_prints_the_mapping_name_and_entries() {
        assert_eq!(format!("{:?}", Mapping::new()), "Mapping {}");
        let mut map = Mapping::new();
        map.insert(key("a"), Value::from(1_u64));
        assert_eq!(
            format!("{map:?}"),
            "Mapping {String(\"a\"): Number(Number(PosInt(1)))}"
        );
    }

    /// A mapping caches its hash, and every way to change what it holds forgets the hash: each
    /// row changes a mapping that has been hashed, and it must then hash as a mapping built
    /// anew from the same entries does. Every row changes the entries, or the cache could stay.
    #[test]
    fn cached_hash_tracks_every_mutation() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        fn digest(map: &Mapping) -> u64 {
            let mut hasher = DefaultHasher::new();
            map.hash(&mut hasher);
            hasher.finish()
        }

        type Change = fn(&mut Mapping);
        let changes: [(&str, Change); 22] = [
            ("insert a new key", |map| {
                map.insert(key("z"), Value::from(9_u64));
            }),
            ("insert over a key", |map| {
                map.insert(key("a"), Value::from(9_u64));
            }),
            ("insert_new", |map| {
                map.insert_new(key("z"), Value::from(9_u64)).expect("new");
            }),
            ("get_mut by str", |map| {
                *map.get_mut("a").expect("a") = Value::from(9_u64);
            }),
            ("get_mut by value", |map| {
                let wanted = key("a");
                *map.get_mut(&wanted).expect("a") = Value::from(9_u64);
            }),
            ("get_str_mut", |map| {
                *map.get_str_mut("a").expect("a") = Value::from(9_u64);
            }),
            ("entry or_insert, absent", |map| {
                let _ = map.entry(key("z")).or_insert(Value::from(9_u64));
            }),
            ("entry or_insert, present, written through", |map| {
                *map.entry(key("a")).or_insert(Value::Null) = Value::from(9_u64);
            }),
            ("entry or_insert_with, absent", |map| {
                map.entry(key("z")).or_insert_with(|| Value::from(9_u64));
            }),
            ("entry or_insert_with, present, written through", |map| {
                *map.entry(key("a")).or_insert_with(|| Value::Null) = Value::from(9_u64);
            }),
            ("entry and_modify", |map| {
                drop(
                    map.entry(key("a"))
                        .and_modify(|value| *value = Value::from(9_u64)),
                );
            }),
            ("remove by str", |map| {
                map.remove("a").expect("a");
            }),
            ("remove by value", |map| {
                let wanted = key("a");
                map.remove(&wanted).expect("a");
            }),
            ("shift_remove by str", |map| {
                map.shift_remove("a").expect("a");
            }),
            ("shift_remove by value", |map| {
                let wanted = key("a");
                map.shift_remove(&wanted).expect("a");
            }),
            ("iter_mut", |map| {
                for (_, value) in map.iter_mut() {
                    *value = Value::from(9_u64);
                }
            }),
            ("a loop over &mut", |map| {
                for (_, value) in &mut *map {
                    *value = Value::from(9_u64);
                }
            }),
            ("values_mut", |map| {
                for value in map.values_mut() {
                    *value = Value::from(9_u64);
                }
            }),
            ("extend", |map| {
                map.extend([(key("z"), Value::from(9_u64))]);
            }),
            ("clear", Mapping::clear),
            ("a value's index, absent", |map| {
                let mut value = Value::Mapping(std::mem::take(map));
                value["z"] = Value::from(9_u64);
                let Value::Mapping(changed) = value else {
                    unreachable!()
                };
                *map = changed;
            }),
            ("a value's index, present", |map| {
                let mut value = Value::Mapping(std::mem::take(map));
                value["a"] = Value::from(9_u64);
                let Value::Mapping(changed) = value else {
                    unreachable!()
                };
                *map = changed;
            }),
        ];
        for (name, change) in changes {
            let mut map = Mapping::new();
            map.insert(key("a"), Value::from(1_u64));
            map.insert(key("b"), Value::from(2_u64));
            let before = digest(&map);
            // Hashed again, it gives the cached hash; the change has to forget that.
            assert_eq!(digest(&map), before, "{name}");
            change(&mut map);
            let fresh: Mapping = map
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            assert_ne!(digest(&fresh), before, "{name} changes nothing");
            assert_eq!(digest(&map), digest(&fresh), "{name}");
        }
        // Giving back spare room changes no entry, and may keep the hash.
        let mut map = Mapping::new();
        map.insert(key("a"), Value::from(1_u64));
        let before = digest(&map);
        map.shrink_to_fit();
        assert_eq!(digest(&map), before);
    }

    /// Without a mark for each kind of number, these write the same bytes, and a mapping of them
    /// cancels out under any key.
    #[test]
    fn what_values_write_tells_them_apart() {
        use std::hash::{Hash, Hasher};

        #[derive(Default)]
        struct Bytes(Vec<u8>);
        impl Hasher for Bytes {
            fn write(&mut self, bytes: &[u8]) {
                self.0.extend_from_slice(bytes);
            }
            fn finish(&self) -> u64 {
                0
            }
        }
        let bytes = |values: &[&str]| {
            let mut bytes = Bytes::default();
            for value in values {
                crate::parse_value(value).expect("a value").hash(&mut bytes);
            }
            bytes.0
        };
        let max = u64::MAX.to_string();
        let wide = ((2_u128 << 64) + 1).to_string();
        let high = ((1_u128 << 127) + 5).to_string();
        let low = format!("-{}", (1_u128 << 127) - 5);
        for (left, right) in [
            (vec!["-1"], vec![max.as_str()]),
            (vec!["1", "0"], vec![wide.as_str(), "~"]),
            (vec!["[-1, -1]"], vec!["[-1, 18446744073709551615]"]),
            (vec![high.as_str()], vec![low.as_str()]),
            (vec!["1.5"], vec!["4609434218613702656"]),
        ] {
            assert_ne!(bytes(&left), bytes(&right), "{left:?} and {right:?}");
        }
    }

    /// Any 65 values of 64 bits have a subset whose XOR is zero. Found by elimination under a
    /// key anyone can compute, the subset splits into two mappings that would hash alike, and
    /// enough of those are keys that take quadratic time.
    #[test]
    fn entries_cannot_be_chosen_to_cancel_out() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash as _, Hasher as _};
        let entry = |index: u32| {
            let mut hasher = DefaultHasher::new();
            key(&format!("e{index}")).hash(&mut hasher);
            Value::Null.hash(&mut hasher);
            hasher.finish()
        };
        let mut basis: Vec<(u64, u128)> = Vec::new();
        let mut subset = None;
        for index in 0..65 {
            let (mut value, mut members) = (entry(index), 1_u128 << index);
            for (reduced, its_members) in &basis {
                if value ^ reduced < value {
                    value ^= reduced;
                    members ^= its_members;
                }
            }
            if value == 0 {
                subset = Some(members);
                break;
            }
            basis.push((value, members));
            basis.sort_unstable_by_key(|(reduced, _)| std::cmp::Reverse(*reduced));
        }
        let subset = subset.expect("65 values are dependent");
        let mut sides = [Mapping::new(), Mapping::new()];
        let mut count = 0;
        for index in 0..65 {
            if subset & (1 << index) != 0 {
                sides[count % 2].insert(key(&format!("e{index}")), Value::Null);
                count += 1;
            }
        }
        let hash = |map: &Mapping| {
            let mut hasher = DefaultHasher::new();
            map.hash(&mut hasher);
            hasher.finish()
        };
        assert_ne!(sides[0], sides[1]);
        assert_ne!(hash(&sides[0]), hash(&sides[1]));
    }

    #[test]
    fn missing_index_is_null() {
        let map = Mapping::new();
        assert_eq!(map["missing"], Value::Null);
    }
}
