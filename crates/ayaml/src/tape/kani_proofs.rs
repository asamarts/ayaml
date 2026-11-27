//! Symbolic Kani proofs for pure flat-array tape index/packing and retention helpers.

use super::builder::initial_node_capacity;
use super::{
    NodeTag, OVERFLOW_SPAN_LEN, TapeNode, pack_span_tag, pack_u32_pair, shift_alias_end,
    unpack_u32_pair,
};
use crate::scratch::clamp_retained_capacity;

#[kani::proof]
fn verify_pack_unpack_span_tag_roundtrip() {
    let span_start: u32 = kani::any();
    let span_len: u32 = kani::any();
    let tag_bits: u8 = kani::any();
    let flags: u8 = kani::any();
    kani::assume(span_len <= OVERFLOW_SPAN_LEN);
    kani::assume(tag_bits <= 12);
    kani::assume(flags <= 0x0F);

    let tag = NodeTag::from_bits(tag_bits);
    let packed = pack_span_tag(span_len, tag, flags);
    let node = TapeNode::new(span_start, span_len, tag, flags, 0);

    assert_eq!(node.tag(), tag);
    assert_eq!(node.flags(), flags);
    assert_eq!(node.span_len_bits(), span_len);
    assert_eq!(packed & 0x00FF_FFFF, span_len);
    if span_len == OVERFLOW_SPAN_LEN {
        assert!(node.inline_span().is_none());
    } else if (span_start as usize)
        .checked_add(span_len as usize)
        .is_some()
    {
        let span = node.inline_span().expect("inline span");
        assert_eq!(span.start(), span_start as usize);
        assert_eq!(span.len(), span_len as usize);
    }
}

#[kani::proof]
fn verify_pack_unpack_u32_pair_payload() {
    let lo: u32 = kani::any();
    let hi: u32 = kani::any();
    let packed = pack_u32_pair(lo, hi);
    assert_eq!(unpack_u32_pair(packed), (lo, hi));
}

#[kani::proof]
fn verify_alias_shift_checked() {
    let end: u32 = kani::any();
    let delta: u32 = kani::any();
    match shift_alias_end(end, delta) {
        Some(shifted) => {
            assert!(shifted >= end);
            assert_eq!(u64::from(shifted), u64::from(end) + u64::from(delta));
        }
        None => {
            assert!(u64::from(end) + u64::from(delta) > u64::from(u32::MAX));
        }
    }
}

#[kani::proof]
fn verify_initial_node_capacity_bounded() {
    let input_len: usize = kani::any();
    let max_events: usize = kani::any();
    let cap = initial_node_capacity(input_len, max_events);
    assert!(cap <= max_events);
    assert!(cap <= 24_576);
    if max_events == 0 {
        assert_eq!(cap, 0);
    }
}

#[kani::proof]
fn verify_retention_clamp_bounds() {
    let capacity: usize = kani::any();
    let max_retain: usize = kani::any();
    let keep = clamp_retained_capacity(capacity, max_retain);
    assert_eq!(keep, capacity <= max_retain);
}
