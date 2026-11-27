use super::{
    TOKEN_ARENA_AHEAD, arena_growth, compact_shift, estimated_growth, mid_input_token_capacity,
    prefix_token_capacity,
};

#[kani::proof]
fn verify_prefix_token_capacity_bounds() {
    let tokens: usize = kani::any();
    let consumed: usize = kani::any();
    let last_tokens: usize = kani::any();
    let last_consumed: usize = kani::any();
    let input_len: usize = kani::any();
    let most: usize = kani::any();

    // Bound bit-width for the 128-bit division so CBMC verifies exhaustively within seconds
    // while covering below-threshold, threshold, and cap regimes. Saturation at `usize::MAX`
    // boundaries is verified in `verify_prefix_token_capacity_saturation_boundaries`.
    kani::assume(tokens <= 1_000_000);
    kani::assume(consumed <= 16_000_000);
    kani::assume(last_tokens <= tokens);
    kani::assume(last_consumed <= consumed);
    kani::assume(input_len <= 16_000_000);
    kani::assume(most <= 16_000_000);

    let result = prefix_token_capacity(
        tokens,
        consumed,
        last_tokens,
        last_consumed,
        input_len,
        most,
    );
    if tokens < 1_024 || consumed == 0 {
        assert_eq!(result, None);
    } else {
        let cap = input_len
            .min(most)
            .min(tokens.saturating_mul(TOKEN_ARENA_AHEAD));
        let Some(capacity) = result else {
            unreachable!("tokens >= 1024 and consumed > 0 must yield Some");
        };
        assert!(capacity >= tokens);
        assert!(capacity <= cap.max(tokens));
    }
}

#[kani::proof]
fn verify_prefix_token_capacity_saturation_boundaries() {
    const BOUNDARIES: [usize; 10] = [
        0,
        1,
        1_023,
        1_024,
        2_048,
        usize::MAX / TOKEN_ARENA_AHEAD,
        (usize::MAX / TOKEN_ARENA_AHEAD) + 1,
        usize::MAX / 2,
        usize::MAX - 1,
        usize::MAX,
    ];
    let pick = || {
        let idx: usize = kani::any();
        kani::assume(idx < BOUNDARIES.len());
        BOUNDARIES[idx]
    };
    let tokens = pick();
    let consumed = pick();
    let last_tokens = pick();
    let last_consumed = pick();
    let input_len = pick();
    let most = pick();

    let result = prefix_token_capacity(
        tokens,
        consumed,
        last_tokens,
        last_consumed,
        input_len,
        most,
    );
    if tokens < 1_024 || consumed == 0 {
        assert_eq!(result, None);
    } else {
        let cap = input_len
            .min(most)
            .min(tokens.saturating_mul(TOKEN_ARENA_AHEAD));
        let Some(capacity) = result else {
            unreachable!("tokens >= 1024 and consumed > 0 must yield Some");
        };
        assert!(capacity >= tokens);
        assert!(capacity <= cap.max(tokens));
    }
}

#[kani::proof]
fn verify_token_arena_growth_helpers_never_overflow() {
    let len: usize = kani::any();
    let estimate: usize = kani::any();
    let chunk: usize = kani::any();
    let most: usize = kani::any();
    let input_len: usize = kani::any();
    let line_count: usize = kani::any();

    let g = arena_growth(len, chunk);
    assert!(g == chunk || g == len);

    let eg = estimated_growth(len, estimate, chunk, most);
    let room = most.saturating_sub(len);
    assert!(eg <= if room == 0 { 1 } else { room });

    let mid = mid_input_token_capacity(input_len, line_count, most);
    if let Some(cap) = mid {
        assert!((4097..=32768).contains(&input_len));
        assert!(line_count > 0);
        assert!(cap <= most);
        assert!(cap <= input_len / 10);
    }
}

#[kani::proof]
fn verify_compact_shift_never_underflows_live_lines() {
    let dead_prefix: usize = kani::any();
    let tokens_len: usize = kani::any();
    let has_live: bool = kani::any();
    let live_start: usize = kani::any();
    let live_end: usize = kani::any();

    kani::assume(live_start <= live_end);
    kani::assume(live_end <= tokens_len);

    let first_live = has_live.then_some(live_start);
    let cut = compact_shift(dead_prefix, tokens_len, first_live);

    assert!(cut <= dead_prefix);
    assert!(cut <= tokens_len);
    if has_live {
        assert!(cut <= live_start);
        assert!(live_start - cut <= live_end - cut);
        assert!(live_end - cut <= tokens_len - cut);
    }
}
