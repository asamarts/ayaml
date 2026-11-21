//! What the writers refuse to write, because no reader of this crate would load it.

use std::borrow::Cow;

use ayaml::{
    Anchor, CollectionStyle, ErrorKind, Event, LimitKind, Limits, Mapping, Number, ScalarStyle,
    Span, Tag, TaggedValue, Value,
};

fn scalar(text: &'static str, anchor: Option<&'static str>) -> Event<'static> {
    Event::Scalar {
        value: Cow::Borrowed(text),
        raw: Cow::Borrowed(text),
        style: ScalarStyle::Plain,
        tag: None,
        anchor: anchor.map(Anchor::new),
        span: Span::default(),
    }
}

fn alias(name: &'static str) -> Event<'static> {
    Event::Alias {
        anchor: Anchor::new(name),
        span: Span::default(),
    }
}

fn sequence(anchor: Option<&'static str>, items: Vec<Event<'static>>) -> Vec<Event<'static>> {
    let mut events = vec![Event::SequenceStart {
        tag: None,
        anchor: anchor.map(Anchor::new),
        style: CollectionStyle::Block,
        span: Span::default(),
    }];
    events.extend(items);
    events.push(Event::SequenceEnd {
        span: Span::default(),
    });
    events
}

/// A mapping of the given keys and values, in turn, in block or in flow style.
fn mapping(flow: bool, entries: Vec<Event<'static>>) -> Vec<Event<'static>> {
    let mut events = vec![Event::MappingStart {
        tag: None,
        anchor: None,
        style: if flow {
            CollectionStyle::Flow
        } else {
            CollectionStyle::Block
        },
        span: Span::default(),
    }];
    events.extend(entries);
    events.push(Event::MappingEnd {
        span: Span::default(),
    });
    events
}

/// A document of a hand-built stream: its directives, and the events of its root.
type Document = (Vec<(&'static str, &'static str)>, Vec<Event<'static>>);

/// A stream of documents, each with its directives and the events of its root.
fn stream(documents: Vec<Document>) -> Vec<Event<'static>> {
    let mut events = vec![Event::StreamStart {
        span: Span::default(),
    }];
    for (directives, root) in documents {
        for (name, value) in directives {
            events.push(Event::Directive {
                name: Cow::Borrowed(name),
                value: Cow::Borrowed(value),
                span: Span::default(),
            });
        }
        events.push(Event::DocumentStart {
            explicit: true,
            span: Span::default(),
        });
        events.extend(root);
        events.push(Event::DocumentEnd {
            explicit: true,
            span: Span::default(),
        });
    }
    events.push(Event::StreamEnd {
        span: Span::default(),
    });
    events
}

/// Each way of writing collected events gives the same answer, within the same limits: the
/// text, or the refusal's message.
fn written(events: &[Event<'_>], limits: &Limits) -> Result<String, String> {
    let options = ayaml::EmitOptions::new().with_limits(limits.clone());
    let collected = ayaml::to_string_events_with(events, &options);
    let mut bytes = Vec::new();
    let streamed = ayaml::to_writer_events_with(&mut bytes, events, &options)
        .map(|()| String::from_utf8(bytes).expect("text"));
    match (collected, streamed) {
        (Ok(text), Ok(streamed)) => {
            assert_eq!(text, streamed);
            Ok(text)
        }
        (Err(error), Err(streamed)) => {
            assert!(
                matches!(error.kind(), ErrorKind::Emitter | ErrorKind::Limit),
                "{error:?}"
            );
            assert_eq!(error.kind(), streamed.kind());
            assert_eq!(error.to_string(), streamed.to_string());
            Err(error.to_string())
        }
        (collected, streamed) => panic!("{collected:?} and {streamed:?}"),
    }
}

/// What the event writer writes, every reader of the crate loads: the events read back, and so
/// do the documents' values.
fn loads(text: &str) {
    let events: Result<Vec<_>, _> = ayaml::parse_events(text).and_then(Iterator::collect);
    events.unwrap_or_else(|error| panic!("{text:?}: {error}"));
    ayaml::parse_documents(text).unwrap_or_else(|error| panic!("{text:?}: {error}"));
}

/// An alias needs a node with its anchor earlier in the same document. The event readers
/// report an alias as it is written, so an event stream can hold one that refers to nothing;
/// the writer refuses it, where it wrote a document that no value reader loads.
#[test]
fn the_event_writer_refuses_an_alias_with_no_anchor_before_it() {
    let limits = Limits::default();
    for (name, events) in [
        (
            "no anchor at all",
            stream(vec![(vec![], sequence(None, vec![alias("x")]))]),
        ),
        (
            "the anchor comes later",
            stream(vec![(
                vec![],
                sequence(None, vec![alias("x"), scalar("v", Some("x"))]),
            )]),
        ),
        (
            "the anchor is another document's",
            stream(vec![
                (vec![], sequence(None, vec![scalar("v", Some("x"))])),
                (vec![], sequence(None, vec![alias("x")])),
            ]),
        ),
        (
            "another name",
            stream(vec![(
                vec![],
                sequence(None, vec![scalar("v", Some("x")), alias("xx")]),
            )]),
        ),
        (
            "the root is the alias",
            stream(vec![(vec![], vec![alias("x")])]),
        ),
        // A mapping's key is written by another path than its other nodes.
        (
            "a block mapping's key",
            stream(vec![(
                vec![],
                mapping(false, vec![alias("x"), scalar("v", None)]),
            )]),
        ),
        (
            "a flow mapping's key",
            stream(vec![(
                vec![],
                mapping(true, vec![alias("x"), scalar("v", None)]),
            )]),
        ),
        (
            "a block mapping's value",
            stream(vec![(
                vec![],
                mapping(false, vec![scalar("k", None), alias("x")]),
            )]),
        ),
        (
            "a flow mapping's value",
            stream(vec![(
                vec![],
                mapping(true, vec![scalar("k", None), alias("x")]),
            )]),
        ),
        (
            "a key whose anchor is its own value's",
            stream(vec![(
                vec![],
                mapping(false, vec![alias("x"), scalar("v", Some("x"))]),
            )]),
        ),
        (
            "a key inside a key",
            stream(vec![(
                vec![],
                mapping(
                    false,
                    [
                        mapping(true, vec![alias("x"), scalar("v", None)]),
                        vec![scalar("w", None)],
                    ]
                    .concat(),
                ),
            )]),
        ),
    ] {
        let refusal = written(&events, &limits).expect_err(name);
        assert!(
            refusal.contains("no node before it in its document has that anchor"),
            "{name}: {refusal}"
        );
    }
    for (name, events) in [
        (
            "an anchor before it",
            stream(vec![(
                vec![],
                sequence(None, vec![scalar("v", Some("x")), alias("x")]),
            )]),
        ),
        (
            "an anchor that is defined again",
            stream(vec![(
                vec![],
                sequence(
                    None,
                    vec![scalar("v", Some("x")), scalar("w", Some("x")), alias("x")],
                ),
            )]),
        ),
        (
            "a key after its anchor",
            stream(vec![(
                vec![],
                mapping(
                    false,
                    vec![
                        scalar("k", None),
                        scalar("v", Some("x")),
                        alias("x"),
                        scalar("w", None),
                    ],
                ),
            )]),
        ),
        (
            "a flow key after its anchor, and a value that is an alias too",
            stream(vec![(
                vec![],
                mapping(
                    true,
                    vec![
                        scalar("k", None),
                        scalar("v", Some("x")),
                        alias("x"),
                        alias("x"),
                    ],
                ),
            )]),
        ),
        (
            "the same name in each document",
            stream(vec![
                (
                    vec![],
                    sequence(None, vec![scalar("v", Some("x")), alias("x")]),
                ),
                (
                    vec![],
                    sequence(None, vec![scalar("v", Some("x")), alias("x")]),
                ),
            ]),
        ),
    ] {
        let text = written(&events, &limits).unwrap_or_else(|refusal| panic!("{name}: {refusal}"));
        loads(&text);
    }
    // An alias inside the collection that has its anchor comes after the anchor, so the writer
    // writes it. Whether a value can hold itself is the value reader's to say.
    let inside = stream(vec![(vec![], sequence(Some("x"), vec![alias("x")]))]);
    assert_eq!(
        written(&inside, &limits).as_deref(),
        Ok("---\n&x\n- *x\n...\n")
    );
}

/// The names of a document's anchors are kept while it is written, so that an alias can be
/// checked, and no more of them than `max_anchors`. A name that is defined again is one name.
#[test]
fn the_event_writer_keeps_within_the_anchor_limit() {
    let limits = Limits::default().with_max_anchors(2);
    let two = stream(vec![
        (
            vec![],
            sequence(
                None,
                vec![
                    scalar("v", Some("a")),
                    scalar("v", Some("b")),
                    scalar("v", Some("a")),
                ],
            ),
        ),
        (
            vec![],
            sequence(None, vec![scalar("v", Some("c")), scalar("v", Some("d"))]),
        ),
    ]);
    written(&two, &limits).expect("two names a document");
    let three = stream(vec![(
        vec![],
        sequence(
            None,
            vec![
                scalar("v", Some("a")),
                scalar("v", Some("b")),
                scalar("v", Some("c")),
            ],
        ),
    )]);
    let emit_two = ayaml::EmitOptions::new().with_limits(limits.clone());
    let error = ayaml::to_string_events_with(&three, &emit_two).expect_err("three names");
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
    assert_eq!(
        error.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::Anchors)
    );
    // Each way of writing collected events takes options, so a caller that reads a document
    // of more anchors than the default allows can write its events back to a writer too.
    let mut bytes = Vec::new();
    let error =
        ayaml::to_writer_events_with(&mut bytes, &three, &emit_two).expect_err("three names");
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
    let mut bytes = Vec::new();
    let emit_three = ayaml::EmitOptions::new().with_limits(limits.with_max_anchors(3));
    ayaml::to_writer_events_with(&mut bytes, &three, &emit_three)
        .expect("three names within a limit of three");
    assert_eq!(
        String::from_utf8(bytes).expect("text"),
        ayaml::to_string_events(&three).expect("within the default")
    );
    // The limits on what is read are none of a writer's: a directive is checked for its
    // syntax, whatever input a caller's limits would let a reader take.
    let small = Limits::default().with_max_input_bytes(8);
    let directed = stream(vec![(
        vec![("YAML", "1.2"), ("TAG", "!e! tag:example.com,2000:")],
        vec![scalar("a", None)],
    )]);
    written(&directed, &small).expect("directives longer than the input limit");
}

/// A directive is written only if the scanner reads it back, in its default mode or in its
/// compatibility mode: a `%YAML` version, a `%TAG` with a handle and a prefix it accepts, and
/// neither twice in one document. A directive of any other name is the application's, and is
/// written as it is.
#[test]
fn the_event_writer_refuses_a_directive_that_no_reader_takes() {
    let limits = Limits::default();
    let one = |directives: Vec<(&'static str, &'static str)>| {
        written(
            &stream(vec![(directives, vec![scalar("a", None)])]),
            &limits,
        )
    };
    for (directives, reason) in [
        (vec![("YAML", "abc")], "invalid %YAML directive"),
        (vec![("YAML", "2.0")], "invalid %YAML directive"),
        (vec![("YAML", "")], "invalid %YAML directive"),
        (vec![("YAML", "1.2 more")], "invalid %YAML directive"),
        (
            vec![("YAML", "1.2"), ("YAML", "1.2")],
            "a document has one %YAML directive",
        ),
        (
            vec![("YAML", "1.2"), ("FOO", "x"), ("YAML", "1.1")],
            "a document has one %YAML directive",
        ),
        (vec![("TAG", "!a tag:x,2000:")], "invalid %TAG handle !a"),
        (vec![("TAG", "e! tag:x,2000:")], "invalid %TAG handle e!"),
        (vec![("TAG", "!e!")], "%TAG requires a handle and a prefix"),
        (vec![("TAG", "")], "%TAG requires a handle and a prefix"),
        (
            vec![("TAG", "!e! tag:x,2000: more")],
            "%TAG requires a handle and a prefix",
        ),
        (vec![("TAG", "!e! !a{b")], "invalid %TAG prefix !a{b"),
        (
            vec![("TAG", "!e! [x y")],
            "%TAG requires a handle and a prefix",
        ),
        (
            vec![("TAG", "!e_f tag:x,2000:")],
            "invalid %TAG handle !e_f",
        ),
        (
            vec![("TAG", "!e! tag:x,2000:"), ("TAG", "!e! tag:y,2000:")],
            "a document gives a handle one prefix",
        ),
        (
            vec![
                ("TAG", "! !a-"),
                ("TAG", "!! tag:x,2000:"),
                ("TAG", "! !b-"),
            ],
            "a document gives a handle one prefix",
        ),
        (
            vec![("TAG", "!! tag:x,2000:"), ("TAG", "!!\ttag:y,2000:")],
            "a document gives a handle one prefix",
        ),
    ] {
        let refusal = one(directives.clone()).expect_err("a refusal");
        assert!(
            refusal.starts_with("cannot write the directive %") && refusal.ends_with(reason),
            "{directives:?}: {refusal}"
        );
    }
    for directives in [
        vec![("YAML", "1.2")],
        vec![("YAML", "1.1")],
        vec![("YAML", "1.2 # the version")],
        vec![("TAG", "!e! tag:x,2000:")],
        vec![
            ("TAG", "! !local-"),
            ("TAG", "!! tag:x,2000:"),
            ("TAG", "!e-1! tag:y,2000:"),
        ],
        vec![("TAG", "!e! tag:x,2000:%20y # with an escape")],
        vec![("FOO", "bar baz"), ("FOO", "bar baz"), ("YAML", "1.2")],
        vec![("yaml", "abc"), ("tag", "anything at all")],
    ] {
        let text =
            one(directives.clone()).unwrap_or_else(|refusal| panic!("{directives:?}: {refusal}"));
        loads(&text);
    }
    // What only the compatibility reader takes is written too, since events that it gave have
    // to be written back: a prefix that starts with a flow indicator, a `_` in a handle, and a
    // byte order mark inside a trailing comment.
    for directive in [
        ("TAG", "!e! [x"),
        ("TAG", "!e! ]tag:x,2000:"),
        ("TAG", "!e! ,x"),
        ("TAG", "!e_f! tag:x,2000:"),
        ("TAG", "!e! tag:x,2000: #\u{feff}"),
        ("YAML", "1.2 #\u{feff}"),
        ("FOO", "bar #\u{feff}"),
        ("FOO", "#\u{feff}"),
    ] {
        let text =
            one(vec![directive]).unwrap_or_else(|refusal| panic!("{directive:?}: {refusal}"));
        let default: Result<Vec<_>, _> = ayaml::parse_events(&text).and_then(Iterator::collect);
        assert!(default.is_err(), "{text:?}");
        #[cfg(feature = "libyaml-compat")]
        {
            let compat_options = ayaml::compat::ParseOptions::new().with_limits(limits.clone());
            let back = ayaml::compat::parse_events_eager_with(&text, &compat_options)
                .unwrap_or_else(|error| panic!("{text:?}: {error}"));
            assert_eq!(back.len(), 6, "{text:?}");
            assert!(
                matches!(&back[1], Event::Directive { name, value, .. }
                    if (name.as_ref(), value.as_ref()) == directive),
                "{text:?}: {:?}",
                back[1]
            );
        }
    }
    // A byte order mark in a directive name or parameter (outside a trailing comment) is refused
    // by both readers, so the writer refuses it too.
    for directive in [
        ("TAG\u{feff}", "!e! tag:x,2000:"),
        ("TAG", "!e!\u{feff} tag:x,2000:"),
        ("TAG", "!e! tag:x,2000:\u{feff}"),
        ("YAML", "1.2\u{feff}"),
        ("FOO", "bar\u{feff}"),
        ("FOO", "bar#\u{feff}"),
    ] {
        let refusal = one(vec![directive]).expect_err("a BOM outside a directive comment");
        assert!(
            refusal.starts_with("cannot write the directive %"),
            "{directive:?}: {refusal}"
        );
    }
    // What a document may hold once, the next document may hold again.
    let each = stream(vec![
        (
            vec![("YAML", "1.2"), ("TAG", "!e! tag:x,2000:")],
            vec![scalar("a", None)],
        ),
        (
            vec![("YAML", "1.2"), ("TAG", "!e! tag:y,2000:")],
            vec![scalar("b", None)],
        ),
    ]);
    loads(&written(&each, &limits).expect("each document has its own directives"));
}

/// The three that count depth count an empty collection as a level: the parser, the event
/// writer, and the value writer. So a value that is written within a limit is read back
/// within it.
#[test]
fn an_empty_collection_is_a_level_to_the_writers_as_to_the_parser() {
    for text in [
        "[]\n",
        "{}\n",
        "- []\n",
        "- {}\n",
        "- - []\n",
        "- - {}\n",
        "a:\n  b: []\n",
        "a:\n  b: {}\n",
        "- a: [[]]\n",
        "- [[], {}]\n",
        "? []\n: {}\n",
        "- - a\n",
        "a: {b: {c: {}}}\n",
        // An empty collection is left again: the ones after it are no deeper for it.
        "- {}\n- {}\n- {}\n",
        "- []\n- []\n- []\n",
        "[{}, {}, [], []]\n",
        "a: {}\nb: {}\nc: []\nd: []\n",
    ] {
        let value = ayaml::parse_value(text).expect(text);
        let events: Vec<_> = ayaml::parse_events(text)
            .and_then(Iterator::collect)
            .expect(text);
        let unlimited = ayaml::to_string(&value).expect(text);
        let mut least = None;
        for depth in 1..=6 {
            let limits = Limits::default().with_max_depth(depth);
            let parse_options = ayaml::ParseOptions::new().with_limits(limits.clone());
            let emit_options = ayaml::EmitOptions::new().with_limits(limits);
            let read = ayaml::parse_value_with(text, &parse_options).is_ok();
            let from_value = ayaml::to_string_with(&value, &emit_options).is_ok();
            let from_events = ayaml::to_string_events_with(&events, &emit_options).is_ok();
            let reread = ayaml::parse_value_with(&unlimited, &parse_options).is_ok();
            assert_eq!(
                (from_value, from_events, reread),
                (read, read, read),
                "{text:?} at depth {depth}"
            );
            if read && least.is_none() {
                least = Some(depth);
            }
        }
        assert!(least.is_some(), "{text:?} is read at some depth");
    }
}

fn tagged(suffix: &str, value: Value) -> Value {
    Value::Tagged(Box::new(TaggedValue::new(
        Tag::new("", format!("tag:yaml.org,2002:{suffix}")),
        value,
    )))
}

fn payloads() -> Vec<Value> {
    let mut mapping = Mapping::new();
    mapping.insert(Value::from("k"), Value::from(1_u64));
    let mut all = vec![
        Value::Null,
        Value::Bool(true),
        Value::Bool(false),
        Value::from(7_u64),
        Value::from(-3_i64),
        Value::Number(Number::from_f64(1.5)),
        Value::Number(Number::from_f64(1e20)),
        Value::Number(Number::from_f64(f64::INFINITY)),
        Value::Number(Number::from_f64(f64::NAN)),
        Value::Sequence(vec![Value::from(1_u64)]),
        Value::Sequence(Vec::new()),
        Value::Mapping(mapping),
        Value::Mapping(Mapping::new()),
    ];
    // Strings that a tag's rule takes, and ones that it doesn't.
    all.extend(
        [
            "", "text", "1", "-3", "1.5", "0x1F", "0o17", "01", " 1", "1e3", ".inf", ".NaN",
            "true", "True", "yes", "null", "~", "Null",
        ]
        .map(Value::from),
    );
    all
}

/// A reader resolves a scalar beneath `!!int`, `!!float`, `!!bool`, or `!!null` by that tag's
/// rule and refuses one that doesn't fit, so a value built by hand with such a tag on such a
/// scalar has no text that a reader loads, and the value writer refuses it. Whatever a reader
/// does load, the writer writes, as it did: the tag on a scalar that fits it, `!!str` on any
/// scalar, any core tag on a collection, and a name that the core schema doesn't define.
///
/// What a reader loads is asked of the reader: the text that the writer gives a payload
/// beneath a tag is the text it gives beneath a local tag, with the tag's name changed.
#[test]
fn the_value_writer_refuses_a_core_tag_on_a_scalar_that_no_reader_takes_beneath_it() {
    let (mut refused, mut wrote) = (0, 0);
    for suffix in [
        "str",
        "int",
        "float",
        "bool",
        "null",
        "seq",
        "omap",
        "pairs",
        "map",
        "set",
        "binary",
        "timestamp",
        "merge",
        "other",
    ] {
        for payload in payloads() {
            let local = Value::Tagged(Box::new(TaggedValue::new(
                Tag::new("!", "x"),
                payload.clone(),
            )));
            let text = ayaml::to_string(&local)
                .expect("a local tag is written on any payload")
                .replacen("!x", &format!("!!{suffix}"), 1);
            let loads = ayaml::parse_value(&text).is_ok();
            let value = tagged(suffix, payload.clone());
            match ayaml::to_string(&value) {
                Ok(written) => {
                    assert!(loads, "!!{suffix} on {payload:?}: wrote {written:?}");
                    assert_eq!(written, text, "!!{suffix} on {payload:?}");
                    wrote += 1;
                }
                Err(error) => {
                    assert!(
                        !loads,
                        "!!{suffix} on {payload:?} is refused, and {text:?} loads"
                    );
                    assert_eq!(error.kind(), ErrorKind::Emitter);
                    assert!(
                        error
                            .to_string()
                            .starts_with(&format!("the tag !!{suffix} cannot be written on ")),
                        "{error}"
                    );
                    refused += 1;
                }
            }
            // As an item and as a mapping's value: the same answer in each place.
            let mut entry = Mapping::new();
            entry.insert(Value::from("key"), value.clone());
            for placed in [
                Value::Sequence(vec![value, Value::Null]),
                Value::Mapping(entry),
            ] {
                assert_eq!(
                    ayaml::to_string(&placed).is_ok(),
                    loads,
                    "!!{suffix} on {payload:?}"
                );
            }
        }
    }
    // The four tags with a rule each refuse the scalars that their rule doesn't take, and
    // every other pair is written.
    assert_eq!(refused + wrote, 14 * 31);
    assert!(
        refused >= 60 && wrote >= 340,
        "{refused} refused, {wrote} wrote"
    );

    // A reader resolves the scalar beneath the tag: these read back as what the tag makes of
    // the text, which is another value than the one that was written.
    for (suffix, payload, read) in [
        ("str", Value::from(1_u64), Value::from("1")),
        ("int", Value::from("1"), Value::from(1_u64)),
        ("bool", Value::from("true"), Value::Bool(true)),
        ("null", Value::from(""), Value::Null),
        (
            "float",
            Value::from(7_u64),
            Value::Number(Number::from_f64(7.0)),
        ),
        ("int", Value::from(7_u64), Value::from(7_u64)),
        ("str", Value::from("text"), Value::from("text")),
    ] {
        let text = ayaml::to_string(&tagged(suffix, payload)).expect("a document");
        assert_eq!(
            ayaml::parse_value(&text).expect("a value"),
            read,
            "{text:?}"
        );
    }
    // A refusal says what no reader takes.
    let error = ayaml::to_string(&tagged("int", Value::from("x"))).expect_err("a refusal");
    assert_eq!(
        error.to_string(),
        "the tag !!int cannot be written on this string: no reader takes \"x\" as one"
    );
    // A local tag, and a tag of another vocabulary, say nothing about the scalar.
    for tag in [
        Tag::new("!", "int"),
        Tag::new("", "tag:example.com,2000:int"),
    ] {
        let value = Value::Tagged(Box::new(TaggedValue::new(tag, Value::from("text"))));
        let text = ayaml::to_string(&value).expect("a document");
        assert_eq!(
            ayaml::parse_value(&text).expect("a value"),
            value,
            "{text:?}"
        );
    }
}

/// A tag that can't be written is refused before the document's first line, so a writer that
/// is given somewhere to write has written nothing of that document.
#[test]
fn a_refused_tag_leaves_nothing_of_its_document_written() {
    let mut items: Vec<Value> = (0..20_000_u64).map(Value::from).collect();
    items.push(tagged("int", Value::from("x")));
    let value = Value::Sequence(items);
    let mut written = Vec::new();
    ayaml::to_writer_value(&mut written, &value).expect_err("a refusal");
    assert!(written.is_empty(), "{} bytes were written", written.len());
    let mut written = Vec::new();
    ayaml::to_writer(&mut written, &value).expect_err("a refusal");
    assert!(written.is_empty(), "{} bytes were written", written.len());
    // In a stream, what is written by then is of the documents before it, and nothing of the
    // one that is refused: the writer hands on what it has in pieces, so a long document
    // before it is partly out.
    let first = Value::Sequence((0..100_000_u64).map(Value::from).collect());
    let first_text = ayaml::to_string_value(&first).expect("a document");
    let mut written = Vec::new();
    ayaml::to_writer_documents(&mut written, &[first, value]).expect_err("a refusal");
    assert!(
        !written.is_empty() && first_text.as_bytes().starts_with(&written),
        "{} bytes were written, of a first document of {}",
        written.len(),
        first_text.len()
    );
}

/// The next number of a fixed sequence (xorshift64*), so the streams are the same in every run.
fn next(state: &mut u64) -> usize {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    usize::try_from(state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33).expect("31 bits")
}

/// A node of a generated stream: a scalar, an alias, or a sequence of a few nodes, with an
/// anchor now and then.
fn node(state: &mut u64, depth: usize, events: &mut Vec<Event<'static>>) {
    const NAMES: [&str; 3] = ["a", "b", "c"];
    let anchor = next(state)
        .is_multiple_of(4)
        .then(|| NAMES[next(state) % NAMES.len()]);
    match next(state) % if depth < 3 { 6 } else { 2 } {
        0 => events.push(scalar(["v", "w", "x y"][next(state) % 3], anchor)),
        1 => events.push(alias(NAMES[next(state) % NAMES.len()])),
        2 | 3 => {
            let mut items = Vec::new();
            for _ in 0..next(state) % 4 {
                node(state, depth + 1, &mut items);
            }
            events.extend(sequence(anchor, items));
        }
        _ => {
            // Keys and values in turn: any node may be a key.
            let mut entries = Vec::new();
            for _ in 0..2 * (next(state) % 3) {
                node(state, depth + 1, &mut entries);
            }
            let mut collection = mapping(next(state).is_multiple_of(2), entries);
            if let Some(Event::MappingStart { anchor: slot, .. }) = collection.first_mut() {
                *slot = anchor.map(Anchor::new);
            }
            events.extend(collection);
        }
    }
}

/// A document of a generated stream: its directives, each with whether the scanner reads it,
/// and the events of its root.
type Generated = (Vec<(&'static str, &'static str, bool)>, Vec<Event<'static>>);

/// What the event writer has to say of a generated stream, worked out from the stream alone:
/// the start of its refusal, for the first directive that the table marks as wrong or that
/// its document has had, or the first alias with no anchor before it in its document.
fn first_fault(documents: &[Generated]) -> Option<String> {
    for (directives, root) in documents {
        let (mut version, mut handles) = (false, Vec::new());
        for &(name, value, reads) in directives {
            let again = match name {
                "YAML" => std::mem::replace(&mut version, true),
                "TAG" => {
                    let handle = value.split(' ').next().unwrap_or("");
                    let seen = handles.contains(&handle);
                    handles.push(handle);
                    seen
                }
                _ => false,
            };
            if !reads || again {
                return Some(format!("cannot write the directive %{name} {value}: "));
            }
        }
        let mut anchors = Vec::new();
        for event in root {
            match event {
                Event::Alias { anchor, .. } if !anchors.contains(&anchor.as_str()) => {
                    return Some(format!(
                        "cannot write the alias {:?}: no node before it in its document has that anchor",
                        anchor.as_str()
                    ));
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
                } => anchors.push(anchor.as_str()),
                _ => {}
            }
        }
    }
    None
}

/// On generated streams of anchors, aliases, and directives, most of them wrong somewhere, the
/// event writer refuses each stream that has a fault, for its first fault, and writes every
/// other. What it writes is read back: as the same number of events, and as values with no
/// alias that names nothing.
#[test]
fn what_the_event_writer_writes_of_a_generated_stream_is_read_back() {
    // Each with whether the scanner reads it.
    const DIRECTIVES: [(&str, &str, bool); 8] = [
        ("YAML", "1.2", true),
        ("YAML", "1.3 # later", true),
        ("YAML", "one", false),
        ("TAG", "!e! tag:x,2000:", true),
        ("TAG", "!! tag:y,2000:", true),
        ("TAG", "!e tag:x,2000:", false),
        ("TAG", "!e!", false),
        ("NOTE", "anything goes", true),
    ];
    let limits = Limits::default();
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let (mut wrote, mut refused) = (0, 0);
    for _ in 0..20_000 {
        let mut documents = Vec::new();
        for _ in 0..=(next(&mut state) % 2) {
            let directives: Vec<_> = (0..next(&mut state) % 3)
                .map(|_| DIRECTIVES[next(&mut state) % DIRECTIVES.len()])
                .collect();
            let mut root = Vec::new();
            node(&mut state, 0, &mut root);
            documents.push((directives, root));
        }
        let fault = first_fault(&documents);
        let events = stream(
            documents
                .into_iter()
                .map(|(directives, root)| {
                    let directives = directives
                        .into_iter()
                        .map(|(name, value, _)| (name, value))
                        .collect();
                    (directives, root)
                })
                .collect(),
        );
        match (written(&events, &limits), fault) {
            (Ok(text), None) => {
                let back: Vec<_> = ayaml::parse_events(&text)
                    .and_then(Iterator::collect)
                    .unwrap_or_else(|error| panic!("{text:?}: {error}"));
                assert_eq!(back.len(), events.len(), "{text:?}");
                // What is left for a value reader to refuse in these streams is nothing about
                // an alias's name: a value that would hold itself, and a key that a mapping
                // has twice.
                if let Err(error) = ayaml::parse_documents(&text) {
                    let message = error.to_string();
                    assert!(
                        message.contains("cannot refer to") || message.contains("duplicate"),
                        "{text:?}: {error}"
                    );
                }
                wrote += 1;
            }
            (Err(refusal), Some(fault)) => {
                assert!(refusal.starts_with(&fault), "{refusal}, for {fault}");
                refused += 1;
            }
            (Ok(text), Some(fault)) => panic!("{text:?} was written, with the fault {fault}"),
            (Err(refusal), None) => panic!("a stream with no fault was refused: {refusal}"),
        }
    }
    assert!(
        wrote > 2_000 && refused > 2_000,
        "{wrote} written, {refused} refused"
    );
}
