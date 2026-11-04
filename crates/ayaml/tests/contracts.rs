use ayaml::{
    Anchor, CollectionStyle, Error, ErrorKind, Event, LimitKind, Limits, Location, Mapping, Marker,
    Number, ScalarStyle, Span, Tag, TaggedValue, Value,
};

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
    assert!(limits.check_input(32 * 1024 * 1024).is_ok());
    assert_eq!(
        limits.check_input(32 * 1024 * 1024 + 1).unwrap_err().kind(),
        LimitKind::InputBytes
    );
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

#[test]
fn typed_serde_from_str_and_from_value_work() {
    let input = "name: sample\ncount: 3\nenabled: true\nitems:\n  - a\n  - b\n";
    let config: TestConfig = ayaml::from_str(input).expect("from_str");
    assert_eq!(
        config,
        TestConfig {
            name: "sample".to_owned(),
            count: 3,
            enabled: true,
            items: vec!["a".to_owned(), "b".to_owned()],
        }
    );

    let value = ayaml::parse_value(input).expect("parse");
    let config: TestConfig = ayaml::from_value(value).expect("from_value");
    assert_eq!(config.count, 3);
}

#[test]
fn typed_serde_enums_and_slices_work() {
    let unit: TestEnum = ayaml::from_value(ayaml::parse_value("Unit: null\n").unwrap()).unwrap();
    assert_eq!(unit, TestEnum::Unit);
    let tuple: TestEnum =
        ayaml::from_value(ayaml::parse_value("Tuple: [1, 2]\n").unwrap()).unwrap();
    assert_eq!(tuple, TestEnum::Tuple(1, 2));

    let config: TestConfig =
        ayaml::from_slice(b"name: bytes\ncount: 1\nenabled: false\nitems: []\n")
            .expect("from_slice");
    assert_eq!(config.name, "bytes");

    let error = ayaml::from_slice::<TestConfig>(&[0xff, 0xfe]).expect_err("invalid UTF-8");
    assert_eq!(error.kind(), ErrorKind::InvalidEncoding);
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

#[test]
fn serde_wide_integer_visitors_work() {
    let wide: Wide = ayaml::from_str("signed: -3\nunsigned: 5\n").expect("wide integers");
    assert_eq!(
        wide,
        Wide {
            signed: -3,
            unsigned: 5
        }
    );
}

#[test]
fn serde_tags_untagged_enums_and_defaults_work() {
    let unit: TaggedEnum = ayaml::from_str("!Unit\n").expect("tagged unit");
    assert_eq!(unit, TaggedEnum::Unit);
    let tuple: TaggedEnum = ayaml::from_str("!Tuple [1, 2]\n").expect("tagged tuple");
    assert_eq!(tuple, TaggedEnum::Tuple(1, 2));

    let number: Untagged = ayaml::from_str("3\n").expect("untagged number");
    assert_eq!(number, Untagged::Number(3));
    let text: Untagged = ayaml::from_str("hello\n").expect("untagged string");
    assert_eq!(text, Untagged::Text("hello".to_owned()));

    let defaults: Defaults = ayaml::from_str("a: 1\n").expect("defaults");
    assert_eq!(
        defaults,
        Defaults {
            a: 1,
            b: "default".to_owned()
        }
    );
    let error = ayaml::from_str::<Strict>("a: 1\nextra: 2\n").expect_err("unknown field");
    assert_eq!(error.kind(), ErrorKind::Serde);
}

#[test]
fn document_deserializer_reads_documents_incrementally() {
    let limits = Limits::default().with_max_documents(2);
    let input = "a: 1\n---\na: 2\n";
    let mut deserializer =
        ayaml::Deserializer::from_str_with_limits(input, &limits).expect("deserializer");
    let first: Strict = deserializer.next().expect("first").expect("first value");
    assert_eq!(first, Strict { a: 1 });
    let second: Strict = deserializer.next().expect("second").expect("second value");
    assert_eq!(second, Strict { a: 2 });
    assert!(deserializer.next::<Strict>().expect("end").is_none());
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

#[test]
fn serde_flatten_tuple_struct_and_non_string_map_keys_work() {
    let value: Flattened = ayaml::from_str("known: 1\nextra: text\nother: 3\n").expect("flatten");
    assert_eq!(value.known, 1);
    assert_eq!(value.rest["extra"], Value::from("text"));
    assert_eq!(value.rest["other"].as_u64(), Some(3));

    let tuple: TupleStruct = ayaml::from_str("[7, name]\n").expect("tuple struct");
    assert_eq!(tuple, TupleStruct(7, "name".to_owned()));

    let unit: UnitStruct = ayaml::from_str("null\n").expect("unit struct");
    assert_eq!(unit, UnitStruct);

    let map: std::collections::BTreeMap<u64, String> =
        ayaml::from_str("{1: one, 2: two}\n").expect("numeric map keys");
    assert_eq!(map[&1], "one");
    assert_eq!(map[&2], "two");
}

#[test]
fn serde_ignored_any_consumes_nested_values() {
    let ignored: serde::de::IgnoredAny =
        ayaml::from_str("a: [1, {b: 2}]\nc: null\n").expect("ignored");
    let _ = ignored;
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct RoundTrip {
    name: String,
    values: Vec<u64>,
}

#[test]
fn to_value_serializes_structs_and_enums() {
    let value = ayaml::to_value(RoundTrip {
        name: "x".to_owned(),
        values: vec![1, 2],
    })
    .expect("to_value");
    assert_eq!(value["name"], Value::from("x"));
    assert_eq!(value["values"][1].as_u64(), Some(2));
    let decoded: RoundTrip = ayaml::from_value(value).expect("round trip");
    assert_eq!(decoded.name, "x");

    let unit = ayaml::to_value(TaggedEnum::Unit).expect("unit");
    assert_eq!(unit, Value::from("Unit"));
    let decoded: TaggedEnum = ayaml::from_value(unit).expect("unit decode");
    assert_eq!(decoded, TaggedEnum::Unit);

    let tuple = ayaml::to_value(TaggedEnum::Tuple(3, 4)).expect("tuple");
    let decoded: TaggedEnum = ayaml::from_value(tuple).expect("tuple decode");
    assert_eq!(decoded, TaggedEnum::Tuple(3, 4));

    assert_eq!(ayaml::to_string(&TaggedEnum::Unit).unwrap(), "Unit\n");
}

#[test]
fn core_collection_tags_are_resolved_and_null_sequences_are_empty() {
    let tagged = ayaml::from_str_borrowed::<Value>("!!seq [1, 2]\n").expect("seq tag");
    assert!(matches!(tagged, Value::Sequence(_)));

    #[derive(Debug, serde::Deserialize)]
    struct Defaults {
        #[serde(default)]
        items: Vec<String>,
    }
    let defaults: Defaults = ayaml::from_str_borrowed("items:\n").expect("null sequence");
    assert!(defaults.items.is_empty());
}

#[test]
fn duplicate_key_diagnostic_points_at_the_first_key() {
    let error = ayaml::from_str_borrowed::<Value>("a: 1\na: 2\n").expect_err("duplicate");
    let location = error.location().expect("location");
    assert_eq!((location.line(), location.column()), (1, 1));
}

#[test]
fn empty_documents_are_null() {
    assert_eq!(ayaml::parse_value("").unwrap(), Value::Null);
    assert_eq!(ayaml::parse_value("\n").unwrap(), Value::Null);
}

#[derive(Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
struct DefaultedDocument {
    name: String,
    count: u32,
}

#[test]
fn default_from_str_uses_the_typed_path() {
    // the default entry point reads an empty document into a struct with
    // defaults, and reports duplicate keys at their first occurrence.
    let defaults: DefaultedDocument = ayaml::from_str("").expect("empty document defaults");
    assert_eq!(defaults, DefaultedDocument::default());

    let duplicate = ayaml::from_str::<Value>("a: 1\na: 2\n").expect_err("duplicate key");
    assert_eq!(duplicate.kind(), ErrorKind::DuplicateKey);
    assert_eq!(
        duplicate.location().map(|l| (l.line(), l.column())),
        Some((1, 1))
    );
}

#[test]
fn anchor_names_follow_the_spec_by_default() {
    // YAML 1.2.2: anchors may contain `?` and `:`.
    assert_eq!(ayaml::parse_value("&a?b\n").unwrap(), Value::Null);
    assert_eq!(
        ayaml::parse_value("key: &an:chor value\n").unwrap(),
        ayaml::parse_value("key: value\n").unwrap()
    );
}

#[test]
fn default_float_targets_follow_yaml_serde_text_parsing() {
    // Plain scalar text is parsed into a float target. Legacy octal
    // spellings parse; radix integers are rejected, as yaml_serde does.
    assert_eq!(ayaml::from_str::<f64>("01\n").unwrap(), 1.0);
    assert_eq!(ayaml::from_str::<f64>("0123\n").unwrap(), 123.0);
    assert!(ayaml::from_str::<f64>("0x1F\n").is_err());
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_float_targets_follow_yaml_serde_text_parsing() {
    assert_eq!(ayaml::compat::from_str::<f64>("01\n").unwrap(), 1.0);
    assert_eq!(ayaml::compat::from_str::<f64>("0123\n").unwrap(), 123.0);
    assert!(ayaml::compat::from_str::<f64>("0x1F\n").is_err());
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn the_compat_feature_does_not_change_root_functions() {
    // enabling the compatibility module must be additive. The root
    // function keeps the YAML 1.2.2 reading; only `compat` keeps libyaml's.
    assert_eq!(ayaml::from_str::<Value>("&a?b\n").unwrap(), Value::Null);
    assert_eq!(
        ayaml::compat::from_str::<Value>("&a?b\n").unwrap(),
        Value::from("?b")
    );
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_anchor_names_stop_like_libyaml() {
    assert_eq!(
        ayaml::compat::from_str::<Value>("&a?b\n").unwrap(),
        Value::from("?b")
    );
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_bom_before_document_marker_is_content() {
    assert_eq!(
        ayaml::compat::from_str::<Value>("\u{feff}---\rX[\n!'\n").unwrap(),
        Value::from("--- X[ !'")
    );
}

#[test]
fn bom_and_carriage_returns_around_document_markers() {
    // A stream-start BOM is not content, so `---` after it starts a document
    //. The compat parser keeps libyaml's reading; see below.
    assert_eq!(
        ayaml::parse_value("\u{feff}---\rX[\n!'\n").unwrap(),
        Value::from("X[ !'")
    );
    assert_eq!(ayaml::parse_value("---\rX").unwrap(), Value::from("X"));
    assert_eq!(ayaml::parse_value("---\r\nkey: value\r\n").unwrap(), {
        let mut mapping = Mapping::new();
        mapping.insert(Value::from("key"), Value::from("value"));
        Value::Mapping(mapping)
    });
}

#[test]
fn root_block_scalar_stops_at_document_markers() {
    assert_eq!(ayaml::parse_value("|\n...\r").unwrap(), Value::from(""));
}

#[test]
fn folded_block_scalar_preserves_breaks_around_more_indented_lines() {
    assert_eq!(
        ayaml::parse_value("> \n $$ $\n  $TTTTTTUTTTTT").unwrap(),
        Value::from("$$ $\n $TTTTTTUTTTTT")
    );
}

#[derive(Debug, PartialEq, serde::Deserialize)]
struct BorrowedConfig<'a> {
    name: &'a str,
    label: &'a str,
    tags: Vec<&'a str>,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct SpannedCount {
    count: u32,
}

#[test]
fn borrowed_deserialization_keeps_simple_strings_borrowed() {
    let input = "name: plain\nlabel: 'quoted'\ntags: [one, \"two\"]\n";
    let config: BorrowedConfig<'_> = ayaml::from_str_borrowed(input).expect("borrowed");
    assert_eq!(
        config,
        BorrowedConfig {
            name: "plain",
            label: "quoted",
            tags: vec!["one", "two"],
        }
    );
    let input_start = input.as_ptr() as usize;
    let input_end = input_start + input.len();
    for text in [config.name, config.label, config.tags[0], config.tags[1]] {
        let start = text.as_ptr() as usize;
        assert!(start >= input_start && start + text.len() <= input_end);
    }
    let value: &str = ayaml::from_str_borrowed("plain\n").expect("scalar");
    assert_eq!(value, "plain");
    let value: &str = ayaml::from_slice_borrowed(b"\"quoted\"\n").expect("slice");
    assert_eq!(value, "quoted");
}

#[test]
fn borrowed_deserialization_errors_keep_source_spans() {
    let error = ayaml::from_str_borrowed::<SpannedCount>("count: nope\n").expect_err("type");
    assert_eq!(error.kind(), ErrorKind::Serde);
    let location = error.location().expect("span");
    assert_eq!(location.line(), 1);
    assert_eq!(location.column(), 8);
}

#[test]
fn borrowed_deserialization_rejects_duplicates_and_trailing_documents() {
    let duplicate = ayaml::from_str_borrowed::<Value>("a: 1\na: 2\n").expect_err("duplicate key");
    assert_eq!(duplicate.kind(), ErrorKind::DuplicateKey);
    let trailing =
        ayaml::from_str_borrowed::<Value>("---\na: 1\n---\nb: 2\n").expect_err("trailing");
    assert_eq!(trailing.kind(), ErrorKind::Parser);
    let trailing =
        ayaml::from_str_borrowed_with_limits::<Value>("---\na: 1\n---\nb: 2\n", &Limits::compat())
            .expect_err("document limit");
    assert_eq!(trailing.kind(), ErrorKind::Limit);
}

#[test]
fn borrowed_serde_covers_typed_shapes() {
    let input = "name: sample\ncount: 3\nenabled: true\nitems:\n  - a\n  - b\n";
    let config: TestConfig = ayaml::from_str_borrowed(input).expect("config");
    assert_eq!(config.name, "sample");
    assert_eq!(config.count, 3);
    assert!(config.enabled);
    assert_eq!(config.items, ["a", "b"]);

    let unit: TaggedEnum = ayaml::from_str_borrowed("!Unit\n").expect("unit");
    assert_eq!(unit, TaggedEnum::Unit);
    let tuple: TaggedEnum = ayaml::from_str_borrowed("!Tuple [1, 2]\n").expect("tuple");
    assert_eq!(tuple, TaggedEnum::Tuple(1, 2));

    let number: Untagged = ayaml::from_str_borrowed("3\n").expect("number");
    assert_eq!(number, Untagged::Number(3));
    let defaults: Defaults = ayaml::from_str_borrowed("a: 1\n").expect("defaults");
    assert_eq!(defaults.b, "default");

    let flattened: Flattened =
        ayaml::from_str_borrowed("known: 1\nextra: text\n").expect("flatten");
    assert_eq!(flattened.rest["extra"], Value::from("text"));
    let tuple_struct: TupleStruct = ayaml::from_str_borrowed("[7, name]\n").expect("tuple struct");
    assert_eq!(tuple_struct, TupleStruct(7, "name".to_owned()));
    let unit_struct: UnitStruct = ayaml::from_str_borrowed("null\n").expect("unit struct");
    assert_eq!(unit_struct, UnitStruct);

    let map: std::collections::BTreeMap<u64, String> =
        ayaml::from_str_borrowed("{1: one, 2: two}\n").expect("numeric map keys");
    assert_eq!(map[&1], "one");
    let ignored: serde::de::IgnoredAny =
        ayaml::from_str_borrowed("a: [1, {b: 2}]\nc: null\n").expect("ignored");
    let _ = ignored;
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct NestedCounts {
    items: Vec<u32>,
}

#[test]
fn borrowed_deserialization_preserves_nested_error_spans() {
    let error = ayaml::from_str_borrowed::<NestedCounts>("items:\n  - 1\n  - nope\n")
        .expect_err("nested type");
    assert_eq!(error.kind(), ErrorKind::Serde);
    let location = error.location().expect("span");
    assert_eq!(location.line(), 3);
    assert_eq!(location.column(), 5);
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

#[test]
fn borrowed_deserializer_streams_documents() {
    let limits = Limits::default().with_max_documents(2);
    let input = "name: one\n---\nname: two\n";
    let mut deserializer =
        ayaml::Deserializer::from_str_with_limits(input, &limits).expect("deserializer");
    let first: BorrowedDocument<'_> = deserializer
        .next_borrowed()
        .expect("first")
        .expect("first value");
    assert_eq!(first.name, "one");
    let second: BorrowedDocument<'_> = deserializer
        .next_borrowed()
        .expect("second")
        .expect("second value");
    assert_eq!(second.name, "two");
    assert!(
        deserializer
            .next_borrowed::<BorrowedDocument<'_>>()
            .expect("end")
            .is_none()
    );

    let mut deserializer = ayaml::Deserializer::from_str_with_limits("one\n---\ntwo\n", &limits)
        .expect("seeded deserializer");
    let first: &str = deserializer
        .next_seed_borrowed(BorrowedScalarSeed)
        .expect("seeded")
        .expect("seeded value");
    assert_eq!(first, "one");
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

#[test]
fn byte_buffer_visitors_work_on_owned_and_borrowed_paths() {
    let borrowed: Bytes = ayaml::from_str_borrowed("hello\n").expect("borrowed bytes");
    assert_eq!(borrowed, Bytes(b"hello".to_vec()));
    let owned: Bytes = ayaml::from_str("hello\n").expect("owned bytes");
    assert_eq!(owned, Bytes(b"hello".to_vec()));
    let from_value: Bytes = ayaml::from_value(Value::from("hi")).expect("value bytes");
    assert_eq!(from_value, Bytes(b"hi".to_vec()));
    let tagged: Bytes = ayaml::from_str_borrowed("!Raw hello\n").expect("tagged bytes");
    assert_eq!(tagged, Bytes(b"hello".to_vec()));
}

/// A BOM may only start a document (YAML 1.2.2 section 5.2, P16); libyaml
/// keeps one inside block-scalar content, and so does compat.
#[test]
fn bom_in_block_scalar_content_is_an_error() {
    assert!(ayaml::parse_value("|-\n  +\n  \u{feff}\n").is_err());
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_block_scalar_content_keeps_bom_characters() {
    assert_eq!(
        ayaml::compat::from_str::<Value>("|-\n  +\n  \u{feff}\n").unwrap(),
        Value::from("+\n\u{feff}")
    );
}

#[test]
fn tagged_null_payloads_round_trip_without_becoming_the_string_null() {
    for input in ["baa:: |\n::a:: !", "!\n", "! null\n"] {
        let value = ayaml::parse_value(input).expect("parse");
        let rendered = ayaml::to_string_value(&value).expect("emit");
        assert_eq!(
            ayaml::parse_value(&rendered).expect("reparse"),
            value,
            "input={input:?} rendered={rendered:?}"
        );
    }
}

#[test]
fn block_scalars_with_leading_content_spaces_use_an_explicit_indent() {
    let value = ayaml::parse_value("a: |1\n  lie one\n  next").expect("parse");
    let rendered = ayaml::to_string_value(&value).expect("emit");
    assert!(rendered.contains("|2"), "rendered={rendered:?}");
    assert_eq!(
        ayaml::parse_value(&rendered).expect("reparse"),
        value,
        "rendered={rendered:?}"
    );
}

#[test]
fn strings_beginning_with_bom_are_quoted_on_emission() {
    let value = Value::from("\u{feff}l  a");
    let rendered = ayaml::to_string_value(&value).expect("emit");
    assert_eq!(ayaml::parse_value(&rendered).expect("reparse"), value);
}

#[test]
fn tagged_null_mapping_keys_round_trip_in_flow_style() {
    for input in ["! : !\n", "!c1\n! : !"] {
        let value = ayaml::parse_value(input).expect("parse");
        let rendered = ayaml::to_string_value(&value).expect("emit");
        assert_eq!(
            ayaml::parse_value(&rendered).expect("reparse"),
            value,
            "input={input:?} rendered={rendered:?}"
        );
    }
}

#[test]
fn control_characters_and_flow_indicators_are_quoted() {
    for text in ["a,b", "a[b", "a{b", "\u{0}\u{c}6,\u{c}"] {
        let value = Value::from(text);
        let rendered = ayaml::to_string_value(&value).expect("emit");
        assert_eq!(
            ayaml::parse_value(&rendered).expect("reparse"),
            value,
            "rendered={rendered:?}"
        );
    }
}

#[test]
fn whitespace_only_block_scalar_lines_fall_back_to_quoted_output() {
    let value = Value::from(" Jes&\n \n");
    let rendered = ayaml::to_string_value(&value).expect("emit");
    assert_eq!(ayaml::parse_value(&rendered).expect("reparse"), value);
}

#[test]
fn newline_only_strings_round_trip_as_block_scalars() {
    for text in ["\n", "\n\n", "\r\n"] {
        let value = Value::from(text);
        let rendered = ayaml::to_string_value(&value).expect("emit");
        assert_eq!(
            ayaml::parse_value(&rendered).expect("reparse"),
            value,
            "rendered={rendered:?}"
        );
    }
}

#[test]
fn per_node_scalar_styles_apply_by_path() {
    use ayaml::{EmitterStyles, NodeStyle, PathSegment};
    let value = ayaml::parse_value("a: plain\nb: [one, two]\nc:\n  d: deep\n").expect("parse");
    let mut styles = EmitterStyles::new();
    styles.set(
        vec![PathSegment::key("a")],
        NodeStyle::default().with_scalar_style(Some(ScalarStyle::SingleQuoted)),
    );
    styles.set(
        vec![PathSegment::key("b"), PathSegment::index(1)],
        NodeStyle::default().with_scalar_style(Some(ScalarStyle::DoubleQuoted)),
    );
    styles.set(
        vec![PathSegment::key("c"), PathSegment::key("d")],
        NodeStyle::default().with_scalar_style(Some(ScalarStyle::SingleQuoted)),
    );
    let rendered = ayaml::to_string_value_with_styles(&value, &styles).expect("emit");
    assert!(rendered.contains("a: 'plain'"), "{rendered:?}");
    assert!(rendered.contains("- one"), "{rendered:?}");
    assert!(rendered.contains("- \"two\""), "{rendered:?}");
    assert!(rendered.contains("d: 'deep'"), "{rendered:?}");
    assert_eq!(ayaml::parse_value(&rendered).expect("reparse"), value);
}

#[test]
fn per_node_styles_apply_in_flow_and_at_the_root() {
    use ayaml::{EmitterOptions, EmitterStyles, NodeStyle, PathSegment};
    let value = ayaml::parse_value("b: [one, two]\n").expect("parse");
    let mut styles = EmitterStyles::new();
    styles.set(
        vec![PathSegment::key("b"), PathSegment::index(1)],
        NodeStyle::default().with_scalar_style(Some(ScalarStyle::SingleQuoted)),
    );
    let rendered = ayaml::to_string_value_with_options_and_styles(
        &value,
        &EmitterOptions::default().with_flow(true),
        &styles,
    )
    .expect("flow");
    assert!(rendered.contains("'two'"), "{rendered:?}");
    assert_eq!(ayaml::parse_value(&rendered).expect("reparse"), value);

    let root = Value::from("root");
    let mut styles = EmitterStyles::new();
    styles.set(
        Vec::new(),
        NodeStyle::default().with_scalar_style(Some(ScalarStyle::DoubleQuoted)),
    );
    let rendered = ayaml::to_string_value_with_styles(&root, &styles).expect("root");
    assert_eq!(rendered, "\"root\"\n");
}

#[test]
fn per_node_flow_overrides_apply_to_collections() {
    use ayaml::{EmitterStyles, NodeStyle, PathSegment};
    let value = ayaml::parse_value("a:\n  b: 1\n  c: 2\nlist:\n  - x\n  - y\n").unwrap();
    let mut styles = EmitterStyles::new();
    styles.set(
        vec![PathSegment::key("a")],
        NodeStyle::default().with_flow(Some(true)),
    );
    styles.set(
        vec![PathSegment::key("list")],
        NodeStyle::default().with_flow(Some(true)),
    );
    let rendered = ayaml::to_string_value_with_styles(&value, &styles).expect("emit");
    assert!(rendered.contains("a: {b: 1, c: 2}"), "{rendered:?}");
    assert!(rendered.contains("list: [x, y]"), "{rendered:?}");
    assert_eq!(ayaml::parse_value(&rendered).unwrap(), value);

    let mut styles = EmitterStyles::new();
    styles.set(Vec::new(), NodeStyle::default().with_flow(Some(true)));
    let rendered = ayaml::to_string_value_with_styles(&value, &styles).expect("root flow");
    assert!(rendered.starts_with('{'), "{rendered:?}");
    assert_eq!(ayaml::parse_value(&rendered).unwrap(), value);
}

#[test]
fn per_node_chomping_overrides_are_best_effort_and_lossless() {
    use ayaml::{Chomping, EmitterStyles, NodeStyle, PathSegment};
    let value = ayaml::parse_value("a: |\n  line\n").unwrap();
    let mut styles = EmitterStyles::new();
    styles.set(
        vec![PathSegment::key("a")],
        NodeStyle::default().with_chomping(Some(Chomping::Keep)),
    );
    let rendered = ayaml::to_string_value_with_styles(&value, &styles).expect("keep");
    assert!(rendered.contains("|+"), "{rendered:?}");
    assert_eq!(ayaml::parse_value(&rendered).unwrap(), value);

    let value = Value::from("line");
    let mut styles = EmitterStyles::new();
    styles.set(
        Vec::new(),
        NodeStyle::default()
            .with_chomping(Some(Chomping::Strip))
            .with_scalar_style(Some(ScalarStyle::Literal)),
    );
    let rendered = ayaml::to_string_value_with_styles(&value, &styles).expect("strip");
    assert!(rendered.starts_with("|-"), "{rendered:?}");
    assert_eq!(ayaml::parse_value(&rendered).unwrap(), value);

    let mut styles = EmitterStyles::new();
    styles.set(
        Vec::new(),
        NodeStyle::default()
            .with_chomping(Some(Chomping::Clip))
            .with_scalar_style(Some(ScalarStyle::Literal)),
    );
    let rendered = ayaml::to_string_value_with_styles(&Value::from("line"), &styles).expect("clip");
    assert!(rendered.starts_with("|-"), "{rendered:?}");
}

#[test]
fn complex_folded_scalars_round_trip_when_folding_is_lossless() {
    use ayaml::EmitterOptions;
    for text in ["a\nb", "a\nb\n", "a\n\nb", "a\nb\n\n", "a\n"] {
        let value = Value::from(text);
        let rendered = ayaml::to_string_value_with_options(
            &value,
            &EmitterOptions::default().with_scalar_style(Some(ScalarStyle::Folded)),
        )
        .expect("folded emit");
        assert!(rendered.starts_with('>'), "rendered={rendered:?}");
        assert_eq!(
            ayaml::parse_value(&rendered).expect("reparse"),
            value,
            "rendered={rendered:?}"
        );
    }

    let value = Value::from("a\n b");
    let rendered = ayaml::to_string_value_with_options(
        &value,
        &EmitterOptions::default().with_scalar_style(Some(ScalarStyle::Folded)),
    )
    .expect("fallback emit");
    assert!(rendered.starts_with('|'), "rendered={rendered:?}");
    assert_eq!(ayaml::parse_value(&rendered).expect("reparse"), value);
}

#[test]
fn flow_explicit_keys_work_without_colons() {
    let expected = {
        let mut mapping = Mapping::new();
        mapping.insert(Value::from("$"), Value::Null);
        Value::Mapping(mapping)
    };
    // `?` directly followed by text is a plain scalar in YAML 1.2.2 (HM87).
    assert_eq!(
        ayaml::parse_value("[?$]").unwrap(),
        Value::Sequence(vec![Value::from("?$")])
    );
    assert_eq!(
        ayaml::parse_value("[? foo]").unwrap(),
        Value::Sequence(vec![{
            let mut mapping = Mapping::new();
            mapping.insert(Value::from("foo"), Value::Null);
            Value::Mapping(mapping)
        }])
    );
    assert_eq!(ayaml::parse_value("{?$}").unwrap(), {
        let mut mapping = Mapping::new();
        mapping.insert(Value::from("?$"), Value::Null);
        Value::Mapping(mapping)
    });
    assert_eq!(ayaml::parse_value("{? $}").unwrap(), expected);
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_flow_question_mark_is_always_an_explicit_key() {
    let expected = {
        let mut mapping = Mapping::new();
        mapping.insert(Value::from("$"), Value::Null);
        Value::Mapping(mapping)
    };
    assert_eq!(
        ayaml::compat::from_str::<Value>("[?$]").unwrap(),
        Value::Sequence(vec![expected.clone()])
    );
    assert_eq!(ayaml::compat::from_str::<Value>("{?$}").unwrap(), expected);
}

#[test]
fn non_printable_characters_are_rejected() {
    // P8: c-printable excludes C0 controls other than tab/LF/CR, DEL, C1
    // controls other than NEL, and U+FFFE/U+FFFF. Escapes can still produce
    // them inside double-quoted scalars, and a quoted scalar may hold all
    // but the C0 controls raw (P32, `tests/regressions.rs`).
    for input in [
        "a: b\u{1}c\n",
        "!c1\u{4}\0\0\n! : !",
        "a\u{7f}\n",
        "a\u{9b}\n",
        "\u{fffe}\n",
    ] {
        let error = ayaml::parse_value(input).expect_err(input);
        assert_eq!(error.kind(), ayaml::ErrorKind::Scanner, "{input:?}");
    }
    assert_eq!(
        ayaml::parse_value("\"\\x01\\x7f\"").unwrap(),
        Value::from("\u{1}\u{7f}")
    );
}

#[test]
fn escaped_quoted_whitespace_survives_line_folding() {
    assert_eq!(
        ayaml::parse_value("  \"\\\t \r\"").unwrap(),
        Value::from("\t ")
    );
}

/// BOMs may start each document prefix (YAML 1.2.2 productions 202 and
/// 211), so they may precede a document, follow one, or precede a `---`;
/// anywhere else in a document they are an error (P16).
#[test]
fn boms_may_only_start_a_document() {
    assert_eq!(
        ayaml::parse_value("\n\u{feff}\n\u{feff}").unwrap(),
        Value::Null
    );
    assert_eq!(
        ayaml::parse_value("\n\u{feff} x").unwrap(),
        Value::from("x")
    );
    assert_eq!(ayaml::parse_value("\u{feff}\u{feff}").unwrap(), Value::Null);
    assert_eq!(
        ayaml::parse_documents_with_limits(
            "a\n\u{feff}# next\n---\nb\n\u{feff}\n",
            &ayaml::Limits::default().with_max_documents(4)
        )
        .unwrap(),
        vec![Value::from("a"), Value::from("b")]
    );
    for input in [
        "a: b\u{feff}c\n",
        "a\u{feff}: b\n",
        "a: b # c\u{feff}\n",
        "a: b\n\u{feff}c: d\n",
        "&a\u{feff} b\n",
        "[a,\n\u{feff}b]\n",
    ] {
        assert!(ayaml::parse_value(input).is_err(), "{input:?}");
    }
    // Quoted scalars may hold a BOM.
    assert_eq!(
        ayaml::parse_value("\"a\u{feff}b\"\n").unwrap(),
        Value::from("a\u{feff}b")
    );
}

/// libyaml skips a BOM at the start of any line and keeps one elsewhere as
/// content; compat keeps that.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_line_start_boms_are_ignored_but_same_line_bom_content_is_kept() {
    let compat = |input: &str| ayaml::compat::from_str::<Value>(input).unwrap();
    assert_eq!(compat("\n\u{feff}\n\u{feff}"), Value::Null);
    assert_eq!(compat("\n\u{feff} x"), Value::from("x"));
    assert_eq!(compat("\u{feff}\u{feff}"), Value::from("\u{feff}"));
    assert_eq!(compat("a: b\u{feff}c\n")["a"], Value::from("b\u{feff}c"));
}

#[test]
fn root_block_scalar_bom_lines_are_blank() {
    assert_eq!(ayaml::parse_value(">\n\u{feff} ").unwrap(), Value::from(""));
    assert_eq!(
        ayaml::parse_value(">\n\u{feff}  ").unwrap(),
        Value::from("")
    );
}

/// A BOM line ends a plain scalar: it starts a document prefix or is an
/// error (P16).
#[test]
fn bom_lines_end_plain_scalars() {
    assert_eq!(
        ayaml::parse_value("  a\n\u{feff}").unwrap(),
        Value::from("a")
    );
    assert!(ayaml::parse_value("  a\n\u{feff}x").is_err());
}

/// libyaml continues a root plain scalar onto a BOM line, keeping the BOM,
/// but ends a nested one there; compat keeps both.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_bom_continuation_lines_join_root_plain_scalars() {
    let compat = |input: &str| ayaml::compat::from_str::<Value>(input).unwrap();
    assert_eq!(compat("  a\n\u{feff}"), Value::from("a \u{feff}"));
    assert_eq!(compat("  a\n\u{feff}x"), Value::from("a \u{feff}x"));
    assert_eq!(
        compat("nems:\n   :map\n\u{feff}\n")["nems"],
        Value::from(":map")
    );
}

#[test]
fn flow_plain_scalars_do_not_consume_trailing_line_breaks() {
    assert_eq!(
        ayaml::parse_value("[-)\r\r]").unwrap(),
        Value::Sequence(vec![Value::from("-)")])
    );
    assert_eq!(
        ayaml::parse_value("[a\n]").unwrap(),
        Value::Sequence(vec![Value::from("a")])
    );
}

#[test]
fn nested_sequences_emit_compactly_and_round_trip() {
    let value = ayaml::parse_value("[[[0]]]").unwrap();
    let rendered = ayaml::to_string_value(&value).unwrap();
    assert_eq!(rendered, "- - - 0\n");
    assert_eq!(ayaml::parse_value(&rendered).unwrap(), value);
}

#[test]
fn generic_value_and_mapping_lookup_accept_compatible_keys() {
    let mut value = ayaml::parse_value("a: 1\nb: [x, y]\n").unwrap();
    assert_eq!(value.get("a"), Some(&Value::from(1_u64)));
    assert_eq!(
        value.get("b").and_then(|items| items.get(1)),
        Some(&Value::from("y"))
    );
    assert!(value.get("missing").is_none());
    assert_eq!(value.get_mut("a").map(|item| item.as_u64()), Some(Some(1)));

    let Value::Mapping(mapping) = &mut value else {
        panic!("mapping");
    };
    assert_eq!(mapping.get("a"), Some(&Value::from(1_u64)));
    assert_eq!(mapping.get(Value::from("a")), Some(&Value::from(1_u64)));
    for item in mapping.values_mut() {
        let _ = item;
    }
}
