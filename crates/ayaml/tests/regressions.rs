//! Regression tests for parser, composer, Serde, and emitter behavior.
//! Expected values follow YAML 1.2.2 and, in compat mode, libyaml.

#[path = "regressions/emitter.rs"]
mod emitter;
#[path = "regressions/entry_points.rs"]
mod entry_points;
#[path = "regressions/indentation.rs"]
mod indentation;
#[path = "regressions/structure.rs"]
mod structure;
#[path = "regressions/typed.rs"]
mod typed;

use serde_json::{Value as Json, json};

fn parse(input: &str) -> Json {
    let value = ayaml::parse_value(input).unwrap_or_else(|error| panic!("{input:?}: {error}"));
    serde_json::to_value(&value).expect("JSON-compatible value")
}

fn parse_documents(input: &str) -> Vec<Json> {
    let limits = ayaml::Limits::default().with_max_documents(16);
    ayaml::parse_documents_with_limits(input, &limits)
        .unwrap_or_else(|error| panic!("{input:?}: {error}"))
        .iter()
        .map(|value| serde_json::to_value(value).expect("JSON-compatible value"))
        .collect()
}

fn rejects(input: &str) {
    assert!(
        ayaml::parse_value(input).is_err(),
        "{input:?} should be rejected, got {:?}",
        ayaml::parse_value(input)
    );
}

/// What a block scalar in a one-entry mapping under a sequence reads as, in both modes.
#[cfg(feature = "libyaml-compat")]
fn block_scalar_under_key(input: &str) -> [ayaml::Value; 2] {
    let read = |value: ayaml::Value| {
        value[0]
            .as_mapping()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .clone()
    };
    [
        read(ayaml::parse_value(input).unwrap_or_else(|error| panic!("{input:?}: {error}"))),
        read(ayaml::compat::from_str(input).unwrap_or_else(|error| panic!("{input:?}: {error}"))),
    ]
}

#[cfg(feature = "libyaml-compat")]
fn rejected_in_both_modes(input: &str) {
    rejects(input);
    assert!(
        ayaml::compat::from_str::<ayaml::Value>(input).is_err(),
        "{input:?} should be rejected in compat mode"
    );
}

/// Each mode's reading of `input`: the value, or the error's text.
fn readings(input: &str) -> Vec<(&'static str, Result<ayaml::Value, String>)> {
    vec![
        (
            "default",
            ayaml::parse_value(input).map_err(|error| error.to_string()),
        ),
        #[cfg(feature = "libyaml-compat")]
        (
            "compat",
            ayaml::compat::from_str::<ayaml::Value>(input).map_err(|error| error.to_string()),
        ),
    ]
}

/// Each input reads as its `equal` does, in each mode.
fn reads_as(cases: &[(&str, &str)]) {
    for &(input, equal) in cases {
        for ((mode, read), (_, expected)) in readings(input).into_iter().zip(readings(equal)) {
            let expected = expected.unwrap_or_else(|error| panic!("{equal:?}: {error}"));
            assert_eq!(read, Ok(expected), "{input:?} in {mode}");
        }
    }
}

/// Each input is refused with an error that starts with its message, in each mode.
fn refused_with(cases: &[(&str, &str)]) {
    for &(input, message) in cases {
        for (mode, read) in readings(input) {
            let error = read.expect_err(input);
            assert!(error.starts_with(message), "{input:?} in {mode}: {error}");
        }
    }
}

#[cfg(feature = "libyaml-compat")]
fn compat(input: &str) -> Json {
    let value = ayaml::compat::from_str::<ayaml::Value>(input)
        .unwrap_or_else(|error| panic!("{input:?}: {error}"));
    serde_json::to_value(&value).expect("JSON-compatible value")
}

/// Deserialize `input` through `from_str`, `from_str_borrowed`, and
/// `from_value`, in that order.
fn every_path<T>(input: &str) -> [Result<T, String>; 3]
where
    T: for<'de> serde::Deserialize<'de>,
{
    [
        ayaml::from_str::<T>(input).map_err(|error| error.to_string()),
        ayaml::from_str_borrowed::<T>(input).map_err(|error| error.to_string()),
        ayaml::parse_value(input)
            .and_then(ayaml::from_value::<T>)
            .map_err(|error| error.to_string()),
    ]
}

#[derive(Debug, PartialEq, serde::Deserialize)]
struct FieldA {
    a: u32,
    #[serde(default)]
    b: u32,
}

#[derive(Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
struct Defaults {
    a: u32,
    v: Vec<u32>,
    m: std::collections::BTreeMap<String, u32>,
}

#[derive(Debug, PartialEq, serde::Deserialize)]
enum Renamed {
    #[serde(rename = "true")]
    Yes,
    #[serde(rename = "1")]
    One,
    Plain,
}

fn merged_keys(input: &str, path: &str) -> Vec<String> {
    let mut value = ayaml::parse_value(input).expect("parse");
    value.apply_merge_keys().expect("merge");
    value[path]
        .as_mapping()
        .expect("mapping")
        .iter()
        .map(|(key, value)| format!("{}={value:?}", key.as_str().expect("string key")))
        .collect()
}

// ---------------------------------------------------------------------------
// Emitters
// ---------------------------------------------------------------------------

fn stream_limits() -> ayaml::Limits {
    ayaml::Limits::default().with_max_documents(16)
}

fn values(input: &str) -> Vec<ayaml::Value> {
    ayaml::parse_documents_with_limits(input, &stream_limits())
        .unwrap_or_else(|error| panic!("{input:?}: {error}"))
}

/// Emit the events of `input` with the collected and the lazy writer; both
/// must agree, and the output must describe the same documents.
fn emit_events(input: &str) -> String {
    let limits = stream_limits();
    let events = ayaml::parse_events_with_limits(input, &limits)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .unwrap_or_else(|error| panic!("{input:?}: {error}"));
    let output = ayaml::to_string_events_with_limits(&events, &limits).expect("emit events");
    let mut lazy = Vec::new();
    let stream = ayaml::parse_events_with_limits(input, &limits).expect("stream");
    ayaml::to_writer_event_stream_with_limits(&mut lazy, stream, &limits).expect("lazy emit");
    assert_eq!(String::from_utf8(lazy).expect("utf8"), output);
    assert_eq!(
        values(&output),
        values(input),
        "{input:?} became {output:?}"
    );
    output
}

fn emit_value(value: &ayaml::Value) -> String {
    let output = ayaml::to_string_value(value).expect("emit");
    assert_eq!(
        &ayaml::parse_value(&output).unwrap_or_else(|error| panic!("{output:?}: {error}")),
        value,
        "{output:?}"
    );
    output
}

/// Whether each entry point reads `input`: the error's text where one refuses it.
fn entry_points(input: &str) -> Vec<(&'static str, Result<(), String>)> {
    fn text<T>(result: Result<T, ayaml::Error>) -> Result<(), String> {
        result.map(drop).map_err(|error| error.to_string())
    }
    let streamed = || -> Result<(), ayaml::Error> {
        let mut deserializer = ayaml::Deserializer::from_str(input)?;
        while deserializer.next_borrowed::<ayaml::Value>()?.is_some() {}
        Ok(())
    };
    let lazy = || -> Result<(), ayaml::Error> {
        for event in ayaml::parse_events(input)? {
            event?;
        }
        Ok(())
    };
    vec![
        ("parse_documents", text(ayaml::parse_documents(input))),
        ("from_str", text(ayaml::from_str::<ayaml::Value>(input))),
        (
            "from_str_borrowed",
            text(ayaml::from_str_borrowed::<ayaml::Value>(input)),
        ),
        ("Deserializer", text(streamed())),
        ("parse_events", text(lazy())),
        #[cfg(feature = "libyaml-compat")]
        (
            "compat::parse_events_with_limits",
            text(ayaml::compat::parse_events_with_limits(
                input,
                &ayaml::Limits::default(),
            )),
        ),
        #[cfg(feature = "libyaml-compat")]
        (
            "compat::from_str",
            text(ayaml::compat::from_str::<ayaml::Value>(input)),
        ),
    ]
}

/// The typed readers, through both of their paths: from a document's nodes, as `from_str`,
/// `Deserializer::next_borrowed`, and compat read, and from a `Value`, as `from_value` and
/// `Deserializer::next` read. The seeded readers take these paths too.
fn every_typed_reader<T>(input: &str) -> Vec<(&'static str, Result<T, String>)>
where
    T: for<'de> serde::Deserialize<'de>,
{
    fn first<T>(document: Result<Option<T>, ayaml::Error>) -> Result<T, String> {
        document
            .map_err(|error| error.to_string())
            .and_then(|document| document.ok_or_else(|| "no document".to_owned()))
    }
    let [owned, borrowed, from_value] = every_path::<T>(input);
    let streamed =
        first(ayaml::Deserializer::from_str(input).and_then(|mut documents| documents.next::<T>()));
    let streamed_nodes = first(
        ayaml::Deserializer::from_str(input)
            .and_then(|mut documents| documents.next_borrowed::<T>()),
    );
    #[allow(
        unused_mut,
        reason = "the mutation is used only by some regression configurations"
    )]
    let mut readers = vec![
        ("from_str", owned),
        ("from_str_borrowed", borrowed),
        ("from_value", from_value),
        ("Deserializer::next", streamed),
        ("Deserializer::next_borrowed", streamed_nodes),
    ];
    #[cfg(feature = "libyaml-compat")]
    readers.push((
        "compat",
        ayaml::compat::from_str::<T>(input).map_err(|error| error.to_string()),
    ));
    readers
}
