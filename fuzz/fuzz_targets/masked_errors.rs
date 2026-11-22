#![no_main]
#![forbid(unsafe_code)]

//! `Error::masked()` secret canary non-leakage fuzz target.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ayaml_fuzz::run_masked_errors(data);
});
