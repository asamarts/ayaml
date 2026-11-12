#![no_main]
#![forbid(unsafe_code)]

//! Parse events and values without panicking.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let _unused = ayaml::parse_value(input);
    let _unused = ayaml::parse_documents(input);
    if let Ok(stream) = ayaml::parse_events(input) {
        let _unused = stream.collect::<Result<Vec<_>, _>>();
    }
});
