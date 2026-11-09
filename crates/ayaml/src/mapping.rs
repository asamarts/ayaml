//! An insertion-ordered YAML mapping backed by `indexmap`.

use std::hash::{BuildHasher, Hash, Hasher, RandomState};
use std::ops::Index as OpsIndex;
use std::sync::OnceLock;

use indexmap::IndexMap;

use crate::value::{NULL_VALUE, Value};

/// A YAML mapping with insertion-order iteration and swap-remove semantics.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Mapping {
    map: IndexMap<Value, Value>,
}

/// A borrowed or owned key accepted by [`Mapping`] lookup operations.
///
/// This is sealed by the set of implementations below: YAML keys may be full
/// [`Value`]s, while string keys are the common ergonomic case. References to
/// either form are accepted without forcing callers to match one ownership
/// convention.
pub trait Index: private::Sealed {
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
            map: IndexMap::with_capacity(capacity),
        }
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
    }

    /// Insert or replace a value, returning the previous value.
    pub fn insert(&mut self, key: Value, value: Value) -> Option<Value> {
        self.map.insert(key, value)
    }

    /// Insert a key that the mapping doesn't hold yet. For a key that it holds, return the key's
    /// text and leave the mapping as it was.
    pub(crate) fn insert_new(&mut self, key: Value, value: Value) -> Result<(), String> {
        match self.map.entry(key) {
            indexmap::map::Entry::Vacant(entry) => {
                entry.insert(value);
                Ok(())
            }
            indexmap::map::Entry::Occupied(entry) => Err(crate::value::key_text(entry.key())),
        }
    }

    /// Get a mutable entry for `key`, inserting lazily through [`Entry`].
    pub fn entry(&mut self, key: Value) -> Entry<'_> {
        Entry {
            inner: self.map.entry(key),
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
        self.map.get(&Value::String(key.to_owned()))
    }

    /// Mutably look up a value by key.
    #[must_use]
    pub fn get_mut<I: Index>(&mut self, key: I) -> Option<&mut Value> {
        Index::get_mut(&key, self)
    }

    /// Mutably look up a value by string key.
    #[must_use]
    pub fn get_str_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.map.get_mut(&Value::String(key.to_owned()))
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
    pub fn iter(&self) -> impl Iterator<Item = (&Value, &Value)> + '_ {
        self.map.iter()
    }

    /// Mutably iterate over entries in insertion order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&Value, &mut Value)> + '_ {
        self.map.iter_mut()
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
        self.map.values_mut()
    }

    /// Get an entry by insertion index.
    #[must_use]
    pub fn get_index(&self, index: usize) -> Option<(&Value, &Value)> {
        self.map.get_index(index)
    }
}

/// A view into one mapping slot, mirroring the `indexmap` entry API.
pub struct Entry<'a> {
    inner: indexmap::map::Entry<'a, Value, Value>,
}

impl<'a> Entry<'a> {
    /// Insert `default` if the key is absent, and return the slot.
    pub fn or_insert(self, default: Value) -> &'a mut Value {
        self.inner.or_insert(default)
    }

    /// Insert the result of `default` if the key is absent, and return the slot.
    pub fn or_insert_with<F: FnOnce() -> Value>(self, default: F) -> &'a mut Value {
        self.inner.or_insert_with(default)
    }

    /// Mutate the value if the key is present, and return the entry.
    pub fn and_modify<F: FnOnce(&mut Value)>(mut self, f: F) -> Self {
        if let indexmap::map::Entry::Occupied(entry) = &mut self.inner {
            f(entry.get_mut());
        }
        self
    }
}

impl std::fmt::Debug for Mapping {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Mapping ")?;
        formatter.debug_map().entries(self.map.iter()).finish()
    }
}

impl Hash for Mapping {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_of_entries(self.map.iter()).hash(state);
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
        mapping.map.get_mut(self)
    }

    fn swap_remove(&self, mapping: &mut Mapping) -> Option<Value> {
        mapping.map.swap_remove(self)
    }

    fn shift_remove(&self, mapping: &mut Mapping) -> Option<Value> {
        mapping.map.shift_remove(self)
    }
}

impl private::Sealed for str {}

impl Index for str {
    fn contains_key(&self, mapping: &Mapping) -> bool {
        mapping.map.contains_key(&Value::String(self.to_owned()))
    }

    fn get<'a>(&self, mapping: &'a Mapping) -> Option<&'a Value> {
        mapping.map.get(&Value::String(self.to_owned()))
    }

    fn get_mut<'a>(&self, mapping: &'a mut Mapping) -> Option<&'a mut Value> {
        mapping.map.get_mut(&Value::String(self.to_owned()))
    }

    fn swap_remove(&self, mapping: &mut Mapping) -> Option<Value> {
        mapping.map.swap_remove(&Value::String(self.to_owned()))
    }

    fn shift_remove(&self, mapping: &mut Mapping) -> Option<Value> {
        mapping.map.shift_remove(&Value::String(self.to_owned()))
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
        }
    }
}

impl Extend<(Value, Value)> for Mapping {
    fn extend<T: IntoIterator<Item = (Value, Value)>>(&mut self, iter: T) {
        self.map.extend(iter);
    }
}

impl IntoIterator for Mapping {
    type Item = (Value, Value);
    type IntoIter = indexmap::map::IntoIter<Value, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.map.into_iter()
    }
}

impl<'a> IntoIterator for &'a Mapping {
    type Item = (&'a Value, &'a Value);
    type IntoIter = indexmap::map::Iter<'a, Value, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.map.iter()
    }
}

impl<'a> IntoIterator for &'a mut Mapping {
    type Item = (&'a Value, &'a mut Value);
    type IntoIter = indexmap::map::IterMut<'a, Value, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.map.iter_mut()
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
        map.entry(key("a")).or_insert(Value::from(1_u64));
        map.entry(key("a"))
            .and_modify(|value| *value = Value::from(2_u64));
        map.entry(key("b")).or_insert_with(|| Value::from(3_u64));
        assert_eq!(map.get("a"), Some(&Value::from(2_u64)));
        assert_eq!(map.get("b"), Some(&Value::from(3_u64)));
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
