#![no_main]
#![forbid(unsafe_code)]

//! Verified `ayaml-edit` scalar replacement and entry removal fuzz target.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ayaml_fuzz::run_edit(data);
});
