#![no_main]
#![forbid(unsafe_code)]

//! Parse raw bytes across every public entry point without panicking.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ayaml_fuzz::run_parse(data);
});
