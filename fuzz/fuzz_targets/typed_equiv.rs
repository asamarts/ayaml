#![no_main]
#![forbid(unsafe_code)]

//! Structure-aware small-tree versus event-stream typed equivalence fuzz target.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ayaml_fuzz::run_typed_equiv(data);
});
