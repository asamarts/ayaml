#![no_main]
#![forbid(unsafe_code)]

//! Emitter oracle: whatever ayaml parses, both emitters must write as text
//! that parses back to the same documents, and the lazy event writer must
//! write exactly what the collected-event writer writes.
//!
//! One thing parses as events and is not written (D97): an alias with no anchor before it in
//! its document, which no value reader loads. The oracle finds the first such alias for itself,
//! and then requires both event writers to refuse the stream for that alias and the value
//! reader to refuse the input. The compatibility reader's events are held to the same.

use std::collections::HashSet;

use ayaml::Event;
use libfuzzer_sys::fuzz_target;

/// The first alias in `events` that has no node with its anchor earlier in its own document.
fn first_dangling_alias<'e>(events: &'e [Event<'_>]) -> Option<&'e str> {
    let mut anchors = HashSet::new();
    for event in events {
        match event {
            Event::DocumentStart { .. } => anchors.clear(),
            Event::Alias { anchor, .. } => {
                if !anchors.contains(anchor.as_str()) {
                    return Some(anchor.as_str());
                }
            }
            Event::Scalar {
                anchor: Some(anchor),
                ..
            }
            | Event::SequenceStart {
                anchor: Some(anchor),
                ..
            }
            | Event::MappingStart {
                anchor: Some(anchor),
                ..
            } => {
                anchors.insert(anchor.as_str());
            }
            _ => {}
        }
    }
    None
}

/// What the writer has to say of the first dangling alias of a stream.
fn refusal_of(name: &str) -> String {
    format!("cannot write the alias {name:?}: no node before it in its document has that anchor")
}

/// The events of a stream but for its directives: the writers add a `%TAG` directive for a tag
/// that no verbatim tag can hold (D82), so what is read back can hold directives that the input
/// did not. A dropped or duplicated scalar, collection, alias, or marker event still shows.
fn without_directives(events: &[Event<'_>]) -> usize {
    events
        .iter()
        .filter(|event| !matches!(event, Event::Directive { .. }))
        .count()
}

/// The compatibility reader's events are written by the same writer. What it writes of them is
/// read back with every event, by that reader or, where it wrote YAML that libyaml's reading
/// refuses, by the default one; and it refuses nothing but a dangling alias.
fn compat_events(input: &str, limits: &ayaml::Limits) {
    let Ok(events) = ayaml::compat::parse_events_eager_with_limits(input, limits) else {
        return;
    };
    match ayaml::to_string_events_with_limits(&events, limits) {
        Ok(rendered) => {
            assert!(
                first_dangling_alias(&events).is_none(),
                "a dangling alias was written; input={input:?} rendered={rendered:?}"
            );
            let wanted = without_directives(&events);
            let read = |events: Result<Vec<Event<'_>>, ayaml::Error>| {
                events.is_ok_and(|events| without_directives(&events) == wanted)
            };
            assert!(
                read(ayaml::compat::parse_events_eager_with_limits(
                    &rendered, limits
                )) || read(
                    ayaml::parse_events_with_limits(&rendered, limits)
                        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                ),
                "what was written of the compatibility reader's events isn't read back; \
                 input={input:?} rendered={rendered:?}"
            );
        }
        Err(error) => {
            let name = first_dangling_alias(&events)
                .unwrap_or_else(|| panic!("event emission failed: {error}; input={input:?}"));
            assert_eq!(error.to_string(), refusal_of(name), "input={input:?}");
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let limits = ayaml::Limits::default().with_max_documents(64);
    compat_events(input, &limits);
    let Ok(stream) = ayaml::parse_events_with_limits(input, &limits) else {
        return;
    };
    let Ok(events) = stream.collect::<Result<Vec<_>, _>>() else {
        return;
    };

    let dangling = first_dangling_alias(&events);
    let rendered = match ayaml::to_string_events_with_limits(&events, &limits) {
        Ok(rendered) => rendered,
        Err(error) => {
            // The refusal is of the first such alias, the lazy writer says the same, and no
            // reader of values loads the input.
            let name = dangling
                .unwrap_or_else(|| panic!("event emission failed: {error}; input={input:?}"));
            assert_eq!(error.to_string(), refusal_of(name), "input={input:?}");
            let stream = ayaml::parse_events_with_limits(input, &limits).expect("parsed above");
            let lazy = ayaml::to_writer_event_stream_with_limits(&mut Vec::new(), stream, &limits)
                .expect_err("the lazy writer wrote a dangling alias");
            assert_eq!(lazy.to_string(), refusal_of(name), "input={input:?}");
            assert!(
                ayaml::parse_documents_with_limits(input, &limits).is_err(),
                "a value reader loads what the event writer refuses; input={input:?}"
            );
            return;
        }
    };
    assert!(
        dangling.is_none(),
        "a dangling alias was written; input={input:?} rendered={rendered:?}"
    );
    let reparsed = ayaml::parse_events_with_limits(&rendered, &limits)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .unwrap_or_else(|error| {
            panic!("event output does not parse: {error}; input={input:?} rendered={rendered:?}")
        });
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
