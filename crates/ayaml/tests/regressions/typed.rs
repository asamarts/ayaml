use super::*;

/// Two keys that differ only in the order of their entries are one key, whatever the target.
#[test]
fn keys_that_differ_only_in_order_are_duplicates_in_a_typed_map() {
    use std::collections::{BTreeMap, HashMap};

    type Keyed = HashMap<BTreeMap<String, u32>, String>;
    let input = "? {a: 1, b: 2}\n: x\n? {b: 2, a: 1}\n: y\n";
    let duplicate = ayaml::from_str::<Keyed>(input).unwrap_err();
    assert_eq!(
        duplicate.kind(),
        ayaml::ErrorKind::DuplicateKey,
        "{duplicate}"
    );
    assert!(duplicate.span().is_some(), "{duplicate}");
    for input in [
        input,
        "? [{a: 1, b: 2}]\n: x\n? [{b: 2, a: 1}]\n: y\n",
        "? &k {a: 1, b: 2}\n: x\n? {b: 2, a: 1}\n: y\n",
        "? {a: 1, b: 2}\n: x\n? &k {b: 2, a: 1}\n: y\n",
        "? &k {a: 1, b: 2}\n: x\n? {c: 3}\n: y\n? *k\n: z\n",
    ] {
        let error = ayaml::from_str::<ayaml::Value>(input).unwrap_err();
        assert_eq!(
            error.kind(),
            ayaml::ErrorKind::DuplicateKey,
            "{input:?}: {error}"
        );
        let error = ayaml::parse_value(input).unwrap_err();
        assert_eq!(
            error.kind(),
            ayaml::ErrorKind::DuplicateKey,
            "{input:?}: {error}"
        );
    }
    let distinct = "? {a: 1, b: 2}\n: x\n? {a: 2, b: 1}\n: y\n? {a: 1}\n: z\n";
    assert_eq!(ayaml::from_str::<Keyed>(distinct).unwrap().len(), 3);
}

/// A closing quote may sit at any indentation, on a line of its own, so only the lines with
/// text in them count.
#[test]
fn a_closing_quote_may_sit_at_the_parents_indentation() {
    assert_eq!(parse("k: 'a\n'\n"), json!({"k": "a "}));
    assert_eq!(parse("k: \"a\n\"\n"), json!({"k": "a "}));
    assert_eq!(parse("- k: 'a\n  '\n"), json!([{"k": "a "}]));
    rejects("k: 'a\nb'\n");
}

/// Mappings used as keys report the depth limit with a location, on every value path.
#[test]
fn a_depth_limit_in_a_key_has_a_location() {
    let located = |error: ayaml::Error| {
        assert_eq!(error.kind(), ayaml::ErrorKind::Limit, "{error}");
        assert!(error.span().is_some(), "{error}");
    };
    for (open, close) in [("[", "]"), ("{", "}")] {
        let input = format!("{}{}: a\n", open.repeat(128), close.repeat(128));
        located(ayaml::parse_value(&input).unwrap_err());
        located(ayaml::parse_documents(&input).unwrap_err());
        located(ayaml::from_str::<ayaml::Value>(&input).unwrap_err());
    }
}

/// Compat mode reads the error's kind, not just its words.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_does_not_mistake_a_scalar_for_a_document_error() {
    for input in ["trailing documents\n", "documents limit exceeded\n"] {
        let error = ayaml::compat::from_str::<u32>(input).unwrap_err();
        assert_eq!(error.kind(), ayaml::ErrorKind::Serde, "{error}");
        assert!(error.span().is_some(), "{error}");
    }
    let error = ayaml::compat::from_str::<ayaml::Value>("a\n---\nb\n").unwrap_err();
    assert_eq!(error.message(), "more than one document in stream");
}

/// The exceptions `docs/limits.md` lists, and that compat's event reader isn't one of them.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_reports_limits_as_the_docs_say() {
    let limits = ayaml::Limits::default();
    let deep = format!("{}{}", "[".repeat(129), "]".repeat(129));
    let error = ayaml::compat::parse_events_with_limits(&deep, &limits).unwrap_err();
    assert_eq!(error.kind(), ayaml::ErrorKind::Limit, "{error}");
    assert!(error.limit().is_some() && error.span().is_some(), "{error}");
    let error = ayaml::compat::from_str::<ayaml::Value>(&deep).unwrap_err();
    assert_eq!(error.kind(), ayaml::ErrorKind::Serde, "{error}");
    assert!(error.limit().is_none() && error.span().is_none(), "{error}");
    let many = "---\na\n".repeat(10_001);
    let error = ayaml::compat::from_str::<ayaml::Value>(&many).unwrap_err();
    assert_eq!(error.kind(), ayaml::ErrorKind::Parser, "{error}");
    assert!(error.limit().is_none() && error.span().is_none(), "{error}");
}

/// In a flow collection, compat mode keeps a `:` in an anchor or alias name, as default mode
/// does. Ending the name there read `[&a:b]` as neither mode's grammar nor libyaml does, and
/// refused an alias to that anchor, which default mode and libyaml both accept.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_keeps_a_colon_in_a_name_in_a_flow_collection() {
    assert_eq!(compat("[&a:b]"), json!([null]));
    assert_eq!(compat("[&a:b, *a:b]"), json!([null, null]));
    for input in ["[&a:b]", "[&a:b, *a:b]", "{&a:!p {x}}", "{&a:b: c}"] {
        let compat = ayaml::compat::from_str::<ayaml::Value>(input).expect("compat");
        assert_eq!(
            compat,
            ayaml::parse_value(input).expect("default"),
            "{input:?}"
        );
    }
    // Outside a flow collection, the name still ends there, and what follows is a plain scalar.
    assert_eq!(compat("&a:b"), json!(":b"));
    assert_eq!(compat("- &a:b\n"), json!([":b"]));
}

/// By default, anchor names hold `:` and `?`, so these are anchored
/// root scalars.
#[test]
fn anchor_names_hold_colons_and_question_marks() {
    assert_eq!(parse("&-:: >\n(0: ("), json!("(0: ("));
    assert_eq!(parse("&a?b: |\nx\n"), json!("x\n"));
}

/// A custom tag keeps its scalar payload resolved.
#[test]
fn custom_tags_resolve_their_payloads() {
    let payload = |input: &str| match ayaml::parse_value(input).expect("parse") {
        ayaml::Value::Tagged(tagged) => tagged.value,
        other => panic!("expected a tagged value, got {other:?}"),
    };
    assert_eq!(payload("!Flag true\n"), ayaml::Value::Bool(true));
    assert_eq!(payload("!New 5\n"), ayaml::Value::from(5_u64));
    assert!(payload("!Unit ~\n").is_null());
    assert_eq!(payload("!Text '5'\n"), ayaml::Value::from("5"));
    // The non-specific tag `!` makes a scalar a string.
    assert_eq!(payload("! 5\n"), ayaml::Value::from("5"));
}

/// Tags compare by their full name, so verbatim and shorthand
/// spellings of a tag are the same tag.
#[test]
fn tags_compare_by_full_name() {
    assert_eq!(
        ayaml::parse_value("!<tag:yaml.org,2002:int> 12\n").expect("parse"),
        ayaml::Value::from(12_u64)
    );
    let shorthand =
        ayaml::parse_value("%TAG !e! tag:example.com,2000:\n---\n!e!foo x\n").expect("parse");
    let verbatim = ayaml::parse_value("!<tag:example.com,2000:foo> x\n").expect("parse");
    assert_eq!(shorthand, verbatim);
    let output = ayaml::to_string_value(&shorthand).expect("emit");
    assert_eq!(ayaml::parse_value(&output).expect("reparse"), shorthand);
}

/// A struct field is named by its key's text, so an integer key does not
/// bind a field by position, and a key naming no field is ignored.
#[test]
fn integer_keys_do_not_bind_fields_by_position() {
    for input in [
        "a: 1\n1: 7\n",
        "a: 1\n0: 5\n",
        "a: 1\ntrue: 5\n",
        "a: 1\n1.5: 2\n",
        "a: 1\n~: 2\n",
    ] {
        for result in every_path::<FieldA>(input) {
            assert_eq!(result, Ok(FieldA { a: 1, b: 0 }), "{input:?}");
        }
    }
    for result in every_path::<FieldA>("a: 1\n'b': 2\n") {
        assert_eq!(result, Ok(FieldA { a: 1, b: 2 }));
    }
}

/// A sequence longer than a tuple or array is an error.
#[test]
fn overlong_sequences_are_an_error() {
    for result in every_path::<(u32, u32)>("[1, 2, 3]\n") {
        assert!(result.is_err(), "{result:?}");
    }
    for result in every_path::<[u32; 2]>("[1, 2, 3]\n") {
        assert!(result.is_err(), "{result:?}");
    }
    for result in every_path::<Vec<(u32, u32)>>("- [1, 2, 3]\n") {
        assert!(result.is_err(), "{result:?}");
    }
    for result in every_path::<(u32, u32)>("[1, 2]\n") {
        assert_eq!(result, Ok((1, 2)));
    }
}

/// An empty value reads as an empty sequence, an empty map, or a struct
/// with its defaults. On the typed path an explicit `~` does not; a `Value`
/// cannot tell the two apart (fixed for `from_str` by the typed path).
#[test]
fn empty_values_are_empty_collections() {
    for result in every_path::<Vec<u32>>("") {
        assert_eq!(result, Ok(Vec::new()));
    }
    for result in every_path::<std::collections::BTreeMap<String, u32>>("") {
        assert_eq!(result, Ok(std::collections::BTreeMap::new()));
    }
    for result in every_path::<Defaults>("") {
        assert_eq!(result, Ok(Defaults::default()));
    }
    for result in every_path::<Defaults>("a: 1\nv:\nm:\n") {
        assert_eq!(
            result,
            Ok(Defaults {
                a: 1,
                ..Defaults::default()
            })
        );
    }
    assert!(ayaml::from_str_borrowed::<Vec<u32>>("~\n").is_err());
    assert!(ayaml::from_str_borrowed::<Defaults>("v: ~\n").is_err());
    assert!(ayaml::from_str_borrowed::<Vec<u32>>("''\n").is_err());
}

/// A `char` and a unit variant are read from the scalar's text, and an
/// empty value or `~` is an empty `Mapping`.
#[test]
fn chars_variants_and_mappings_follow_the_text() {
    for result in every_path::<char>("5\n") {
        assert_eq!(result, Ok('5'));
    }
    for result in every_path::<char>("55\n") {
        assert!(result.is_err(), "{result:?}");
    }
    for (input, variant) in [
        ("true\n", Renamed::Yes),
        ("1\n", Renamed::One),
        ("'true'\n", Renamed::Yes),
        ("Plain\n", Renamed::Plain),
    ] {
        for result in every_path::<Renamed>(input) {
            assert_eq!(result.as_ref(), Ok(&variant), "{input:?}");
        }
    }
    assert!(ayaml::from_str_borrowed::<Renamed>("TRUE\n").is_err());
    for input in ["", "~\n"] {
        for result in every_path::<ayaml::Mapping>(input) {
            assert_eq!(result, Ok(ayaml::Mapping::new()), "{input:?}");
        }
    }
    for result in every_path::<ayaml::Mapping>("a\n") {
        assert!(result.is_err(), "{result:?}");
    }
}

/// `Number`'s order is total and agrees with `Eq`: NaN sorts after
/// every other float, and equals itself.
#[test]
fn number_order_is_total() {
    use ayaml::Number;
    let nan = Number::from(f64::NAN);
    let one = Number::from(1.0_f64);
    assert_eq!(nan.cmp(&nan), std::cmp::Ordering::Equal);
    assert_eq!(one.cmp(&nan), std::cmp::Ordering::Less);
    assert_eq!(nan.cmp(&one), std::cmp::Ordering::Greater);
    let mut numbers = [
        nan,
        Number::from(2.0_f64),
        Number::from(-1_i64),
        one,
        Number::from(3_u64),
    ];
    numbers.sort();
    assert_eq!(
        numbers.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["-1", "3", "1.0", "2.0", ".nan"]
    );
    let set: std::collections::BTreeSet<Number> = [nan, one, nan].into_iter().collect();
    assert_eq!(set.len(), 2);
}

/// An alias refers to the most recent anchor before it, so an anchor
/// redefined inside its own anchored collection wins over the collection.
#[test]
fn inner_anchor_redefinitions_win() {
    for input in ["a: &x {k: &x v}\nb: *x\n", "a: &x\n  k: &x v\nb: *x\n"] {
        assert_eq!(
            parse(input),
            json!({"a": {"k": "v"}, "b": "v"}),
            "{input:?}"
        );
        let typed: std::collections::BTreeMap<String, Json> =
            ayaml::from_str_borrowed(input).expect("typed");
        assert_eq!(typed["b"], json!("v"), "{input:?}");
    }
    assert_eq!(
        parse("a: &x [&x a, *x]\nb: *x\n"),
        json!({"a": ["a", "a"], "b": "a"})
    );
    assert_eq!(
        parse("a: &x 1\nb: &x 2\nc: *x\n"),
        json!({"a": 1, "b": 2, "c": 2})
    );
}

/// `value[0]` finds an integer key and looks through tags, an index past the
/// end panics instead of growing the sequence (or wrapping and clearing it at
/// `usize::MAX`), and writing a key into a null makes it a mapping.
#[test]
fn indexing_finds_integer_keys_and_does_not_grow_sequences() {
    let mapping = ayaml::parse_value("{0: zero, 1: one}\n").expect("parse");
    assert_eq!(mapping[0], ayaml::Value::from("zero"));
    assert_eq!(mapping.get(1), Some(&ayaml::Value::from("one")));
    let tagged = ayaml::parse_value("!t {a: [!u x]}\n").expect("parse");
    assert_eq!(tagged["a"][0].as_str(), Some("x"));
    assert!(tagged.is_mapping() && tagged["a"].is_sequence() && tagged.is_tagged());

    let mut null = ayaml::Value::Null;
    null["a"]["b"] = ayaml::Value::from(1_u64);
    assert_eq!(
        serde_json::to_value(&null).expect("json"),
        json!({"a": {"b": 1}})
    );
    let mut sequence = ayaml::parse_value("[1, 2]\n").expect("parse");
    sequence[1] = ayaml::Value::from(5_u64);
    assert_eq!(
        serde_json::to_value(&sequence).expect("json"),
        json!([1, 5])
    );
    for index in [2, usize::MAX >> 24, usize::MAX] {
        let result = std::panic::catch_unwind(|| {
            let mut sequence = ayaml::parse_value("[1, 2]\n").expect("parse");
            sequence[index] = ayaml::Value::Null;
        });
        assert!(result.is_err(), "index {index}");
    }
}

/// `as_f64` converts integers, so a `Value` integer equals the same float.
#[test]
fn integers_read_as_floats() {
    let five = ayaml::Value::from(5_u64);
    assert_eq!(five.as_f64(), Some(5.0));
    assert_eq!(five, 5.0_f64);
    assert_eq!(ayaml::Value::from(-3_i64).as_f64(), Some(-3.0));
    assert!(!five.is_f64());
    assert_ne!(ayaml::Value::from(5.0_f64), 5_u64);
}

/// Tags survive `to_value`, `from_value`, and serialization. A
/// tagged value is a one-entry map from `!` and the tag's Serde name to the
/// value, and ayaml's serializers turn that map back into
/// the tag. Core, `%TAG`, and verbatim tags keep their full names.
#[test]
fn tags_round_trip_through_serde() {
    for input in [
        "!Foo 1\n",
        "!Foo {a: 1}\n",
        "- !Foo [1]\n- x\n",
        "%TAG !e! tag:x,2000:\n--- !e!foo 1\n",
        "!<tag:x,2000:y> 1\n",
        "!!str 1\n",
        "{!k a: 1}\n",
    ] {
        let value = ayaml::parse_value(input).expect("parse");
        assert_eq!(
            ayaml::to_value(&value).expect("to_value"),
            value,
            "{input:?}"
        );
        assert_eq!(
            ayaml::from_value::<ayaml::Value>(value.clone()).expect("from_value"),
            value,
            "{input:?}"
        );
        let output = ayaml::to_string(&value).expect("to_string");
        assert_eq!(
            ayaml::parse_value(&output).expect("reparse"),
            value,
            "{input:?}"
        );
    }
    let value = ayaml::parse_value("!Foo {a: 1}\n").expect("parse");
    assert_eq!(
        serde_json::to_value(&value).expect("json"),
        json!({ "!Foo": { "a": 1 } })
    );
    let value = ayaml::parse_value("%TAG !e! tag:x,2000:\n--- !e!foo 1\n").expect("parse");
    assert_eq!(
        serde_json::to_value(&value).expect("json"),
        json!({ "!<tag:x,2000:foo>": 1 })
    );
    // A string key that only starts with `!` stays a string.
    let value = ayaml::parse_value("'!Foo': 1\n").expect("parse");
    assert!(ayaml::to_value(&value).expect("to_value").is_mapping());
    assert!(!ayaml::to_value(&value).expect("to_value").is_tagged());
    // Tagged enum variants still deserialize by their suffix.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    enum Shape {
        Circle(u32),
    }
    for result in every_path::<Shape>("!Circle 5\n") {
        assert_eq!(result, Ok(Shape::Circle(5)));
    }
}

/// Core collection tags restate the node kind, so every composer drops them;
/// `!!seq [1, 2]` reads into a `Vec`.
#[test]
fn core_collection_tags_are_dropped() {
    for (input, expected) in [
        ("!!seq [1, 2]\n", json!([1, 2])),
        ("!!map {a: 1}\n", json!({ "a": 1 })),
        ("!!set {a, b}\n", json!({ "a": null, "b": null })),
        ("!!omap [a: 1]\n", json!([{ "a": 1 }])),
        ("!!pairs [a: 1]\n", json!([{ "a": 1 }])),
        ("- !<tag:yaml.org,2002:map> {a: 1}\n", json!([{ "a": 1 }])),
    ] {
        assert_eq!(parse(input), expected, "{input:?}");
        let typed: Json = ayaml::from_str_borrowed(input).expect("typed");
        assert_eq!(typed, expected, "{input:?}");
    }
    for result in every_path::<Vec<u32>>("!!seq [1, 2]\n") {
        assert_eq!(result, Ok(vec![1, 2]));
    }
    // Other tags stay.
    assert!(ayaml::parse_value("!Foo [1]\n").expect("parse").is_tagged());
}

/// The typed path keeps tags, so `from_str_borrowed::<Value>`
/// and the multi-document cursor give the same values as `parse_value`.
#[test]
fn the_typed_path_keeps_tags() {
    for input in ["!Foo 1\n", "a: !Foo [1, !Bar 2]\n", "{!k a: 1}\n"] {
        assert_eq!(
            ayaml::from_str_borrowed::<ayaml::Value>(input).expect("typed"),
            ayaml::parse_value(input).expect("parse"),
            "{input:?}"
        );
    }
    let mut documents =
        ayaml::Deserializer::from_str_with_limits("--- !A 1\n--- !B 2\n", &stream_limits())
            .expect("stream");
    let first: ayaml::Value = documents.next_borrowed().expect("first").expect("document");
    assert_eq!(first, ayaml::parse_value("!A 1\n").expect("parse"));
}

/// Merging keeps the mapping's own keys in source order and appends the merged
/// keys. Chained merges flatten fully.
#[test]
fn merge_order_and_chains() {
    let base = "b: &b {y: 1, x: 1}\n";
    assert_eq!(
        merged_keys(&format!("{base}m:\n  z: 0\n  <<: *b\n  x: 2\n"), "m"),
        merged_keys("m: {z: 0, x: 2, y: 1}\n", "m")
    );
    assert_eq!(
        merged_keys(&format!("{base}m:\n  <<: *b\n  z: 0\n  w: 0\n"), "m"),
        merged_keys("m: {z: 0, w: 0, y: 1, x: 1}\n", "m")
    );
    assert_eq!(
        merged_keys(
            "a: &a {x: 1}\nb: &b {<<: *a, y: 2}\nc:\n  <<: *b\n  z: 3\n",
            "c"
        ),
        merged_keys("c: {z: 3, y: 2, x: 1}\n", "c")
    );
    assert_eq!(
        merged_keys(
            "a: &a {x: 1, k: a}\nb: &b {y: 2, k: b}\nc:\n  <<: [*a, *b]\n",
            "c"
        ),
        merged_keys("c: {x: 1, k: a, y: 2}\n", "c")
    );
}

/// Integers up to 128 bits are exact. A typed target reads them, and the value
/// model keeps them.
#[test]
fn wide_integers() {
    let above = "18446744073709551616\n";
    let below = "-9223372036854775809\n";
    for result in every_path::<u128>(above) {
        assert_eq!(result, Ok(u128::from(u64::MAX) + 1));
    }
    for result in every_path::<i128>(below) {
        assert_eq!(result, Ok(i128::from(i64::MIN) - 1));
    }
    for result in every_path::<f64>(above) {
        assert_eq!(result, Ok(18_446_744_073_709_551_616.0));
    }
    assert!(every_path::<u64>(above).iter().all(Result::is_err));
    // The typed path keeps the source text for a string target. The Value
    // path canonicalizes it.
    assert_eq!(
        ayaml::from_str_borrowed::<String>("0x10000000000000000\n").expect("typed"),
        "0x10000000000000000"
    );
    let value =
        ayaml::parse_value("[0x10000000000000000, -0o2000000000000000000001]\n").expect("parse");
    assert_eq!(value[0], ayaml::Value::from(1_u128 << 64));
    assert_eq!(value[1], ayaml::Value::from(-(1_i128 << 64) - 1));
    // Past 128 bits a decimal integer is a float and a radix integer is a
    // string.
    let value = ayaml::parse_value(
        "[340282366920938463463374607431768211456, 0x100000000000000000000000000000000]\n",
    )
    .expect("parse");
    assert!(value[0].as_number().is_some_and(ayaml::Number::is_f64));
    assert_eq!(value[1], "0x100000000000000000000000000000000");
}

/// 128-bit integers serialize as plain integers and read back.
#[test]
fn wide_integers_serialize() {
    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Wide {
        big: u128,
        small: i128,
    }
    let wide = Wide {
        big: u128::MAX,
        small: i128::MIN,
    };
    let text = ayaml::to_string(&wide).expect("emit");
    assert_eq!(
        text,
        "big: 340282366920938463463374607431768211455\nsmall: -170141183460469231731687303715884105728\n"
    );
    assert_eq!(ayaml::from_str::<Wide>(&text).expect("decode"), wide);
    let value = ayaml::to_value(&wide).expect("to_value");
    assert_eq!(value["big"], ayaml::Value::from(u128::MAX));
    assert_eq!(ayaml::from_value::<Wide>(value).expect("from_value"), wide);
    // A string that spells a wide integer stays a string.
    let text = ayaml::to_string(&"18446744073709551616").expect("emit");
    assert_eq!(text, "\"18446744073709551616\"\n");
}

/// Explicit core tags follow the YAML 1.2.2 core schema. `!!float 01` is 1.0,
/// `!!int 0123` is 123, and `!!null ''` is null.
#[test]
fn explicit_core_tags_follow_the_core_schema() {
    let value = ayaml::parse_value(
        "- !!float 01\n- !!float 1\n- !!int 0123\n- !!null ''\n- !!int 18446744073709551616\n",
    )
    .expect("parse");
    assert_eq!(value[0], 1.0);
    assert_eq!(value[1], 1.0);
    assert_eq!(value[2], 123_u64);
    assert!(value[3].is_null());
    assert_eq!(value[4], ayaml::Value::from(u128::from(u64::MAX) + 1));
    for input in [
        "!!float 0x1F\n",
        "!!int 1_000\n",
        "!!null x\n",
        "!!bool yes\n",
    ] {
        assert!(ayaml::parse_value(input).is_err(), "{input:?}");
    }
    // Untagged, a leading zero keeps the scalar a string.
    assert_eq!(ayaml::parse_value("01\n").expect("parse"), "01");
}
