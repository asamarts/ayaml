//! A typed reader's error names the path to what it rejected, on every entry point.

// These targets exist to be read and to fail, so no test looks at their fields.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::fmt::Debug;
use std::marker::PhantomData;

use ayaml::{Deserializer, Error, ErrorKind, Value};
use serde::Deserialize;
use serde::de::{DeserializeOwned, IgnoredAny};

/// Every typed entry point, and whether it reports source spans.
fn every_reader<T: DeserializeOwned + Debug>(input: &str) -> Vec<(&'static str, Error, bool)> {
    fn failed<T: Debug>(
        name: &'static str,
        result: Result<T, Error>,
        spans: bool,
    ) -> (&'static str, Error, bool) {
        match result {
            Ok(value) => panic!("{name} read {value:?}"),
            Err(error) => (name, error, spans),
        }
    }
    let seed = PhantomData::<T>;
    let first = |result: Result<Option<T>, Error>| result.map(|value| value.expect("a document"));
    let mut readers = vec![
        failed("from_str", ayaml::from_str::<T>(input), true),
        failed("from_slice", ayaml::from_slice::<T>(input.as_bytes()), true),
        failed(
            "from_str_borrowed",
            ayaml::from_str_borrowed::<T>(input),
            true,
        ),
        failed(
            "Deserializer::next_borrowed",
            first(
                Deserializer::from_str(input)
                    .expect("a stream")
                    .next_borrowed::<T>(),
            ),
            true,
        ),
        failed(
            "Deserializer::next_seed_borrowed",
            first(
                Deserializer::from_str(input)
                    .expect("a stream")
                    .next_seed_borrowed(seed),
            ),
            true,
        ),
        failed(
            "Deserializer::next",
            first(Deserializer::from_str(input).expect("a stream").next::<T>()),
            false,
        ),
        failed(
            "Deserializer::next_seed",
            first(
                Deserializer::from_str(input)
                    .expect("a stream")
                    .next_seed(seed),
            ),
            false,
        ),
    ];
    #[cfg(feature = "libyaml-compat")]
    {
        readers.push(failed(
            "compat::from_str",
            ayaml::compat::from_str::<T>(input),
            true,
        ));
        readers.push(failed(
            "compat::from_str_seed",
            ayaml::compat::from_str_seed(input, seed),
            true,
        ));
    }
    let value = ayaml::parse_value(input).expect("a value");
    readers.push(failed(
        "from_value",
        ayaml::from_value::<T>(value.clone()),
        false,
    ));
    readers.push(failed(
        "from_value_seed",
        ayaml::from_value_seed(value.clone(), seed),
        false,
    ));
    readers.push(failed("T::deserialize", T::deserialize(value), false));
    readers
}

/// Every reader gives `path` before its message, keeps the Serde kind, and the readers that report
/// spans point at the line and column.
#[track_caller]
fn assert_path<T: DeserializeOwned + Debug>(input: &str, path: &str, location: (usize, usize)) {
    for (reader, error, spans) in every_reader::<T>(input) {
        assert_eq!(error.kind(), ErrorKind::Serde, "{reader}: {error}");
        let message = error.message();
        let rest = message
            .strip_prefix(path)
            .and_then(|rest| rest.strip_prefix(": "))
            .unwrap_or_else(|| panic!("{reader}: {message:?} has no path {path:?}"));
        assert!(
            !rest.contains(&format!("{path}: ")),
            "{reader}: path repeated in {message:?}"
        );
        if spans {
            let found = error
                .location()
                .unwrap_or_else(|| panic!("{reader}: no location"));
            assert_eq!(
                (found.line(), found.column()),
                location,
                "{reader}: {error}"
            );
            assert!(error.to_string().starts_with(path), "{reader}: {error}");
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Task {
    title: String,
    #[serde(default)]
    effort: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    key: String,
    #[serde(flatten)]
    task: Task,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    #[serde(default)]
    tasks: Vec<Task>,
    #[serde(default)]
    entries: Vec<Entry>,
}

const PLAN: &str = "\
tasks:
  - title: first
  - title: second
    efort: 1
";

#[test]
fn a_sequence_of_nested_structs_reports_the_path_and_the_accepted_fields() {
    let error = ayaml::from_str::<Plan>(PLAN).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Serde);
    assert_eq!(
        error.to_string(),
        "tasks[1]: unknown field `efort`, expected `title` or `effort` at line 4 column 5"
    );
    let location = error.location().expect("a location");
    assert_eq!((location.line(), location.column()), (4, 5));
    assert_path::<Plan>(PLAN, "tasks[1]", (4, 5));
}

#[test]
fn a_flattened_struct_keeps_the_outer_path() {
    let input = "entries:\n  - key: a\n    title: t\n    efort: 1\n";
    let error = ayaml::from_str::<Plan>(input).unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("entries[0]: unknown field `efort`"),
        "{error}"
    );
    assert_path::<Plan>(input, "entries[0]", (2, 5));
    // A type error inside a flattened field reports the flattened struct's parent too.
    assert_path::<Plan>(
        "entries:\n  - key: a\n    title: [x]\n",
        "entries[0]",
        (2, 5),
    );
}

#[test]
fn a_type_error_names_each_field_and_index() {
    #[derive(Debug, Deserialize)]
    struct Inner {
        count: u8,
    }
    #[derive(Debug, Deserialize)]
    struct Middle {
        items: Vec<Inner>,
    }
    #[derive(Debug, Deserialize)]
    struct Outer {
        middle: Middle,
    }
    let input = "middle:\n  items:\n    - count: 1\n    - count: 2\n    - count: x\n";
    assert_path::<Outer>(input, "middle.items[2].count", (5, 14));
    let error = ayaml::from_str::<Outer>(input).unwrap_err();
    assert_eq!(
        error.to_string(),
        "middle.items[2].count: invalid type: string \"x\", expected u8 at line 5 column 14"
    );
    // A nested sequence reports each index, and a missing field reports what holds it.
    assert_path::<Vec<Vec<u8>>>("- [1]\n- [2, x]\n", "[1][1]", (2, 7));
    assert_path::<Outer>("middle:\n  items:\n    - {}\n", "middle.items[0]", (3, 7));
}

#[test]
fn a_root_error_gains_no_prefix() {
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Root {
        a: u8,
    }
    for input in ["x\n", "b: 1\n", "{}\n", "[]\n"] {
        for (reader, error, _) in every_reader::<Root>(input) {
            assert!(
                error.message().starts_with("invalid type")
                    || error.message().starts_with("unknown field")
                    || error.message().starts_with("missing field")
                    || error.message().starts_with("invalid length"),
                "{reader}: {error}"
            );
        }
    }
    let error = ayaml::from_str::<u8>("x\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid type: string \"x\", expected u8 at line 1 column 1"
    );
}

#[test]
fn map_keys_render_deterministically() {
    type Map = BTreeMap<String, u8>;
    assert_path::<BTreeMap<String, Map>>("ports:\n  web: x\n", "ports.web", (2, 8));
    assert_path::<Map>("runs-on: x\n", "runs-on", (1, 10));
    assert_path::<Map>("\"80\": x\n", "[\"80\"]", (1, 7));
    assert_path::<Map>("\"a.b\": x\n", "[\"a.b\"]", (1, 8));
    assert_path::<Map>("\"a b\": x\n", "[\"a b\"]", (1, 8));
    assert_path::<Map>("\"\": x\n", "[\"\"]", (1, 5));
    assert_path::<Map>("\"a\\\"b\": x\n", "[\"a\\\"b\"]", (1, 9));
    assert_path::<Map>("\"a\\nb\": x\n", "[\"a\\u{a}b\"]", (1, 9));
    assert_path::<Map>("名前: x\n", "[\"名前\"]", (1, 5));
    assert_path::<BTreeMap<String, Map>>("\"x y\":\n  z: x\n", "[\"x y\"].z", (2, 6));
    // A long key is cut short at the same place on every reader.
    let long = "k".repeat(1000);
    let path = format!("[\"{}…\"]", "k".repeat(64));
    assert_path::<Map>(&format!("{long}: x\n"), &path, (1, 1003));
}

#[test]
fn scalar_keys_that_are_not_strings_read_as_their_values() {
    assert_path::<BTreeMap<u16, u8>>("80: x\n", "[\"80\"]", (1, 5));
    assert_path::<BTreeMap<u16, u8>>("0x50: x\n", "[\"80\"]", (1, 7));
    assert_path::<BTreeMap<bool, u8>>("true: x\n", "true", (1, 7));
    assert_path::<HashMap<Value, u8>>("~: x\n", "null", (1, 4));
    assert_path::<HashMap<Value, u8>>("1.5: x\n", "[\"1.5\"]", (1, 6));
    assert_path::<HashMap<Value, u8>>("!t k: x\n", "k", (1, 7));
    assert_path::<HashMap<Value, u8>>("!t 7: x\n", "[\"7\"]", (1, 7));
}

#[test]
fn a_collection_key_has_a_short_deterministic_path() {
    assert_path::<HashMap<Value, u8>>("? [a, b]\n: x\n", "[?]", (2, 3));
    assert_path::<HashMap<Value, u8>>("? {a: b}\n: x\n", "[?]", (2, 3));
    assert_path::<HashMap<Value, BTreeMap<String, u8>>>("? [a]\n: {c: x}\n", "[?].c", (2, 7));
}

#[test]
fn a_key_that_fails_names_what_holds_it() {
    assert_path::<BTreeMap<String, BTreeMap<u8, u8>>>("m:\n  x: 1\n", "m", (2, 3));
}

#[test]
fn tuples_options_and_newtypes_add_only_what_they_hold() {
    #[derive(Debug, Deserialize)]
    struct Pair(u8, u8);
    #[derive(Debug, Deserialize)]
    struct Wrapper(Inner);
    #[derive(Debug, Deserialize)]
    struct Inner {
        a: u8,
    }
    #[derive(Debug, Deserialize)]
    struct Holder {
        #[serde(default)]
        tuple: Option<(u8, u8)>,
        #[serde(default)]
        pair: Option<Pair>,
        #[serde(default)]
        wrapper: Option<Wrapper>,
        #[serde(default)]
        boxed: Option<Box<Option<Inner>>>,
        #[serde(default)]
        scalar: Option<u8>,
    }
    assert_path::<Holder>("tuple: [1, x]\n", "tuple[1]", (1, 12));
    assert_path::<Holder>("pair: [1, x]\n", "pair[1]", (1, 11));
    assert_path::<Holder>("wrapper: {a: x}\n", "wrapper.a", (1, 14));
    assert_path::<Holder>("boxed: {a: x}\n", "boxed.a", (1, 12));
    assert_path::<Holder>("scalar: x\n", "scalar", (1, 9));
}

#[test]
fn an_enum_adds_the_variant_for_what_it_holds() {
    #[derive(Debug, Deserialize)]
    enum Shape {
        Unit,
        New(u8),
        Tuple(u8, u8),
        Fields { a: u8 },
    }
    #[derive(Debug, Deserialize)]
    struct Holder {
        shape: Shape,
    }
    assert_path::<Holder>("shape:\n  New: x\n", "shape.New", (2, 8));
    assert_path::<Holder>("shape:\n  Tuple: [1, x]\n", "shape.Tuple[1]", (2, 14));
    assert_path::<Holder>("shape:\n  Fields: {a: x}\n", "shape.Fields.a", (2, 15));
    assert_path::<Holder>("shape: !New x\n", "shape.New", (1, 13));
    assert_path::<Holder>("shape: !Fields {a: x}\n", "shape.Fields.a", (1, 20));
    // The variant's name fails in the enum's own place.
    assert_path::<Holder>("shape: Other\n", "shape", (1, 8));
    assert_path::<Holder>("shape:\n  Unit: 1\n", "shape.Unit", (2, 9));
    assert_path::<Vec<Shape>>("- Unit\n- New: x\n", "[1].New", (2, 8));
}

#[test]
fn an_alias_reads_as_the_place_it_is_used() {
    #[derive(Debug, Deserialize)]
    struct Inner {
        a: u8,
    }
    #[derive(Debug, Deserialize)]
    struct Holder {
        base: IgnoredAny,
        used: Inner,
    }
    // The location is where the text is, and the path is where it's read.
    assert_path::<Holder>("base: &b {a: x}\nused: *b\n", "used.a", (1, 14));
    #[derive(Debug, Deserialize)]
    struct Keys {
        k: IgnoredAny,
        a: u8,
    }
    assert_path::<Keys>("k: &k a\n*k : x\n", "a", (2, 6));
}

#[test]
fn a_deep_path_keeps_its_deepest_steps() {
    #[derive(Debug, Deserialize)]
    struct Tree(BTreeMap<String, Tree>);
    let name = "k".repeat(60);
    let mut input = "1".to_owned();
    for _ in 0..100 {
        input = format!("{{{name}: {input}}}");
    }
    input.push('\n');
    for (reader, error, _) in every_reader::<Tree>(&input) {
        let (path, _) = error.message().split_once(": ").expect("a path");
        assert!(path.starts_with(&format!("….{name}.")), "{reader}: {path}");
        assert!(path.len() < 1100, "{reader}: {}", path.len());
    }
}

#[test]
fn a_location_in_a_key_leaves_the_error_s_own_location() {
    let error = ayaml::from_str::<BTreeMap<String, u8>>("at line 9: x\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "[\"at line 9\"]: invalid type: string \"x\", expected u8 at line 1 column 12"
    );
}

#[test]
fn a_string_key_reads_the_same_whatever_the_target() {
    #[derive(Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
    enum Kind {
        Left,
    }
    #[derive(Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
    struct Name(String);

    fn owned<T: DeserializeOwned>(input: &str) -> Result<T, Error> {
        ayaml::from_value(ayaml::parse_value(input).expect("a value"))
    }

    let keys: BTreeMap<String, u8> = owned("a: 1\nb: 2\n").unwrap();
    assert_eq!(keys.len(), 2);
    let keys: BTreeMap<Option<String>, u8> = owned("a: 1\n").unwrap();
    assert_eq!(keys.keys().next().unwrap().as_deref(), Some("a"));
    let keys: BTreeMap<Name, u8> = owned("a: 1\n").unwrap();
    assert_eq!(keys.keys().next().unwrap().0, "a");
    let keys: BTreeMap<Kind, u8> = owned("Left: 1\n").unwrap();
    assert_eq!(keys.keys().next().unwrap(), &Kind::Left);
    let keys: BTreeMap<char, u8> = owned("a: 1\n").unwrap();
    assert_eq!(keys.keys().next().unwrap(), &'a');
    let keys: HashMap<Value, u8> = owned("a: 1\n").unwrap();
    assert_eq!(keys.keys().next().unwrap(), &Value::String("a".to_owned()));
    let keys: BTreeMap<IgnoredKey, u8> = owned("a: 1\n").unwrap();
    assert_eq!(keys.len(), 1);
    assert!(owned::<BTreeMap<u8, u8>>("a: 1\n").is_err());
    assert!(owned::<BTreeMap<Kind, u8>>("Right: 1\n").is_err());
    assert!(owned::<BTreeMap<Vec<u8>, u8>>("a: 1\n").is_err());

    /// A key read as bytes, as a byte-string target asks for it.
    #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
    struct IgnoredKey(Vec<u8>);
    impl<'de> Deserialize<'de> for IgnoredKey {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Bytes;
            impl serde::de::Visitor<'_> for Bytes {
                type Value = IgnoredKey;
                fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    formatter.write_str("bytes")
                }
                fn visit_bytes<E>(self, value: &[u8]) -> Result<IgnoredKey, E> {
                    Ok(IgnoredKey(value.to_vec()))
                }
            }
            deserializer.deserialize_bytes(Bytes)
        }
    }
    let keys: BTreeMap<IgnoredKey, u8> = owned("ab: 1\n").unwrap();
    assert_eq!(keys.keys().next().unwrap().0, b"ab");
}
