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
        let options = ayaml::EmitterOptions::default()
            .with_flow(flow)
            .with_explicit_start(true)
            .with_explicit_end(true)
            .with_scalar_style(style)
            .with_indent(indent)
            .with_indent_sequences(indent_sequences);
        let output = ayaml::to_string_value_with_options(&value, &options).expect("emit");
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

    /// Whatever parses, both emitters write as text that parses back.
    #[test]
    fn emitters_accept_whatever_parses(input in yamlish_text()) {
        let Ok(events) = ayaml::parse_events(&input).and_then(Iterator::collect::<Result<Vec<_>, _>>)
        else {
            return Ok(());
        };
        let output = ayaml::to_string_events(&events).expect("emit events");
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
