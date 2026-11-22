#![no_main]
#![forbid(unsafe_code)]

//! `SpannedTree` span integrity and `LineIndex` bijectivity fuzz target.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ayaml_fuzz::run_spanned(data);
});
