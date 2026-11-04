#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let _ = ayaml::parse_value(input);
    let _ = ayaml::parse_documents(input);
    if let Ok(stream) = ayaml::parse_events(input) {
        let _ = stream.collect::<Result<Vec<_>, _>>();
    }
});
