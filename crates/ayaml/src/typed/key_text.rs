//! The text of a mapping key: what a scalar key is compared by, and what a diagnostic names.

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt::Write as _;

use super::{Node, NodeKind, resolve};
use crate::value::{key_text_full, push_quoted_key};

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
    buckets: HashMap<u64, KeyIndexSlot>,
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
    buckets: HashMap<u64, KeyNodeSlot<'de>>,
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
        NodeKind::Bool(value) => Some(Cow::Owned(value.to_string())),
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
