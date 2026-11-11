use super::*;

#[test]
fn span_offsets_are_byte_based_and_locations_are_one_based() {
    let text = "é: 1\n";
    let span = Span::new(Marker::new(0, 1, 1), Marker::new(2, 1, 2));
    assert_eq!(span.slice(text), Some("é"));
    assert_eq!(span.start().location(), Location::new(1, 1));
    assert_eq!(span.len(), 2);
}

#[test]
fn duplicate_key_diagnostics_print_the_key_and_location() {
    let error = ayaml::parse_value("a: 1\na: 2\n").expect_err("duplicate");
    let message = error.to_string();
    assert!(
        message.contains("duplicate mapping key: \"a\""),
        "{message}"
    );
    assert!(message.contains("line 2"), "{message}");
}

#[test]
fn error_categories_and_locations_are_public() {
    let span = Span::new(Marker::new(3, 2, 4), Marker::new(4, 2, 5));
    let error = Error::new(ErrorKind::Alias, "unknown anchor").with_span(span);
    assert_eq!(error.kind(), ErrorKind::Alias);
    assert_eq!(error.location(), Some(Location::new(2, 4)));
    assert_eq!(error.to_string(), "unknown anchor at line 2 column 4");
}

#[test]
fn resource_limit_errors_keep_their_structured_limit() {
    let limits = Limits::default();
    let exceeded = limits.check_input(32 * 1024 * 1024 + 1).unwrap_err();
    let error: Error = exceeded.into();
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(error.limit(), Some(exceeded));
}

#[test]
fn compatibility_limits_are_inclusive() {
    let limits = Limits::default();
    assert!(limits.check_input(32 * 1024 * 1024 - 1).is_ok());
    assert!(limits.check_input(32 * 1024 * 1024).is_ok());
    assert_eq!(
        limits.check_input(32 * 1024 * 1024 + 1).unwrap_err().kind(),
        LimitKind::InputBytes
    );
    assert!(limits.check_depth(127).is_ok());
    assert!(limits.check_depth(128).is_ok());
    assert_eq!(
        limits.check_depth(129).unwrap_err().kind(),
        LimitKind::Depth
    );
}

#[test]
fn values_and_mappings_preserve_order_and_semantics() {
    let mut map = Mapping::new();
    map.insert(Value::from("b"), Value::from(2_u64));
    map.insert(Value::from("a"), Value::from(1_u64));
    map.insert(Value::from(1_u64), Value::from("int"));
    map.insert(Value::Number(Number::from(1.0)), Value::from("float"));

    let keys: Vec<_> = map.keys().cloned().collect();
    assert_eq!(
        keys,
        vec![
            Value::from("b"),
            Value::from("a"),
            Value::from(1_u64),
            Value::Number(Number::from(1.0)),
        ]
    );
    assert_eq!(map["a"], Value::from(1_u64));
    assert_eq!(map.len(), 4);
}

#[test]
fn remove_swaps_the_last_entry() {
    let mut map = Mapping::new();
    for key in ["a", "b", "c", "d"] {
        map.insert(Value::from(key), Value::Null);
    }
    map.remove(Value::from("b"));
    let keys: Vec<_> = map.keys().cloned().collect();
    assert_eq!(
        keys,
        vec![Value::from("a"), Value::from("d"), Value::from("c"),]
    );
}

#[test]
fn events_carry_style_tag_anchor_and_span() {
    let span = Span::new(Marker::new(0, 1, 1), Marker::new(2, 1, 3));
    let event = Event::Scalar {
        value: "hi".into(),
        raw: "\"hi\"".into(),
        style: ScalarStyle::DoubleQuoted,
        tag: Some(Tag::local("Note")),
        anchor: Some(Anchor::new("a")),
        span,
    };
    assert_eq!(event.span(), span);
    assert!(matches!(
        event,
        Event::Scalar {
            style: ScalarStyle::DoubleQuoted,
            ..
        }
    ));

    let sequence = Event::SequenceStart {
        tag: None,
        anchor: None,
        style: CollectionStyle::Flow,
        span,
    };
    assert_eq!(sequence.span(), span);
}

#[test]
fn tagged_values_are_represented() {
    let value = Value::from(TaggedValue::new(Tag::local("Custom"), Value::from("x")));
    assert!(value.is_tagged());
    assert_eq!(value.as_tagged().unwrap().tag.to_string(), "!Custom");
}

#[test]
fn number_display_is_yaml_safe() {
    assert_eq!(Number::from(1.0_f64).to_string(), "1.0");
    assert_eq!(Number::from(f64::NAN).to_string(), ".nan");
    assert_eq!(Number::from(f64::NEG_INFINITY).to_string(), "-.inf");
}

#[test]
fn parse_value_uses_the_compat_profile() {
    let value = ayaml::parse_value(
        "word: yes\noctal: 0123\nbinary: 0b11\nhex: 0x1f\nexponent: 1e3\nnull: ~\n",
    )
    .expect("parse");
    assert_eq!(value["word"], Value::from("yes"));
    assert_eq!(value["octal"], Value::from("0123"));
    assert_eq!(value["binary"].as_u64(), Some(3));
    assert_eq!(value["hex"].as_u64(), Some(31));
    assert_eq!(value["exponent"].as_f64(), Some(1000.0));
    assert!(value["null"].is_null());
}

#[test]
fn parse_value_materializes_aliases_and_keeps_merge_literal() {
    let input = "base: &b {x: 1}\nuse: *b\nmerged:\n  <<: *b\n  y: 2\n";
    let value = ayaml::parse_value(input).expect("parse");
    assert_eq!(value["use"], value["base"]);
    assert_eq!(value["merged"]["<<"], value["base"]);
    assert_eq!(value["merged"]["y"].as_u64(), Some(2));
}

#[test]
fn parse_value_rejects_duplicate_keys_and_trailing_documents() {
    let duplicate = ayaml::parse_value("a: 1\na: 2\n").expect_err("duplicate");
    assert_eq!(duplicate.kind(), ErrorKind::DuplicateKey);
    let trailing = ayaml::parse_value("a: 1\n---\nb: 2\n").expect_err("trailing");
    // A single-document entry point rejects a second document itself.
    assert_eq!(trailing.kind(), ErrorKind::Parser);
    // The compatibility profile stops the stream at its document limit first.
    let limited = ayaml::parse_value_with_limits("a: 1\n---\nb: 2\n", &Limits::compat())
        .expect_err("document limit");
    assert_eq!(limited.kind(), ErrorKind::Limit);
}

#[test]
fn limit_errors_carry_a_source_span() {
    let cases: &[(&str, ayaml::Limits)] = &[
        ("a: 1\n", ayaml::Limits::default().with_max_input_bytes(1)),
        ("a:\n  b: 1\n", ayaml::Limits::default().with_max_depth(1)),
        (
            "abcdef\n",
            ayaml::Limits::default().with_max_scalar_bytes(2),
        ),
        ("a: 1\n", ayaml::Limits::default().with_max_events(1)),
        ("a: &x 1\n", ayaml::Limits::default().with_max_anchors(0)),
        (
            "!!str a\n",
            ayaml::Limits::default().with_max_tag_expansion_bytes(1),
        ),
        (
            "- a\n- b\n",
            ayaml::Limits::default().with_max_collection_entries(1),
        ),
        (
            "a: &x [1, 2]\nb: *x\n",
            ayaml::Limits::default()
                .with_max_alias_expansion_bytes(0)
                .with_alias_bytes_per_input_byte(0),
        ),
    ];
    for (input, limits) in cases {
        let error = ayaml::parse_value_with_limits(input, limits).expect_err(input);
        assert_eq!(error.kind(), ErrorKind::Limit, "{input:?}: {error}");
        assert!(error.span().is_some(), "{input:?}: {error}");
        assert!(error.limit().is_some(), "{input:?}: {error}");
    }

    let limits = ayaml::Limits::default().with_max_documents(1);
    let error = ayaml::parse_documents_with_limits("a\n---\nb\n", &limits).expect_err("docs");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert!(error.span().is_some(), "{error}");
}

#[test]
fn parse_documents_returns_each_document() {
    let limits = Limits::default().with_max_documents(2);
    let documents =
        ayaml::parse_documents_with_limits("a: 1\n---\nb: 2\n", &limits).expect("parse");
    assert_eq!(documents.len(), 2);
    assert_eq!(documents[0]["a"].as_u64(), Some(1));
    assert_eq!(documents[1]["b"].as_u64(), Some(2));
}

#[test]
fn parse_value_retains_custom_tags() {
    let value = ayaml::parse_value("tagged: !Custom value\n").expect("parse");
    assert!(value["tagged"].is_tagged());
    assert_eq!(
        value["tagged"].as_tagged().unwrap().tag.to_string(),
        "!Custom"
    );
}

#[test]
fn merge_adapter_is_opt_in_and_recursive() {
    let mut value = ayaml::parse_value(
        "base: &b {x: 1, y: 1}\nmerged:\n  <<: *b\n  x: 2\n  nested:\n    <<: *b\n    z: 3\n",
    )
    .expect("parse");
    assert_eq!(value["merged"]["<<"], value["base"]);
    value.apply_merge_keys().expect("merge");
    assert_eq!(value["merged"]["x"].as_u64(), Some(2));
    assert_eq!(value["merged"]["y"].as_u64(), Some(1));
    assert_eq!(value["merged"]["nested"]["x"].as_u64(), Some(1));
    assert_eq!(value["merged"]["nested"]["z"].as_u64(), Some(3));
    assert!(
        !value["merged"]
            .as_mapping()
            .unwrap()
            .contains_key(Value::from("<<"))
    );
}

#[test]
fn public_event_stream_preserves_duplicates_and_metadata() {
    let events: Vec<_> = ayaml::parse_events("a: 1\na: 2\n")
        .expect("events")
        .collect::<Result<Vec<_>, _>>()
        .expect("event items");
    let scalars: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::Scalar { value, .. } => Some(value.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(scalars, ["a", "1", "a", "2"]);
    assert!(matches!(events.first(), Some(Event::StreamStart { .. })));
    assert!(matches!(events.last(), Some(Event::StreamEnd { .. })));
}

#[test]
fn public_event_stream_can_stop_early_and_reports_limits() {
    let mut events = ayaml::parse_events("a: 1\nb: 2\n").expect("events");
    assert!(matches!(events.next(), Some(Ok(Event::StreamStart { .. }))));
    drop(events);

    let limits = Limits::default().with_max_input_bytes(2);
    let error = ayaml::parse_events_with_limits("a: 1\n", &limits).expect_err("limit");
    assert_eq!(error.kind(), ErrorKind::Limit);
}

#[test]
fn event_stream_spans_documents_lazily() {
    let limits = Limits::default().with_max_documents(2);
    let events: Vec<_> = ayaml::parse_events_with_limits("--- a\n--- b\n", &limits)
        .expect("events")
        .collect::<Result<Vec<_>, _>>()
        .expect("event items");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::DocumentStart { .. }))
            .count(),
        2
    );
}

/// Every event's line and column agree with its offset, an empty node's and an implicit
/// document end's too, where a document or the input ends.
#[test]
fn event_places_match_their_offsets() {
    let inputs = [
        "a:",
        "a:\n",
        "a:\n\n# c\n",
        "- a:\n",
        "a:\n--- b\n",
        "a:\n\n# c\n\n--- b\n",
        "---\n---\n",
        "- a:\n--- !t\nx:\n  - y: 1\n    z:\n",
        "a:\n...\n",
        "a:\n...\n---\nb:\n",
    ];
    let readers: &[(&str, ReadEvents)] = &[
        ("parse_events", |input| {
            ayaml::parse_events(input)
                .expect("stream")
                .collect::<Result<Vec<_>, _>>()
                .expect("events")
        }),
        #[cfg(feature = "libyaml-compat")]
        ("compat::parse_events_with_limits", |input| {
            ayaml::compat::parse_events_with_limits(input, &Limits::default()).expect("events")
        }),
    ];
    for &(name, read) in readers {
        for input in inputs {
            for event in read(input) {
                for marker in [event.span().start(), event.span().end()] {
                    assert_eq!(
                        (marker.line(), marker.column()),
                        place_of(input, marker.index()),
                        "{name} {input:?} {event:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn event_stream_defers_syntax_errors_to_items_and_enforces_global_document_limit() {
    let mut events = ayaml::parse_events("%YAML 1.2\n").expect("stream");
    assert!(matches!(events.next(), Some(Ok(Event::StreamStart { .. }))));
    assert!(matches!(events.next(), Some(Err(_))));
    assert!(events.next().is_none());

    let mut docs =
        ayaml::parse_events_with_limits("--- a\n--- b\n", &Limits::compat()).expect("stream");
    let error = docs
        .by_ref()
        .find_map(|event| event.err())
        .expect("document limit");
    assert_eq!(error.kind(), ErrorKind::Limit);
}

#[test]
fn every_default_reader_reports_the_same_first_error() {
    fn signature(error: ayaml::Error) -> (ErrorKind, Option<(usize, usize)>, String) {
        (
            error.kind(),
            error
                .location()
                .map(|location| (location.line(), location.column())),
            error.message().to_owned(),
        )
    }

    for input in [
        "a: 1\na: 2\n",
        "[1, 2]: a\n[1, 2]: b\n",
        "!t 1: a\n!t 1: b\n",
        "a: [1,\n2\n",
        "- key: first\n\n  second\n",
    ] {
        let wanted = signature(ayaml::parse_value(input).unwrap_err());
        assert_eq!(
            signature(ayaml::parse_documents(input).unwrap_err()),
            wanted,
            "parse_documents differs for {input:?}"
        );
        assert_eq!(
            signature(ayaml::from_str::<Value>(input).unwrap_err()),
            wanted,
            "from_str differs for {input:?}"
        );
        assert_eq!(
            signature(ayaml::from_str_borrowed::<Value>(input).unwrap_err()),
            wanted,
            "from_str_borrowed differs for {input:?}"
        );
        let streamed = ayaml::Deserializer::from_str(input)
            .and_then(|mut documents| documents.next::<Value>())
            .unwrap_err();
        assert_eq!(
            signature(streamed),
            wanted,
            "Deserializer::next differs for {input:?}"
        );
    }
}

#[test]
fn an_unterminated_flow_collection_reports_a_later_line() {
    for input in ["[\na\n", "a: [1,\n2\n"] {
        let error = ayaml::parse_value(input).unwrap_err();
        let location = error.location().expect("location");
        assert!(
            location.line() > 1,
            "{input:?} reported line {}: {error}",
            location.line()
        );
    }
}

#[test]
fn a_depth_error_carries_the_same_span_on_every_reader() {
    let deep = format!("{}a\n", "- ".repeat(129));
    let wanted = Some((1, 257));
    let error = ayaml::parse_value(&deep).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(
        error
            .location()
            .map(|location| (location.line(), location.column())),
        wanted
    );
    let error = ayaml::parse_events(&deep)
        .and_then(|events| events.collect::<Result<Vec<_>, _>>())
        .unwrap_err();
    assert!(
        error.location().is_some(),
        "parse_events lost the depth span: {error}"
    );
    let error = ayaml::Deserializer::from_str(&deep)
        .and_then(|mut documents| documents.next::<Value>())
        .unwrap_err();
    assert_eq!(
        error
            .location()
            .map(|location| (location.line(), location.column())),
        wanted,
        "{error}"
    );
}

#[test]
fn a_read_ahead_limit_does_not_hide_an_earlier_error() {
    let input = format!("}}-x\n{}\n", "[".repeat(300));
    let error = ayaml::parse_value(&input).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Parser);
    assert_eq!(
        error
            .location()
            .map(|location| (location.line(), location.column())),
        Some((1, 1)),
        "{error}"
    );
    assert!(error.message().contains('}'), "{error}");
    let typed = ayaml::from_str::<Value>(&input).unwrap_err();
    assert_eq!(typed.kind(), ErrorKind::Parser);
    assert_eq!(
        typed
            .location()
            .map(|location| (location.line(), location.column())),
        Some((1, 1)),
        "{typed}"
    );
    #[cfg(feature = "libyaml-compat")]
    {
        let compat = ayaml::compat::from_str::<Value>(&input).unwrap_err();
        assert_eq!(
            compat
                .location()
                .map(|location| (location.line(), location.column())),
            Some((1, 1)),
            "{compat}"
        );
    }
}

#[test]
fn misplaced_block_lines_say_what_is_wrong() {
    for (input, message, line, column) in [
        (
            "- key: first\n\n  second\n",
            "a mapping line is missing a `:`",
            3,
            3,
        ),
        (
            "a:\n  - b\n - c\n",
            "a block entry is indented less than its parent",
            3,
            2,
        ),
        (
            "-\n|2 ",
            "a block scalar is not indented past its parent",
            2,
            1,
        ),
        (
            "? \n|",
            "a block scalar is not indented past its parent",
            2,
            1,
        ),
    ] {
        let error = ayaml::parse_value(input).unwrap_err();
        assert_eq!(error.message(), message, "{input:?}: {error}");
        assert_eq!(
            error
                .location()
                .map(|location| (location.line(), location.column())),
            Some((line, column)),
            "{input:?}: {error}"
        );
    }
}

/// A deeply nested Rust value returns a depth error before serialization can exhaust a 2 MiB
/// stack. The limit is the parser's 128, and `to_string` and `to_writer` serialize through
/// `to_value` first.
#[test]
fn deep_serialization_returns_a_depth_error_on_a_small_stack() {
    fn nested(depth: usize) -> Value {
        let mut value = Value::from(1_u64);
        for _ in 0..depth {
            value = Value::Sequence(vec![value]);
        }
        value
    }

    fn on_small_stack(body: impl FnOnce() + Send + 'static) {
        std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(body)
            .expect("spawn")
            .join()
            .expect("no panic");
    }

    fn assert_depth(error: ayaml::Error) {
        assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
        assert_eq!(
            error.limit().map(|limit| limit.kind()),
            Some(LimitKind::Depth),
            "{error}"
        );
    }

    for depth in [128, 129] {
        let value = nested(depth);
        on_small_stack(move || {
            let to_value = ayaml::to_value(&value);
            let to_string = ayaml::to_string(&value);
            let mut output = Vec::new();
            let to_writer = ayaml::to_writer(&mut output, &value);
            if depth <= 128 {
                to_value.expect("to_value at the limit");
                to_string.expect("to_string at the limit");
                to_writer.expect("to_writer at the limit");
            } else {
                assert_depth(to_value.expect_err("to_value above the limit"));
                assert_depth(to_string.expect_err("to_string above the limit"));
                assert_depth(to_writer.expect_err("to_writer above the limit"));
            }
        });
    }

    // A tag writes as a one-entry map, so a chain of tags is not collection depth. It still
    // recurses in the serializer and must stop at the same stack bound.
    fn tagged(depth: usize) -> Value {
        let mut value = Value::from(1_u64);
        for _ in 0..depth {
            value = Value::from(TaggedValue::new(Tag::new("tag:x:", "t"), value));
        }
        value
    }
    for depth in [128, 129] {
        let value = tagged(depth);
        on_small_stack(move || {
            let to_value = ayaml::to_value(&value);
            let to_string = ayaml::to_string(&value);
            let mut output = Vec::new();
            let to_writer = ayaml::to_writer(&mut output, &value);
            if depth <= 128 {
                to_value.expect("to_value at the tag limit");
                // The emitter refuses tags on tags, but not because of the depth limit.
                assert_eq!(
                    to_string.expect_err("nested tags").kind(),
                    ErrorKind::Emitter
                );
                assert_eq!(
                    to_writer.expect_err("nested tags").kind(),
                    ErrorKind::Emitter
                );
            } else {
                assert_depth(to_value.expect_err("to_value above the tag limit"));
                assert_depth(to_string.expect_err("to_string above the tag limit"));
                assert_depth(to_writer.expect_err("to_writer above the tag limit"));
            }
        });
    }
}
