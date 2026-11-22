//! Property tests for emitter, parser, and Serde round trips.

#![allow(
    let_underscore_drop,
    clippy::redundant_clone,
    reason = "test fixtures favor compact setup, generated inputs, and literal YAML braces"
)]

use ayaml::Value;
use proptest::prelude::*;

proptest! {
    #[test]
    fn emitted_strings_round_trip(text in any::<String>()) {
        let value = Value::from(text.as_str());
        let output = ayaml::to_string_value(&value).expect("emit");
        let decoded = ayaml::parse_value(&output).expect("reparse");
        prop_assert_eq!(decoded, value, "output={:?}", output);
    }

    #[test]
    fn parser_does_not_panic(input in any::<String>()) {
        let _ = ayaml::parse_value(&input);
    }

    #[test]
    fn emitted_string_sequences_round_trip(values in prop::collection::vec(any::<String>(), 0..8)) {
        let value = Value::Sequence(values.into_iter().map(Value::from).collect());
        let output = ayaml::to_string_value(&value).expect("emit");
        let decoded = ayaml::parse_value(&output).expect("reparse");
        prop_assert_eq!(decoded, value, "output={:?}", output);
    }
}

proptest! {
    #[test]
    fn borrowed_value_matches_composed_value(input in any::<String>()) {
        let composed = ayaml::parse_value(&input);
        let borrowed = ayaml::from_str_borrowed::<Value>(&input);
        match (composed, borrowed) {
            (Ok(composed), Ok(borrowed)) => {
                // Both paths keep tags, so they compare exactly, as the
                // `borrowed` fuzz target does.
                prop_assert_eq!(composed, borrowed);
            }
            (Err(_), Err(_)) => {}
            (Ok(value), Err(error)) => {
                prop_assert!(false, "composed {value:?}, borrowed error {error:?}");
            }
            (Err(error), Ok(value)) => {
                prop_assert!(false, "composed error {error:?}, borrowed {value:?}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Emitter oracles over generated values
// ---------------------------------------------------------------------------

/// Text that exercises YAML syntax: indicators, spaces, line breaks, quotes,
/// escapes, and look-alikes of other types.
fn yamlish_text() -> impl Strategy<Value = String> {
    prop_oneof![
        any::<String>(),
        "[-?:,#&*!|>'\"%@`\\[\\]{} a0.~\\n\\t]{0,10}",
        prop::sample::select(vec![
            "",
            "~",
            "null",
            "true",
            "no",
            "-1",
            "+1",
            "0x1F",
            "0o17",
            "1e3",
            ".inf",
            "-.inf",
            ".nan",
            "18446744073709551616",
            "-9223372036854775809",
            "...",
            "--- a",
            "- a",
            "? a",
            ": a",
            "a: b",
            "a #b",
            "#a",
            "a\n",
            "\n",
            "\n\n",
            " a",
            "a ",
            " \n",
            "a\n\nb\n\n",
            "\u{a0}",
            "\u{feff}a",
            "\u{85}",
            "\u{2028}",
            "\u{fffe}",
            "\t",
        ])
        .prop_map(str::to_owned),
    ]
}

fn scalar_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<f64>().prop_map(Value::from),
        yamlish_text().prop_map(Value::from),
    ]
}

fn tag_suffix() -> impl Strategy<Value = String> {
    prop_oneof![
        "[A-Za-z][A-Za-z0-9]{0,6}",
        "[a-z %!:/#,\\[\\]{}\u{e9}]{1,6}",
    ]
}

/// Nested values: sequences, mappings with any keys, and single-tagged
/// values, because YAML cannot hold a tag on a tagged value.
fn nested_value() -> impl Strategy<Value = Value> {
    scalar_value().prop_recursive(4, 48, 5, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(Value::Sequence),
            prop::collection::vec((inner.clone(), inner.clone()), 0..5).prop_map(|entries| {
                let mut mapping = ayaml::Mapping::new();
                for (key, value) in entries {
                    mapping.insert(key, value);
                }
                Value::Mapping(mapping)
            }),
            (tag_suffix(), inner).prop_map(|(suffix, value)| {
                if value.is_tagged() {
                    value
                } else {
                    Value::from(ayaml::TaggedValue::new(ayaml::Tag::local(suffix), value))
                }
            }),
        ]
    })
}

/// The first alias in `events` that has no node with its anchor earlier in its own document.
fn first_dangling_alias<'e>(events: &'e [ayaml::Event<'_>]) -> Option<&'e str> {
    use ayaml::Event;

    let mut anchors = std::collections::HashSet::new();
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

fn reparse(output: &str) -> Value {
    ayaml::parse_value(output).unwrap_or_else(|error| panic!("{error}; output={output:?}"))
}

// The default 256 cases keep `cargo test` fast; set PROPTEST_CASES for
// deeper runs.
proptest! {
    #[test]
    fn nested_values_round_trip(value in nested_value()) {
        let output = ayaml::to_string_value(&value).expect("emit");
        prop_assert_eq!(reparse(&output), value, "output={:?}", output);
    }

    #[test]
    fn nested_values_round_trip_with_options(
        value in nested_value(),
        flow in any::<bool>(),
        indent in 2_usize..=9,
        indent_sequences in any::<bool>(),
        style in prop::option::of(prop::sample::select(vec![
            ayaml::ScalarStyle::Plain,
            ayaml::ScalarStyle::SingleQuoted,
            ayaml::ScalarStyle::DoubleQuoted,
            ayaml::ScalarStyle::Literal,
            ayaml::ScalarStyle::Folded,
        ])),
    ) {
        let options = ayaml::EmitOptions::new()
            .with_flow(flow)
            .with_explicit_start(true)
            .with_explicit_end(true)
            .with_scalar_style(style)
            .with_indent(indent)
            .with_indent_sequences(indent_sequences);
        let output = ayaml::to_string_value_with(&value, &options).expect("emit");
        prop_assert_eq!(reparse(&output), value, "output={:?}", output);
    }

    /// Event emission of the parsed text keeps the documents, and the lazy
    /// writer writes exactly what the collected writer writes.
    #[test]
    fn event_emission_round_trips_generated_documents(value in nested_value()) {
        let text = ayaml::to_string_value(&value).expect("emit");
        let events = ayaml::parse_events(&text)
            .and_then(Iterator::collect::<Result<Vec<_>, _>>)
            .expect("events");
        let output = ayaml::to_string_events(&events).expect("emit events");
        prop_assert_eq!(reparse(&output), value, "text={:?} output={:?}", text, output);
        let mut lazy = Vec::new();
        ayaml::to_writer_event_stream(&mut lazy, ayaml::parse_events(&text).expect("stream"))
            .expect("lazy emit");
        prop_assert_eq!(String::from_utf8(lazy).expect("utf8"), output);
    }

    /// Whatever parses, both emitters write as text that parses back. The one exception is an
    /// alias with no anchor before it in its document, which the event readers report as it is
    /// written and the event writer refuses, since no value reader loads it (D97).
    #[test]
    fn emitters_accept_whatever_parses(input in yamlish_text()) {
        let Ok(events) = ayaml::parse_events(&input).and_then(Iterator::collect::<Result<Vec<_>, _>>)
        else {
            return Ok(());
        };
        let dangling = first_dangling_alias(&events);
        let output = match ayaml::to_string_events(&events) {
            Ok(output) => output,
            Err(error) => {
                // The refusal is of the first such alias, and no reader of values loads the
                // stream that holds it.
                let name = dangling.unwrap_or_else(|| panic!("{error}; input={input:?}"));
                prop_assert_eq!(
                    error.to_string(),
                    format!(
                        "cannot write the alias {name:?}: no node before it in its document has that anchor"
                    ),
                    "input={:?}",
                    input
                );
                prop_assert!(ayaml::parse_documents(&input).is_err(), "input={:?}", input);
                return Ok(());
            }
        };
        prop_assert!(dangling.is_none(), "input={:?} output={:?}", input, output);
        let reparsed = ayaml::parse_events(&output)
            .and_then(Iterator::collect::<Result<Vec<_>, _>>)
            .unwrap_or_else(|error| panic!("{error}; input={input:?} output={output:?}"));
        prop_assert_eq!(reparsed.len(), events.len(), "output={:?}", output);
        if let Ok(value) = ayaml::parse_value(&input) {
            prop_assert_eq!(reparse(&output), value.clone());
            let output = ayaml::to_string_value(&value).expect("emit value");
            prop_assert_eq!(reparse(&output), value, "output={:?}", output);
        }
    }
}

// ---------------------------------------------------------------------------
// Serde round trips
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
enum Shape {
    Unit,
    Newtype(i64),
    Flag(bool),
    Tuple(i32, String),
    Struct {
        name: Option<String>,
        sizes: Vec<u16>,
    },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Document {
    names: Vec<Vec<String>>,
    pairs: Vec<(i32, i32)>,
    maybe: Option<String>,
    nothing: Option<u8>,
    shape: Shape,
    shapes: Vec<Shape>,
    table: std::collections::BTreeMap<String, Vec<i64>>,
    ratio: f64,
    small: f32,
    letter: char,
    unit: (),
}

fn shape() -> impl Strategy<Value = Shape> {
    prop_oneof![
        Just(Shape::Unit),
        any::<i64>().prop_map(Shape::Newtype),
        any::<bool>().prop_map(Shape::Flag),
        (any::<i32>(), yamlish_text()).prop_map(|(a, b)| Shape::Tuple(a, b)),
        (
            prop::option::of(yamlish_text()),
            prop::collection::vec(any::<u16>(), 0..4)
        )
            .prop_map(|(name, sizes)| Shape::Struct { name, sizes }),
    ]
}

prop_compose! {
    fn document()(
        names in prop::collection::vec(prop::collection::vec(yamlish_text(), 0..3), 0..3),
        pairs in prop::collection::vec((any::<i32>(), any::<i32>()), 0..3),
        maybe in prop::option::of(yamlish_text()),
        nothing in prop::option::of(any::<u8>()),
        shape in shape(),
        shapes in prop::collection::vec(shape(), 0..3),
        table in prop::collection::btree_map(yamlish_text(), prop::collection::vec(any::<i64>(), 0..3), 0..3),
        ratio in any::<f64>().prop_filter("NaN is never equal", |value| !value.is_nan()),
        small in any::<f32>().prop_filter("NaN is never equal", |value| !value.is_nan()),
        letter in any::<char>(),
    ) -> Document {
        Document { names, pairs, maybe, nothing, shape, shapes, table, ratio, small, letter, unit: () }
    }
}

/// The round trip below can't notice a matching mistake in the emitter and the
/// reader, so this pins both sides to literal text.
#[test]
fn serde_types_match_literal_yaml() {
    let document = Document {
        names: vec![vec!["a".to_owned(), "b c".to_owned()], Vec::new()],
        pairs: vec![(1, -2)],
        maybe: Some("kept".to_owned()),
        nothing: None,
        shape: Shape::Newtype(7),
        shapes: vec![
            Shape::Unit,
            Shape::Flag(true),
            Shape::Tuple(3, "t".to_owned()),
            Shape::Struct {
                name: None,
                sizes: vec![4, 5],
            },
        ],
        table: std::collections::BTreeMap::from([("k".to_owned(), vec![6])]),
        ratio: 1.5,
        small: 0.25,
        letter: 'z',
        unit: (),
    };
    let text = "\
names:
- - a
  - b c
- []
pairs:
- - 1
  - -2
maybe: kept
nothing: null
shape: !Newtype 7
shapes:
- Unit
- !Flag true
-
  !Tuple
  - 3
  - t
-
  !Struct
  name: null
  sizes:
  - 4
  - 5
table:
  k:
  - 6
ratio: 1.5
small: 0.25
letter: z
unit: null
";
    assert_eq!(ayaml::to_string(&document).expect("emit"), text);
    assert_eq!(ayaml::from_str::<Document>(text).expect("read"), document);
}

proptest! {
    #[test]
    fn serde_types_round_trip(document in document()) {
        let output = ayaml::to_string(&document).expect("emit");
        let decoded: Document = ayaml::from_str(&output)
            .unwrap_or_else(|error| panic!("{error}; output={output:?}"));
        prop_assert_eq!(decoded, document, "output={:?}", output);
    }
}

proptest! {
    /// Equal tags hash alike whatever form wrote them: a local tag with its own handle, a verbatim
    /// tag, an expanded handle, or a core tag. The hasher is deterministic, and the test checks
    /// every pair in each spelling group.
    #[test]
    fn equal_tag_spellings_hash_alike(suffix in tag_suffix()) {
        use std::cmp::Ordering;
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        fn hash(tag: &ayaml::Tag) -> u64 {
            let mut hasher = DefaultHasher::new();
            tag.hash(&mut hasher);
            hasher.finish()
        }

        let groups = [
            vec![
                ayaml::Tag::local(suffix.as_str()),
                ayaml::Tag::new("!", suffix.as_str()),
                ayaml::Tag::new("", format!("!{suffix}")),
            ],
            vec![
                ayaml::Tag::new("tag:x:", suffix.as_str()),
                ayaml::Tag::new("", format!("tag:x:{suffix}")),
            ],
            vec![
                ayaml::Tag::core(suffix.as_str()),
                ayaml::Tag::new("tag:yaml.org,2002:", suffix.as_str()),
                ayaml::Tag::new("", format!("tag:yaml.org,2002:{suffix}")),
            ],
        ];
        for tags in groups {
            for left in &tags {
                for right in &tags {
                    prop_assert_eq!(left, right);
                    prop_assert_eq!(hash(left), hash(right));
                    prop_assert_eq!(left.cmp(right), Ordering::Equal);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Structured YAML property suites (AY-036)
// ---------------------------------------------------------------------------

#[path = "properties/invariants.rs"]
mod invariants;
#[path = "properties/structured.rs"]
mod structured;

use structured::{mutated_structured_yaml, structured_yaml_document, structured_yaml_stream};

#[derive(Default, Debug)]
struct ConstructCoverage {
    samples: usize,
    parsed_ok: usize,
    block_mappings: usize,
    flow_mappings: usize,
    block_sequences: usize,
    flow_sequences: usize,
    anchors: usize,
    aliases: usize,
    local_tags: usize,
    core_tags: usize,
    verbatim_or_handle_tags: usize,
    literal_block_scalars: usize,
    folded_block_scalars: usize,
    single_quoted_scalars: usize,
    double_quoted_scalars: usize,
    empty_plain_scalars: usize,
    directives: usize,
    bom_docs: usize,
    crlf_docs: usize,
    comment_docs: usize,
    multi_doc_streams: usize,
}

#[test]
fn structured_yaml_generator_covers_all_constructs() {
    use ayaml::{CollectionStyle, Event, ScalarStyle};
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::deterministic();
    let doc_strategy = structured_yaml_document();
    let stream_strategy = structured_yaml_stream();
    let mut cov = ConstructCoverage::default();

    for _ in 0..256 {
        let input = doc_strategy
            .new_tree(&mut runner)
            .expect("generate doc")
            .current();
        cov.samples += 1;
        if input.starts_with('\u{feff}') {
            cov.bom_docs += 1;
        }
        if input.contains("\r\n") {
            cov.crlf_docs += 1;
        }
        if input.contains('#') {
            cov.comment_docs += 1;
        }
        let events: Vec<Event<'_>> = ayaml::parse_events(&input)
            .and_then(Iterator::collect)
            .unwrap_or_else(|error| panic!("generated invalid YAML: {error}\ninput:\n{input}"));
        let _value = ayaml::parse_value(&input).unwrap_or_else(|error| {
            panic!("generated uncomposable YAML: {error}\ninput:\n{input}")
        });
        cov.parsed_ok += 1;

        for event in events {
            match event {
                Event::MappingStart {
                    style, anchor, tag, ..
                } => {
                    match style {
                        CollectionStyle::Block => cov.block_mappings += 1,
                        CollectionStyle::Flow => cov.flow_mappings += 1,
                        _ => {}
                    }
                    if anchor.is_some() {
                        cov.anchors += 1;
                    }
                    if let Some(tag) = tag {
                        record_tag(&mut cov, &tag);
                    }
                }
                Event::SequenceStart {
                    style, anchor, tag, ..
                } => {
                    match style {
                        CollectionStyle::Block => cov.block_sequences += 1,
                        CollectionStyle::Flow => cov.flow_sequences += 1,
                        _ => {}
                    }
                    if anchor.is_some() {
                        cov.anchors += 1;
                    }
                    if let Some(tag) = tag {
                        record_tag(&mut cov, &tag);
                    }
                }
                Event::Scalar {
                    value,
                    style,
                    anchor,
                    tag,
                    ..
                } => {
                    match style {
                        ScalarStyle::Literal => cov.literal_block_scalars += 1,
                        ScalarStyle::Folded => cov.folded_block_scalars += 1,
                        ScalarStyle::SingleQuoted => cov.single_quoted_scalars += 1,
                        ScalarStyle::DoubleQuoted => cov.double_quoted_scalars += 1,
                        ScalarStyle::Plain if value.is_empty() => {
                            cov.empty_plain_scalars += 1;
                        }
                        _ => {}
                    }
                    if anchor.is_some() {
                        cov.anchors += 1;
                    }
                    if let Some(tag) = tag {
                        record_tag(&mut cov, &tag);
                    }
                }
                Event::Alias { .. } => cov.aliases += 1,
                Event::Directive { .. } => cov.directives += 1,
                _ => {}
            }
        }
    }

    for _ in 0..64 {
        let input = stream_strategy
            .new_tree(&mut runner)
            .expect("generate stream")
            .current();
        let docs = ayaml::parse_documents(&input)
            .unwrap_or_else(|error| panic!("generated invalid stream: {error}\ninput:\n{input}"));
        if docs.len() >= 2 {
            cov.multi_doc_streams += 1;
        }
    }

    eprintln!(
        "structured YAML coverage ({} docs, {} parsed ok): block_map={} flow_map={} block_seq={} flow_seq={} anchors={} aliases={} local_tags={} core_tags={} verbatim_or_handle_tags={} literal_block={} folded_block={} single_quoted={} double_quoted={} empty_plain={} directives={} bom={} crlf={} comments={} multi_doc_streams={}",
        cov.samples,
        cov.parsed_ok,
        cov.block_mappings,
        cov.flow_mappings,
        cov.block_sequences,
        cov.flow_sequences,
        cov.anchors,
        cov.aliases,
        cov.local_tags,
        cov.core_tags,
        cov.verbatim_or_handle_tags,
        cov.literal_block_scalars,
        cov.folded_block_scalars,
        cov.single_quoted_scalars,
        cov.double_quoted_scalars,
        cov.empty_plain_scalars,
        cov.directives,
        cov.bom_docs,
        cov.crlf_docs,
        cov.comment_docs,
        cov.multi_doc_streams,
    );

    assert_eq!(cov.parsed_ok, cov.samples, "all generated docs must parse");
    assert!(cov.block_mappings >= 20, "{cov:?}");
    assert!(cov.flow_mappings >= 20, "{cov:?}");
    assert!(cov.block_sequences >= 20, "{cov:?}");
    assert!(cov.flow_sequences >= 20, "{cov:?}");
    assert!(cov.anchors >= 20, "{cov:?}");
    assert!(cov.aliases >= 20, "{cov:?}");
    assert!(cov.local_tags >= 20, "{cov:?}");
    assert!(cov.core_tags >= 10, "{cov:?}");
    assert!(cov.verbatim_or_handle_tags >= 10, "{cov:?}");
    assert!(cov.literal_block_scalars >= 15, "{cov:?}");
    assert!(cov.folded_block_scalars >= 15, "{cov:?}");
    assert!(cov.single_quoted_scalars >= 20, "{cov:?}");
    assert!(cov.double_quoted_scalars >= 20, "{cov:?}");
    assert!(cov.empty_plain_scalars >= 5, "{cov:?}");
    assert!(cov.directives >= 20, "{cov:?}");
    assert!(cov.bom_docs >= 10, "{cov:?}");
    assert!(cov.crlf_docs >= 15, "{cov:?}");
    assert!(cov.comment_docs >= 30, "{cov:?}");
    assert_eq!(cov.multi_doc_streams, 64, "{cov:?}");
}

fn record_tag(cov: &mut ConstructCoverage, tag: &ayaml::Tag) {
    let text = tag.to_string();
    if tag.is_core() {
        cov.core_tags += 1;
    } else if text.starts_with('!') && !text.starts_with("!<") {
        cov.local_tags += 1;
    } else {
        cov.verbatim_or_handle_tags += 1;
    }
}

proptest! {
    /// Structured valid YAML parses identically across every default value and typed reader.
    #[test]
    fn structured_yaml_readers_agree(input in structured_yaml_document()) {
        let composed = ayaml::parse_value(&input)
            .unwrap_or_else(|error| panic!("parse_value failed: {error}\ninput:\n{input}"));
        let typed = ayaml::from_str::<Value>(&input)
            .unwrap_or_else(|error| panic!("from_str failed: {error}\ninput:\n{input}"));
        let borrowed = ayaml::from_str_borrowed::<Value>(&input)
            .unwrap_or_else(|error| panic!("from_str_borrowed failed: {error}\ninput:\n{input}"));
        let mut cursor = ayaml::Deserializer::from_str(&input)
            .unwrap_or_else(|error| panic!("Deserializer::from_str failed: {error}\ninput:\n{input}"));
        let cursor_borrowed = cursor
            .next_borrowed::<Value>()
            .unwrap_or_else(|error| panic!("next_borrowed failed: {error}\ninput:\n{input}"))
            .expect("one document");
        prop_assert!(
            cursor
                .next_borrowed::<Value>()
                .expect("eof check")
                .is_none()
        );
        let mut owned_cursor = ayaml::Deserializer::from_str(&input)
            .unwrap_or_else(|error| panic!("Deserializer::from_str failed: {error}\ninput:\n{input}"));
        let cursor_owned = owned_cursor
            .next::<Value>()
            .unwrap_or_else(|error| panic!("next failed: {error}\ninput:\n{input}"))
            .expect("one document");
        prop_assert!(owned_cursor.next::<Value>().expect("eof check").is_none());

        prop_assert_eq!(&composed, &typed, "input:\n{}", input);
        prop_assert_eq!(&composed, &borrowed, "input:\n{}", input);
        prop_assert_eq!(&composed, &cursor_borrowed, "input:\n{}", input);
        prop_assert_eq!(&composed, &cursor_owned, "input:\n{}", input);

        #[cfg(feature = "libyaml-compat")]
        {
            let _ = ayaml::compat::from_str::<Value>(&input);
        }
    }

    /// Structured valid YAML round-trips through value emission, event emission, and lazy stream
    /// event emission.
    #[test]
    fn structured_yaml_events_and_values_round_trip(input in structured_yaml_document()) {
        let value = ayaml::parse_value(&input)
            .unwrap_or_else(|error| panic!("parse_value failed: {error}\ninput:\n{input}"));
        let emitted_value = ayaml::to_string_value(&value).expect("emit value");
        prop_assert_eq!(
            reparse(&emitted_value),
            value.clone(),
            "input:\n{}\nemitted_value:\n{}",
            input,
            emitted_value
        );

        let events = ayaml::parse_events(&input)
            .and_then(Iterator::collect::<Result<Vec<_>, _>>)
            .expect("parse events");
        let emitted_events = ayaml::to_string_events(&events)
            .unwrap_or_else(|error| panic!("emit events failed: {error}\ninput:\n{input}"));
        let mut lazy = Vec::new();
        ayaml::to_writer_event_stream(&mut lazy, ayaml::parse_events(&input).expect("stream"))
            .expect("lazy event emit");
        prop_assert_eq!(
            String::from_utf8(lazy).expect("utf8"),
            emitted_events.clone(),
            "input:\n{}",
            input
        );
        prop_assert_eq!(
            reparse(&emitted_events),
            value,
            "input:\n{}\nemitted_events:\n{}",
            input,
            emitted_events
        );
    }

    /// Generated multi-document structured streams agree across `parse_documents`, `Deserializer`,
    /// `to_string_documents`, and `to_string_events`.
    #[test]
    fn structured_yaml_stream_round_trips(input in structured_yaml_stream()) {
        let docs = ayaml::parse_documents(&input)
            .unwrap_or_else(|error| panic!("parse_documents failed: {error}\ninput:\n{input}"));
        let mut cursor = ayaml::Deserializer::from_str(&input).expect("cursor");
        let mut streamed = Vec::new();
        while let Some(doc) = cursor.next_borrowed::<Value>().expect("next_borrowed") {
            streamed.push(doc);
        }
        prop_assert_eq!(&streamed, &docs, "input:\n{}", input);

        let emitted_docs = ayaml::to_string_documents(&docs).expect("emit documents");
        let reparsed_docs = ayaml::parse_documents(&emitted_docs).unwrap_or_else(|error| {
            panic!("reparse documents failed: {error}\ninput:\n{input}\nemitted:\n{emitted_docs}")
        });
        prop_assert_eq!(reparsed_docs, docs.clone(), "input:\n{}", input);

        let events = ayaml::parse_events(&input)
            .and_then(Iterator::collect::<Result<Vec<_>, _>>)
            .expect("events");
        let emitted_events = ayaml::to_string_events(&events).expect("emit stream events");
        let reparsed_from_events = ayaml::parse_documents(&emitted_events).unwrap_or_else(|error| {
            panic!("reparse event stream failed: {error}\ninput:\n{input}\nemitted:\n{emitted_events}")
        });
        prop_assert_eq!(reparsed_from_events, docs, "input:\n{}", input);
    }

    /// Near-valid mutated structured YAML never panics and agrees between composed and borrowed
    /// value readers.
    #[test]
    fn mutated_structured_yaml_readers_agree(input in mutated_structured_yaml()) {
        let composed = ayaml::parse_value(&input);
        let borrowed = ayaml::from_str_borrowed::<Value>(&input);
        match (composed, borrowed) {
            (Ok(composed), Ok(borrowed)) => prop_assert_eq!(composed, borrowed),
            (Err(_), Err(_)) => {}
            (Ok(value), Err(error)) => {
                prop_assert!(
                    false,
                    "composed {value:?}, borrowed error {error:?}\ninput:\n{input}"
                );
            }
            (Err(error), Ok(value)) => {
                prop_assert!(
                    false,
                    "composed error {error:?}, borrowed {value:?}\ninput:\n{input}"
                );
            }
        }
    }
}
