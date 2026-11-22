#![no_main]
#![forbid(unsafe_code)]

//! `Value::apply_merge_keys` and `apply_merge_keys_with_limits` fuzz target.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ayaml_fuzz::run_merge_keys(data);
});
