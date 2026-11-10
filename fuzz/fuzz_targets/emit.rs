#![no_main]

//! Emitter oracle: whatever ayaml parses, both emitters must write as text
//! that parses back to the same documents, and the lazy event writer must
//! write exactly what the collected-event writer writes.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let limits = ayaml::Limits::default().with_max_documents(64);
    let Ok(stream) = ayaml::parse_events_with_limits(input, &limits) else {
        return;
    };
    let Ok(events) = stream.collect::<Result<Vec<_>, _>>() else {
        return;
    };

    let rendered = ayaml::to_string_events_with_limits(&events, &limits)
        .unwrap_or_else(|error| panic!("event emission failed: {error}; input={input:?}"));
    let reparsed = ayaml::parse_events_with_limits(&rendered, &limits)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .unwrap_or_else(|error| {
            panic!("event output does not parse: {error}; input={input:?} rendered={rendered:?}")
        });
    // The writers add a `%TAG` directive for a tag that no verbatim tag can hold (D82), so the
    // reparse can hold directives the input did not. Count the rest: a dropped or duplicated
    // scalar, collection, alias, or marker event still fails.
    let without_directives = |events: &[ayaml::Event<'_>]| {
        events
            .iter()
            .filter(|event| !matches!(event, ayaml::Event::Directive { .. }))
            .count()
    };
    assert_eq!(
        without_directives(&reparsed),
        without_directives(&events),
        "non-directive event count changed; input={input:?} rendered={rendered:?}"
    );

    let stream = ayaml::parse_events_with_limits(input, &limits).expect("parsed above");
    let mut lazy = Vec::new();
    ayaml::to_writer_event_stream_with_limits(&mut lazy, stream, &limits)
        .unwrap_or_else(|error| panic!("lazy emission failed: {error}; input={input:?}"));
    assert_eq!(
        lazy,
        rendered.as_bytes(),
        "lazy and collected writers differ; input={input:?}"
    );

    let Ok(documents) = ayaml::parse_documents_with_limits(input, &limits) else {
        return;
    };
    assert_eq!(
        ayaml::parse_documents_with_limits(&rendered, &limits)
            .ok()
            .as_ref(),
        Some(&documents),
        "event output changes the documents; input={input:?} rendered={rendered:?}"
    );
    let rendered = ayaml::to_string_documents_with_limits(&documents, &limits)
        .unwrap_or_else(|error| panic!("value emission failed: {error}; input={input:?}"));
    let decoded = ayaml::parse_documents_with_limits(&rendered, &limits).unwrap_or_else(|error| {
        panic!("value output does not parse: {error}; input={input:?} rendered={rendered:?}")
    });
    assert_eq!(
        decoded, documents,
        "value output changes the documents; input={input:?} rendered={rendered:?}"
    );
});
