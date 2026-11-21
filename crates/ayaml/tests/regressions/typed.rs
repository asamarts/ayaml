#![allow(
    clippy::items_after_statements,
    clippy::match_same_arms,
    reason = "test fixtures favor compact setup, generated inputs, and literal YAML braces"
)]

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
    let options = ayaml::compat::ParseOptions::new();
    let deep = format!("{}{}", "[".repeat(129), "]".repeat(129));
    let error = ayaml::compat::parse_events_eager_with(&deep, &options).unwrap_err();
    assert_eq!(error.kind(), ayaml::ErrorKind::Limit, "{error}");
    assert!(error.limit().is_some() && error.span().is_some(), "{error}");
    let error = ayaml::compat::from_str::<ayaml::Value>(&deep).unwrap_err();
    assert_eq!(error.kind(), ayaml::ErrorKind::Serde, "{error}");
    assert!(error.limit().is_none() && error.span().is_some(), "{error}");
    let many = "---\na\n".repeat(10_001);
    let error = ayaml::compat::from_str::<ayaml::Value>(&many).unwrap_err();
    assert_eq!(error.kind(), ayaml::ErrorKind::Parser, "{error}");
    assert!(error.limit().is_none() && error.span().is_some(), "{error}");
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

/// An empty source value reads as an empty sequence, map, or struct with its
/// defaults on the source-based readers. `from_value` has only a `Null`, which
/// it treats as an explicit null, so it rejects it. An explicit `~` is rejected
/// by every reader.
#[test]
fn empty_values_are_empty_collections() {
    let results = every_path::<Vec<u32>>("");
    assert_eq!(results[0], Ok(Vec::new()));
    assert_eq!(results[1], Ok(Vec::new()));
    assert!(results[2].is_err(), "{results:?}");
    let results = every_path::<std::collections::BTreeMap<String, u32>>("");
    assert_eq!(results[0], Ok(std::collections::BTreeMap::new()));
    assert_eq!(results[1], Ok(std::collections::BTreeMap::new()));
    assert!(results[2].is_err(), "{results:?}");
    let results = every_path::<Defaults>("");
    assert_eq!(results[0], Ok(Defaults::default()));
    assert_eq!(results[1], Ok(Defaults::default()));
    assert!(results[2].is_err(), "{results:?}");
    let results = every_path::<Defaults>("a: 1\nv:\nm:\n");
    assert_eq!(
        results[0],
        Ok(Defaults {
            a: 1,
            ..Defaults::default()
        })
    );
    assert_eq!(
        results[1],
        Ok(Defaults {
            a: 1,
            ..Defaults::default()
        })
    );
    assert!(results[2].is_err(), "{results:?}");
    let results = every_path::<Vec<u32>>("~\n");
    assert!(results.iter().all(Result::is_err), "{results:?}");
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
    for input in ["true\n", "1\n"] {
        for result in every_path::<Renamed>(input) {
            assert!(result.is_err(), "{input:?}: {result:?}");
        }
    }
    for (input, variant) in [("'true'\n", Renamed::Yes), ("Plain\n", Renamed::Plain)] {
        for result in every_path::<Renamed>(input) {
            assert_eq!(result.as_ref(), Ok(&variant), "{input:?}");
        }
    }
    assert!(ayaml::from_str_borrowed::<Renamed>("TRUE\n").is_err());
    for input in ["", "~\n"] {
        let results = every_path::<ayaml::Mapping>(input);
        if input.is_empty() {
            assert_eq!(results[0], Ok(ayaml::Mapping::new()));
            assert_eq!(results[1], Ok(ayaml::Mapping::new()));
            assert!(results[2].is_err(), "{results:?}");
        } else {
            assert!(results.iter().all(Result::is_err), "{results:?}");
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
        ayaml::Deserializer::from_str_with("--- !A 1\n--- !B 2\n", &stream_parse_options())
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

/// A drop-in compat reader shouldn't refuse forms that default mode and libyaml each read. These
/// are the small scanner/parser meetings AY-067 lists: flow `?` before `-` or a `!` tag, anchors
/// that end at `?` or `:`, a block scalar under an anchored `:` key, a NEL in a comment, a BOM marker,
/// and a quoted scalar below a BOM key.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_reads_what_default_mode_and_libyaml_read() {
    let compat = |input: &str| {
        ayaml::compat::from_str::<ayaml::Value>(input)
            .unwrap_or_else(|error| panic!("{input:?}: {error}"))
    };
    let default = |input: &str| {
        ayaml::parse_value(input).unwrap_or_else(|error| panic!("{input:?}: {error}"))
    };

    // A flow `?` before `-` is an explicit key whose key is the scalar `-`.
    assert_eq!(compat("[?-]"), default("[{'-': null}]"));
    assert_eq!(compat("{?-}"), default("{'-': null}"));
    // `!!!` is libyaml's tag `!!` with suffix `!`; it only has to be read, not constructed.
    assert!(compat("[?!!!,]").as_sequence().is_some());

    // A name that stops on `?` or `:` must not leave a token that fails. These read as default
    // mode reads the whole name.
    for input in ["[&a:a?]", "{&a:b?}", "&a\u{85}?"] {
        assert_eq!(compat(input), default(input), "{input:?}");
    }

    // A block scalar may sit at the parent's column under an anchored `:` key, as libyaml reads
    // it.
    assert_eq!(compat("&a::\n|"), default("{':': ''}"));
    assert_eq!(compat("&a::\n|\n x"), default("{':': 'x'}"));

    // NEL, LS, and PS are content in compat, but a `:` before one stays in the name, as default
    // mode reads it.
    for input in [
        "&a:\u{85}\n  ---",
        "&a:\u{85}\n  b: c",
        "&a:\u{85}\n  b",
        "&::\u{2028}",
        "&\":\u{85}",
    ] {
        assert_eq!(compat(input), default(input), "{input:?}");
    }

    // A NEL in a comment after a flow `?` is content, as it is in every other comment.
    assert!(ayaml::compat::from_str::<ayaml::Value>("{?# \u{85}}").is_err());
    assert_eq!(compat("[?#\n [1]]"), default("[{[1]: null}]"));

    // Outside a flow collection, a `:` before a flow indicator ends a name and becomes text.
    assert_eq!(compat("&r:}"), ayaml::Value::from(":}"));

    // A `...` that ends a line after a stream-start BOM is a document marker, as in default mode.
    assert_eq!(
        compat("\u{feff}...\n...\u{2028}"),
        ayaml::Value::from("...\u{2028}")
    );

    // A comment right after a flow `?` is a comment, as libyaml reads it. The second form's key
    // is spelled `:` rather than null only because the tag is an unknown core tag; the read is
    // accepted either way.
    assert_eq!(compat("{?#\n}"), default("{null: null}"));
    assert!(compat("[?#\n::]").as_sequence().is_some());

    // A mark that starts the key's line must not tighten a quoted scalar's own indent check.
    for input in ["# c\n\u{feff}k:\n  'a\n b'", "# c\n\u{feff}k:\n  ['a\n b']"] {
        assert_eq!(compat(input), default(input), "{input:?}");
    }
}

/// A core tag names a node kind. On a node of another kind both modes drop it, so values, typed
/// readers, and event streams read the node without the tag.
#[test]
fn core_tags_on_other_node_kinds_are_dropped() {
    let cases = [
        ("!!map [a]", "[a]"),
        ("!!seq a", "a"),
        ("!!str {}", "{}"),
        ("a: !!int [1]", "a: [1]"),
        ("a: !!str\n  b: 1", "a:\n  b: 1"),
        ("!!null []", "[]"),
        ("%TAG !e! tag:yaml.org,2002:\n--- !e!str {}\n", "{}\n"),
    ];
    for (input, expected_input) in cases {
        let expected = ayaml::parse_value(expected_input).expect("expected value");
        assert_eq!(
            ayaml::parse_value(input).expect("parse_value"),
            expected,
            "{input:?}"
        );
        for result in every_path::<ayaml::Value>(input) {
            assert_eq!(result.expect("typed path"), expected, "{input:?}");
        }
        let events: Vec<ayaml::Event<'_>> = ayaml::parse_events(input)
            .expect("events")
            .collect::<Result<_, _>>()
            .expect("events");
        for event in events {
            match event {
                ayaml::Event::SequenceStart { tag, .. }
                | ayaml::Event::MappingStart { tag, .. } => {
                    assert!(tag.is_none(), "{input:?}");
                }
                ayaml::Event::Scalar { tag, .. } => {
                    assert!(tag.is_none(), "{input:?}");
                }
                _ => {}
            }
        }

        #[cfg(feature = "libyaml-compat")]
        {
            assert_eq!(
                ayaml::compat::from_str::<ayaml::Value>(input).expect("compat"),
                expected,
                "{input:?}"
            );
            let events =
                ayaml::compat::parse_events_eager_with(input, &ayaml::compat::ParseOptions::new())
                    .expect("compat events");
            for event in events {
                match event {
                    ayaml::Event::SequenceStart { tag, .. }
                    | ayaml::Event::MappingStart { tag, .. } => {
                        assert!(tag.is_none(), "compat {input:?}");
                    }
                    ayaml::Event::Scalar { tag, .. } => {
                        assert!(tag.is_none(), "compat {input:?}");
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Typed decoding checks the shape of the node, not only whether the target's visitor can consume
/// it: sequences do not fill structs, a bare name does not fill a data-carrying variant, null is
/// not an empty collection, and `from_value` does not stringify a non-string value.
#[test]
fn typed_shapes_and_nulls_are_checked_on_every_reader() {
    use ayaml::Value;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Pair {
        a: u32,
        b: u32,
    }
    #[derive(Debug, Deserialize, PartialEq)]
    struct Point {
        x: u32,
    }
    #[derive(Debug, Deserialize, PartialEq)]
    enum Data {
        Data(u32),
        Unit,
    }
    #[derive(Debug, Deserialize, PartialEq)]
    enum Shape {
        Point(Point),
        Unit,
    }
    #[derive(Debug, Deserialize, PartialEq)]
    enum Named {
        #[serde(rename = "true")]
        True,
        #[serde(rename = "1")]
        One,
    }

    for input in ["- 1\n- 2\n", "[1, 2]\n"] {
        assert!(
            every_path::<Pair>(input).iter().all(Result::is_err),
            "{input:?}"
        );
    }
    for input in ["Data\n", "Data: ~\n", "Point: ~\n"] {
        assert!(
            every_path::<Data>(input).iter().all(Result::is_err),
            "{input:?}"
        );
    }
    for input in ["~", "!list ~"] {
        assert!(
            every_path::<Vec<u32>>(input).iter().all(Result::is_err),
            "{input:?}"
        );
        assert!(
            every_path::<Point>(input).iter().all(Result::is_err),
            "{input:?}"
        );
    }
    assert!(every_path::<Shape>("Point: ~\n").iter().all(Result::is_err));
    for input in ["~", "!list ~"] {
        assert!(
            every_path::<std::collections::BTreeMap<String, u32>>(input)
                .iter()
                .all(Result::is_err),
            "{input:?}"
        );
    }

    assert_eq!(
        ayaml::from_value::<String>(Value::from("text")).unwrap(),
        "text"
    );
    assert_eq!(
        ayaml::from_value::<String>(ayaml::parse_value("!x text\n").unwrap()).unwrap(),
        "text"
    );
    for value in [
        Value::from(5_u64),
        Value::from(true),
        Value::Null,
        ayaml::parse_value("!x true\n").unwrap(),
    ] {
        assert!(
            ayaml::from_value::<String>(value.clone()).is_err(),
            "{value:?}"
        );
    }

    for input in ["true\n", "1\n"] {
        assert!(
            every_path::<Named>(input).iter().all(Result::is_err),
            "{input:?}"
        );
    }
    for (input, expected) in [("'true'\n", Named::True), ("'1'\n", Named::One)] {
        for result in every_path::<Named>(input) {
            assert_eq!(result.as_ref(), Ok(&expected), "{input:?}");
        }
    }

    #[cfg(feature = "libyaml-compat")]
    {
        assert!(ayaml::compat::from_str::<Pair>("- 1\n- 2\n").is_err());
        assert!(ayaml::compat::from_str::<Data>("Data\n").is_err());
        assert!(ayaml::compat::from_str::<Vec<u32>>("~").is_err());
        assert!(ayaml::compat::from_str::<Point>("!list ~").is_err());
        assert!(ayaml::compat::from_str::<Shape>("Point: ~\n").is_err());
        assert!(ayaml::compat::from_str::<Named>("true\n").is_err());
        assert_eq!(
            ayaml::compat::from_str::<Named>("'true'\n").unwrap(),
            Named::True
        );
    }

    for prefix in ["tag:x:", "foo", "tag:x", "foo/"] {
        let source = format!("%TAG !e! {prefix}\n--- !e!a 1\n");
        let composed = ayaml::parse_value(&source).unwrap();
        let values = vec![
            composed.clone(),
            ayaml::from_str::<Value>(&source).unwrap(),
            ayaml::from_str_borrowed::<Value>(&source).unwrap(),
            ayaml::from_value::<Value>(composed).unwrap(),
            ayaml::Deserializer::from_str(&source)
                .unwrap()
                .next::<Value>()
                .unwrap()
                .unwrap(),
            ayaml::Deserializer::from_str(&source)
                .unwrap()
                .next_borrowed::<Value>()
                .unwrap()
                .unwrap(),
        ];
        #[cfg(feature = "libyaml-compat")]
        let values = {
            let mut values = values;
            values.push(ayaml::compat::from_str::<Value>(&source).unwrap());
            values
        };
        let mut parts = Vec::new();
        for value in values {
            let Value::Tagged(tagged) = value else {
                panic!("expected a tagged value");
            };
            assert_eq!(
                format!("{}{}", tagged.tag.handle(), tagged.tag.suffix()),
                format!("{prefix}a")
            );
            parts.push((
                tagged.tag.handle().to_owned(),
                tagged.tag.suffix().to_owned(),
            ));
        }
        assert!(
            parts.windows(2).all(|pair| pair[0] == pair[1]),
            "{prefix:?}: {parts:?}"
        );
    }

    for input in ["~", "!list ~"] {
        assert!(
            ayaml::Deserializer::from_str(input)
                .unwrap()
                .next::<Vec<u32>>()
                .is_err(),
            "{input:?}"
        );
    }
    assert!(
        ayaml::Deserializer::from_str("Point: ~\n")
            .unwrap()
            .next::<Shape>()
            .is_err()
    );
}

/// The typed reader builds scalar nodes without materializing a `Value` for strings; every scalar
/// form, tag, and style still reads as the composer reads it.
#[test]
fn typed_scalars_read_like_composed_values() {
    for input in [
        "text\n",
        "'text'\n",
        "\"a\\tb\"\n",
        "~",
        "true\n",
        "1\n",
        "1.5\n",
        "! 1\n",
        "!text\n",
        "!x text\n",
        "!x ~\n",
        "!x true\n",
        "!x 1\n",
        "!x 1.5\n",
        "!!str 1\n",
        "!!int 1\n",
        "!!seq 1\n",
    ] {
        let expected =
            ayaml::parse_value(input).unwrap_or_else(|error| panic!("{input:?}: {error}"));
        assert_eq!(
            ayaml::from_str::<ayaml::Value>(input)
                .unwrap_or_else(|error| panic!("{input:?}: {error}")),
            expected,
            "{input:?}"
        );
        assert_eq!(
            ayaml::from_str_borrowed::<ayaml::Value>(input)
                .unwrap_or_else(|error| panic!("{input:?}: {error}")),
            expected,
            "{input:?}"
        );
        #[cfg(feature = "libyaml-compat")]
        assert_eq!(
            ayaml::compat::from_str::<ayaml::Value>(input)
                .unwrap_or_else(|error| panic!("{input:?}: {error}")),
            expected,
            "{input:?}"
        );
    }
}

/// What each reader says of an input, read into a map of `T`.
type Readings<T> = Vec<(
    &'static str,
    Result<std::collections::BTreeMap<String, T>, ayaml::Error>,
)>;

/// The readers of one document from text that build as they parse (D91), each into a map of `T`.
fn document_readers<T: serde::de::DeserializeOwned>(input: &str) -> Readings<T> {
    #[cfg_attr(
        not(feature = "libyaml-compat"),
        allow(unused_mut, reason = "the list is extended only by the compat readers")
    )]
    let mut readers = vec![
        ("from_str", ayaml::from_str(input)),
        ("from_slice", ayaml::from_slice(input.as_bytes())),
    ];
    #[cfg(feature = "libyaml-compat")]
    readers.extend([
        ("compat::from_str", ayaml::compat::from_str(input)),
        (
            "compat::from_str_seed",
            ayaml::compat::from_str_seed(input, std::marker::PhantomData),
        ),
    ]);
    readers
}

/// Every reader that decodes from text, each into a map of `T`: the readers of one document, and
/// the cursor over a stream, which builds a document from its events once it has them all.
fn source_readers<T: serde::de::DeserializeOwned>(input: &str) -> Readings<T> {
    let mut readers = document_readers(input);
    readers.extend([
        (
            "Deserializer::next_borrowed",
            ayaml::Deserializer::from_str(input)
                .and_then(|mut cursor| cursor.next_borrowed())
                .map(|document| document.expect("a document")),
        ),
        (
            "Deserializer::next_seed_borrowed",
            ayaml::Deserializer::from_str(input)
                .and_then(|mut cursor| cursor.next_seed_borrowed(std::marker::PhantomData))
                .map(|document| document.expect("a document")),
        ),
    ]);
    readers
}

/// A scalar that a core tag makes a number, a boolean, or a null reads into a string target by
/// its text, whatever its style, as a plain one does. Its text is what the source spells: a
/// hexadecimal integer stays hexadecimal, and a float keeps its trailing zero. Where only an
/// escape or an escaped line break spells the value, the text is the decoded value.
#[test]
fn a_scalar_a_core_tag_retypes_reads_into_a_string_by_its_text() {
    for (input, text) in [
        ("x: !!int 12\n", "12"),
        ("x: !!int \"12\"\n", "12"),
        ("x: !!int '12'\n", "12"),
        ("x: !!int |-\n  12\n", "12"),
        ("x: !!int >-\n  12\n", "12"),
        ("x: !!int \"0x1F\"\n", "0x1F"),
        ("x: !!float \"1.50\"\n", "1.50"),
        ("x: !!bool 'True'\n", "True"),
        ("x: !!null \"~\"\n", "~"),
        ("x: !!null 'null'\n", "null"),
        ("x: !!int \"1\\x32\"\n", "12"),
        ("x: !!int \"\\x31\"\n", "1"),
        ("x: !!int \"1\\\n  2\"\n", "12"),
        ("x: !!float \"1.\\x35\"\n", "1.5"),
        ("x: !!bool \"tr\\x75e\"\n", "true"),
        ("x: !!null \"\\x7e\"\n", "~"),
    ] {
        for (reader, result) in source_readers::<String>(input) {
            let read = result.unwrap_or_else(|error| panic!("{reader} {input:?}: {error}"));
            assert_eq!(read["x"], text, "{reader} {input:?}");
        }
    }
    // The tag still decides the type that a target of that type reads.
    for input in ["x: !!int \"0x1F\"\n", "x: !!int \"\\x331\"\n"] {
        for (reader, result) in source_readers::<u32>(input) {
            assert_eq!(result.expect(reader)["x"], 31, "{reader} {input:?}");
        }
    }
}

/// A target that borrows reads that text from the source where the source holds it: a plain
/// null, boolean, or number, and a quoted one under a core tag. Where only decoding gives the
/// text, there is nothing in the source to borrow, and an owned target still reads it.
#[test]
fn a_scalar_that_is_not_a_string_lends_its_text_from_the_source() {
    let read = ayaml::from_str_borrowed::<Vec<&str>>("[1, true, ~, 1.50, 0x1F]\n").expect("texts");
    assert_eq!(read, ["1", "true", "~", "1.50", "0x1F"]);
    let read = ayaml::from_str_borrowed::<Vec<&str>>("[!!int \"12\", !!bool 'True']\n");
    assert_eq!(read.expect("texts"), ["12", "True"]);
    let escaped = "[!!int \"1\\x32\"]\n";
    let error = ayaml::from_str_borrowed::<Vec<&str>>(escaped).expect_err("no text to lend");
    assert_eq!(error.kind(), ayaml::ErrorKind::Serde, "{error}");
    assert_eq!(
        ayaml::from_str::<Vec<String>>(escaped).expect("a text"),
        ["12"]
    );
}

/// A float target reads a number by what it is, and from a plain scalar's text besides, so
/// that an integer too wide for 64 bits and a spelling with a leading zero read. A quoted
/// scalar under `!!int` is a number with no plain text: it reads as the integer it is. A plain
/// radix integer is refused, since its text is no float's.
#[test]
fn a_float_target_reads_a_tagged_integer_by_its_style() {
    for (input, expected) in [
        ("x: !!int \"0x1F\"\n", Some(31.0)),
        ("x: !!int '12'\n", Some(12.0)),
        ("x: !!int 12\n", Some(12.0)),
        ("x: !!int 0x1F\n", None),
        ("x: 0x1F\n", None),
    ] {
        for (reader, result) in source_readers::<f64>(input) {
            if let Some(expected) = expected {
                let read = result.unwrap_or_else(|error| panic!("{reader} {input:?}: {error}"));
                assert!(
                    (read["x"] - expected).abs() < f64::EPSILON,
                    "{reader} {input:?}"
                );
            } else {
                let error = result.expect_err(reader);
                assert_eq!(error.kind(), ayaml::ErrorKind::Serde, "{reader} {input:?}");
            }
        }
    }
}

/// A custom tag doesn't make a scalar a string; its text does. So a plain spelling with a
/// leading zero under a custom tag reads into a float as it does with no tag, which 0.1.3 did
/// and a later change lost. A tag that makes it a string still does.
#[test]
fn a_custom_tag_keeps_the_text_a_float_target_reads() {
    for input in [
        "x: 080\n",
        "x: !port 080\n",
        "x: !port 80\n",
        "x: !port 80.0\n",
    ] {
        for (reader, result) in source_readers::<f64>(input) {
            let read = result.unwrap_or_else(|error| panic!("{reader} {input:?}: {error}"));
            assert!(
                (read["x"] - 80.0).abs() < f64::EPSILON,
                "{reader} {input:?}"
            );
        }
    }
    for input in [
        "x: !!str 080\n",
        "x: ! 080\n",
        "x: !port '080'\n",
        "x: !port abc\n",
    ] {
        for (reader, result) in source_readers::<f64>(input) {
            let error = result.expect_err(reader);
            assert_eq!(error.kind(), ayaml::ErrorKind::Serde, "{reader} {input:?}");
        }
    }
}

/// An alias is charged for what a target may copy of its node. A string target copies a
/// number's text, so an alias to a number that is spelled at length is charged for that text, as
/// an alias to a string of the same length is, whatever style or tag spells the number.
#[test]
fn an_alias_to_a_number_is_charged_for_its_text() {
    use ayaml::{LimitExceeded, LimitKind, Limits};

    const ALIASES: usize = 400;
    let zeros = "0".repeat(64 << 10);
    // Without the charge, the aliases would copy 25 MiB under a budget of one.
    let options = ayaml::ParseOptions::new()
        .with_limits(Limits::default().with_max_alias_expansion_bytes(1 << 20));
    for number in [
        format!("1.{zeros}"),
        format!("!!int {zeros}1"),
        format!("!!int \"{zeros}1\""),
        format!("!!int \"{zeros}\\x31\""),
        format!("x{zeros}"),
    ] {
        let input = format!("- &a {number}\n{}", "- *a\n".repeat(ALIASES));
        let cursor = ayaml::Deserializer::from_str_with(&input, &options)
            .and_then(|mut cursor| cursor.next_borrowed::<Vec<String>>().map(drop));
        let readers = [
            (
                "from_str_with",
                ayaml::from_str_with::<Vec<String>>(&input, &options).map(drop),
            ),
            (
                "from_str_with into Value",
                ayaml::from_str_with::<Vec<ayaml::Value>>(&input, &options).map(drop),
            ),
            ("Deserializer::next_borrowed", cursor),
        ];
        for (reader, result) in readers {
            let error = result.expect_err(reader);
            assert_eq!(
                error.limit().map(LimitExceeded::kind),
                Some(LimitKind::AliasExpansion),
                "{reader}, a number of {} bytes: {error}",
                number.len()
            );
        }
    }
    // A number of ordinary length costs a node, so as many aliases to one are read.
    let input = format!("- &a 1.5\n{}", "- *a\n".repeat(ALIASES));
    let read = ayaml::from_str_with::<Vec<String>>(&input, &options);
    assert_eq!(read.expect("aliases to a short number").len(), ALIASES + 1);
}

/// A key names a field by its text, never by its number: an integer key is not the field at that
/// index, whatever style and escapes spell it.
#[test]
fn a_number_key_names_a_field_by_its_text() {
    #[derive(Debug, serde::Deserialize)]
    struct Fields {
        a: u32,
        #[serde(default)]
        b: u32,
    }
    let read = |input: &str| -> Vec<(&'static str, Result<Fields, ayaml::Error>)> {
        vec![
            ("from_str", ayaml::from_str(input)),
            (
                "Deserializer::next_borrowed",
                ayaml::Deserializer::from_str(input)
                    .and_then(|mut cursor| cursor.next_borrowed())
                    .map(|document| document.expect("a document")),
            ),
        ]
    };
    for input in [
        "0: 7\n",
        "!!int \"0\": 7\n",
        "!!int \"0\\x30\": 7\n",
        "!!int '0': 7\n",
    ] {
        for (reader, result) in read(input) {
            let error = result.expect_err(reader);
            assert!(
                error.to_string().contains("missing field `a`"),
                "{reader} {input:?}: {error}"
            );
        }
    }
    // The key after `a` is no field's name, so it is passed over and `b` keeps its default.
    for input in ["a: 1\n1: 7\n", "a: 1\n!!int \"\\x2b1\": 7\n"] {
        for (reader, result) in read(input) {
            let fields = result.unwrap_or_else(|error| panic!("{reader} {input:?}: {error}"));
            assert_eq!((fields.a, fields.b), (1, 0), "{reader} {input:?}");
        }
    }
}

/// An explicit null is not an empty collection in any style, tagged or not (D87). A scalar with
/// no text at all, quoted or block, is a missing value, as an empty plain one is.
#[test]
fn a_quoted_null_under_a_tag_is_an_explicit_null() {
    use std::collections::BTreeMap;

    #[derive(Debug, Default, serde::Deserialize)]
    #[serde(default)]
    struct Defaults {
        a: u32,
    }
    #[derive(Debug, PartialEq, serde::Deserialize)]
    enum Shape {
        Point { x: u32 },
    }
    assert_eq!(
        ayaml::from_str::<Shape>("Point:\n  x: 1\n").ok(),
        Some(Shape::Point { x: 1 })
    );
    for input in [
        "x: !!null \"~\"\n",
        "x: !!null 'null'\n",
        "x: !!null \"\\x7e\"\n",
        "x: !!null ~\n",
        "x: ~\n",
    ] {
        for (reader, result) in source_readers::<Vec<i32>>(input) {
            let error = result.expect_err(reader);
            assert!(
                error.to_string().contains("expected a sequence"),
                "{reader} {input:?}: {error}"
            );
        }
        for (reader, result) in source_readers::<BTreeMap<String, i32>>(input) {
            let error = result.expect_err(reader);
            assert!(
                error.to_string().contains("expected a map"),
                "{reader} {input:?}: {error}"
            );
        }
        for (reader, result) in source_readers::<Defaults>(input) {
            let error = result.expect_err(reader);
            assert!(
                error.to_string().contains("expected struct Defaults"),
                "{reader} {input:?}: {error}"
            );
        }
        for (reader, result) in source_readers::<Option<i32>>(input) {
            assert_eq!(result.expect(reader)["x"], None, "{reader} {input:?}");
        }
        // A struct variant, named by the entry's key.
        let variant = input.replacen("x:", "Point:", 1);
        let error = ayaml::from_str::<Shape>(&variant).expect_err(&variant);
        assert_eq!(
            error.kind(),
            ayaml::ErrorKind::Serde,
            "{variant:?}: {error}"
        );
    }
    for input in [
        "x:\n",
        "x: !!null\n",
        "x: !!null \"\"\n",
        "x: !!null ''\n",
        "x: !!null |-\n",
        "x: !!null \"\\\n\"\n",
    ] {
        for (reader, result) in source_readers::<Vec<i32>>(input) {
            let read = result.unwrap_or_else(|error| panic!("{reader} {input:?}: {error}"));
            assert!(read["x"].is_empty(), "{reader} {input:?}");
        }
        for (reader, result) in source_readers::<BTreeMap<String, i32>>(input) {
            let read = result.unwrap_or_else(|error| panic!("{reader} {input:?}: {error}"));
            assert!(read["x"].is_empty(), "{reader} {input:?}");
        }
        for (reader, result) in source_readers::<Defaults>(input) {
            let read = result.unwrap_or_else(|error| panic!("{reader} {input:?}: {error}"));
            assert_eq!(read["x"].a, 0, "{reader} {input:?}");
        }
    }
}

/// A bare variant name holds nothing, so it fills a unit variant and an optional one, and it is
/// not an empty collection for a variant that holds one (D87). A tag with no value, and a key
/// with no value, are a value the source doesn't write, which still reads as an empty one.
#[test]
fn a_bare_variant_name_does_not_fill_a_collection() {
    use std::collections::BTreeMap;

    #[derive(Debug, PartialEq, serde::Deserialize)]
    enum Holds {
        List(Vec<u32>),
        Map(BTreeMap<String, u32>),
        Opt(Option<u32>),
        Data(u32),
        Text(String),
        Unit,
    }
    for input in ["List\n", "Map\n", "Data\n", "Text\n", "\"List\"\n"] {
        for (path, result) in ["from_str", "from_str_borrowed", "from_value"]
            .into_iter()
            .zip(every_path::<Holds>(input))
        {
            assert!(result.is_err(), "{path} {input:?}: {result:?}");
        }
    }
    for (input, variant) in [("Opt\n", Holds::Opt(None)), ("Unit\n", Holds::Unit)] {
        for (path, result) in ["from_str", "from_str_borrowed", "from_value"]
            .into_iter()
            .zip(every_path::<Holds>(input))
        {
            assert_eq!(result.as_ref(), Ok(&variant), "{path} {input:?}");
        }
    }
    for (input, variant) in [
        ("List: []\n", Holds::List(Vec::new())),
        ("!List []\n", Holds::List(Vec::new())),
        ("List:\n", Holds::List(Vec::new())),
        ("!List\n", Holds::List(Vec::new())),
        ("Map: {}\n", Holds::Map(BTreeMap::new())),
    ] {
        assert_eq!(
            ayaml::from_str::<Holds>(input).as_ref().ok(),
            Some(&variant),
            "{input:?}"
        );
    }
}

/// Comment lines to at least `bytes` bytes: the lines, and how many.
fn comments(bytes: usize) -> (String, usize) {
    const LINE: &str = "# a comment line that holds no token\n";
    let lines = bytes.div_ceil(LINE.len());
    (LINE.repeat(lines), lines)
}

/// `text` with every number after `line ` lowered by `lines`.
fn lines_lowered(text: &str, lines: usize) -> String {
    let mut lowered = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("line ") {
        let (before, after) = rest.split_at(at + "line ".len());
        lowered.push_str(before);
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if let Ok(number) = after[..digits].parse::<usize>() {
            lowered.push_str(&(number - lines).to_string());
        }
        rest = &after[digits..];
    }
    lowered.push_str(rest);
    lowered
}

/// A reader of one document takes one of three paths by the input's length: at most 4 KiB, at
/// most 8 KiB, and above. With two problems in a document, each path reports the one that comes
/// first, after any that the scanner finds, as `parse_value` does. Comment lines before the
/// document move the place of the problem and change nothing else, in the message either.
#[test]
fn a_typed_reader_reports_the_first_problem_at_every_input_size() {
    use ayaml::ErrorKind;

    // Each input, and the kind of its first problem for a reader that refuses a repeated key.
    let cases = [
        // A duplicate key, then a sequence that never closes, which the parser finds.
        ("a: 1\na: 2\nb: [\n", ErrorKind::DuplicateKey),
        // A duplicate key, then a quote that never closes. The scanner reads the whole input
        // before anything is parsed, so what it finds comes first.
        ("a: 1\na: 2\nb: \"x\n", ErrorKind::Scanner),
        // An alias to no anchor, then a mapping that never closes.
        ("a: *nowhere\nb: {\n", ErrorKind::Composition),
        // A tag that its scalar can't take, then a duplicate key: both the builder's.
        ("a: !!int x\nb: 1\nb: 2\n", ErrorKind::Resolution),
        // One problem alone, of each kind.
        ("a: 1\na: 2\n", ErrorKind::DuplicateKey),
        ("a: [\n", ErrorKind::Parser),
        ("a: *nowhere\n", ErrorKind::Composition),
    ];
    for (input, kind) in cases {
        let value = ayaml::parse_value(input).expect_err(input);
        assert_eq!(value.kind(), kind, "{input:?}: parse_value says {value}");
        // What each reader says of the input alone. Compat reads a repeated key as libyaml
        // does, so there it is no problem; the others say what `parse_value` says, word for
        // word.
        let alone = document_readers::<ayaml::Value>(input);
        for (reader, result) in &alone {
            if !reader.starts_with("compat") {
                let error = result.as_ref().expect_err(reader);
                assert_eq!(error.to_string(), value.to_string(), "{reader} {input:?}");
            }
        }
        for bytes in [4_200, 8_300] {
            let (before, lines) = comments(bytes);
            let longer = format!("{before}{input}");
            for ((reader, result), (_, alone)) in document_readers::<ayaml::Value>(&longer)
                .into_iter()
                .zip(&alone)
            {
                let (error, alone) = match (result, alone) {
                    (Ok(read), Ok(alone)) => {
                        assert_eq!(&read, alone, "{reader} {input:?} after {bytes}");
                        continue;
                    }
                    (Err(error), Err(alone)) => (error, alone),
                    (result, alone) => {
                        panic!("{reader} {input:?} after {bytes}: {result:?}, alone {alone:?}")
                    }
                };
                assert_eq!(
                    error.kind(),
                    alone.kind(),
                    "{reader} {input:?} after {bytes}: {error}"
                );
                assert_eq!(
                    lines_lowered(&error.to_string(), lines),
                    alone.to_string(),
                    "{reader} {input:?} after {bytes}"
                );
                assert_eq!(
                    error.span().map(ayaml::Span::start),
                    alone.span().map(|span| span.start() + before.len()),
                    "{reader} {input:?} after {bytes}: {error}"
                );
            }
        }
    }
    // The cursor over a stream reads a document's events before it builds from them, at every
    // size, so within a document it reports a syntax error before an earlier duplicate key.
    for bytes in [0, 4_200, 8_300] {
        let (before, _) = comments(bytes);
        let input = format!("{before}a: 1\na: 2\nb: [\n");
        let error = ayaml::Deserializer::from_str(&input)
            .and_then(|mut cursor| cursor.next_borrowed::<ayaml::Value>())
            .expect_err("a syntax error");
        assert_eq!(error.kind(), ErrorKind::Parser, "after {bytes}: {error}");
    }
}

/// The default reader takes a seed, with default or explicit limits, borrowing from the input
/// and keeping default duplicate-key, trailing-document, and tag rules across small and large
/// documents.
#[test]
fn default_from_str_seed_reads_and_enforces_limits() {
    use std::marker::PhantomData;

    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct Borrowed<'a> {
        #[serde(borrow)]
        name: &'a str,
        count: u32,
    }

    let read = ayaml::from_str_seed("name: ayaml\ncount: 3\n", PhantomData::<Borrowed<'_>>)
        .expect("seed read");
    assert_eq!(
        read,
        Borrowed {
            name: "ayaml",
            count: 3
        }
    );
    let duplicate = ayaml::from_str_seed("a: 1\na: 2\n", PhantomData::<ayaml::Value>)
        .expect_err("duplicate key");
    assert_eq!(duplicate.kind(), ayaml::ErrorKind::DuplicateKey);
    let trailing = ayaml::from_str_seed("a: 1\n---\nb: 2\n", PhantomData::<ayaml::Value>)
        .expect_err("trailing doc");
    assert_eq!(trailing.kind(), ayaml::ErrorKind::Parser);
    let tight = ayaml::ParseOptions::new().with_limits(ayaml::Limits::new().with_max_events(1));
    let limited = ayaml::from_str_seed_with("a: 1\n", &tight, PhantomData::<ayaml::Value>)
        .expect_err("limit");
    assert_eq!(limited.kind(), ayaml::ErrorKind::Limit);
    // Global tags remain tagged on the default seed reader.
    let tagged = ayaml::from_str_seed("!!timestamp 2026-01-01\n", PhantomData::<ayaml::Value>)
        .expect("tagged value");
    assert!(tagged.is_tagged(), "{tagged:?}");
    // Large (> 4 KiB) inputs read the same through events.
    let (prefix, _) = comments(4_200);
    let large = format!("{prefix}name: ayaml\ncount: 7\n");
    let read = ayaml::from_str_seed(&large, PhantomData::<Borrowed<'_>>).expect("large seed read");
    assert_eq!(
        read,
        Borrowed {
            name: "ayaml",
            count: 7
        }
    );
}

/// Under `compat::GlobalTags::Drop`, non-core global tags drop as a libyaml-based reader drops
/// them: a scalar beneath one reads as text for `deserialize_any` and parses as the requested
/// type for a typed target, a collection beneath one reads as untagged, and local/core/`!` tags
/// keep their usual reading. Both small (tree) and > 4 KiB (event) reads agree, and the default
/// readers are unchanged.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_global_tags_drop_matches_libyaml_readings() {
    use std::collections::BTreeMap;
    use std::marker::PhantomData;

    use ayaml::compat::{GlobalTags, ParseOptions};

    const DROP: ParseOptions = ParseOptions::new().with_global_tags(GlobalTags::Drop);
    assert_eq!(ParseOptions::default(), ParseOptions::new());
    assert_eq!(ParseOptions::default().global_tags, GlobalTags::Keep);
    assert_eq!(DROP.global_tags, GlobalTags::Drop);

    // Issue #191 documents.
    let json_val: Json = ayaml::compat::from_str_with("a: !!timestamp 2026-01-01\n", &DROP)
        .expect("timestamp as json");
    assert_eq!(json_val, json!({"a": "2026-01-01"}));
    assert!(ayaml::compat::from_str::<Json>("a: !!timestamp 2026-01-01\n").is_err());
    assert!(ayaml::from_str::<Json>("a: !!timestamp 2026-01-01\n").is_err());

    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct Flattened {
        title: String,
        #[serde(flatten)]
        extra: BTreeMap<String, Json>,
    }
    let flattened: Flattened =
        ayaml::compat::from_str_with("title: t\ncreated: !!timestamp 2024-01-15\n", &DROP)
            .expect("flattened");
    assert_eq!(flattened.title, "t");
    assert_eq!(flattened.extra["created"], json!("2024-01-15"));

    assert_eq!(
        ayaml::compat::from_str_with::<u32>("!!timestamp 5\n", &DROP).expect("u32"),
        5
    );
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct Single {
        a: u32,
    }
    assert_eq!(
        ayaml::compat::from_str_with::<Single>("!!foo {a: 1}\n", &DROP).expect("struct"),
        Single { a: 1 }
    );
    assert_eq!(
        ayaml::compat::from_str_with::<Json>("a: !!binary [1]\n", &DROP).expect("binary seq"),
        json!({"a": [1]})
    );

    // Every global tag form on scalars and collections for deserialize_any and typed targets.
    for (input, expected) in [
        ("a: !!timestamp 2026-01-01\n", json!({"a": "2026-01-01"})),
        ("a: !!binary aGVsbG8=\n", json!({"a": "aGVsbG8="})),
        ("a: !!timestamp 5\n", json!({"a": "5"})),
        ("a: !!timestamp true\n", json!({"a": "true"})),
        ("a: !!timestamp ~\n", json!({"a": "~"})),
        ("a: !<tag:example.com,2000:x> 0x1F\n", json!({"a": "0x1F"})),
        (
            "%TAG !e! tag:example.com,2000:\n---\na: !e!x 1\n",
            json!({"a": "1"}),
        ),
        (
            "%TAG ! tag:example.com,2000:\n---\na: !x hello\n",
            json!({"a": "hello"}),
        ),
        ("a: !!timestamp {b: 1}\n", json!({"a": {"b": 1}})),
        (
            "a: !<tag:example.com,2000:l> [1, 2]\n",
            json!({"a": [1, 2]}),
        ),
        ("a: &x !!timestamp 5\nb: *x\n", json!({"a": "5", "b": "5"})),
        (
            "a: &x !!binary [1, 2]\nb: *x\n",
            json!({"a": [1, 2], "b": [1, 2]}),
        ),
    ] {
        let via_from: Json = ayaml::compat::from_str_with(input, &DROP)
            .unwrap_or_else(|error| panic!("{input:?}: {error}"));
        let via_seed: Json = ayaml::compat::from_str_seed_with(input, &DROP, PhantomData)
            .unwrap_or_else(|error| panic!("seed {input:?}: {error}"));
        assert_eq!(via_from, expected, "{input:?}");
        assert_eq!(via_seed, expected, "seed {input:?}");
    }
}

/// Typed targets, enums, options, limits, local tags, `!`, and core tags under
/// `compat::GlobalTags::Drop` across small (tree) and > 4 KiB (events) inputs.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_global_tags_drop_handles_typed_targets_and_keeps_local_tags() {
    use std::marker::PhantomData;

    use ayaml::compat::{GlobalTags, ParseOptions};

    const DROP: ParseOptions = ParseOptions::new().with_global_tags(GlobalTags::Drop);

    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct Single {
        a: u32,
    }
    #[derive(Debug, PartialEq, serde::Deserialize)]
    #[serde(rename_all = "lowercase")]
    enum Status {
        Todo,
        Done,
    }
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct Unit;
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct Count(u32);
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct AllTypes<'a> {
        b: bool,
        i: i128,
        u: u128,
        f: f64,
        c: char,
        #[serde(borrow)]
        s: &'a str,
        opt_num: Option<u32>,
        opt_tilde: Option<String>,
        wrapped: Count,
        pair: (u8, String),
        items: Vec<u32>,
        single: Single,
        status_bare: Status,
        status_map: Status,
        status_key: Status,
    }
    let (prefix, _) = comments(4_200);
    for (tag, directive) in [
        ("!!timestamp ", ""),
        ("!<tag:example.com,2000:x> ", ""),
        ("!e!x ", "%TAG !e! tag:example.com,2000:\n---\n"),
    ] {
        let body = format!(
            "{directive}\
             b: {tag}true\n\
             i: {tag}-5\n\
             u: {tag}0x1F\n\
             f: {tag}1.5\n\
             c: {tag}x\n\
             s: {tag}5\n\
             opt_num: {tag}5\n\
             opt_tilde: {tag}~\n\
             wrapped: {tag}7\n\
             pair: {tag}[1, ok]\n\
             items: {tag}[1, 2]\n\
             single: {tag}{{a: 9}}\n\
             status_bare: {tag}done\n\
             status_map: {tag}{{todo: ~}}\n\
             status_key: {{{tag}done: ~}}\n"
        );
        for doc in [body.clone(), format!("{prefix}{body}")] {
            let read: AllTypes<'_> = ayaml::compat::from_str_with(&doc, &DROP)
                .unwrap_or_else(|error| panic!("{tag:?} len={}: {error}", doc.len()));
            assert_eq!(
                read,
                AllTypes {
                    b: true,
                    i: -5,
                    u: 31,
                    f: 1.5,
                    c: 'x',
                    s: "5",
                    opt_num: Some(5),
                    opt_tilde: Some("~".to_owned()),
                    wrapped: Count(7),
                    pair: (1, "ok".to_owned()),
                    items: vec![1, 2],
                    single: Single { a: 9 },
                    status_bare: Status::Done,
                    status_map: Status::Todo,
                    status_key: Status::Done,
                }
            );
        }
        // `~` beneath a dropped global tag is text, not null, so unit and Option<u32> refuse it.
        let tilde = format!("{directive}{tag}~\n");
        assert!(ayaml::compat::from_str_with::<()>(&tilde, &DROP).is_err());
        assert!(ayaml::compat::from_str_with::<Unit>(&tilde, &DROP).is_err());
        assert!(ayaml::compat::from_str_with::<Option<u32>>(&tilde, &DROP).is_err());
        // Quoted scalars beneath a dropped global tag stay strings.
        let quoted = format!("{directive}{tag}\"5\"\n");
        assert!(ayaml::compat::from_str_with::<u32>(&quoted, &DROP).is_err());
        assert_eq!(
            ayaml::compat::from_str_with::<String>(&quoted, &DROP).expect("quoted str"),
            "5"
        );
    }

    // `deserialize_any_with` and explicit limits inside `ParseOptions`.
    let tight = DROP
        .clone()
        .with_limits(ayaml::Limits::default().with_max_events(1));
    assert!(ayaml::compat::from_str_with::<Json>("a: !!timestamp 1\n", &tight).is_err());
    assert!(
        ayaml::compat::from_str_seed_with("a: !!timestamp 1\n", &tight, PhantomData::<Json>)
            .is_err()
    );
    assert!(
        ayaml::compat::deserialize_any_with("a: !!timestamp 1\n", &DROP, serde::de::IgnoredAny,)
            .is_ok()
    );
    assert!(
        ayaml::compat::deserialize_any_with("a: !!timestamp 1\n", &tight, serde::de::IgnoredAny,)
            .is_err()
    );

    // Local tags, `!`, and core tags keep their usual reading under `GlobalTags::Drop`.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    enum Shape {
        Circle(f64),
    }
    assert_eq!(
        ayaml::compat::from_str_with::<Shape>("!Circle 2.5\n", &DROP).expect("local enum"),
        Shape::Circle(2.5)
    );
    let kept: ayaml::Value =
        ayaml::compat::from_str_with("a: !custom 1\n", &DROP).expect("local tag in Value");
    assert!(kept["a"].is_tagged());
    let local_err = ayaml::compat::from_str_with::<Json>("a: !custom 1\n", &DROP)
        .expect_err("local tag refused as JSON");
    assert!(local_err.message().contains("`!custom`"), "{local_err}");
    let bang_err = ayaml::compat::from_str_with::<Json>("a: ! 1\n", &DROP)
        .expect_err("non-specific ! refused as JSON");
    assert!(bang_err.message().contains("`!`"), "{bang_err}");
    assert!(ayaml::compat::from_str_with::<bool>("! true\n", &DROP).is_err());
    assert_eq!(
        ayaml::compat::from_str_with::<u32>("!!int \"5\"\n", &DROP).expect("core int"),
        5
    );
    assert!(ayaml::compat::from_str_with::<u32>("!!int x\n", &DROP).is_err());
    assert!(ayaml::compat::from_str_with::<u32>("!!str 5\n", &DROP).is_err());
    // `%TAG` mapping to a local `!` prefix stays local.
    let local_handle = "%TAG !e! !\n---\na: !e!custom 1\n";
    assert!(ayaml::compat::from_str_with::<Json>(local_handle, &DROP).is_err());
}
