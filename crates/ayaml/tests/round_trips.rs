//! What each writer writes reads back as the value or events it was given (AY-018): multi-document
//! streams with explicit markers, tags that a verbatim tag can't hold, long keys, and Serde input
//! that a mapping can't hold.

use std::fmt;

use ayaml::{Emitter, EmitterOptions, Event, Limits, Tag, TaggedValue, Value};
use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};

/// Every way to write one value as a document.
fn each_value_writer(value: &Value) -> Vec<(&'static str, String)> {
    let limits = Limits::default();
    let options = EmitterOptions::default();
    let flow = EmitterOptions::default().with_flow(true);
    let marked = EmitterOptions::default()
        .with_explicit_start(true)
        .with_explicit_end(true);
    let mut written = vec![
        (
            "to_string_value",
            ayaml::to_string_value(value).expect("to_string_value"),
        ),
        ("to_string", ayaml::to_string(value).expect("to_string")),
        (
            "flow",
            Emitter::new(&limits, &flow)
                .value_to_string(value)
                .expect("flow"),
        ),
        (
            "markers",
            Emitter::new(&limits, &marked)
                .value_to_string(value)
                .expect("markers"),
        ),
    ];
    let mut bytes = Vec::new();
    ayaml::to_writer_value(&mut bytes, value).expect("to_writer_value");
    written.push(("to_writer_value", String::from_utf8(bytes).unwrap()));
    let mut bytes = Vec::new();
    Emitter::new(&limits, &options)
        .to_writer(&mut bytes, value)
        .expect("to_writer");
    written.push(("to_writer", String::from_utf8(bytes).unwrap()));
    written
}

fn reads_back(value: &Value) {
    for (writer, text) in each_value_writer(value) {
        let read = ayaml::parse_value(&text)
            .unwrap_or_else(|error| panic!("{writer} wrote {text:?}, which fails: {error}"));
        assert_eq!(read, *value, "{writer} wrote {text:?}");
    }
}

/// What the event writer writes for `events`, to a string and to a writer, which agree.
fn event_writer_text(events: &[Event<'_>]) -> String {
    let text = ayaml::to_string_events(events).expect("to_string_events");
    let mut bytes = Vec::new();
    ayaml::to_writer_events(&mut bytes, events).expect("to_writer_events");
    assert_eq!(String::from_utf8(bytes).unwrap(), text, "to_writer_events");
    text
}

/// What the event writer writes for `source`'s events, collected and streamed, which agree.
fn source_events_text(source: &str) -> String {
    let events: Vec<Event<'_>> = ayaml::parse_events(source)
        .expect("events")
        .collect::<Result<_, _>>()
        .expect("events");
    let text = event_writer_text(&events);
    let mut bytes = Vec::new();
    ayaml::to_writer_event_stream(&mut bytes, ayaml::parse_events(source).expect("events"))
        .expect("to_writer_event_stream");
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        text,
        "to_writer_event_stream"
    );
    text
}

/// What `source` reads as, written by every writer, reads back the same.
fn source_reads_back(source: &str) {
    let value = ayaml::parse_value(source).expect("source");
    reads_back(&value);
    let text = source_events_text(source);
    let read = ayaml::parse_value(&text)
        .unwrap_or_else(|error| panic!("the event writer wrote {text:?}, which fails: {error}"));
    assert_eq!(read, value, "the event writer wrote {text:?}");
}

fn documents(options: &EmitterOptions, values: &[Value]) -> Vec<(&'static str, String)> {
    let limits = Limits::default();
    let emitter = Emitter::new(&limits, options);
    let mut bytes = Vec::new();
    emitter
        .documents_to_writer(&mut bytes, values)
        .expect("documents_to_writer");
    vec![
        (
            "documents_to_string",
            emitter
                .documents_to_string(values)
                .expect("documents_to_string"),
        ),
        ("documents_to_writer", String::from_utf8(bytes).unwrap()),
    ]
}

#[test]
fn every_document_reads_back_once_whatever_markers_the_writer_adds() {
    let values = [
        Value::from(1_u64),
        Value::Null,
        Value::from("text"),
        ayaml::parse_value("a: [1, 2]").unwrap(),
        Value::Sequence(vec![]),
    ];
    for options in [
        EmitterOptions::default(),
        EmitterOptions::default().with_explicit_start(true),
        EmitterOptions::default().with_explicit_end(true),
        EmitterOptions::default()
            .with_explicit_start(true)
            .with_explicit_end(true),
        EmitterOptions::default()
            .with_flow(true)
            .with_explicit_start(true),
    ] {
        for count in 1..=values.len() {
            for (writer, text) in documents(&options, &values[..count]) {
                let read = ayaml::parse_documents(&text).unwrap_or_else(|error| {
                    panic!("{writer} wrote {text:?}, which fails: {error}")
                });
                assert_eq!(
                    read,
                    values[..count],
                    "{writer} with {options:?} wrote {text:?}"
                );
            }
        }
    }
    let limits = Limits::default();
    let mut bytes = Vec::new();
    ayaml::to_writer_documents_with_limits(&mut bytes, &values, &limits).unwrap();
    for text in [
        String::from_utf8(bytes).unwrap(),
        ayaml::to_string_documents(&values).unwrap(),
    ] {
        assert_eq!(ayaml::parse_documents(&text).unwrap(), values, "{text:?}");
    }
}

#[test]
fn a_tag_with_a_percent_escape_keeps_its_name() {
    for source in [
        "%TAG !e! tag:x:\n--- !e!a%20b v",
        "%TAG !e! tag:x:\n--- !e!50%25 v",
        "%TAG !e! tag:x:\n--- !e!caf%c3%a9 v",
        "%TAG !e! tag:x:\n--- !e!a%21b v",
        "%TAG !e! tag:x:\n--- !e!%5B%5D%2C%7B%7D v",
        "%TAG !e! tag:x:\n--- {!e!a%20b k: !e!c%25 v}",
        "%TAG !e! tag:x:\n--- !e!a%20b [!e!a%20b x, !e!b%20c y]",
        "%TAG !e! tag:x:\n--- !e!a%20b\nk: !e!c%20d\n  - 1\n",
        "%TAG !e! tag:x:\n---\n? !e!a%20b k\n: !e!a%20b |\n  text\n",
    ] {
        source_reads_back(source);
    }
}

#[test]
fn a_tag_from_a_prefix_without_a_scheme_keeps_its_name() {
    for source in [
        "%TAG !e! foo\n--- !e!bar v",
        "%TAG !e! foo\n--- [!e!bar 1, !e!baz 2]",
        "%TAG !e! foo:\n--- !e!bar v",
        "%TAG !e! a%20b\n--- !e!c v",
    ] {
        source_reads_back(source);
    }
}

#[test]
fn a_built_tag_that_no_verbatim_tag_can_hold_reads_back() {
    for tag in [
        Tag::new("tag:x:", "a b"),
        Tag::new("tag:x:", "50%"),
        Tag::new("foo", "bar"),
        Tag::new("", "foobar"),
        Tag::new("tag:x:", "é"),
        Tag::new("tag:x:", "<>"),
        Tag::new("tag:x:a b:", "c"),
        Tag::new("x", "y"),
        Tag::new("xy", ""),
    ] {
        let value = Value::from(TaggedValue::new(tag.clone(), Value::from("v")));
        reads_back(&value);
        let mut mapping = ayaml::Mapping::new();
        mapping.insert(
            value.clone(),
            Value::from(TaggedValue::new(tag.clone(), Value::from(1_u64))),
        );
        let mapping = Value::Mapping(mapping);
        reads_back(&mapping);
        // The event writer, with the tag on every scalar of a block and a flow mapping.
        for (source, expected) in [("v", &value), ("v: 1", &mapping), ("{v: 1}", &mapping)] {
            let events: Vec<Event<'_>> = ayaml::parse_events(source)
                .unwrap()
                .map(|event| {
                    let mut event = event.unwrap();
                    if let Event::Scalar { tag: slot, .. } = &mut event {
                        *slot = Some(tag.clone());
                    }
                    event
                })
                .collect();
            let text = event_writer_text(&events);
            let read = ayaml::parse_value(&text).unwrap_or_else(|error| {
                panic!("the event writer wrote {text:?}, which fails: {error}")
            });
            assert_eq!(read, *expected, "the event writer wrote {text:?}");
        }
    }
}

#[test]
fn a_tag_that_yaml_cannot_hold_is_an_error() {
    for tag in [Tag::new("x", ""), Tag::new("", "x"), Tag::new("", "")] {
        let value = Value::from(TaggedValue::new(tag.clone(), Value::from("v")));
        let error = ayaml::to_string_value(&value).expect_err("no form");
        assert_eq!(error.kind(), ayaml::ErrorKind::Emitter, "{error}");
        assert!(ayaml::to_string(&value).is_err());
        let mut sequence = vec![Value::from(1_u64); 3];
        sequence.push(value);
        let mut bytes = Vec::new();
        // Nothing is written before the error.
        assert!(ayaml::to_writer_value(&mut bytes, &Value::Sequence(sequence)).is_err());
        assert!(bytes.is_empty(), "{bytes:?}");
        let events: Vec<Event<'_>> = ayaml::parse_events("v")
            .unwrap()
            .map(|event| {
                let mut event = event.unwrap();
                if let Event::Scalar { tag: slot, .. } = &mut event {
                    *slot = Some(tag.clone());
                }
                event
            })
            .collect();
        assert!(ayaml::to_string_events(&events).is_err());
        let mut bytes = Vec::new();
        assert!(ayaml::to_writer_events(&mut bytes, &events).is_err());
        assert!(bytes.is_empty(), "{bytes:?}");
    }
}

/// A writer that keeps what it is given, and how many writes gave it.
#[derive(Default)]
struct Writes {
    bytes: Vec<u8>,
    calls: usize,
}

impl std::io::Write for Writes {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(buf);
        self.calls += 1;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_streamed_document_declares_its_tags_after_earlier_documents_are_written() {
    // Each document is longer than the writer's 8 KiB chunk, and a later one's only tag that needs
    // a directive comes at its end, so the earlier documents are written before it is placed.
    let long = |name: &str| {
        (0..800)
            .map(|i| format!("{name}{i}: value {i}\n"))
            .collect::<String>()
    };
    let source = format!(
        "{}...\n%TAG !e! tag:x:\n---\n{}last: !e!a%20b v\n...\n%TAG !f! foo\n--- !f!bar 1\n",
        long("a"),
        long("b"),
    );
    let text = source_events_text(&source);
    assert_eq!(
        ayaml::parse_documents(&text).unwrap(),
        ayaml::parse_documents(&source).unwrap(),
        "{text}"
    );
    let mut writes = Writes::default();
    ayaml::to_writer_event_stream(&mut writes, ayaml::parse_events(&source).unwrap()).unwrap();
    assert_eq!(String::from_utf8(writes.bytes).unwrap(), text);
    assert!(writes.calls >= 3, "{} writes", writes.calls);
}

#[test]
fn several_documents_each_declare_the_tags_they_use() {
    let first = Value::from(TaggedValue::new(
        Tag::new("tag:x:", "a b"),
        Value::from(1_u64),
    ));
    let second = Value::from(TaggedValue::new(
        Tag::new("tag:y:", "c d"),
        Value::from(2_u64),
    ));
    let third = Value::from(TaggedValue::new(
        Tag::new("tag:x:", "e"),
        Value::from(3_u64),
    ));
    let values = [first, second, third];
    for options in [
        EmitterOptions::default(),
        EmitterOptions::default().with_explicit_end(true),
        EmitterOptions::default().with_explicit_start(true),
    ] {
        for (writer, text) in documents(&options, &values) {
            let read = ayaml::parse_documents(&text)
                .unwrap_or_else(|error| panic!("{writer} wrote {text:?}, which fails: {error}"));
            assert_eq!(read, values, "{writer} wrote {text:?}");
        }
    }
}

#[test]
fn a_tagged_key_too_long_to_be_implicit_reads_back() {
    let long = "k".repeat(1100);
    let tagged = |tag: Tag, value: Value| Value::from(TaggedValue::new(tag, value));
    for key in [
        tagged(Tag::local("t"), Value::from(long.as_str())),
        tagged(Tag::local(&*"t".repeat(1100)), Value::from("k")),
        tagged(Tag::new("tag:x:", "a b".repeat(400)), Value::from("k")),
        tagged(Tag::local("t"), Value::from(1_u64)),
    ] {
        let long = ayaml::to_string_value(&key)
            .unwrap()
            .trim_end()
            .chars()
            .count()
            > 128;
        let mut mapping = ayaml::Mapping::new();
        mapping.insert(key, Value::from(1_u64));
        let value = Value::Mapping(mapping);
        reads_back(&value);
        // ayaml reads an implicit key of any length in a flow mapping, as YAML allows, but readers
        // built on libyaml refuse one past 1024 characters there too, so the writers use `? ` past
        // 128, as libyaml's own emitter does.
        let limits = Limits::default();
        let flow = EmitterOptions::default().with_flow(true);
        let text = Emitter::new(&limits, &flow)
            .value_to_string(&value)
            .unwrap();
        assert_eq!(text.contains("{? "), long, "{text:?}");
        reads_back(&Value::Sequence(vec![
            value.clone(),
            Value::Sequence(vec![value]),
        ]));
    }
    // In compat mode too.
    let mut mapping = ayaml::Mapping::new();
    mapping.insert(
        Value::from(TaggedValue::new(
            Tag::local("t"),
            Value::from("k".repeat(130)),
        )),
        Value::from(1_u64),
    );
    let value = Value::Mapping(mapping);
    for (writer, text) in each_value_writer(&value) {
        let read = ayaml::parse_value_with_limits(&text, &Limits::compat())
            .unwrap_or_else(|error| panic!("{writer} wrote {text:?}, which compat fails: {error}"));
        assert_eq!(read, value, "{writer} wrote {text:?}");
    }
}

#[derive(Serialize)]
struct Inner {
    a: u8,
    b: u8,
}

#[derive(Serialize)]
struct Outer {
    a: u8,
    #[serde(flatten)]
    inner: Inner,
}

#[derive(Serialize)]
struct Twice {
    #[serde(flatten)]
    first: Inner,
    #[serde(flatten)]
    second: Inner,
}

#[test]
fn a_key_that_a_flattened_struct_repeats_is_an_error() {
    let outer = Outer {
        a: 1,
        inner: Inner { a: 2, b: 3 },
    };
    let twice = Twice {
        first: Inner { a: 1, b: 2 },
        second: Inner { a: 3, b: 4 },
    };
    for result in [
        ayaml::to_value(&outer).map(drop),
        ayaml::to_string(&outer).map(drop),
        ayaml::to_value(&twice).map(drop),
        ayaml::to_string(&twice).map(drop),
    ] {
        let error = result.expect_err("a repeated key");
        assert!(error.to_string().contains("duplicate"), "{error}");
    }
}

/// A map that writes the same key twice, as a hand-written `Serialize` can.
struct Repeats;

impl Serialize for Repeats {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("k", &1)?;
        map.serialize_entry("k", &2)?;
        map.end()
    }
}

#[test]
fn a_key_that_a_map_repeats_is_an_error() {
    assert!(ayaml::to_value(&Repeats).is_err());
    assert!(ayaml::to_string(&Repeats).is_err());
}

/// A `Display` that writes part of its text and then fails.
struct Fails;

impl fmt::Display for Fails {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("par")?;
        Err(fmt::Error)
    }
}

struct FailingKey;

impl Serialize for FailingKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_key(&Collected(Fails))?;
        map.serialize_value(&1)?;
        map.end()
    }
}

struct Collected<T>(T);

impl<T: fmt::Display> Serialize for Collected<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

#[test]
fn a_display_that_fails_is_an_error_not_a_prefix() {
    assert!(ayaml::to_value(&FailingKey).is_err());
    assert!(ayaml::to_string(&FailingKey).is_err());
    assert!(ayaml::to_value(Collected(Fails)).is_err());
    assert!(ayaml::to_string(&vec![Collected(Fails)]).is_err());
}

#[test]
fn each_tag_takes_the_first_form_that_reads_back() {
    for (tag, written) in [
        (Tag::local("x"), "!x v\n"),
        (Tag::local("a b"), "!a%20b v\n"),
        (Tag::new("tag:yaml.org,2002:", "x"), "!!x v\n"),
        (Tag::new("tag:x:", "y"), "!<tag:x:y> v\n"),
        (Tag::new("tag:x:", "50%25"), "!<tag:x:50%25> v\n"),
        (
            Tag::new("tag:x:", "50%"),
            "%TAG !t0! tag:x:\n---\n!t0!50%25 v\n",
        ),
        (
            Tag::new("tag:x:", "a b"),
            "%TAG !t0! tag:x:\n---\n!t0!a%20b v\n",
        ),
        (Tag::new("foo", "bar"), "%TAG !t0! foo\n---\n!t0!bar v\n"),
        // A name kept whole is split after its last `:`, `/` or `#`, or else before its last
        // character. A first character that isn't a letter starts no URI scheme.
        (Tag::new("", "foobar"), "%TAG !t0! fooba\n---\n!t0!r v\n"),
        (
            Tag::new("", "tag:x:a b"),
            "%TAG !t0! tag:x:\n---\n!t0!a%20b v\n",
        ),
        (Tag::new("", "1abc:x"), "%TAG !t0! 1abc:\n---\n!t0!x v\n"),
        (Tag::new("", "a/b c"), "%TAG !t0! a/\n---\n!t0!b%20c v\n"),
        (Tag::new("", "x#y z"), "%TAG !t0! x#\n---\n!t0!y%20z v\n"),
    ] {
        let value = Value::from(TaggedValue::new(tag.clone(), Value::from("v")));
        assert_eq!(ayaml::to_string_value(&value).unwrap(), written, "{tag:?}");
        // The reader resolves a core tag on a scalar, so `!!x v` reads as the string.
        if !written.starts_with("!!") {
            reads_back(&value);
        }
    }
}

#[test]
fn every_tag_of_a_mapping_is_declared_before_it() {
    let tagged = |prefix: &str, text: &str| {
        Value::from(TaggedValue::new(Tag::new(prefix, "x y"), Value::from(text)))
    };
    let mut mapping = ayaml::Mapping::new();
    mapping.insert(tagged("tag:a:", "k"), tagged("tag:b:", "v"));
    mapping.insert(tagged("tag:c:", "k"), tagged("tag:d:", "v"));
    let value = Value::Mapping(mapping);
    assert_eq!(
        ayaml::to_string_value(&value).unwrap(),
        "%TAG !t0! tag:a:\n%TAG !t1! tag:b:\n%TAG !t2! tag:c:\n%TAG !t3! tag:d:\n---\n\
         ? !t0!x%20y k\n: !t1!x%20y v\n? !t2!x%20y k\n: !t3!x%20y v\n"
    );
    reads_back(&value);
}

/// The events of `source`, with its scalars under `tags` in turn.
fn events_with_tags<'a>(source: &'a str, tags: &[Tag]) -> Vec<Event<'a>> {
    let mut next = 0;
    ayaml::parse_events(source)
        .unwrap()
        .map(|event| {
            let mut event = event.unwrap();
            if let Event::Scalar { tag: slot, .. } = &mut event {
                *slot = Some(tags[next % tags.len()].clone());
                next += 1;
            }
            event
        })
        .collect()
}

#[test]
fn the_event_writer_keeps_each_document_apart() {
    // A later document gets `---`, an explicit first one keeps it, and a directive after a
    // document that `...` didn't end gets one first.
    for (source, written) in [
        ("a\n...\nb\n", "a\n...\n---\nb\n"),
        ("--- a\n--- b\n", "---\na\n---\nb\n"),
        ("a\n...\n--- b\n...\n", "a\n...\n---\nb\n...\n"),
        (
            "%TAG !! tag:other:\n--- [!!x a, !<tag:yaml.org,2002:y> b]\n",
            "%TAG !! tag:other:\n---\n[!<tag:other:x> a, !<tag:yaml.org,2002:y> b]\n",
        ),
        // A handle that the input declares isn't added again.
        (
            "%TAG !t0! tag:z:\n%TAG !e! tag:x:\n--- [!t0!q a, !e!a%20b b]\n",
            "%TAG !t0! tag:z:\n%TAG !e! tag:x:\n%TAG !t1! tag:x:\n---\n[!<tag:z:q> a, !t1!a%20b b]\n",
        ),
        // Each added handle comes after the last, past every handle that the input declares, as
        // when the event writer writes the value writer's output again.
        (
            "%TAG !t0! tag:a:\n%TAG !t1! tag:b:\n--- [!t0!x%20y a, !t1!x%20y b]\n",
            "%TAG !t0! tag:a:\n%TAG !t1! tag:b:\n%TAG !t2! tag:a:\n%TAG !t3! tag:b:\n---\n\
             [!t2!x%20y a, !t3!x%20y b]\n",
        ),
    ] {
        let text = source_events_text(source);
        assert_eq!(text, written, "{source:?}");
        assert_eq!(
            ayaml::parse_documents(&text).unwrap(),
            ayaml::parse_documents(source).unwrap(),
            "{source:?}"
        );
    }
    let tags = [Tag::new("tag:x:", "a b"), Tag::new("tag:y:", "c d")];
    for source in [
        "a\n--- b\n",
        "a\n...\n--- b\n",
        "--- a\n--- b\n",
        "a\n...\nb\n",
    ] {
        let events = events_with_tags(source, &tags);
        let text = event_writer_text(&events);
        assert_eq!(
            text, "%TAG !t0! tag:x:\n---\n!t0!a%20b a\n...\n%TAG !t0! tag:y:\n---\n!t0!c%20d b\n",
            "{source:?}"
        );
    }
    // A first document that starts with no `---` and holds only an empty scalar gets one, or it
    // would vanish.
    let events: Vec<Event<'_>> = ayaml::parse_events("---\n")
        .unwrap()
        .map(|event| {
            let mut event = event.unwrap();
            if let Event::DocumentStart { explicit, .. } = &mut event {
                *explicit = false;
            }
            event
        })
        .collect();
    let text = event_writer_text(&events);
    assert_eq!(
        ayaml::parse_documents(&text).unwrap(),
        [Value::Null],
        "{text:?}"
    );
}

#[test]
fn a_flow_key_takes_the_explicit_form_past_128_characters() {
    let tagged = |tag: Tag, value: Value| Value::from(TaggedValue::new(tag, value));
    let t = |length: usize| Tag::local("t".repeat(length));
    // Each pair writes an implicit key of 128 characters, then one of 129.
    let cases = [
        (Value::from("k".repeat(128)), Value::from("k".repeat(129))),
        // `!t` and a plain string.
        (
            tagged(Tag::local("t"), Value::from("k".repeat(125))),
            tagged(Tag::local("t"), Value::from("k".repeat(126))),
        ),
        // `!t` and a string that a flow collection quotes.
        (
            tagged(
                Tag::local("t"),
                Value::from(format!("{}: x", "k".repeat(120))),
            ),
            tagged(
                Tag::local("t"),
                Value::from(format!("{}: x", "k".repeat(121))),
            ),
        ),
        (
            tagged(t(122), Value::from(true)),
            tagged(t(122), Value::from(false)),
        ),
        (
            tagged(t(124), Value::from(12_u64)),
            tagged(t(124), Value::from(123_u64)),
        ),
        (tagged(t(126), Value::Null), tagged(t(127), Value::Null)),
    ];
    let limits = Limits::default();
    let flow = EmitterOptions::default().with_flow(true);
    for (short, long) in cases {
        for (key, explicit) in [(short, false), (long, true)] {
            let mut mapping = ayaml::Mapping::new();
            mapping.insert(key.clone(), Value::from(1_u64));
            let value = Value::Mapping(mapping);
            let text = Emitter::new(&limits, &flow)
                .value_to_string(&value)
                .unwrap();
            assert_eq!(text.starts_with("{? "), explicit, "{text:?}");
            if !explicit {
                let written = &text[1..text.rfind(": 1}").unwrap()];
                assert_eq!(written.chars().count(), 128, "{text:?}");
            }
            reads_back(&value);
        }
    }
    // The scalar style option quotes keys too, and its quotes count, a doubled `'` as two. So
    // does a string key in a block mapping.
    let quoted_cases = [
        (
            true,
            tagged(Tag::local("t"), Value::from("k".repeat(123))),
            tagged(Tag::local("t"), Value::from("k".repeat(124))),
        ),
        (
            true,
            Value::from(format!("'{}", "k".repeat(124))),
            Value::from(format!("'{}", "k".repeat(125))),
        ),
        (
            false,
            Value::from("k".repeat(126)),
            Value::from("k".repeat(127)),
        ),
    ];
    for (flow, short, long) in quoted_cases {
        let options = EmitterOptions::default()
            .with_flow(flow)
            .with_scalar_style(Some(ayaml::ScalarStyle::SingleQuoted));
        let (open, close) = if flow { ("{", ": 1}") } else { ("", ": 1") };
        for (key, explicit) in [(short, false), (long, true)] {
            let mut mapping = ayaml::Mapping::new();
            mapping.insert(key.clone(), Value::from(1_u64));
            let value = Value::Mapping(mapping);
            let text = Emitter::new(&limits, &options)
                .value_to_string(&value)
                .unwrap();
            assert_eq!(text.starts_with(&format!("{open}? ")), explicit, "{text:?}");
            if !explicit {
                let written = &text[open.len()..text.rfind(close).unwrap()];
                assert_eq!(written.chars().count(), 128, "{text:?}");
            }
            assert_eq!(ayaml::parse_value(&text).unwrap(), value, "{text:?}");
        }
    }
}

#[test]
fn a_writer_gets_its_output_in_chunks_not_a_write_for_each_token() {
    let value = ayaml::parse_value("a: [1, 2, {b: c}]\nd: !x e\n").unwrap();
    let mut writes = Writes::default();
    ayaml::to_writer_value(&mut writes, &value).unwrap();
    assert_eq!(writes.calls, 1);
    let source = "a: 1\n--- [b]\n...\n%TAG !e! tag:x:\n--- !e!a%20b c\n";
    let events: Vec<Event<'_>> = ayaml::parse_events(source)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut writes = Writes::default();
    ayaml::to_writer_events(&mut writes, &events).unwrap();
    assert_eq!(writes.calls, 1);
    let mut writes = Writes::default();
    ayaml::to_writer_event_stream(&mut writes, ayaml::parse_events(source).unwrap()).unwrap();
    assert_eq!(writes.calls, 1);
    assert_eq!(
        String::from_utf8(writes.bytes).unwrap(),
        ayaml::to_string_events(&events).unwrap()
    );
}

/// `value` inside `depth` one-item sequences, or one-entry mappings.
fn nested(mut value: Value, depth: usize, mappings: bool) -> Value {
    for _ in 0..depth {
        value = if mappings {
            let mut mapping = ayaml::Mapping::new();
            mapping.insert(Value::from("k"), value);
            Value::Mapping(mapping)
        } else {
            Value::Sequence(vec![value])
        };
    }
    value
}

/// Take a value of one-item collections and tags on tags apart a level at a time, which dropping
/// it whole would do on the stack.
fn take_apart(mut value: Value) {
    loop {
        value = match value {
            Value::Sequence(mut items) => match items.pop() {
                Some(item) => item,
                None => return,
            },
            Value::Mapping(mapping) => match mapping.into_iter().next() {
                Some((_, item)) => item,
                None => return,
            },
            Value::Tagged(tagged) => tagged.value,
            _ => return,
        };
    }
}

/// The writer looks for the tags that need a `%TAG` directive before it writes, and that look stops
/// where the writer does: at a collection past the depth limit, and at a tag on a tagged value. So a
/// value far deeper than the limit, or a long chain of tags on tags, is an error on a small stack,
/// and a tag at the limit still gets its directive.
#[test]
fn the_look_for_directives_stops_where_the_writer_does() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let limits = Limits::default().with_max_depth(8);
            let tagged = || Value::from(TaggedValue::new(Tag::new("tag:x:", "a b"), "v".into()));
            for (mappings, written) in [
                (false, "- - - - - - - - !t0!a%20b v\n"),
                (
                    true,
                    "k:\n  k:\n    k:\n      k:\n        k:\n          k:\n            k:\n              \
                     k: !t0!a%20b v\n",
                ),
            ] {
                let at_the_limit = nested(tagged(), 8, mappings);
                assert_eq!(
                    ayaml::to_string_value_with_limits(&at_the_limit, &limits).unwrap(),
                    format!("%TAG !t0! tag:x:\n---\n{written}")
                );
                reads_back(&at_the_limit);

                let deep = nested(tagged(), 100_000, mappings);
                let error = ayaml::to_string_value_with_limits(&deep, &limits).unwrap_err();
                assert_eq!(error.kind(), ayaml::ErrorKind::Limit, "{error}");
                let mut bytes = Vec::new();
                let error =
                    ayaml::to_writer_value_with_limits(&mut bytes, &deep, &limits).unwrap_err();
                assert_eq!(error.kind(), ayaml::ErrorKind::Limit, "{error}");
                take_apart(deep);
            }

            let mut chain = tagged();
            for _ in 0..100_000 {
                chain = Value::from(TaggedValue::new(Tag::new("tag:x:", "a b"), chain));
            }
            let error = ayaml::to_string_value(&chain).unwrap_err();
            assert_eq!(error.kind(), ayaml::ErrorKind::Emitter, "{error}");
            take_apart(chain);
        })
        .unwrap()
        .join()
        .unwrap();
}
