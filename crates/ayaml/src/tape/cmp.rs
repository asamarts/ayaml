//! Canonical hashing, semantic equality, `Value` comparison, and bounded key formatting for
//! [`TapeStorage`].

use std::fmt::Write as _;
use std::hash::{BuildHasher, Hash, Hasher};

use crate::limits::GlobalHasher;
use crate::value::{Value, key_text_full, push_quoted_key};

use super::{NodeTag, TapeStorage};

/// Canonical SipHash-1-3 hash of a tape node (matching `Value` / `Mapping` equality semantics).
///
/// Sequences and tags are traversed iteratively; nested mappings hash each entry recursively
/// (bounded by `MAX_DEPTH_CAP == 128`, matching `Mapping::hash`).
pub(crate) fn tape_node_hash(storage: &TapeStorage<'_>, root: usize) -> u64 {
    let mut hasher = GlobalHasher.build_hasher();
    hash_tape_node_into(storage, root, &mut hasher);
    hasher.finish()
}

fn hash_tape_node_into<H: Hasher>(storage: &TapeStorage<'_>, root: usize, state: &mut H) {
    enum Step {
        Node(usize),
        /// Order-independent XOR accumulator for mapping entries.
        MapEnd {
            acc: u64,
            cursor: usize,
            end: usize,
        },
    }

    // Fast path for non-collection scalar keys (99%+ of YAML mappings).
    let root_node = storage.node(root);
    match root_node.tag() {
        NodeTag::EmptyNull | NodeTag::ExplicitNull => {
            0_u8.hash(state);
            return;
        }
        NodeTag::Bool => {
            1_u8.hash(state);
            (root_node.payload() != 0).hash(state);
            return;
        }
        NodeTag::PosInt | NodeTag::NegInt | NodeTag::Float | NodeTag::WideInt => {
            2_u8.hash(state);
            if let Some(num) = storage.number_at(root) {
                num.hash(state);
            }
            return;
        }
        NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr => {
            3_u8.hash(state);
            storage.str_at(root).unwrap_or("").hash(state);
            return;
        }
        _ => {}
    }

    let mut stack = vec![Step::Node(root)];
    while let Some(step) = stack.pop() {
        match step {
            Step::Node(idx) => {
                let node = storage.node(idx);
                match node.tag() {
                    NodeTag::EmptyNull | NodeTag::ExplicitNull => 0_u8.hash(state),
                    NodeTag::Bool => {
                        1_u8.hash(state);
                        (node.payload() != 0).hash(state);
                    }
                    NodeTag::PosInt | NodeTag::NegInt | NodeTag::Float | NodeTag::WideInt => {
                        2_u8.hash(state);
                        if let Some(num) = storage.number_at(idx) {
                            num.hash(state);
                        }
                    }
                    NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr => {
                        3_u8.hash(state);
                        storage.str_at(idx).unwrap_or("").hash(state);
                    }
                    NodeTag::Sequence => {
                        let (len, end) = node.payload_pair();
                        4_u8.hash(state);
                        (len as usize).hash(state);
                        // Collect direct child indices in reverse for left-to-right hashing.
                        let base = stack.len();
                        let mut c = idx + 1;
                        while c < end as usize {
                            stack.push(Step::Node(c));
                            c = storage.node(c).subtree_end(c);
                        }
                        stack[base..].reverse();
                    }
                    NodeTag::Mapping => {
                        let (len, end) = node.payload_pair();
                        5_u8.hash(state);
                        (len as usize).hash(state);
                        stack.push(Step::MapEnd {
                            acc: 0,
                            cursor: idx + 1,
                            end: end as usize,
                        });
                    }
                    NodeTag::Tagged => {
                        6_u8.hash(state);
                        if let Some(tag) = storage.tag_at(idx) {
                            tag.hash(state);
                        }
                        stack.push(Step::Node(idx + 1));
                    }
                }
            }
            Step::MapEnd {
                mut acc,
                mut cursor,
                end,
            } => {
                // Compute each entry's hash with a fresh randomized SipHash-1-3 hasher and XOR
                // them for order-independent mapping hashing without O(N^2) collision chains.
                while cursor < end {
                    let key_idx = cursor;
                    let val_idx = storage.node(key_idx).subtree_end(key_idx);
                    cursor = storage.node(val_idx).subtree_end(val_idx);

                    let mut entry_hasher = GlobalHasher.build_hasher();
                    0x01_u8.hash(&mut entry_hasher);
                    entry_hasher.write_u64(tape_node_hash(storage, key_idx));
                    0x02_u8.hash(&mut entry_hasher);
                    entry_hasher.write_u64(tape_node_hash(storage, val_idx));
                    acc ^= entry_hasher.finish();
                }
                acc.hash(state);
            }
        }
    }
}

/// Canonical YAML semantic equality between two nodes in the same [`TapeStorage`],
/// matching `Value::eq` (including order-independent mapping equality and `NaN == NaN`).
pub(crate) fn tape_node_eq(storage: &TapeStorage<'_>, a: usize, b: usize) -> bool {
    if a == b {
        return true;
    }
    // Fast scalar check avoids allocating a stack for normal keys.
    if let Some(eq) = scalar_eq(storage, a, b) {
        return eq;
    }

    let mut stack = vec![(a, b)];
    while let Some((left, right)) = stack.pop() {
        if left == right {
            continue;
        }
        if let Some(eq) = scalar_eq(storage, left, right) {
            if !eq {
                return false;
            }
            continue;
        }
        let ln = storage.node(left);
        let rn = storage.node(right);
        match (ln.tag(), rn.tag()) {
            (NodeTag::Tagged, NodeTag::Tagged) => {
                if storage.tag_at(left) != storage.tag_at(right) {
                    return false;
                }
                stack.push((left + 1, right + 1));
            }
            (NodeTag::Sequence, NodeTag::Sequence) => {
                let (llen, lend) = ln.payload_pair();
                let (rlen, rend) = rn.payload_pair();
                if llen != rlen {
                    return false;
                }
                let mut lc = left + 1;
                let mut rc = right + 1;
                while lc < lend as usize && rc < rend as usize {
                    stack.push((lc, rc));
                    lc = storage.node(lc).subtree_end(lc);
                    rc = storage.node(rc).subtree_end(rc);
                }
            }
            (NodeTag::Mapping, NodeTag::Mapping) => {
                if !mapping_eq(storage, left, right) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

#[inline]
fn scalar_eq(storage: &TapeStorage<'_>, a: usize, b: usize) -> Option<bool> {
    let ln = storage.node(a);
    let rn = storage.node(b);
    match (ln.tag(), rn.tag()) {
        (
            NodeTag::EmptyNull | NodeTag::ExplicitNull,
            NodeTag::EmptyNull | NodeTag::ExplicitNull,
        ) => Some(true),
        (NodeTag::Bool, NodeTag::Bool) => Some((ln.payload() != 0) == (rn.payload() != 0)),
        (
            NodeTag::PosInt | NodeTag::NegInt | NodeTag::Float | NodeTag::WideInt,
            NodeTag::PosInt | NodeTag::NegInt | NodeTag::Float | NodeTag::WideInt,
        ) => Some(storage.number_at(a) == storage.number_at(b)),
        (
            NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr,
            NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr,
        ) => Some(storage.str_at(a) == storage.str_at(b)),
        (
            NodeTag::Sequence | NodeTag::Mapping | NodeTag::Tagged,
            NodeTag::Sequence | NodeTag::Mapping | NodeTag::Tagged,
        ) => None,
        _ => Some(false),
    }
}

/// Order-independent mapping equality in O(N) average time using hashed buckets for large maps.
fn mapping_eq(storage: &TapeStorage<'_>, left: usize, right: usize) -> bool {
    let (llen, lend) = storage.node(left).payload_pair();
    let (rlen, rend) = storage.node(right).payload_pair();
    if llen != rlen {
        return false;
    }
    let len = llen as usize;
    if len == 0 {
        return true;
    }
    if len <= 12 {
        let mut lc = left + 1;
        while lc < lend as usize {
            let lk = lc;
            let lv = storage.node(lk).subtree_end(lk);
            lc = storage.node(lv).subtree_end(lv);

            let mut found = false;
            let mut rc = right + 1;
            while rc < rend as usize {
                let rk = rc;
                let rv = storage.node(rk).subtree_end(rk);
                rc = storage.node(rv).subtree_end(rv);
                if tape_node_eq(storage, lk, rk) {
                    if !tape_node_eq(storage, lv, rv) {
                        return false;
                    }
                    found = true;
                    break;
                }
            }
            if !found {
                return false;
            }
        }
        return true;
    }

    // Build a temporary SipHash-1-3 index of right's entries for O(N) lookup.
    let mut right_table: std::collections::HashMap<u64, Vec<(usize, usize)>, GlobalHasher> =
        std::collections::HashMap::with_capacity_and_hasher(len, GlobalHasher);
    let mut rc = right + 1;
    while rc < rend as usize {
        let rk = rc;
        let rv = storage.node(rk).subtree_end(rk);
        rc = storage.node(rv).subtree_end(rv);
        let hash = tape_node_hash(storage, rk);
        right_table.entry(hash).or_default().push((rk, rv));
    }

    let mut lc = left + 1;
    while lc < lend as usize {
        let lk = lc;
        let lv = storage.node(lk).subtree_end(lk);
        lc = storage.node(lv).subtree_end(lv);
        let hash = tape_node_hash(storage, lk);
        let Some(bucket) = right_table.get(&hash) else {
            return false;
        };
        let mut matched = false;
        for &(rk, rv) in bucket {
            if tape_node_eq(storage, lk, rk) {
                if !tape_node_eq(storage, lv, rv) {
                    return false;
                }
                matched = true;
                break;
            }
        }
        if !matched {
            return false;
        }
    }
    true
}

/// Compare a tape node against an owned [`Value`] without allocating a temporary `Value`.
pub(crate) fn tape_eq_value(storage: &TapeStorage<'_>, root: usize, target: &Value) -> bool {
    let mut stack = vec![(root, target)];
    while let Some((idx, val)) = stack.pop() {
        let node = storage.node(idx);
        match (node.tag(), val) {
            (NodeTag::EmptyNull | NodeTag::ExplicitNull, Value::Null) => {}
            (NodeTag::Bool, Value::Bool(b)) if (node.payload() != 0) == *b => {}
            (
                NodeTag::PosInt | NodeTag::NegInt | NodeTag::Float | NodeTag::WideInt,
                Value::Number(num),
            ) if storage.number_at(idx) == Some(*num) => {}
            (NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr, Value::String(s))
                if storage.str_at(idx) == Some(s.as_str()) => {}
            (NodeTag::Tagged, Value::Tagged(tagged)) => {
                if storage.tag_at(idx) != Some(&tagged.tag) {
                    return false;
                }
                stack.push((idx + 1, &tagged.value));
            }
            (NodeTag::Sequence, Value::Sequence(items)) => {
                let (len, end) = node.payload_pair();
                if len as usize != items.len() {
                    return false;
                }
                let mut c = idx + 1;
                for item in items {
                    if c >= end as usize {
                        return false;
                    }
                    stack.push((c, item));
                    c = storage.node(c).subtree_end(c);
                }
            }
            (NodeTag::Mapping, Value::Mapping(map)) => {
                let (len, end) = node.payload_pair();
                if len as usize != map.len() {
                    return false;
                }
                let mut c = idx + 1;
                while c < end as usize {
                    let k_idx = c;
                    let v_idx = storage.node(k_idx).subtree_end(k_idx);
                    c = storage.node(v_idx).subtree_end(v_idx);
                    let key_val = storage.to_value_at(k_idx);
                    let Some(expected_val) = map.get(&key_val) else {
                        return false;
                    };
                    stack.push((v_idx, expected_val));
                }
            }
            _ => return false,
        }
    }
    true
}

/// Human-readable bounded diagnostic text for a mapping key in [`TapeStorage`],
/// matching [`crate::value::key_text`].
pub(crate) fn tape_key_text(storage: &TapeStorage<'_>, root: usize) -> String {
    let mut output = String::new();
    push_tape_key_text(storage, root, &mut output);
    output
}

fn push_tape_key_text(storage: &TapeStorage<'_>, root: usize, output: &mut String) {
    enum Step {
        Node(usize),
        Literal(&'static str),
        SeqRest { cursor: usize, end: usize },
        MapRest { cursor: usize, end: usize },
    }

    let mut stack = vec![Step::Node(root)];
    while let Some(step) = stack.pop() {
        if key_text_full(output) {
            return;
        }
        match step {
            Step::Literal(s) => output.push_str(s),
            Step::Node(idx) => {
                let node = storage.node(idx);
                match node.tag() {
                    NodeTag::EmptyNull | NodeTag::ExplicitNull => output.push_str("null"),
                    NodeTag::Bool => {
                        let _ = write!(output, "{}", node.payload() != 0);
                    }
                    NodeTag::PosInt | NodeTag::NegInt | NodeTag::Float | NodeTag::WideInt => {
                        if let Some(num) = storage.number_at(idx) {
                            let _ = write!(output, "{num}");
                        }
                    }
                    NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr => {
                        push_quoted_key(storage.str_at(idx).unwrap_or(""), output);
                    }
                    NodeTag::Sequence => {
                        let (_, end) = node.payload_pair();
                        output.push('[');
                        if idx + 1 < end as usize {
                            let first = idx + 1;
                            let next = storage.node(first).subtree_end(first);
                            stack.push(Step::Literal("]"));
                            if next < end as usize {
                                stack.push(Step::SeqRest {
                                    cursor: next,
                                    end: end as usize,
                                });
                            }
                            stack.push(Step::Node(first));
                        } else {
                            output.push(']');
                        }
                    }
                    NodeTag::Mapping => {
                        let (_, end) = node.payload_pair();
                        output.push('{');
                        if idx + 1 < end as usize {
                            let k_idx = idx + 1;
                            let v_idx = storage.node(k_idx).subtree_end(k_idx);
                            let next = storage.node(v_idx).subtree_end(v_idx);
                            stack.push(Step::Literal("}"));
                            if next < end as usize {
                                stack.push(Step::MapRest {
                                    cursor: next,
                                    end: end as usize,
                                });
                            }
                            stack.push(Step::Node(v_idx));
                            stack.push(Step::Literal(": "));
                            stack.push(Step::Node(k_idx));
                        } else {
                            output.push('}');
                        }
                    }
                    NodeTag::Tagged => {
                        if let Some(tag) = storage.tag_at(idx) {
                            let _ = write!(output, "{tag} ");
                        }
                        stack.push(Step::Node(idx + 1));
                    }
                }
            }
            Step::SeqRest { cursor, end } => {
                output.push_str(", ");
                if key_text_full(output) {
                    output.push('…');
                    return;
                }
                let next = storage.node(cursor).subtree_end(cursor);
                if next < end {
                    stack.push(Step::SeqRest { cursor: next, end });
                }
                stack.push(Step::Node(cursor));
            }
            Step::MapRest { cursor, end } => {
                output.push_str(", ");
                if key_text_full(output) {
                    output.push('…');
                    return;
                }
                let k_idx = cursor;
                let v_idx = storage.node(k_idx).subtree_end(k_idx);
                let next = storage.node(v_idx).subtree_end(v_idx);
                if next < end {
                    stack.push(Step::MapRest { cursor: next, end });
                }
                stack.push(Step::Node(v_idx));
                stack.push(Step::Literal(": "));
                stack.push(Step::Node(k_idx));
            }
        }
    }
}
