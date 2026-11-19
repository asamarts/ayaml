//! A typed read builds its value from the parser's events. It has to give what a read that
//! builds a tree of the document first gives: the same value, or the same error at the same
//! place. These tests read documents both ways, into targets that read as they like.
#![cfg(feature = "bench-internals")]

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt::{self, Debug, Formatter};

use ayaml::internals::{from_str_by_events, from_str_by_tree};
use ayaml::{Limits, Value};
use serde::Deserialize;
use serde::de::{Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

#[derive(Debug, PartialEq, Deserialize)]
struct Config<'a> {
    name: Option<String>,
    #[serde(borrow)]
    a: Option<&'a str>,
    b: Option<f64>,
    #[serde(default)]
    items: Vec<Item>,
    #[serde(default)]
    map: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Strict {
    a: Option<serde_json::Value>,
    b: Option<Vec<serde_json::Value>>,
    k: Option<String>,
}

#[derive(Debug, PartialEq, Deserialize)]
struct Item {
    id: Option<u32>,
    #[serde(flatten)]
    rest: HashMap<String, Value>,
}

#[derive(Debug, PartialEq, Deserialize)]
#[allow(
    non_camel_case_types,
    reason = "the variants are named as the documents' keys are"
)]
enum Shape {
    Unit,
    New(i64),
    Tuple(i64, String),
    Struct { a: Option<i64>, b: Option<String> },
    List(Vec<serde_json::Value>),
    Map(BTreeMap<String, serde_json::Value>),
    a,
    k,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(untagged)]
enum Untagged {
    Int(i64),
    Text(String),
    List(Vec<Untagged>),
    Map(BTreeMap<String, Untagged>),
    Unit(()),
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "type")]
enum Internal {
    A { a: Option<i64> },
    B { b: Option<String> },
}

#[derive(Debug, PartialEq, Deserialize)]
struct Nested(Vec<Option<Inner>>);
#[derive(Debug, PartialEq, Deserialize)]
struct Inner(BTreeMap<String, Option<Box<Nested>>>);
#[derive(Debug, PartialEq, Deserialize)]
struct Unit;
#[derive(Debug, PartialEq, Deserialize)]
struct Pair(serde_json::Value, serde_json::Value);

/// A value that is read and not kept. Two of them are equal.
#[derive(Deserialize)]
struct Ignored(IgnoredAny);

impl Debug for Ignored {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let Self(IgnoredAny) = self;
        formatter.write_str("Ignored")
    }
}

impl PartialEq for Ignored {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// Keeps the integers of a collection and counts what didn't read as one. Its visitor takes an
/// entry's error and goes on to the next entry, which a tree lets it do.
#[derive(Debug, PartialEq)]
struct Lenient {
    items: Vec<i64>,
    errors: usize,
}

/// More entries than any document here has, so a visitor that is never told the end fails.
const MOST_ENTRIES: usize = 10_000;

impl<'de> Deserialize<'de> for Lenient {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Read;
        impl<'de> Visitor<'de> for Read {
            type Value = Lenient;

            fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                formatter.write_str("a collection")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Lenient, A::Error> {
                let mut read = Lenient {
                    items: Vec::new(),
                    errors: 0,
                };
                for _ in 0..MOST_ENTRIES {
                    match access.next_element::<i64>() {
                        Ok(Some(item)) => read.items.push(item),
                        Ok(None) => return Ok(read),
                        Err(_) => read.errors += 1,
                    }
                }
                Err(serde::de::Error::custom("the sequence never ended"))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Lenient, A::Error> {
                let mut read = Lenient {
                    items: Vec::new(),
                    errors: 0,
                };
                for _ in 0..MOST_ENTRIES {
                    match access.next_key::<String>() {
                        Ok(Some(_)) => {}
                        Ok(None) => return Ok(read),
                        Err(_) => read.errors += 1,
                    }
                    match access.next_value::<Lenient>() {
                        Ok(inner) => {
                            read.items.extend(inner.items);
                            read.errors += inner.errors;
                        }
                        Err(_) => read.errors += 1,
                    }
                }
                Err(serde::de::Error::custom("the mapping never ended"))
            }
        }
        deserializer.deserialize_any(Read)
    }
}

/// Reads nothing from its deserializer. It holds a byte, so that a map of them is no set.
#[derive(Debug, PartialEq)]
struct Nothing(u8);

impl<'de> Deserialize<'de> for Nothing {
    fn deserialize<D: Deserializer<'de>>(_: D) -> Result<Self, D::Error> {
        Ok(Self(0))
    }
}

/// Fails before it reads anything, so its error is placed by whatever handed it the node. It
/// holds a byte, so that a map of them is no set.
#[derive(Debug, PartialEq)]
struct Refusing(u8);

impl<'de> Deserialize<'de> for Refusing {
    fn deserialize<D: Deserializer<'de>>(_: D) -> Result<Self, D::Error> {
        Err(serde::de::Error::custom("refused"))
    }
}

#[derive(Debug, PartialEq, Deserialize)]
struct Holding(Refusing);

/// Reads a mapping's keys and none of its values, or a sequence's first item and no more.
#[derive(Debug, PartialEq)]
struct Partial(Vec<String>);

impl<'de> Deserialize<'de> for Partial {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Read;
        impl<'de> Visitor<'de> for Read {
            type Value = Partial;

            fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                formatter.write_str("a collection")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Partial, A::Error> {
                let first = access.next_element::<Partial>()?;
                Ok(Partial(first.map(|first| first.0).unwrap_or_default()))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Partial, A::Error> {
                let mut keys = Vec::new();
                while let Some(key) = access.next_key::<serde_json::Value>()? {
                    keys.push(key.to_string());
                }
                Ok(Partial(keys))
            }
        }
        deserializer.deserialize_any(Read)
    }
}

/// Asks a mapping for a value before any key, takes that error, and then reads it in order.
#[derive(Debug, PartialEq)]
struct ValueFirst {
    refused: bool,
    entries: Vec<(String, Ignored)>,
}

impl<'de> Deserialize<'de> for ValueFirst {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Read;
        impl<'de> Visitor<'de> for Read {
            type Value = ValueFirst;

            fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                formatter.write_str("a mapping")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<ValueFirst, A::Error> {
                let refused = access.next_value::<IgnoredAny>().is_err();
                let mut entries = Vec::new();
                while let Some(entry) = access.next_entry()? {
                    entries.push(entry);
                }
                Ok(ValueFirst { refused, entries })
            }
        }
        deserializer.deserialize_map(Read)
    }
}

/// How the two readers' outcomes differ on one target, if they do.
fn difference<'de, T: Deserialize<'de> + Debug + PartialEq>(
    input: &'de str,
    limits: &Limits,
    compat: bool,
) -> Option<String> {
    let tree = from_str_by_tree::<T>(input, limits, compat);
    let events = from_str_by_events::<T>(input, limits, compat);
    let same = match (&tree, &events) {
        // A value that holds a NaN is not equal to itself, so what is printed is compared too.
        (Ok(tree), Ok(events)) => tree == events || format!("{tree:?}") == format!("{events:?}"),
        (Err(tree), Err(events)) => format!("{tree:?} {tree}") == format!("{events:?} {events}"),
        _ => false,
    };
    (!same).then(|| format!("the tree gives {tree:?}\nand the events give {events:?}"))
}

/// Which targets a document is read into.
#[derive(Clone, Copy, PartialEq)]
enum Targets {
    /// Every target.
    All,
    /// One target of each kind: a tree of any value, a struct, an enum, and a visitor that
    /// goes on after an error.
    Few,
}

/// The first target that the two readers read differently, with how.
fn first_difference(
    input: &str,
    limits: &Limits,
    compat: bool,
    targets: Targets,
) -> Option<String> {
    macro_rules! each {
        ($($target:ty),* $(,)?) => {
            $(
                if let Some(how) = difference::<$target>(input, limits, compat) {
                    return Some(format!("into {}, {how}", stringify!($target)));
                }
            )*
        };
    }
    each!(Value, serde_json::Value, Config<'_>, Shape, Lenient);
    if targets == Targets::Few {
        return None;
    }
    each!(
        Ignored,
        Strict,
        Vec<Item>,
        Vec<Shape>,
        BTreeMap<String, Shape>,
        Untagged,
        Internal,
        Nested,
        Unit,
        Pair,
        String,
        &str,
        Cow<'_, str>,
        Vec<&str>,
        Vec<Option<i64>>,
        Vec<f64>,
        Vec<char>,
        (i64, String),
        [serde_json::Value; 2],
        Option<Vec<serde_json::Value>>,
        Option<BTreeMap<String, serde_json::Value>>,
        HashMap<String, serde_json::Value>,
        BTreeMap<i64, serde_json::Value>,
        HashMap<Value, Value>,
        Vec<BTreeMap<String, Option<String>>>,
        f64,
        bool,
        (),
        Vec<Lenient>,
        Nothing,
        Vec<Nothing>,
        BTreeMap<String, Nothing>,
        Partial,
        ValueFirst,
        BTreeMap<String, ValueFirst>,
        Refusing,
        Option<Refusing>,
        Holding,
        Vec<Refusing>,
        Vec<Option<Refusing>>,
        BTreeMap<String, Holding>,
    );
    None
}

/// Limits that the generated documents reach: a depth, a collection's size, and a count of
/// events that a few lines pass.
fn tight() -> Limits {
    Limits::default()
        .with_max_depth(3)
        .with_max_collection_entries(4)
        .with_max_events(40)
}

fn assert_same(input: &str, targets: Targets) {
    for compat in [false, true] {
        for limits in [Limits::default(), tight()] {
            if let Some(how) = first_difference(input, &limits, compat, targets) {
                panic!("compat {compat}, {input:?} read {how}");
            }
        }
    }
}

/// Documents that reach what a stream of events has to keep or check itself: anchors and what
/// reads them again, tags, enums in each form, duplicate keys at each level, collections where
/// a scalar is wanted and the reverse, and faults at the start, in the middle, and at the end.
const DOCUMENTS: &[&str] = &[
    "",
    "\n",
    "# only a comment\n",
    "---\n",
    "---\n...\n",
    "...\n",
    "a\n",
    "~\n",
    "null\n",
    "''\n",
    "12\n",
    "0x1F\n",
    "1.5\n",
    "true\n",
    "[1, 2, 3]\n",
    "[1, two, 3]\n",
    "[1, [2, x], 3, {k: 4}]\n",
    "- 1\n- two\n- [3, x]\n- {k: 4}\n- 5\n",
    "{a: 1, b: [1, 2], k: text}\n",
    "a: 1\nb: [1, x]\nk: text\n",
    "a: 1\nb:\n- 1\n- two\nk: text\nzzz: 4\n",
    "name: n\na: text\nb: 1.5\nitems:\n- id: 1\n  x: y\n- id: 2\nmap:\n  k: [1, 2]\n",
    "name: n\nitems:\n- id: one\n",
    "items:\n- id: 1\n  id: 2\n",
    "a: 1\na: 2\n",
    "1: a\n'1': b\n",
    "a: {k: 1, k: 2}\n",
    "- {k: 1, k: 2}\n",
    "? [1, 2]\n: a\n? [1, 2]\n: b\n",
    "? {x: 1}\n: a\n",
    "a: &n {x: 1}\nb: *n\n",
    "a: &n [1, x]\nb: *n\nc: *n\n",
    "- &n 1\n- *n\n- *n\n",
    "a: *missing\n",
    "a: &n [*n]\n",
    "&n a: 1\n*n : 2\n",
    "a: &n 1\nb: &n 2\nc: *n\n",
    "base: &base {a: 1}\nother:\n  <<: *base\n  b: 2\n",
    "!t [1, 2]\n",
    "!t {a: 1}\n",
    "a: !t [1, 2]\nb: !t {k: 1}\nc: !t 3\n",
    "!!seq [1, 2]\n",
    "!!map {a: 1}\n",
    "!!str [1, 2]\n",
    "!!set {a, b}\n",
    "!!binary [1]\n",
    "- !!int '12'\n- !!str 12\n- !!null ''\n- !!float 1\n- !!bool 'true'\n",
    "- !!int x\n",
    "!!int \"0\\x30\": 7\n",
    "Unit\n",
    "List\n",
    "New: 5\n",
    "New: [5]\n",
    "Tuple: [1, x]\n",
    "Tuple: [1]\n",
    "Struct: {a: 1, b: x}\n",
    "Struct:\n",
    "List: [1, 2]\n",
    "Map: {k: 1}\n",
    "!Unit\n",
    "!New 5\n",
    "!List [1, 2]\n",
    "!Struct {a: 1}\n",
    "{New: 5, Unit: }\n",
    "{}\n",
    "[]\n",
    "- Unit\n- New: 5\n- !Tuple [1, x]\n- Struct: {a: 1}\n",
    "k: Unit\na: {New: 5}\n",
    "type: A\na: 1\n",
    "type: B\nb: x\n",
    "type: C\n",
    "- - a: [~]\n  -\n",
    "- - x: [{y: }]\n",
    "- [[[[1]]]]\n",
    "a: {b: {c: {d: 1}}}\n",
    "[1, 2\n",
    "a: [1, 2\n",
    "a: 1\nb: [1, 2\n",
    "a: one\nb\n",
    "a: 1\n- b\n",
    "a: \"\\q\"\n",
    "- 1\n- \"unterminated\n",
    "a: 1\n---\nb: 2\n",
    "a: 1\n...\n---\nb: 2\n",
    "a: 1\n...\n",
    "%YAML 1.2\n---\na: 1\n",
    "a: |\n  text\nb: >\n  folded\n  text\n",
    "- \"a\\tb\"\n- 'it''s'\n- \"x\\u0041\"\n",
    "\"a\\tb\": 1\n",
    "a: 1 # comment\nb: two # comment\n",
    "? a\n: 1\n? b\n: 2\n",
    "? a\n? b\n",
    "[a, b]: 1\n",
    "a:\n  b:\n    c:\n      d:\n        e: 1\n",
    "- 1\n- 2\n- 3\n- 4\n- 5\n- 6\n",
    "a: 1\nb: 2\nc: 3\nd: 4\ne: 5\n",
];

#[test]
fn both_readers_give_the_same_for_each_document() {
    for document in DOCUMENTS {
        assert_same(document, Targets::All);
    }
}

/// A read takes other paths through the tree reader past 4 KiB and past 8 KiB, and reads one
/// way through the events at every size.
#[test]
fn both_readers_give_the_same_at_every_size() {
    for document in DOCUMENTS {
        for comments in [4_200, 8_300] {
            let mut padded = "# pad\n".repeat(comments / 6);
            padded.push_str(document);
            assert_same(&padded, Targets::Few);
        }
    }
}

const LINES: &[&str] = &[
    "a: 1",
    "b: two",
    "k:",
    "a: [1, x]",
    "b: {k: 1}",
    "k: |",
    "a: &n 3",
    "b: *n",
    "k: !t 4",
    "a: !!str 5",
    "- 1",
    "- two",
    "-",
    "- [1, [2]]",
    "- {a: 1, a: 2}",
    "- &n {k: 1}",
    "- *n",
    "- !t [1]",
    "- - 1",
    "- a: 1",
    "- New: 5",
    "? a",
    ": 1",
    "Unit",
    "!Unit",
    "New: 5",
    "Struct: {a: 1}",
    "type: A",
    "<<: *n",
    "[1, 2",
    "text",
    "---",
    "...",
    "# comment",
    "",
];

fn next(state: &mut u64) -> usize {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    usize::try_from(state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33).expect("31 bits")
}

fn generated(state: &mut u64) -> String {
    let lines = 1 + next(state) % 7;
    let mut text = String::new();
    for _ in 0..lines {
        let indent = [0, 0, 0, 2, 2, 4, 1][next(state) % 7];
        text.push_str(&" ".repeat(indent));
        text.push_str(LINES[next(state) % LINES.len()]);
        text.push('\n');
    }
    text
}

/// The documents are few lines from a pool that holds every form above, at any indentation, so
/// most of what they hold stands where no hand-picked document puts it.
#[test]
fn both_readers_give_the_same_for_generated_documents() {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut read = (0_usize, 0_usize);
    for _ in 0..1_500 {
        let document = generated(&mut state);
        assert_same(&document, Targets::All);
        let outcome = from_str_by_events::<Value>(&document, &Limits::default(), false);
        read.0 += usize::from(outcome.is_ok());
        read.1 += usize::from(outcome.is_err());
    }
    // The pool gives documents of both kinds, so both readers' values and errors are compared.
    assert!(read.0 > 150 && read.1 > 150, "{read:?}");
}

/// The readers agree on what a visitor that goes on after an error gets from the entries after
/// it, which a stream gives only if it passes over the rest of the entry that failed.
#[test]
fn a_visitor_that_goes_on_after_an_error_reads_the_entries_after_it() {
    let limits = Limits::default();
    let input = "- 1\n- [a, {b: [c]}]\n- 2\n- {k: [3, x]}\n- 4\n";
    let read = from_str_by_events::<Lenient>(input, &limits, false).expect("a lenient read");
    assert_eq!(
        read,
        Lenient {
            items: vec![1, 2, 4],
            errors: 2
        }
    );
    // A mapping's values are read as collections, so each scalar value is an error too.
    let input = "a: 1\nb: [x, 2, [y], 3]\nc: {d: [4, z], e: [[5], 6]}\nf: [7]\n";
    let read = from_str_by_events::<Lenient>(input, &limits, false).expect("a lenient read");
    assert_eq!(
        read,
        Lenient {
            items: vec![2, 3, 4, 6, 7],
            errors: 5
        }
    );
}

/// A fault in the document is the answer whatever the target does with the error it is given,
/// as it is where the document is built before the target reads any of it.
#[test]
fn a_fault_in_the_document_stands_when_a_visitor_goes_on_after_it() {
    let limits = Limits::default();
    for input in [
        "- 1\n- {k: 1, k: 2}\n- 3\n",
        "- 1\n- *missing\n- 3\n",
        "a: 1\nb: [1, 2\n",
        "- 1\n- 2\n---\n- 3\n",
    ] {
        let tree = from_str_by_tree::<Lenient>(input, &limits, false).expect_err(input);
        let events = from_str_by_events::<Lenient>(input, &limits, false).expect_err(input);
        assert_eq!(format!("{tree:?}"), format!("{events:?}"), "{input:?}");
        assert_ne!(tree.kind(), ayaml::ErrorKind::Serde, "{input:?}: {tree}");
    }
}

thread_local! {
    /// The errors that [`Asking`] was given by the sequence it read last on this thread.
    static REFUSALS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Asks a sequence for items until it is told the end, whatever errors it is given, and gives
/// up after a hundred of those.
struct Asking;

impl<'de> Deserialize<'de> for Asking {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Read;
        impl<'de> Visitor<'de> for Read {
            type Value = Asking;

            fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                formatter.write_str("a sequence")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Asking, A::Error> {
                let mut refusals = 0;
                while refusals < 100 {
                    match access.next_element::<i64>() {
                        Ok(Some(_)) => {}
                        Ok(None) => break,
                        Err(_) => refusals += 1,
                    }
                }
                REFUSALS.set(refusals);
                Ok(Asking)
            }
        }
        deserializer.deserialize_seq(Read)
    }
}

/// After a fault in the document nothing more can be read, so the collections that a visitor
/// is reading tell it that they have ended. A visitor that takes every error and asks again
/// is given the fault's error once, and then the end.
#[test]
fn a_visitor_that_asks_on_after_a_fault_is_told_the_end() {
    let limits = Limits::default();
    // The last fault is far enough into the document that the visitor is reading when the
    // parser comes to it: it reads a few events ahead of its reader.
    let unclosed = format!("{}- [3, 4\n", "- 1\n".repeat(60));
    for input in [
        "- 1\n- *missing\n- 3\n- 4\n",
        "- 1\n- {k: 1, k: 2}\n- 3\n",
        unclosed.as_str(),
    ] {
        REFUSALS.set(usize::MAX);
        let error = from_str_by_events::<Asking>(input, &limits, false)
            .err()
            .unwrap_or_else(|| panic!("{input:?} has a fault"));
        assert_ne!(error.kind(), ayaml::ErrorKind::Serde, "{input:?}: {error}");
        assert_eq!(REFUSALS.get(), 1, "{input:?}");
    }
    // An item that doesn't suit the target is no fault: the visitor reads the items after it.
    REFUSALS.set(usize::MAX);
    assert!(from_str_by_events::<Asking>("- 1\n- x\n- [2]\n- 3\n", &limits, false).is_ok());
    assert_eq!(REFUSALS.get(), 2);
}

/// What a target doesn't read is still checked: a duplicate key, an alias, and a second
/// document are found in a value that is ignored, in entries a visitor leaves, and after a
/// target that reads nothing.
#[test]
fn what_a_target_does_not_read_is_still_checked() {
    let limits = Limits::default();
    let faulty = [
        "ignored: {k: 1, k: 2}\n",
        "ignored: *missing\n",
        "ignored: [1, {k: 1, k: 2}]\n",
        "a: 1\n---\nb: 2\n",
    ];
    for input in faulty {
        let error = from_str_by_events::<Ignored>(input, &limits, false).expect_err(input);
        assert_ne!(error.kind(), ayaml::ErrorKind::Serde, "{input:?}: {error}");
        let error = from_str_by_events::<Nothing>(input, &limits, false).expect_err(input);
        assert_ne!(error.kind(), ayaml::ErrorKind::Serde, "{input:?}: {error}");
        let error = from_str_by_events::<Partial>(input, &limits, false).expect_err(input);
        assert_ne!(error.kind(), ayaml::ErrorKind::Serde, "{input:?}: {error}");
        let error = from_str_by_events::<BTreeMap<String, Nothing>>(input, &limits, false)
            .expect_err(input);
        assert_ne!(error.kind(), ayaml::ErrorKind::Serde, "{input:?}: {error}");
    }
    assert_eq!(
        from_str_by_events::<Vec<Nothing>>("- {k: 1}\n- [2]\n- 3\n", &limits, false).unwrap(),
        [Nothing(0), Nothing(0), Nothing(0)]
    );
}
