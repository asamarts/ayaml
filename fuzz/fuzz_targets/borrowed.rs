#![no_main]

//! The borrowed (typed) path must compose the same `Value` as the composed
//! path, tags included (V5, V22), one document at a time and as a stream.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let composed = ayaml::parse_value(input);
    let borrowed = ayaml::from_str_borrowed::<ayaml::Value>(input);
    match (composed, borrowed) {
        (Ok(composed), Ok(borrowed)) => assert_eq!(composed, borrowed, "input={input:?}"),
        (Err(_), Err(_)) => {}
        (composed, borrowed) => {
            panic!("composed {composed:?}, borrowed {borrowed:?}, input={input:?}");
        }
    }

    let limits = ayaml::Limits::default().with_max_documents(4);
    let Ok(mut deserializer) = ayaml::Deserializer::from_str_with_limits(input, &limits) else {
        return;
    };
    let mut streamed = Vec::new();
    loop {
        match deserializer.next_borrowed::<ayaml::Value>() {
            Ok(Some(value)) => streamed.push(value),
            Ok(None) => break,
            Err(_) => return,
        }
    }
    let Ok(composed) = ayaml::parse_documents_with_limits(input, &limits) else {
        return;
    };
    assert_eq!(streamed, composed, "input={input:?}");
});
