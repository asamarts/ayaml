#![no_main]

//! The borrowed (typed) path must compose the same `Value` as the composed
//! path, tags included, one document at a time and as a stream.

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
    let streamed = streamed_documents(input, &limits);
    let composed = ayaml::parse_documents_with_limits(input, &limits);
    match (streamed, composed) {
        (Ok(streamed), Ok(composed)) => assert_eq!(streamed, composed, "input={input:?}"),
        (Err(_), Err(_)) => {}
        // The streamed path accepts a block collection on a later `---` line, and the
        // composed path rejects it. AY-025 rejects these on every entry point.
        (Ok(_), Err(_)) if collection_on_marker_line(input) => {}
        (streamed, composed) => {
            panic!("streamed {streamed:?}, composed {composed:?}, input={input:?}");
        }
    }
});

fn streamed_documents(
    input: &str,
    limits: &ayaml::Limits,
) -> Result<Vec<ayaml::Value>, ayaml::Error> {
    let mut deserializer = ayaml::Deserializer::from_str_with_limits(input, limits)?;
    let mut documents = Vec::new();
    while let Some(document) = deserializer.next_borrowed::<ayaml::Value>()? {
        documents.push(document);
    }
    Ok(documents)
}

fn collection_on_marker_line(input: &str) -> bool {
    input.split(['\n', '\r']).any(|line| {
        line.strip_prefix("---").is_some_and(|rest| {
            let content = rest.trim_start();
            rest.starts_with([' ', '\t'])
                && (content.starts_with(['-', '?', ':'])
                    || content.ends_with(':')
                    || content.contains(": "))
        })
    })
}
