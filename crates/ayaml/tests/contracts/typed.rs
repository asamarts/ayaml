use super::*;

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
fn duplicate_key_diagnostic_points_at_the_duplicate_key() {
    let error = ayaml::from_str_borrowed::<Value>("a: 1\na: 2\n").expect_err("duplicate");
    let location = error.location().expect("location");
    assert_eq!((location.line(), location.column()), (2, 1));
}

#[test]
fn a_typed_map_reports_keys_that_differ_only_in_type() {
    use std::collections::BTreeMap;

    let input = "1: 1\n\"1\": 2\n";
    let error = ayaml::from_str::<BTreeMap<String, u64>>(input).expect_err("duplicate");
    assert_eq!(error.kind(), ErrorKind::DuplicateKey);
    assert_eq!(
        error
            .location()
            .map(|location| (location.line(), location.column())),
        Some((2, 1))
    );

    let error = ayaml::from_str::<Value>(input).expect_err("duplicate");
    assert_eq!(error.kind(), ErrorKind::DuplicateKey);

    let value = ayaml::parse_value(input).expect("the generic value keeps both keys");
    assert_eq!(value.as_mapping().expect("mapping").len(), 2);
}

#[test]
fn a_complex_duplicate_key_prints_readably_and_shortly() {
    for input in [
        "[1, 2]: a\n[1, 2]: b\n",
        "!t 1: a\n!t 1: b\n",
        "? {a: 1, b: 2}\n: x\n? {a: 1, b: 2}\n: y\n",
    ] {
        let error = ayaml::from_str::<Value>(input).expect_err("duplicate");
        assert_eq!(error.kind(), ErrorKind::DuplicateKey, "{input:?}");
        let message = error.message();
        assert!(!message.contains("Node {"), "{message}");
        assert!(!message.contains("Sequence("), "{message}");
        assert!(!message.contains("Mapping("), "{message}");
        assert!(message.chars().count() < 120, "{message}");
    }
    let long = format!("[{}]: a\n[{}]: b\n", "1, ".repeat(200), "1, ".repeat(200));
    let error = ayaml::from_str::<Value>(&long).expect_err("duplicate");
    assert!(error.message().chars().count() < 120, "{}", error.message());
    assert!(error.message().contains('…'), "{}", error.message());
}

#[test]
fn a_duplicate_key_with_location_like_text_keeps_its_location() {
    let input = "\"at line 9\": 1\n\"at line 9\": 2\n";
    let error = ayaml::from_str::<Value>(input).expect_err("duplicate");
    assert_eq!(error.kind(), ErrorKind::DuplicateKey);
    assert_eq!(
        error
            .location()
            .map(|location| (location.line(), location.column())),
        Some((2, 1))
    );
    assert!(error.message().contains("at line 9"), "{error}");
}

#[test]
fn empty_documents_are_null() {
    assert_eq!(ayaml::parse_value("").unwrap(), Value::Null);
    assert_eq!(ayaml::parse_value("\n").unwrap(), Value::Null);
}

#[test]
fn default_from_str_uses_the_typed_path() {
    // the default entry point reads an empty document into a struct with
    // defaults, and reports a duplicate key at the duplicate occurrence, as
    // `parse_value` does.
    let defaults: DefaultedDocument = ayaml::from_str("").expect("empty document defaults");
    assert_eq!(defaults, DefaultedDocument::default());

    let duplicate = ayaml::from_str::<Value>("a: 1\na: 2\n").expect_err("duplicate key");
    assert_eq!(duplicate.kind(), ErrorKind::DuplicateKey);
    assert_eq!(
        duplicate.location().map(|l| (l.line(), l.column())),
        Some((2, 1))
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
fn default_float_targets_parse_plain_scalar_text() {
    // Plain scalar text is parsed into a float target. Legacy octal
    // spellings parse; radix integers are rejected.
    assert_eq!(ayaml::from_str::<f64>("01\n").unwrap(), 1.0);
    assert_eq!(ayaml::from_str::<f64>("0123\n").unwrap(), 123.0);
    assert!(ayaml::from_str::<f64>("0x1F\n").is_err());
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_float_targets_parse_plain_scalar_text() {
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

#[test]
fn borrowed_deserialization_preserves_nested_error_spans() {
    let error = ayaml::from_str_borrowed::<NestedCounts>("items:\n  - 1\n  - nope\n")
        .expect_err("nested type");
    assert_eq!(error.kind(), ErrorKind::Serde);
    let location = error.location().expect("span");
    assert_eq!(location.line(), 3);
    assert_eq!(location.column(), 5);
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

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_classifies_by_structured_data_not_hostile_key_names() {
    use std::collections::BTreeMap;

    for key in [
        "collection depth limit",
        "trailing documents",
        "unexpected token in mapping value",
    ] {
        let input = format!("{key}: x\n");
        let error = ayaml::compat::from_str::<BTreeMap<String, u8>>(&input).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Serde, "{key}: {error}");
    }
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_keeps_locations_on_depth_and_document_errors() {
    let deep = format!("{}a\n", "- ".repeat(129));
    let error = ayaml::compat::from_str::<Value>(&deep).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Serde);
    assert_eq!(error.message(), "recursion limit exceeded");
    assert_eq!(
        error
            .location()
            .map(|location| (location.line(), location.column())),
        Some((1, 257))
    );

    let error = ayaml::compat::from_str::<Value>("a: 1\n---\nb: 2\n").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Parser);
    assert_eq!(error.message(), "more than one document in stream");
    assert_eq!(
        error
            .location()
            .map(|location| (location.line(), location.column())),
        Some((2, 1))
    );
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_reports_an_error_inside_the_first_document_before_a_second() {
    let error = ayaml::compat::from_str::<Value>("*x\n---\na: 1\n").unwrap_err();
    assert!(
        error.message().contains("unknown anchor"),
        "the second-document error won: {error}"
    );
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_maps_a_bom_directive_to_the_libyaml_token_error() {
    let error = ayaml::compat::from_str::<Value>("\u{feff}%YAML 1.2\n---\na: 1\n").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Scanner);
    assert_eq!(
        error.message(),
        "found character that cannot start any token"
    );
    assert!(error.span().is_none(), "{error}");
}

/// Every typed reader keeps a mapping's keys in source order. The source is deliberately not
/// sorted, so a reader that sorts or reverses keys fails.
#[test]
fn typed_mapping_keeps_source_key_order() {
    let input = "z: 1\na: 2\nm: 3\nb: 4\n";
    let expected = vec![
        Value::from("z"),
        Value::from("a"),
        Value::from("m"),
        Value::from("b"),
    ];
    let assert_order = |mapping: &Mapping| {
        assert_eq!(
            mapping.keys().cloned().collect::<Vec<_>>(),
            expected,
            "typed mapping key order"
        );
    };

    let mapping: Mapping = ayaml::from_str(input).expect("from_str");
    assert_order(&mapping);
    let mapping: Mapping = ayaml::from_str_borrowed(input).expect("from_str_borrowed");
    assert_order(&mapping);
    let mapping: Mapping = ayaml::from_slice(input.as_bytes()).expect("from_slice");
    assert_order(&mapping);
    let mapping: Mapping =
        ayaml::from_value(ayaml::parse_value(input).expect("value")).expect("from_value");
    assert_order(&mapping);
    let mut documents = ayaml::Deserializer::from_str(input).expect("deserializer");
    let mapping: Mapping = documents.next().expect("a document").expect("a mapping");
    assert_order(&mapping);

    #[cfg(feature = "libyaml-compat")]
    {
        let mapping: Mapping = ayaml::compat::from_str(input).expect("compat");
        assert_order(&mapping);
    }
}
