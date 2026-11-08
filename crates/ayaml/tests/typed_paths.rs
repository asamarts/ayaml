//! A typed reader's error names the path to what it rejected, on every entry point.

// These targets exist to be read and to fail, so no test looks at their fields.
#![allow(dead_code)]

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt::{self, Debug};
use std::marker::PhantomData;

use ayaml::{Deserializer, Error, ErrorKind, Value};
use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer as Deserializer2, IgnoredAny, MapAccess, Visitor};

/// What every typed entry point reads from `input`, and whether it reports source spans.
fn every_reading<T: DeserializeOwned + Debug>(
    input: &str,
) -> Vec<(&'static str, Result<T, Error>, bool)> {
    let seed = PhantomData::<T>;
    let first = |result: Result<Option<T>, Error>| result.map(|value| value.expect("a document"));
    let mut readings = vec![
        ("from_str", ayaml::from_str::<T>(input), true),
        ("from_slice", ayaml::from_slice::<T>(input.as_bytes()), true),
        (
            "from_str_borrowed",
            ayaml::from_str_borrowed::<T>(input),
            true,
        ),
        (
            "Deserializer::next_borrowed",
            first(
                Deserializer::from_str(input)
                    .expect("a stream")
                    .next_borrowed::<T>(),
            ),
            true,
        ),
        (
            "Deserializer::next_seed_borrowed",
            first(
                Deserializer::from_str(input)
                    .expect("a stream")
                    .next_seed_borrowed(seed),
            ),
            true,
        ),
        (
            "Deserializer::next",
            first(Deserializer::from_str(input).expect("a stream").next::<T>()),
            false,
        ),
        (
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
        readings.push((
            "compat::from_str",
            ayaml::compat::from_str::<T>(input),
            true,
        ));
        readings.push((
            "compat::from_str_seed",
            ayaml::compat::from_str_seed(input, seed),
            true,
        ));
    }
    let value = ayaml::parse_value(input).expect("a value");
    readings.push(("from_value", ayaml::from_value::<T>(value.clone()), false));
    readings.push((
        "from_value_seed",
        ayaml::from_value_seed(value.clone(), seed),
        false,
    ));
    readings.push(("T::deserialize", T::deserialize(value), false));
    readings
}

/// Every typed entry point's error, and whether it reports source spans.
fn every_reader<T: DeserializeOwned + Debug>(input: &str) -> Vec<(&'static str, Error, bool)> {
    every_reading::<T>(input)
        .into_iter()
        .map(|(name, result, spans)| match result {
            Ok(value) => panic!("{name} read {value:?}"),
            Err(error) => (name, error, spans),
        })
        .collect()
}

/// Every typed entry point reads `expected` from `input`.
#[track_caller]
fn assert_reads<T: DeserializeOwned + Debug + PartialEq>(input: &str, expected: &T) {
    for (name, result, _) in every_reading::<T>(input) {
        match result {
            Ok(value) => assert_eq!(&value, expected, "{name}: {input:?}"),
            Err(error) => panic!("{name}: {input:?}: {error}"),
        }
    }
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
struct Server {
    host: String,
    #[serde(default)]
    port: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    key: String,
    #[serde(flatten)]
    server: Server,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    servers: Vec<Server>,
    #[serde(default)]
    entries: Vec<Entry>,
}

const CONFIG: &str = "\
servers:
  - host: first
  - host: second
    prot: 1
";

#[test]
fn a_sequence_of_nested_structs_reports_the_path_and_the_accepted_fields() {
    let error = ayaml::from_str::<Config>(CONFIG).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Serde);
    assert_eq!(
        error.to_string(),
        "servers[1]: unknown field `prot`, expected `host` or `port` at line 4 column 5"
    );
    let location = error.location().expect("a location");
    assert_eq!((location.line(), location.column()), (4, 5));
    assert_path::<Config>(CONFIG, "servers[1]", (4, 5));
}

#[test]
fn a_flattened_struct_keeps_the_outer_path() {
    let input = "entries:\n  - key: a\n    host: t\n    prot: 1\n";
    let error = ayaml::from_str::<Config>(input).unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("entries[0]: unknown field `prot`"),
        "{error}"
    );
    assert_path::<Config>(input, "entries[0]", (2, 5));
    // A type error inside a flattened field reports the flattened struct's parent too.
    assert_path::<Config>(
        "entries:\n  - key: a\n    host: [x]\n",
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

/// A key read as an enum names a variant with no content, so a variant that holds something fails
/// inside the key, and the error names the variant from the key's text.
#[test]
fn a_key_read_as_an_enum_names_its_variant() {
    #[derive(Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
    enum Side {
        Left,
        Right(u8),
    }
    assert_path::<BTreeMap<Side, u8>>("Right: 1\n", "Right", (1, 1));
    assert_path::<BTreeMap<String, BTreeMap<Side, u8>>>("m:\n  Right: 1\n", "m.Right", (2, 3));
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

/// An error's text never decides whether it says where the error is, so a value or a field name
/// that holds the words `at line` keeps the error's location on every reader that has one.
#[test]
fn a_value_or_a_field_that_quotes_a_location_keeps_the_error_s_own() {
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Only {
        a: Option<u8>,
    }

    let cases = [
        (
            every_reader::<BTreeMap<String, u8>>("a: \"at line 9\"\n"),
            "a: invalid type: string \"at line 9\", expected u8",
            (1, 4),
        ),
        (
            every_reader::<Only>("at line 1: 2\n"),
            "unknown field `at line 1`, expected `a`",
            (1, 1),
        ),
    ];
    for (readers, message, (line, column)) in cases {
        for (reader, error, spans) in readers {
            assert_eq!(error.message(), message, "{reader}");
            if spans {
                assert_eq!(
                    error.to_string(),
                    format!("{message} at line {line} column {column}"),
                    "{reader}"
                );
                let found = error.location().expect("a location");
                assert_eq!((found.line(), found.column()), (line, column), "{reader}");
            } else {
                assert_eq!(error.to_string(), message, "{reader}");
            }
        }
    }
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

/// A visitor may implement only the owned form of a key, as these do, however they ask for it.
#[test]
fn a_string_key_reads_for_a_target_that_takes_only_the_owned_form() {
    #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
    struct OwnedText<const HOW: u8>(String);
    impl<'de, const HOW: u8> Deserialize<'de> for OwnedText<HOW> {
        fn deserialize<D: Deserializer2<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Text<const HOW: u8>;
            // Implementing the owned form alone is the case under test.
            #[allow(clippy::serde_api_misuse)]
            impl<const HOW: u8> Visitor<'_> for Text<HOW> {
                type Value = OwnedText<HOW>;
                fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    formatter.write_str("an owned string")
                }
                fn visit_string<E>(self, value: String) -> Result<OwnedText<HOW>, E> {
                    Ok(OwnedText(value))
                }
            }
            match HOW {
                0 => deserializer.deserialize_string(Text::<HOW>),
                1 => deserializer.deserialize_str(Text::<HOW>),
                2 => deserializer.deserialize_identifier(Text::<HOW>),
                _ => deserializer.deserialize_any(Text::<HOW>),
            }
        }
    }
    #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
    struct OwnedBytes<const HOW: u8>(Vec<u8>);
    impl<'de, const HOW: u8> Deserialize<'de> for OwnedBytes<HOW> {
        fn deserialize<D: Deserializer2<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Bytes<const HOW: u8>;
            impl<const HOW: u8> Visitor<'_> for Bytes<HOW> {
                type Value = OwnedBytes<HOW>;
                fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    formatter.write_str("an owned byte buffer")
                }
                fn visit_byte_buf<E>(self, value: Vec<u8>) -> Result<OwnedBytes<HOW>, E> {
                    Ok(OwnedBytes(value))
                }
            }
            if HOW == 0 {
                deserializer.deserialize_byte_buf(Bytes::<HOW>)
            } else {
                deserializer.deserialize_bytes(Bytes::<HOW>)
            }
        }
    }
    fn only_key<K: DeserializeOwned + Ord>(input: &str) -> K {
        let keys: BTreeMap<K, u8> =
            ayaml::from_value(ayaml::parse_value(input).expect("a value")).expect("a map");
        keys.into_keys().next().expect("a key")
    }
    assert_eq!(only_key::<OwnedText<0>>("ab: 1\n").0, "ab");
    assert_eq!(only_key::<OwnedText<1>>("ab: 1\n").0, "ab");
    assert_eq!(only_key::<OwnedText<2>>("ab: 1\n").0, "ab");
    assert_eq!(only_key::<OwnedText<3>>("ab: 1\n").0, "ab");
    assert_eq!(only_key::<OwnedBytes<0>>("ab: 1\n").0, b"ab");
    assert_eq!(only_key::<OwnedBytes<1>>("ab: 1\n").0, b"ab");
    // A variant's name is read the same way, from a mapping or a tag.
    #[derive(Debug)]
    struct Variant(String);
    impl<'de> Deserialize<'de> for Variant {
        fn deserialize<D: Deserializer2<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Choice;
            impl<'de> Visitor<'de> for Choice {
                type Value = Variant;
                fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    formatter.write_str("a variant")
                }
                fn visit_enum<A: serde::de::EnumAccess<'de>>(
                    self,
                    data: A,
                ) -> Result<Variant, A::Error> {
                    let (name, variant): (OwnedText<2>, _) = data.variant()?;
                    serde::de::VariantAccess::unit_variant(variant)?;
                    Ok(Variant(name.0))
                }
            }
            deserializer.deserialize_enum("Variant", &["left"], Choice)
        }
    }
    for input in ["left\n", "left: null\n", "!left\n"] {
        let variant: Variant =
            ayaml::from_value(ayaml::parse_value(input).expect("a value")).expect(input);
        assert_eq!(variant.0, "left", "{input:?}");
    }
}

/// Reads a tagged node as a tag and a number, as a target that isn't an enum does.
#[derive(Debug)]
struct TagAndNumber;

impl<'de> Deserialize<'de> for TagAndNumber {
    fn deserialize<D: Deserializer2<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Reader;
        impl<'de> Visitor<'de> for Reader {
            type Value = TagAndNumber;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a tagged number")
            }
            fn visit_enum<A: serde::de::EnumAccess<'de>>(
                self,
                data: A,
            ) -> Result<TagAndNumber, A::Error> {
                let (_tag, value): (String, _) = data.variant()?;
                serde::de::VariantAccess::newtype_variant::<u8>(value)?;
                Ok(TagAndNumber)
            }
        }
        deserializer.deserialize_any(Reader)
    }
}

/// A tag isn't a key in the document, so a path to what's under it doesn't hold it. An enum's
/// variant is, whichever way the document writes it.
#[test]
fn a_tag_on_a_target_that_is_not_an_enum_is_not_a_path_step() {
    #[derive(Debug, Deserialize)]
    enum Kind {
        Left(u8),
    }
    for (name, error, _) in every_reader::<BTreeMap<String, TagAndNumber>>("a: !t x\n") {
        let text = error.to_string();
        assert!(text.starts_with("a: "), "{name}: {text}");
    }
    for (name, error, _) in every_reader::<BTreeMap<String, Kind>>("a: !Left x\n") {
        let text = error.to_string();
        assert!(text.starts_with("a.Left: "), "{name}: {text}");
    }
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn a_tag_is_not_a_path_step_for_a_duplicate_key_in_compat() {
    for (input, expected) in [
        (
            "a: !t {b: 1, b: 2}\n",
            "a: duplicate entry with key \"b\" at line 1 column 7",
        ),
        (
            "- !t [{b: 1, b: 2}]\n",
            "[0][0]: duplicate entry with key \"b\" at line 1 column 7",
        ),
        (
            "a: !t {c: !u {b: 1, b: 2}}\n",
            "a.c: duplicate entry with key \"b\" at line 1 column 14",
        ),
    ] {
        let error = ayaml::compat::from_str::<Value>(input).expect_err("a duplicate");
        assert_eq!(error.to_string(), expected, "{input:?}");
    }
}

/// Reads one key and asks for its value twice, or asks for a value before reading any key.
#[derive(Debug)]
struct Greedy<const KEY_FIRST: bool>;

impl<'de, const KEY_FIRST: bool> Deserialize<'de> for Greedy<KEY_FIRST> {
    fn deserialize<D: Deserializer2<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(Self)
    }
}

impl<'de, const KEY_FIRST: bool> Visitor<'de> for Greedy<KEY_FIRST> {
    type Value = Self;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a mapping")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self, A::Error> {
        if KEY_FIRST {
            map.next_key::<IgnoredAny>()?;
            map.next_value::<IgnoredAny>()?;
        }
        map.next_value::<IgnoredAny>()?;
        Ok(self)
    }
}

#[test]
fn a_map_gives_each_value_once_and_only_after_its_key() {
    let input = "a: 1\nb: 2\n";
    let before = ayaml::from_str::<Greedy<false>>(input).expect_err("no key was read");
    let twice = ayaml::from_str::<Greedy<true>>(input).expect_err("the value was read");
    for error in [before, twice] {
        assert!(
            error.to_string().contains("map value requested before key"),
            "{error}"
        );
    }
}

/// A tag outside YAML's own names means what the application says, so a target that isn't an
/// enum, a string, or a `Value` reads the node under it, on every reader. A YAML tag still means
/// what YAML says: `!!str 5` is a string.
#[test]
fn an_application_tag_leaves_a_plain_target_s_reading() {
    #[derive(Debug, Deserialize, PartialEq)]
    struct Server {
        port: u16,
    }
    #[derive(Debug, Deserialize, PartialEq)]
    struct Unit;
    #[derive(Debug, Deserialize, PartialEq)]
    struct Pair(u8, u8);
    for input in [
        "!port 5",
        "!<tag:example.com,2000:port> 5",
        "%TAG !e! tag:example.com,2000:\n--- !e!port 5",
    ] {
        assert_reads::<u32>(input, &5);
        assert_reads::<Option<u32>>(input, &Some(5));
    }
    assert_reads::<Vec<u32>>("!list [1, 2]", &vec![1, 2]);
    assert_reads::<Vec<u32>>("!list\n- 1\n- 2", &vec![1, 2]);
    assert_reads::<Vec<u32>>("!list", &vec![]);
    assert_reads::<(u8, u8)>("!pair [1, 2]", &(1, 2));
    assert_reads::<Pair>("!pair [1, 2]", &Pair(1, 2));
    assert_reads::<()>("!none ~", &());
    assert_reads::<Unit>("!none ~", &Unit);
    assert_reads::<bool>("!flag true", &true);
    assert_reads::<f64>("!ratio 1.5", &1.5);
    assert_reads::<BTreeMap<String, u16>>("!map {a: 1}", &BTreeMap::from([("a".to_owned(), 1)]));
    assert_reads::<Server>("!server {port: 80}", &Server { port: 80 });
    assert_reads::<Server>("port: !port 80", &Server { port: 80 });
    // A `Value` keeps the tag, and an enum takes it as the variant.
    let value = ayaml::from_str::<Value>("!port 5").expect("a value");
    assert!(matches!(value, Value::Tagged(_)), "{value:?}");
    for (name, error, _) in every_reader::<u32>("!!str 5") {
        assert!(
            error
                .message()
                .starts_with("invalid type: string \"5\", expected u32"),
            "{name}: {error}"
        );
    }
    // A YAML tag that a node keeps, as a collection keeps `!!binary`, isn't read past either.
    for (name, error, _) in every_reader::<Vec<u32>>("!!binary [1, 2]") {
        assert!(
            error.message().starts_with("invalid type: enum"),
            "{name}: {error}"
        );
    }
}

/// A tag with no value, as in `!Point`, fills a struct variant's defaults, as an empty value fills
/// a struct. A bare name names a unit variant, so `Point` fills none, and neither does `!Point ~`
/// on the readers that can tell it from an empty value.
#[test]
fn a_bare_tag_fills_a_struct_variant_s_defaults() {
    #[derive(Debug, Deserialize, PartialEq)]
    enum Shape {
        Point {
            #[serde(default)]
            x: u32,
        },
        Line(u32),
        #[serde(rename = "true")]
        Flag {
            #[serde(default)]
            x: u32,
        },
        #[serde(rename = "1")]
        One {
            #[serde(default)]
            x: u32,
        },
    }
    #[derive(Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
    enum Key {
        Point {
            #[serde(default)]
            x: u32,
        },
    }
    assert_reads(
        "- !Point\n- !Point {x: 2}\n- Point:\n- !Line 1",
        &vec![
            Shape::Point { x: 0 },
            Shape::Point { x: 2 },
            Shape::Point { x: 0 },
            Shape::Line(1),
        ],
    );
    // So does a scalar that names a variant renamed to it, or a key that names one.
    for input in ["Point", "true", "1"] {
        for (name, error, _) in every_reader::<Shape>(input) {
            assert!(
                error.message().contains("expected struct variant"),
                "{name}: {input:?}: {error}"
            );
        }
    }
    for (name, error, _) in every_reader::<BTreeMap<Key, u32>>("Point: 1") {
        assert!(
            error.message().contains("expected struct variant"),
            "{name}: {error}"
        );
    }
    let error = ayaml::from_str::<Shape>("!Point ~").expect_err("an explicit null");
    assert_eq!(
        error.to_string(),
        "Point: invalid type: unit value, expected struct variant Shape::Point at line 1 column 8"
    );
}

/// A visitor that takes an enum through `deserialize_any`, as a hand-written one may, gets a tag's
/// value as a struct variant's fields, so a tag with no value has none, on every reader.
#[test]
fn a_bare_tag_reached_through_any_fills_a_struct_variant() {
    #[derive(Debug, PartialEq)]
    struct Fields(usize);
    struct Variant;
    struct Count;
    impl<'de> Visitor<'de> for Variant {
        type Value = Fields;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a struct variant")
        }
        fn visit_enum<A: serde::de::EnumAccess<'de>>(self, data: A) -> Result<Fields, A::Error> {
            use serde::de::VariantAccess;
            let (_, variant) = data.variant::<String>()?;
            variant.struct_variant(&["x"], Count)
        }
    }
    impl<'de> Visitor<'de> for Count {
        type Value = Fields;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("fields")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Fields, A::Error> {
            let mut count = 0;
            while map.next_entry::<String, u32>()?.is_some() {
                count += 1;
            }
            Ok(Fields(count))
        }
    }
    impl<'de> Deserialize<'de> for Fields {
        fn deserialize<D: Deserializer2<'de>>(deserializer: D) -> Result<Self, D::Error> {
            deserializer.deserialize_any(Variant)
        }
    }
    assert_reads("!Point", &Fields(0));
    assert_reads("!Point {x: 1}", &Fields(1));
}

/// A plain scalar's text keeps the blanks before a comment or its line's end, and its value
/// doesn't, so the value is still borrowed from the input.
#[test]
fn a_plain_scalar_before_a_comment_or_blanks_is_borrowed() {
    #[derive(Debug, Deserialize)]
    struct Borrowing<'a> {
        #[serde(borrow)]
        text: &'a str,
        #[serde(borrow)]
        cow: Cow<'a, str>,
    }
    for input in [
        "text: abc # c\ncow: def\t# d: e\n",
        "text: abc   \ncow: def\t\n",
    ] {
        let mut readings = Vec::new();
        readings.push((
            "from_str_borrowed",
            ayaml::from_str_borrowed::<Borrowing<'_>>(input),
        ));
        readings.push((
            "Deserializer::next_borrowed",
            Deserializer::from_str(input)
                .expect("a stream")
                .next_borrowed()
                .map(|value| value.expect("a document")),
        ));
        #[cfg(feature = "libyaml-compat")]
        readings.push(("compat::from_str", ayaml::compat::from_str(input)));
        for (name, reading) in readings {
            let value = reading.unwrap_or_else(|error| panic!("{name}: {input:?}: {error}"));
            assert_eq!((value.text, value.cow.as_ref()), ("abc", "def"), "{name}");
            assert!(
                matches!(value.cow, Cow::Borrowed(_)),
                "{name}: {input:?} wasn't borrowed"
            );
        }
    }
    // Only blanks are dropped: by default a line separator is content, so the text still holds it.
    let text: &str = ayaml::from_str_borrowed("a\u{2028}  # c").expect("a separator is content");
    assert_eq!(text, "a\u{2028}");
    // A value that isn't the text, as when lines fold, is still owned.
    let folded =
        ayaml::from_str_borrowed::<Borrowing<'_>>("text: a\ncow: b\n  c\n").expect("folded");
    assert!(matches!(folded.cow, Cow::Owned(ref text) if text == "b c"));
}
