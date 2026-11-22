#![no_main]
#![forbid(unsafe_code)]

//! Arbitrary `Event` stream emission consistency and reparse fuzz target.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ayaml_fuzz::run_event_emit(data);
});
