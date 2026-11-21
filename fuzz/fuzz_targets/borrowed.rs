#![no_main]
#![forbid(unsafe_code)]

//! The borrowed (typed) path must compose the same `Value` as the composed
//! path, tags included, one document at a time and as a stream. A typed read builds its value
//! from the parser's events, and must give what a read that builds a tree first gives.

use libfuzzer_sys::fuzz_target;

/// Both typed readers give the same value, or the same error at the same place.
macro_rules! same_by_tree_and_by_events {
    ($target:ty, $input:expr, $compat:expr) => {{
        let limits = ayaml::Limits::default();
        let tree = ayaml::internals::from_str_by_tree::<$target>($input, &limits, $compat);
        let events = ayaml::internals::from_str_by_events::<$target>($input, &limits, $compat);
        // What is printed is compared: a NaN is not equal to itself.
        let (tree, events) = match (tree, events) {
            (Ok(tree), Ok(events)) => (format!("{tree:?}"), format!("{events:?}")),
            (Err(tree), Err(events)) => {
                (format!("{tree:?} {tree}"), format!("{events:?} {events}"))
            }
            (tree, events) => {
                panic!("by tree {tree:?}, by events {events:?}, input={:?}", $input)
            }
        };
        assert_eq!(tree, events, "compat={} input={:?}", $compat, $input);
    }};
}

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let composed = ayaml::parse_value(input);
    let borrowed = ayaml::from_str_borrowed::<ayaml::Value>(input);
    match (composed, borrowed) {
        (Ok(composed), Ok(borrowed)) => assert_eq!(composed, borrowed, "input={input:?}"),
        // A typed map rejects scalar keys whose rendered text collides before a narrower key type
        // can silently keep only one; the composed value can keep both. This is deliberate.
        (Ok(_), Err(error)) if error.kind() == ayaml::ErrorKind::DuplicateKey => {}
        (Err(_), Err(_)) => {}
        (composed, borrowed) => {
            panic!("composed {composed:?}, borrowed {borrowed:?}, input={input:?}");
        }
    }

    for compat in [false, true] {
        same_by_tree_and_by_events!(ayaml::Value, input, compat);
        same_by_tree_and_by_events!(serde_json::Value, input, compat);
    }

    let options =
        ayaml::ParseOptions::new().with_limits(ayaml::Limits::new().with_max_documents(4));
    let streamed = streamed_documents(input, &options);
    let composed = ayaml::parse_documents_with(input, &options);
    match (streamed, composed) {
        (Ok(streamed), Ok(composed)) => assert_eq!(streamed, composed, "input={input:?}"),
        // The same deliberate typed-map duplicate-key difference applies to a stream.
        (Err(error), Ok(_)) if error.kind() == ayaml::ErrorKind::DuplicateKey => {}
        (Err(_), Err(_)) => {}
        (streamed, composed) => {
            panic!("streamed {streamed:?}, composed {composed:?}, input={input:?}");
        }
    }
});

fn streamed_documents(
    input: &str,
    options: &ayaml::ParseOptions,
) -> Result<Vec<ayaml::Value>, ayaml::Error> {
    let mut deserializer = ayaml::Deserializer::from_str_with(input, options)?;
    let mut documents = Vec::new();
    while let Some(document) = deserializer.next_borrowed::<ayaml::Value>()? {
        documents.push(document);
    }
    Ok(documents)
}
