//! Contract tests for the public value, typed, and writer surfaces.

#[path = "contracts/core.rs"]
mod core;
#[path = "contracts/typed.rs"]
mod typed;
#[path = "contracts/writer.rs"]
mod writer;

use ayaml::{
    Anchor, CollectionStyle, Error, ErrorKind, Event, LimitKind, Limits, Location, Mapping, Marker,
    Number, ScalarStyle, Span, Tag, TaggedValue, Value,
};

/// A reader of a whole event stream.
type ReadEvents = fn(&str) -> Vec<Event<'_>>;

/// The line and column of a byte offset, counted from the text alone.
fn place_of(input: &str, index: usize) -> (usize, usize) {
    let before = &input[..index];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |text| text.chars().count())
        + 1;
    (line, column)
}

#[derive(Debug, PartialEq, serde::Deserialize)]
struct TestConfig {
    name: String,
    count: u64,
    enabled: bool,
    items: Vec<String>,
}

#[derive(Debug, PartialEq, serde::Deserialize)]
enum TestEnum {
    Unit,
    Tuple(u64, u64),
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
enum TaggedEnum {
    Unit,
    Tuple(u64, u64),
}

#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(untagged)]
enum Untagged {
    Number(u64),
    Text(String),
}

#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(default)]
struct Defaults {
    a: u64,
    b: String,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            a: 9,
            b: "default".to_owned(),
        }
    }
}

#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Strict {
    a: u64,
}

#[derive(Debug, PartialEq, serde::Deserialize)]
struct Wide {
    signed: i128,
    unsigned: u128,
}

#[derive(Debug, PartialEq, serde::Deserialize)]
struct Flattened {
    known: u64,
    #[serde(flatten)]
    rest: std::collections::BTreeMap<String, Value>,
}

#[derive(Debug, PartialEq, serde::Deserialize)]
struct TupleStruct(u64, String);

#[derive(Debug, PartialEq, serde::Deserialize)]
struct UnitStruct;

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct RoundTrip {
    name: String,
    values: Vec<u64>,
}

#[derive(Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
struct DefaultedDocument {
    name: String,
    count: u32,
}

#[derive(Debug, PartialEq, serde::Deserialize)]
struct BorrowedConfig<'a> {
    name: &'a str,
    label: &'a str,
    tags: Vec<&'a str>,
}

#[derive(Debug, serde::Deserialize)]
#[allow(
    dead_code,
    reason = "the shared test harness uses different subsets from each contract module"
)]
struct SpannedCount {
    count: u32,
}

#[derive(Debug, serde::Deserialize)]
#[allow(
    dead_code,
    reason = "the shared test harness uses different subsets from each contract module"
)]
struct NestedCounts {
    items: Vec<u32>,
}

#[derive(Debug, PartialEq, serde::Deserialize)]
struct BorrowedDocument<'a> {
    name: &'a str,
}

struct BorrowedScalarSeed;

impl<'de> serde::de::DeserializeSeed<'de> for BorrowedScalarSeed {
    type Value = &'de str;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        <&'de str as serde::Deserialize<'de>>::deserialize(deserializer)
    }
}

#[derive(Debug, PartialEq)]
struct Bytes(Vec<u8>);

struct BytesVisitor;

impl<'de> serde::de::Visitor<'de> for BytesVisitor {
    type Value = Bytes;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("bytes")
    }

    fn visit_bytes<E: serde::de::Error>(self, value: &[u8]) -> Result<Self::Value, E> {
        Ok(Bytes(value.to_vec()))
    }

    fn visit_borrowed_bytes<E: serde::de::Error>(self, value: &'de [u8]) -> Result<Self::Value, E> {
        Ok(Bytes(value.to_vec()))
    }

    fn visit_byte_buf<E: serde::de::Error>(self, value: Vec<u8>) -> Result<Self::Value, E> {
        Ok(Bytes(value))
    }
}

impl<'de> serde::Deserialize<'de> for Bytes {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_bytes(BytesVisitor)
    }
}

type ValueReader = fn(&str) -> Result<Value, Error>;

/// Each way to read a document into a value, with its name.
fn value_readers() -> Vec<(&'static str, ValueReader)> {
    vec![
        ("parse_value", ayaml::parse_value),
        ("from_str", ayaml::from_str::<Value>),
        ("from_str_borrowed", |input| {
            ayaml::from_str_borrowed::<Value>(input)
        }),
        ("parse_documents", |input| {
            ayaml::parse_documents(input).map(|mut documents| documents.remove(0))
        }),
        #[cfg(feature = "libyaml-compat")]
        ("compat::from_str", |input| {
            ayaml::compat::from_str::<Value>(input)
        }),
    ]
}
