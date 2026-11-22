#![no_main]
#![forbid(unsafe_code)]

//! Structure-aware YAML parsing and round-trip fuzz target.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ayaml_fuzz::run_structured(data);
});
