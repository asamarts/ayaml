//! The text of a mapping key: what a scalar key is compared by, and what a diagnostic names.

use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use super::{Node, NodeKind, resolve};
use crate::mapping::{GlobalHasher, SMALL_LIMIT};
use crate::value::{key_text_full, push_quoted_key};

/// Scalar mapping keys seen so far in an open typed mapping.
///
/// Small mappings (up to [`SMALL_LIMIT`] entries) store keys inline with linear lookup and zero
/// heap allocation; larger mappings promote to a cryptographic SipHash-1-3 `HashSet` using the
/// process-wide [`GlobalHasher`] seed.
pub(super) enum SeenScalarKeys<'de> {
    Small {
        len: u8,
        keys: [Cow<'de, str>; SMALL_LIMIT],
    },
    Large(HashSet<Cow<'de, str>, GlobalHasher>),
}

impl<'de> SeenScalarKeys<'de> {
    #[inline]
    pub(super) const fn new() -> Self {
        Self::Small {
            len: 0,
            keys: [const { Cow::Borrowed("") }; SMALL_LIMIT],
        }
    }

    /// Insert `key` if absent. Returns `true` if inserted, `false` if duplicate.
    #[inline]
    pub(super) fn insert(&mut self, key: Cow<'de, str>) -> bool {
        match self {
            Self::Small { len, keys } => {
                let count = usize::from(*len);
                for existing in &keys[..count] {
                    if existing.as_ref() == key.as_ref() {
                        return false;
                    }
                }
                if count < SMALL_LIMIT {
                    keys[count] = key;
                    *len += 1;
                    return true;
                }
                self.promote_and_insert(key)
            }
            Self::Large(set) => set.insert(key),
        }
    }

    #[cold]
    #[inline(never)]
    fn promote_and_insert(&mut self, key: Cow<'de, str>) -> bool {
        let Self::Small { len, keys } = self else {
            unreachable!("promote_and_insert called on Large SeenScalarKeys");
        };
        let count = usize::from(*len);
        let mut set = HashSet::with_capacity_and_hasher(SMALL_LIMIT * 2, GlobalHasher);
        for slot in &mut keys[..count] {
            let existing = std::mem::replace(slot, Cow::Borrowed(""));
            set.insert(existing);
        }
        let inserted = set.insert(key);
        *self = Self::Large(set);
        inserted
    }
}

/// Entry indices in an open mapping's `entries` that share one cached node hash.
enum KeyIndexSlot {
    One(usize),
    Many(Vec<usize>),
}

impl KeyIndexSlot {
    fn contains(&self, entries: &[(Node<'_>, Node<'_>)], key: &Node<'_>) -> bool {
        match self {
            Self::One(index) => &entries[*index].0 == key,
            Self::Many(indices) => indices.iter().any(|&index| &entries[index].0 == key),
        }
    }

    fn push(&mut self, index: usize) {
        match self {
            Self::One(first) => *self = Self::Many(vec![*first, index]),
            Self::Many(indices) => indices.push(index),
        }
    }
}

/// Non-scalar keys seen in an open tree mapping, indexed by cached node hash and referencing
/// `entries` directly so collection keys are never cloned.
#[derive(Default)]
pub(super) struct SeenNodeIndices {
    buckets: HashMap<u64, KeyIndexSlot, GlobalHasher>,
}

impl SeenNodeIndices {
    /// Insert `index` for `key` if no equal key is already in `entries`. Returns `true` if `key`
    /// is a duplicate.
    pub(super) fn insert_is_duplicate(
        &mut self,
        entries: &[(Node<'_>, Node<'_>)],
        key: &Node<'_>,
        index: usize,
    ) -> bool {
        let hash = key.cached_hash();
        match self.buckets.entry(hash) {
            Entry::Vacant(slot) => {
                slot.insert(KeyIndexSlot::One(index));
                false
            }
            Entry::Occupied(mut slot) => {
                if slot.get().contains(entries, key) {
                    true
                } else {
                    slot.get_mut().push(index);
                    false
                }
            }
        }
    }
}

/// Owned non-scalar keys in a streaming mapping that share one cached node hash.
enum KeyNodeSlot<'de> {
    One(Box<Node<'de>>),
    Many(Vec<Node<'de>>),
}

impl<'de> KeyNodeSlot<'de> {
    fn contains(&self, key: &Node<'de>) -> bool {
        match self {
            Self::One(existing) => existing.as_ref() == key,
            Self::Many(nodes) => nodes.iter().any(|existing| existing == key),
        }
    }

    fn push(&mut self, key: Node<'de>) {
        match self {
            Self::One(_) => {
                let Self::One(first) = std::mem::replace(self, Self::Many(Vec::new())) else {
                    unreachable!();
                };
                *self = Self::Many(vec![*first, key]);
            }
            Self::Many(nodes) => nodes.push(key),
        }
    }
}

/// Non-scalar keys seen in an open streaming mapping, moved in by cached node hash without
/// cloning.
#[derive(Default)]
pub(super) struct SeenNodeKeys<'de> {
    buckets: HashMap<u64, KeyNodeSlot<'de>, GlobalHasher>,
}

impl<'de> SeenNodeKeys<'de> {
    /// Move `key` into the set if no equal key is present. Returns `Err(key)` on duplicate so the
    /// caller can format diagnostics from the incoming key without cloning it.
    pub(super) fn insert(&mut self, key: Node<'de>) -> Result<(), Node<'de>> {
        let hash = key.cached_hash();
        match self.buckets.entry(hash) {
            Entry::Vacant(slot) => {
                slot.insert(KeyNodeSlot::One(Box::new(key)));
                Ok(())
            }
            Entry::Occupied(mut slot) => {
                if slot.get().contains(&key) {
                    Err(key)
                } else {
                    slot.get_mut().push(key);
                    Ok(())
                }
            }
        }
    }
}

/// The text of a scalar key, whatever its scalar type. Two keys with the same text but different
/// types, as `1` and `"1"`, are one key to a target whose key type is narrower.
pub(super) fn scalar_key_text<'de>(node: &Node<'de>) -> Option<Cow<'de, str>> {
    let node = resolve(node);
    match &node.kind {
        NodeKind::Null => Some(Cow::Borrowed("null")),
        NodeKind::Bool(value) => Some(Cow::Borrowed(if *value { "true" } else { "false" })),
        NodeKind::Number(value) => Some(Cow::Owned(value.to_string())),
        NodeKind::String(value) => Some(value.clone()),
        _ => None,
    }
}

/// A human-readable mapping key for typed diagnostics.
pub(super) fn node_key_text(node: &Node<'_>) -> String {
    let mut text = String::new();
    push_node_key_text(resolve(node), &mut text);
    text
}

fn push_node_key_text(node: &Node<'_>, output: &mut String) {
    if key_text_full(output) {
        return;
    }
    match &node.kind {
        NodeKind::Null => output.push_str("null"),
        NodeKind::Bool(value) => {
            let _ = write!(output, "{value}");
        }
        NodeKind::Number(value) => {
            let _ = write!(output, "{value}");
        }
        NodeKind::String(value) => push_quoted_key(value, output),
        NodeKind::Sequence(items) => {
            output.push('[');
            for (index, item) in items.iter().enumerate() {
                if key_text_full(output) {
                    output.push('…');
                    return;
                }
                if index > 0 {
                    output.push_str(", ");
                }
                push_node_key_text(item, output);
            }
            if !key_text_full(output) {
                output.push(']');
            }
        }
        NodeKind::Mapping(entries) => {
            output.push('{');
            for (index, (key, value)) in entries.iter().enumerate() {
                if key_text_full(output) {
                    output.push('…');
                    return;
                }
                if index > 0 {
                    output.push_str(", ");
                }
                push_node_key_text(key, output);
                if !key_text_full(output) {
                    output.push_str(": ");
                }
                push_node_key_text(value, output);
            }
            if !key_text_full(output) {
                output.push('}');
            }
        }
        NodeKind::Tagged(tag, inner) => {
            let _ = write!(output, "{tag} ");
            push_node_key_text(inner, output);
        }
        NodeKind::Alias(target) => push_node_key_text(target, output),
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::SeenScalarKeys;
    use crate::mapping::SMALL_LIMIT;

    #[test]
    fn seen_scalar_keys_promotes_cleanly_at_small_limit_boundary() {
        let mut seen = SeenScalarKeys::new();
        for i in 0..SMALL_LIMIT {
            assert!(
                seen.insert(Cow::Owned(format!("k{i}"))),
                "unique small key {i}"
            );
            assert!(
                !seen.insert(Cow::Owned(format!("k{i}"))),
                "duplicate small key {i}"
            );
        }
        assert!(matches!(seen, SeenScalarKeys::Small { .. }));
        // Inserting key `SMALL_LIMIT` promotes to the SipHash-1-3 HashSet while preserving all
        // previously inserted keys.
        assert!(seen.insert(Cow::Borrowed("promote")));
        assert!(matches!(seen, SeenScalarKeys::Large(_)));
        assert!(!seen.insert(Cow::Borrowed("promote")));
        for i in 0..SMALL_LIMIT {
            assert!(
                !seen.insert(Cow::Owned(format!("k{i}"))),
                "promoted set must reject earlier key {i}"
            );
        }
        for i in SMALL_LIMIT..32 {
            assert!(seen.insert(Cow::Owned(format!("k{i}"))));
            assert!(!seen.insert(Cow::Owned(format!("k{i}"))));
        }
    }

    #[test]
    fn seen_node_indices_and_seen_node_keys_handle_duplicates_and_hash_collisions() {
        use std::sync::atomic::Ordering;

        use super::{Node, NodeKind, SeenNodeIndices, SeenNodeKeys};
        use crate::span::Span;

        let span = Span::empty(0);
        let s = |v: &'static str| Node::new(NodeKind::String(Cow::Borrowed(v)), span);
        let m1 = Node::new(
            NodeKind::Mapping(vec![(s("a"), s("1")), (s("b"), s("2"))]),
            span,
        );
        let m1_reordered = Node::new(
            NodeKind::Mapping(vec![(s("b"), s("2")), (s("a"), s("1"))]),
            span,
        );
        let m2 = Node::new(NodeKind::Mapping(vec![(s("c"), s("3"))]), span);
        let m3 = Node::new(NodeKind::Mapping(vec![(s("d"), s("4"))]), span);
        // Force identical cached hashes on distinct mappings to exercise `KeyIndexSlot::Many` and
        // `KeyNodeSlot::Many` collision buckets directly.
        m2.hash.store(0xDEAD_BEEF, Ordering::Relaxed);
        m3.hash.store(0xDEAD_BEEF, Ordering::Relaxed);

        let entries = vec![
            (m1.clone(), s("v1")),
            (m2.clone(), s("v2")),
            (m3.clone(), s("v3")),
        ];
        let mut indices = SeenNodeIndices::default();
        assert!(!indices.insert_is_duplicate(&entries, &entries[0].0, 0));
        assert!(indices.insert_is_duplicate(&entries, &m1_reordered, 1));
        assert!(!indices.insert_is_duplicate(&entries, &entries[1].0, 1));
        assert!(!indices.insert_is_duplicate(&entries, &entries[2].0, 2));
        assert!(indices.insert_is_duplicate(&entries, &m3, 3));

        let mut keys = SeenNodeKeys::default();
        assert!(keys.insert(m1).is_ok());
        assert!(keys.insert(m1_reordered).is_err());
        assert!(keys.insert(m2).is_ok());
        assert!(keys.insert(m3.clone()).is_ok());
        assert!(keys.insert(m3).is_err());
    }
}
