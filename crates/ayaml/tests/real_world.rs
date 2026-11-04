//! Minimal fixtures reduced from public real-world YAML files.
//!
//! Each file in `tests/real-world/` reproduces, in a few lines, a source
//! shape. `tests/real-world/README.md` names the source of each one. For every
//! fixture this test checks:
//!
//! - the composed documents, serialized with serde_json, against the
//!   fixture's `.json` file (set `AYAML_BLESS=1` to rewrite them, then review
//!   the diff by hand);
//! - the reference compatibility implementation against the expectation
//!   listed below;
//! - the typed path and the document cursor against the composed documents;
//! - the compat path, for single-document fixtures;
//! - value and event round trips through both emitters.
//!
//! The fixtures live outside `tests/fixtures`, whose loops in
//! `ayaml-conformance` and `ayaml-diff` require the reference implementation
//! to agree.

use std::path::{Path, PathBuf};

/// How yaml_serde reads a fixture.
#[derive(Clone, Copy, Debug)]
enum YamlSerde {
    /// The same documents.
    Same,
    /// It rejects the fixture.
    Rejects,
    /// It reads these documents (serde_json text) instead.
    Reads(&'static str),
}

const FIXTURES: &[(&str, YamlSerde)] = &[
    // P1: `- - a` with `- b` below it held one scalar, `"a - b"`.
    ("compact-nested-sequence", YamlSerde::Same),
    // P6: the second `...` composed an extra null document.
    ("repeated-document-end", YamlSerde::Same),
    // P32: quoted scalars hold any non-C0 character (YAML 1.2.2 section
    // 5.1). libyaml rejects DEL anywhere in the stream.
    ("quoted-delete-character", YamlSerde::Rejects),
    // V15: integers are exact up to 128 bits. yaml_serde's `Value` holds
    // 64-bit integers only.
    ("wide-integer-content", YamlSerde::Rejects),
    // V22: an empty stream holds no documents; yaml_serde reads one
    // null document.
    ("empty-file", YamlSerde::Reads("[null]")),
];

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/real-world")
}

fn limits() -> ayaml::Limits {
    ayaml::Limits::default().with_max_documents(16)
}

fn json(documents: &impl serde::Serialize) -> String {
    let mut text = serde_json::to_string_pretty(documents).expect("serde_json");
    text.push('\n');
    text
}

fn yaml_serde_documents(text: &str) -> Result<Vec<yaml_serde::Value>, String> {
    let mut documents = Vec::new();
    for document in yaml_serde::Deserializer::from_str(text) {
        // The iterator keeps yielding the same error after a failure.
        documents.push(
            <yaml_serde::Value as serde::Deserialize>::deserialize(document)
                .map_err(|error| error.to_string())?,
        );
    }
    Ok(documents)
}

fn check(name: &str, yaml_serde: YamlSerde) {
    let dir = fixture_dir();
    let text = std::fs::read_to_string(dir.join(format!("{name}.yml"))).expect("fixture");
    let documents = ayaml::parse_documents_with_limits(&text, &limits())
        .unwrap_or_else(|error| panic!("{name}: {error}"));

    // Golden documents.
    let golden_path = dir.join(format!("{name}.json"));
    let actual = json(&documents);
    if std::env::var_os("AYAML_BLESS").is_some() {
        std::fs::write(&golden_path, &actual).expect("bless");
    }
    let golden = std::fs::read_to_string(&golden_path).expect("golden");
    assert_eq!(actual, golden, "{name}: documents differ from {name}.json");

    // yaml_serde.
    match (yaml_serde, yaml_serde_documents(&text)) {
        (YamlSerde::Same, Ok(baseline)) => {
            assert_eq!(
                json(&baseline),
                golden,
                "{name}: yaml_serde reads it differently"
            );
        }
        (YamlSerde::Rejects, Err(_)) => {}
        (YamlSerde::Reads(expected), Ok(baseline)) => {
            let expected: serde_json::Value = serde_json::from_str(expected).expect("expected");
            assert_eq!(
                json(&baseline),
                json(&expected),
                "{name}: yaml_serde changed"
            );
        }
        (expectation, result) => {
            panic!("{name}: expected yaml_serde {expectation:?}, got {result:?}");
        }
    }

    // The typed path and the document cursor.
    let mut cursor = ayaml::Deserializer::from_str_with_limits(&text, &limits()).expect("cursor");
    let mut streamed = Vec::new();
    while let Some(document) = cursor.next_borrowed::<ayaml::Value>().expect("next") {
        streamed.push(document);
    }
    assert_eq!(streamed, documents, "{name}: document cursor");
    if documents.len() <= 1 {
        let single = documents.first().cloned().unwrap_or(ayaml::Value::Null);
        let typed = ayaml::from_str_borrowed::<ayaml::Value>(&text).expect("typed");
        assert_eq!(typed, single, "{name}: from_str_borrowed");
        // Compat reads an empty stream as one null document.
        #[cfg(feature = "libyaml-compat")]
        assert_eq!(
            ayaml::compat::from_str::<ayaml::Value>(&text).expect("compat"),
            single,
            "{name}: compat"
        );
    }

    // Round trips through both emitters.
    for document in &documents {
        let output = ayaml::to_string_value(document).expect("emit value");
        assert_eq!(
            &ayaml::parse_value(&output).expect("reparse value"),
            document,
            "{name}: value round trip: {output:?}"
        );
    }
    let events = ayaml::parse_events_with_limits(&text, &limits())
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .expect("events");
    let output = ayaml::to_string_events_with_limits(&events, &limits()).expect("emit events");
    assert_eq!(
        ayaml::parse_documents_with_limits(&output, &limits()).expect("reparse events"),
        documents,
        "{name}: event round trip: {output:?}"
    );
}

#[test]
fn real_world_fixtures() {
    for (name, yaml_serde) in FIXTURES {
        check(name, *yaml_serde);
    }
}

/// Every fixture file is listed, so none is skipped silently.
#[test]
fn every_fixture_is_listed() {
    let mut names: Vec<String> = std::fs::read_dir(fixture_dir())
        .expect("fixture dir")
        .filter_map(|entry| {
            let path = entry.expect("entry").path();
            (path.extension()? == "yml").then(|| path.file_stem()?.to_str().map(str::to_owned))?
        })
        .collect();
    names.sort();
    let mut listed: Vec<&str> = FIXTURES.iter().map(|(name, _)| *name).collect();
    listed.sort_unstable();
    assert_eq!(names, listed);
}
