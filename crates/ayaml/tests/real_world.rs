//! Minimal fixtures reduced from public real-world YAML files.
//!
//! Each file in `tests/real-world/` reproduces, in a few lines, a source
//! shape. `tests/real-world/README.md` summarizes the sources, and each
//! `<name>.oracle.json` file sits next to `<name>.json` to record the
//! independent oracle, specification/test-suite provenance, canonical YAML
//! Test Suite event stream, and any reason why the `.json` golden cannot be
//! derived from a generic JSON or `libyaml` oracle alone.
//!
//! For every fixture this test checks:
//!
//! - the `.oracle.json` provenance metadata and pinned YAML Test Suite citations;
//! - the event stream against the independent `expected_events` oracle;
//! - the composed documents against an independent hand-constructed expected
//!   value (`expected_documents`) before checking or blessing `.json`, so a
//!   parser bug cannot be blessed into a golden;
//! - the composed documents, serialized with `serde_json`, against the
//!   fixture's `.json` file (set `AYAML_BLESS=1` to rewrite formatting after
//!   independent checks pass; `0` or leaving it unset doesn't, and any other
//!   value fails);
//! - the typed path and the document cursor against the composed documents;
//! - the compat path, for single-document fixtures;
//! - value and event round trips through both emitters.
//!
//! The fixtures live outside `tests/fixtures`, so the conformance fixture loops
//! don't pick them up.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ayaml::{CollectionStyle, Event, Mapping, Number, ScalarStyle, Tag, TaggedValue, Value};

const FIXTURES: &[&str] = &[
    // `- - a` with `- b` below it is two entries, not one scalar `"a - b"`.
    "compact-nested-sequence",
    // The second `...` composes no extra null document.
    "repeated-document-end",
    // Quoted scalars hold any `nb-json` character (YAML 1.2.2 section 5.1).
    // libyaml rejects DEL anywhere in the stream.
    "quoted-delete-character",
    // Integers are exact up to 128 bits.
    "wide-integer-content",
    // An empty stream holds no documents.
    "empty-file",
];

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureOracle {
    fixture: String,
    json_golden_origin: String,
    independent_event_oracle: String,
    cannot_use_independent_json_reason: Option<String>,
    source_provenance: String,
    yaml_1_2_2_refs: Vec<String>,
    yaml_test_suite_refs: Vec<String>,
    expected_events: Vec<String>,
}

/// Whether to rewrite the goldens. A value that means neither yes nor no must not rewrite them,
/// and treating it as no would hide the mistake.
fn bless_requested(value: Option<&std::ffi::OsStr>) -> bool {
    match value.map(std::ffi::OsStr::to_str) {
        None | Some(Some("0")) => false,
        Some(Some("1")) => true,
        Some(other) => panic!("AYAML_BLESS must be 0 or 1, or unset, not {other:?}"),
    }
}

fn repository_root() -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    if root.join(".alint.yml").is_file() {
        return Some(root);
    }
    assert!(
        manifest.join("Cargo.toml.orig").is_file(),
        "{} is neither in the repository nor an unpacked package",
        manifest.display()
    );
    None
}

fn fixture_dir() -> Option<PathBuf> {
    let root = repository_root()?;
    let dir = root.join("tests/real-world");
    assert!(
        dir.is_dir(),
        "real-world fixture directory missing at {}",
        dir.display()
    );
    Some(dir)
}

fn suite_src_dir() -> Option<PathBuf> {
    let root = repository_root()?;
    let dir = root.join("tests/data/yaml-test-suite/src");
    assert!(
        dir.is_dir(),
        "YAML Test Suite directory missing at {}",
        dir.display()
    );
    Some(dir)
}

fn parse_options() -> ayaml::ParseOptions {
    ayaml::ParseOptions::new().with_limits(ayaml::Limits::new().with_max_documents(16))
}

fn emit_options() -> ayaml::EmitOptions {
    ayaml::EmitOptions::new().with_limits(ayaml::Limits::new().with_max_documents(16))
}

fn json(documents: &impl serde::Serialize) -> String {
    let mut text = serde_json::to_string_pretty(documents).expect("serde_json");
    text.push('\n');
    text
}

fn format_event_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{8}' => out.push_str("\\b"),
            '\0' => out.push_str("\\0"),
            '\u{7}' => out.push_str("\\a"),
            '\u{b}' => out.push_str("\\v"),
            '\u{c}' => out.push_str("\\f"),
            '\u{1b}' => out.push_str("\\e"),
            '\u{7f}' => out.push_str("\\x7f"),
            '\u{85}' => out.push_str("\\N"),
            '\u{a0}' => out.push_str("\\_"),
            '\u{2028}' => out.push_str("\\L"),
            '\u{2029}' => out.push_str("\\P"),
            '\u{feff}' => out.push_str("\\uFEFF"),
            other => out.push(other),
        }
    }
    out
}

fn format_event_props(anchor: Option<&ayaml::Anchor<'_>>, tag: Option<&Tag>) -> String {
    let mut out = String::new();
    if let Some(anchor) = anchor {
        out.push_str(" &");
        out.push_str(anchor.as_str());
    }
    if let Some(tag) = tag {
        let _ = write!(out, " <{}{}>", tag.handle(), tag.suffix());
    }
    out
}

/// Format events into canonical YAML Test Suite event notation so they can be checked directly
/// against the independent `expected_events` oracle in each `.oracle.json` file.
fn format_suite_events(events: &[Event<'_>]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event {
            Event::StreamStart { .. } => "+STR".to_owned(),
            Event::StreamEnd { .. } => "-STR".to_owned(),
            Event::DocumentStart { explicit, .. } => {
                if *explicit { "+DOC ---" } else { "+DOC" }.to_owned()
            }
            Event::DocumentEnd { explicit, .. } => {
                if *explicit { "-DOC ..." } else { "-DOC" }.to_owned()
            }
            Event::MappingStart {
                anchor, tag, style, ..
            } => {
                let flow = if *style == CollectionStyle::Flow {
                    " {}"
                } else {
                    ""
                };
                format!(
                    "+MAP{flow}{}",
                    format_event_props(anchor.as_ref(), tag.as_ref())
                )
            }
            Event::MappingEnd { .. } => "-MAP".to_owned(),
            Event::SequenceStart {
                anchor, tag, style, ..
            } => {
                let flow = if *style == CollectionStyle::Flow {
                    " []"
                } else {
                    ""
                };
                format!(
                    "+SEQ{flow}{}",
                    format_event_props(anchor.as_ref(), tag.as_ref())
                )
            }
            Event::SequenceEnd { .. } => "-SEQ".to_owned(),
            Event::Scalar {
                value,
                anchor,
                tag,
                style,
                ..
            } => {
                let sigil = match style {
                    ScalarStyle::Plain => ':',
                    ScalarStyle::SingleQuoted => '\'',
                    ScalarStyle::DoubleQuoted => '"',
                    ScalarStyle::Literal => '|',
                    ScalarStyle::Folded => '>',
                    _ => unreachable!("all current ScalarStyle variants handled"),
                };
                format!(
                    "=VAL{} {sigil}{}",
                    format_event_props(anchor.as_ref(), tag.as_ref()),
                    format_event_escape(value)
                )
            }
            Event::Alias { anchor, .. } => format!("=ALI *{}", anchor.as_str()),
            Event::Directive { name, value, .. } => format!("%{name} {value}"),
            _ => unreachable!("all current Event variants handled"),
        })
        .collect()
}

fn map(entries: Vec<(&str, Value)>) -> Value {
    let mut mapping = Mapping::new();
    for (key, value) in entries {
        mapping.insert(Value::from(key), value);
    }
    Value::Mapping(mapping)
}

/// Independent expected values constructed directly from YAML 1.2.2 rules and documented profile
/// boundaries (never generated by `ayaml` itself).
fn expected_documents(name: &str) -> Vec<Value> {
    match name {
        "compact-nested-sequence" => {
            let expected_json = serde_json::json!([
                {
                    "intrinsics": [
                        {
                            "name": "vand{neon_type.no}",
                            "compose": [
                                {
                                    "FnCall": [
                                        "simd_and",
                                        ["a", "b"]
                                    ]
                                }
                            ]
                        },
                        {
                            "name": "vabd{neon_type.no}",
                            "compose": [
                                {
                                    "FnCall": [
                                        "simd_cast",
                                        [
                                            {
                                                "FnCall": [
                                                    "vabd_{neon_type[0]}",
                                                    ["c", "d"]
                                                ]
                                            }
                                        ]
                                    ]
                                }
                            ]
                        }
                    ]
                }
            ]);
            serde_json::from_value(expected_json).expect("convert independent JSON to Vec<Value>")
        }
        "empty-file" => Vec::new(),
        "quoted-delete-character" => vec![Value::from(TaggedValue::new(
            Tag::local("COFF"),
            map(vec![
                (
                    "header",
                    map(vec![("Machine", Value::from("IMAGE_FILE_MACHINE_AMD64"))]),
                ),
                (
                    "symbols",
                    Value::Sequence(vec![
                        map(vec![
                            ("Name", Value::from("__imp_foo")),
                            ("Value", Value::from(0_u64)),
                        ]),
                        map(vec![
                            ("Name", Value::from("__NULL_IMPORT_DESCRIPTOR")),
                            ("Value", Value::from(0_u64)),
                        ]),
                        map(vec![
                            ("Name", Value::from("\u{7f}foo_NULL_THUNK_DATA")),
                            ("Value", Value::from(0_u64)),
                        ]),
                    ]),
                ),
            ]),
        ))],
        "repeated-document-end" => vec![
            Value::from(TaggedValue::new(
                Tag::local("mach-o"),
                map(vec![(
                    "FileHeader",
                    map(vec![
                        ("magic", Value::from(0xFEED_FACF_u64)),
                        ("ncmds", Value::from(1_u64)),
                    ]),
                )]),
            )),
            Value::from(TaggedValue::new(
                Tag::local("mach-o"),
                map(vec![(
                    "FileHeader",
                    map(vec![
                        ("magic", Value::from(0xFEED_FACF_u64)),
                        ("ncmds", Value::from(2_u64)),
                    ]),
                )]),
            )),
        ],
        "wide-integer-content" => vec![Value::from(TaggedValue::new(
            Tag::local("minidump"),
            map(vec![(
                "Streams",
                Value::Sequence(vec![
                    map(vec![
                        ("Type", Value::from("ThreadList")),
                        (
                            "Threads",
                            Value::Sequence(vec![map(vec![
                                ("Thread Id", Value::from(0x0000_399E_u64)),
                                (
                                    "Stack",
                                    map(vec![
                                        (
                                            "Start of Memory Range",
                                            Value::from(0x0000_7FFE_FB76_A9B8_u64),
                                        ),
                                        (
                                            "Content",
                                            Value::Number(Number::from_u128(
                                                30_044_000_000_000_000_000_000_000_000_000_u128,
                                            )),
                                        ),
                                    ]),
                                ),
                            ])]),
                        ),
                    ]),
                    map(vec![
                        ("Type", Value::from("MemoryList")),
                        (
                            "Memory Ranges",
                            Value::Sequence(vec![
                                map(vec![
                                    (
                                        "Start of Memory Range",
                                        Value::from(0x0000_7FFE_FB76_A000_u64),
                                    ),
                                    ("Content", Value::Number(Number::from_f64(3.0044e+39))),
                                ]),
                                map(vec![
                                    (
                                        "Start of Memory Range",
                                        Value::from(0x0000_7FFE_FB76_B000_u64),
                                    ),
                                    ("Content", Value::from("0000000000000000")),
                                ]),
                                map(vec![
                                    (
                                        "Start of Memory Range",
                                        Value::from(0x0000_7FFE_FB76_C000_u64),
                                    ),
                                    ("Content", Value::from("30044000000000000000000000000000")),
                                ]),
                            ]),
                        ),
                    ]),
                ]),
            )]),
        ))],
        other => panic!("missing independent expected-value oracle for fixture {other:?}"),
    }
}

fn load_and_validate_oracle(name: &str, dir: &Path) -> FixtureOracle {
    let oracle_path = dir.join(format!("{name}.oracle.json"));
    let oracle_text = std::fs::read_to_string(&oracle_path).unwrap_or_else(|error| {
        panic!(
            "{name}: missing oracle sidecar {}: {error}",
            oracle_path.display()
        )
    });
    let oracle: FixtureOracle = serde_json::from_str(&oracle_text)
        .unwrap_or_else(|error| panic!("{name}: invalid oracle JSON: {error}"));

    assert_eq!(oracle.fixture, name, "{name}: oracle fixture name mismatch");
    assert!(
        !oracle.independent_event_oracle.trim().is_empty(),
        "{name}: independent_event_oracle must be documented"
    );
    assert!(
        !oracle.source_provenance.trim().is_empty(),
        "{name}: source_provenance must be documented"
    );
    assert!(
        !oracle.yaml_1_2_2_refs.is_empty(),
        "{name}: yaml_1_2_2_refs must cite YAML 1.2.2 rules"
    );
    assert!(
        !oracle.yaml_test_suite_refs.is_empty(),
        "{name}: yaml_test_suite_refs must cite pinned suite cases"
    );
    assert!(
        !oracle.expected_events.is_empty(),
        "{name}: expected_events must not be empty"
    );

    match oracle.json_golden_origin.as_str() {
        "independent" => assert!(
            oracle.cannot_use_independent_json_reason.is_none(),
            "{name}: independent golden should not record a cannot_use_independent_json_reason"
        ),
        "independent_with_documented_profile_notes" => assert!(
            oracle
                .cannot_use_independent_json_reason
                .as_deref()
                .is_some_and(|reason| !reason.trim().is_empty()),
            "{name}: non-pure-JSON golden must state why its .json cannot come purely from an external JSON/libyaml oracle"
        ),
        other => panic!("{name}: unexpected json_golden_origin {other:?}"),
    }

    if let Some(suite_dir) = suite_src_dir() {
        for case_id in &oracle.yaml_test_suite_refs {
            let case_path = suite_dir.join(format!("{case_id}.yaml"));
            assert!(
                case_path.is_file(),
                "{name}: cited YAML Test Suite case {case_id:?} does not exist at {}",
                case_path.display()
            );
        }
    }

    oracle
}

fn check(name: &str, dir: &Path) {
    let text = std::fs::read_to_string(dir.join(format!("{name}.yml"))).expect("fixture");
    let oracle = load_and_validate_oracle(name, dir);

    // 1. Check the independent event-stream oracle first.
    let events = ayaml::parse_events_with(&text, &parse_options())
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .unwrap_or_else(|error| panic!("{name}: parse_events_with: {error}"));
    assert_eq!(
        format_suite_events(&events),
        oracle.expected_events,
        "{name}: event stream differs from independent oracle in {name}.oracle.json"
    );

    // 2. Check composed documents against the independent Rust expected-value oracle BEFORE
    // comparing or blessing `.json`, so a parser regression cannot be blessed into goldens.
    let documents = ayaml::parse_documents_with(&text, &parse_options())
        .unwrap_or_else(|error| panic!("{name}: {error}"));
    let expected = expected_documents(name);
    assert_eq!(
        documents, expected,
        "{name}: documents differ from independent expected-value oracle"
    );

    // 3. For fixtures whose golden `.json` is pure independent JSON, also confirm the `.json`
    // file parses independently through `serde_json` to the exact expected `serde_json::Value`.
    let golden_path = dir.join(format!("{name}.json"));
    let actual = json(&documents);
    if bless_requested(std::env::var_os("AYAML_BLESS").as_deref()) {
        std::fs::write(&golden_path, &actual).expect("bless");
    }
    let golden = std::fs::read_to_string(&golden_path)
        .expect("golden")
        .replace("\r\n", "\n");
    assert_eq!(actual, golden, "{name}: documents differ from {name}.json");

    if oracle.json_golden_origin == "independent" {
        let golden_json: serde_json::Value =
            serde_json::from_str(&golden).expect("independent golden parses as JSON");
        let expected_json = serde_json::to_value(&expected).expect("serialize expected");
        assert_eq!(
            golden_json, expected_json,
            "{name}: independent golden JSON differs from expected value"
        );
    }

    // 4. The typed path and the document cursor.
    let mut cursor = ayaml::Deserializer::from_str_with(&text, &parse_options()).expect("cursor");
    let mut streamed = Vec::new();
    while let Some(document) = cursor.next_borrowed::<Value>().expect("next") {
        streamed.push(document);
    }
    assert_eq!(streamed, documents, "{name}: document cursor");
    if documents.len() <= 1 {
        let single = documents.first().cloned().unwrap_or(Value::Null);
        let typed = ayaml::from_str_borrowed::<Value>(&text).expect("typed");
        assert_eq!(typed, single, "{name}: from_str_borrowed");
        // Compat reads an empty stream as one null document.
        #[cfg(feature = "libyaml-compat")]
        assert_eq!(
            ayaml::compat::from_str::<Value>(&text).expect("compat"),
            single,
            "{name}: compat"
        );
    }

    // 5. Round trips through both emitters.
    for document in &documents {
        let output = ayaml::to_string_value(document).expect("emit value");
        assert_eq!(
            &ayaml::parse_value(&output).expect("reparse value"),
            document,
            "{name}: value round trip: {output:?}"
        );
    }
    let output = ayaml::to_string_events_with(&events, &emit_options()).expect("emit events");
    assert_eq!(
        ayaml::parse_documents_with(&output, &parse_options()).expect("reparse events"),
        documents,
        "{name}: event round trip: {output:?}"
    );
}

#[test]
fn real_world_fixtures() {
    let Some(dir) = fixture_dir() else {
        return;
    };
    for name in FIXTURES {
        check(name, &dir);
    }
}

#[test]
fn bless_takes_only_its_documented_values() {
    use std::ffi::OsStr;
    assert!(!bless_requested(None));
    assert!(!bless_requested(Some(OsStr::new("0"))));
    assert!(bless_requested(Some(OsStr::new("1"))));
    for wrong in ["", "2", "true", "no"] {
        let outcome = std::panic::catch_unwind(|| bless_requested(Some(OsStr::new(wrong))));
        assert!(outcome.is_err(), "{wrong:?}");
    }
}

/// Every fixture file has a matching `.json` golden and `.oracle.json` provenance sidecar, and
/// none is skipped silently.
#[test]
fn every_fixture_is_listed_with_an_oracle() {
    let Some(dir) = fixture_dir() else {
        return;
    };
    let mut yml_names = Vec::new();
    let mut json_names = Vec::new();
    let mut oracle_names = Vec::new();
    for entry in std::fs::read_dir(dir).expect("fixture dir") {
        let path = entry.expect("entry").path();
        let Some(file_name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
            continue;
        };
        if file_name == "README.md" {
            continue;
        }
        if let Some(stem) = file_name.strip_suffix(".yml") {
            yml_names.push(stem.to_owned());
        } else if let Some(stem) = file_name.strip_suffix(".oracle.json") {
            oracle_names.push(stem.to_owned());
        } else if let Some(stem) = file_name.strip_suffix(".json") {
            json_names.push(stem.to_owned());
        } else {
            panic!("unexpected file in tests/real-world/: {file_name}");
        }
    }
    yml_names.sort();
    json_names.sort();
    oracle_names.sort();
    let mut listed = FIXTURES.to_vec();
    listed.sort_unstable();
    assert_eq!(yml_names, listed, ".yml fixtures mismatch");
    assert_eq!(json_names, listed, ".json goldens mismatch");
    assert_eq!(oracle_names, listed, ".oracle.json sidecars mismatch");
}
