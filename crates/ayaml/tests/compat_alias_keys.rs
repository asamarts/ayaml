//! An alias directly followed by `:` is a key in compat, as libyaml reads it, unless an anchor in
//! the same document has that exact name. Default mode keeps the colon in the name, as YAML 1.2.2
//! says.
#![cfg(feature = "libyaml-compat")]

use std::collections::BTreeMap;
use std::fmt;

use ayaml::{Error, ErrorKind, Event, Limits, Value, compat};
use serde::Deserialize;
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde_json::json;

fn read(input: &str) -> serde_json::Value {
    let value: Value = compat::from_str(input).unwrap_or_else(|error| panic!("{input:?}: {error}"));
    serde_json::to_value(&value).expect("json")
}

fn refused(input: &str) -> Error {
    match compat::from_str::<Value>(input) {
        Ok(value) => panic!("{input:?} read {value:?}"),
        Err(error) => error,
    }
}

/// Reads a mapping's keys, for the entry point that takes a visitor.
struct Keys;

impl<'de> Visitor<'de> for Keys {
    type Value = Vec<String>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a mapping")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Vec<String>, A::Error> {
        let mut keys = Vec::new();
        while let Some((key, _)) = map.next_entry::<String, IgnoredAny>()? {
            keys.push(key);
        }
        Ok(keys)
    }
}

fn aliases(input: &str) -> Vec<String> {
    compat::parse_events_with_limits(input, &Limits::default())
        .expect("events")
        .into_iter()
        .filter_map(|event| match event {
            Event::Alias { anchor, .. } => Some(anchor.as_str().to_owned()),
            _ => None,
        })
        .collect()
}

#[test]
fn an_alias_right_before_a_colon_is_a_block_key() {
    assert_eq!(
        read("name: &items_key items\n*items_key: []\n"),
        json!({"name": "items", "items": []})
    );
    assert_eq!(
        read("name: &size_key size\n*size_key: null\n"),
        json!({"name": "size", "size": null})
    );
    assert_eq!(read("x: &a k\n*a:\n"), json!({"x": "k", "k": null}));
    assert_eq!(read("x: &a k\n*a:"), json!({"x": "k", "k": null}));
    assert_eq!(read("x: &a k\n*a:\t1\n"), json!({"x": "k", "k": 1}));
    assert_eq!(read("x: &a k\n*a: # c\n  1\n"), json!({"x": "k", "k": 1}));
    assert_eq!(
        read("- &a k\n- *a: 1\n  j: 2\n"),
        json!(["k", {"k": 1, "j": 2}])
    );
    assert_eq!(
        read("x: &a k\ny:\n  *a: 1\n"),
        json!({"x": "k", "y": {"k": 1}})
    );
    assert_eq!(
        compat::from_str::<Value>("x: &a k\n? *a: 1\n: 2\n").unwrap(),
        ayaml::parse_value("x: k\n{k: 1}: 2\n").unwrap()
    );
    // Names may hold more than letters.
    assert_eq!(
        read("x: &a.b-c_d k\n*a.b-c_d: 1\n"),
        json!({"x": "k", "k": 1})
    );
}

#[test]
fn every_compat_entry_point_reads_it_alike() {
    #[derive(Debug, Deserialize, PartialEq)]
    struct Record {
        name: String,
        items: Vec<u8>,
    }
    let input = "name: &key items\n*key: [1]\n";
    let want = Record {
        name: "items".to_owned(),
        items: vec![1],
    };
    assert_eq!(compat::from_str::<Record>(input).unwrap(), want);
    let seeded: Record = compat::from_str_seed(input, std::marker::PhantomData).unwrap();
    assert_eq!(seeded, want);
    let map: BTreeMap<String, Value> = compat::from_str(input).unwrap();
    assert_eq!(map.len(), 2);
    assert_eq!(
        compat::deserialize_any(input, Keys).unwrap(),
        ["name", "items"]
    );
    assert_eq!(
        aliases(input),
        vec!["key".to_owned()],
        "the event stream ends the name too"
    );
}

#[test]
fn an_exact_anchor_keeps_the_colon() {
    // 2SXE and W5VH.
    assert_eq!(
        read("&a: key: &a value\nfoo:\n  *a:\n"),
        json!({"key": "value", "foo": "key"})
    );
    assert_eq!(aliases("&a: key: &a value\nfoo:\n  *a:\n"), ["a:"]);
    assert_eq!(
        read("a: &:@*!$\"<foo>: scalar a\nb: *:@*!$\"<foo>:\n"),
        json!({"a": "scalar a", "b": "scalar a"})
    );
    // With `a` defined too, the exact name still wins.
    assert_eq!(
        read("x: &a: one\ny: &a two\nz: *a:\n"),
        json!({"x": "one", "y": "two", "z": "one"})
    );
    // A longer name that starts the same way is another name.
    assert_eq!(
        read("x: &ab: one\ny: &a two\n*a: 3\n"),
        json!({"x": "one", "y": "two", "two": 3})
    );
    // An anchor defined later isn't there yet, so the name ends, and `a` is unknown.
    let error = refused("*a: 1\nb: &a: 2\n");
    assert!(error.to_string().contains("unknown anchor"), "{error}");
}

#[test]
fn the_names_reset_at_each_document() {
    assert_eq!(aliases("&a: x\n---\n*a:\n"), ["a"]);
    assert_eq!(aliases("&a: x\n...\n---\n*a:\n"), ["a"]);
    assert_eq!(aliases("&a: x\n...\n*a:\n"), ["a"]);
    assert_eq!(aliases("--- &a: x\n---\n*a:\n"), ["a"]);
    assert_eq!(aliases("&a: x\n---\n&a: y\n---\n*a:\n"), ["a"]);
    assert_eq!(aliases("x: &a: 1\ny: *a:\n"), ["a:"]);
    assert_eq!(aliases("x: &a: 1\n---\n*a:\n"), ["a"]);
}

#[test]
fn an_alias_matching_no_anchor_is_unknown() {
    for input in [
        "*a: 1\n",
        "b: 1\n*a: 1\n",
        "x: &b: 1\n*a: 1\n",
        "x: &ab k\n*a: 1\n",
        "*a:\n",
        "*a:",
        "- *a:\n",
        "{*a: 1}\n",
        "[*a: 1]\n",
    ] {
        let error = refused(input);
        assert!(
            error.to_string().contains("unknown anchor"),
            "{input:?}: {error}"
        );
    }
    // A name that is only a colon has nothing to give up.
    assert_eq!(aliases("*:\n"), [":"]);
    assert!(refused("*:\n").to_string().contains("unknown anchor"));
    refused("*: 1\n");
}

#[test]
fn a_flow_key_reads_as_a_block_key_does() {
    assert_eq!(
        read("x: &a k\ny: {*a: 1}\n"),
        json!({"x": "k", "y": {"k": 1}})
    );
    assert_eq!(
        read("x: &a k\ny: [*a: 1]\n"),
        json!({"x": "k", "y": [{"k": 1}]})
    );
    assert_eq!(
        read("x: &a k\ny: {*a: 1, b: 2}\n"),
        json!({"x": "k", "y": {"k": 1, "b": 2}})
    );
    assert_eq!(
        read("x: &a k\ny: {*a: }\n"),
        json!({"x": "k", "y": {"k": null}})
    );
    assert_eq!(
        read("x: &a k\ny: {*a:}\n"),
        json!({"x": "k", "y": {"k": null}})
    );
    assert_eq!(
        read("x: &a k\ny: [*a:]\n"),
        json!({"x": "k", "y": [{"k": null}]})
    );
    assert_eq!(
        read("x: &a k\ny: [*a:, 1]\n"),
        json!({"x": "k", "y": [{"k": null}, 1]})
    );
    assert_eq!(read("x: [&a:, *a:]\n"), json!({"x": [null, null]}));
}

#[test]
fn a_block_scalar_after_the_key_is_indented_from_the_key() {
    assert_eq!(read("x: &a k\n*a: |\n  y\n"), json!({"x": "k", "k": "y\n"}));
    assert_eq!(
        read("x: &a k\n*a: |1\n  y\n"),
        json!({"x": "k", "k": " y\n"})
    );
    assert_eq!(
        read("- &a k\n- *a: |1\n    y\n"),
        json!(["k", {"k": " y\n"}])
    );
    // Too little indentation is an error here, as it is after a plain key.
    refused("x: &a k\n*a: |\ny\n");
    refused("j: |\ny\n");
    // A key that is a flow collection holding the alias indents it as any key does.
    assert_eq!(
        compat::from_str::<Value>("x: &a k\n{*a: 1}: |1\n  y\n").unwrap(),
        ayaml::parse_value("x: k\n{k: 1}: |1\n  y\n").unwrap()
    );
    // Under an exact anchor, `*a:` is a whole node and takes no block scalar as its value.
    refused("x: &a: k\n*a: |\n  y\n");
}

/// The check on a block scalar's first line finds its owner by reading the lines before it, with
/// the same names the scanner keeps. Read without them, this line owns less, and the scalar's text
/// reads as content.
#[test]
fn a_block_scalar_after_a_key_keeps_its_header_check() {
    assert_eq!(
        refused("- &a: k\n- *a: : |\n  |*a*b: ").to_string(),
        "unexpected '*' in block scalar header at line 3 column 4"
    );
}

#[test]
fn a_duplicate_is_found_at_its_mapping_as_libyaml_finds_it() {
    #[derive(Debug, Deserialize)]
    struct Record {
        #[allow(dead_code)]
        name: String,
        #[allow(dead_code)]
        items: Vec<u8>,
    }
    let error = compat::from_str::<Record>("items: []\nname: &key items\n*key: []\n")
        .expect_err("a duplicate field");
    assert_eq!(error.kind(), ErrorKind::Serde, "{error}");
    assert!(
        error.to_string().contains("duplicate field `items`"),
        "{error}"
    );
    let location = error.location().expect("a location");
    assert_eq!((location.line(), location.column()), (1, 1), "{error}");

    for (input, line, column) in [
        ("items: []\nname: &key items\n*key: []\n", 1, 1),
        ("items: []\nname: &key items\n*key : []\n", 1, 1),
        ("item: one\nitem: two\n", 1, 1),
        ("a: 1\nb:\n  x: 1\n  x: 2\n", 3, 3),
        ("- a: 1\n  a: 2\n", 1, 3),
        ("{a: 1, a: 2}\n", 1, 1),
        ("x: {a: 1, a: 2}\n", 1, 4),
        ("? a\n: 1\n? a\n: 2\n", 1, 1),
    ] {
        let error = refused(input);
        assert!(
            error.to_string().contains("duplicate"),
            "{input:?}: {error}"
        );
        let location = error.location().expect("a location");
        assert_eq!(
            (location.line(), location.column()),
            (line, column),
            "{input:?}: {error}"
        );
    }
}

#[test]
fn the_separated_form_works_in_both_modes() {
    for input in [
        "name: &key items\n*key : []\n",
        "name: &key items\n? *key\n: []\n",
    ] {
        let want = json!({"name": "items", "items": []});
        assert_eq!(read(input), want, "{input:?}");
        let value = ayaml::parse_value(input).unwrap_or_else(|error| panic!("{input:?}: {error}"));
        assert_eq!(serde_json::to_value(&value).unwrap(), want, "{input:?}");
    }
}

#[test]
fn default_mode_keeps_the_colon_in_the_name() {
    let input = "name: &key items\n*key: []\n";
    let error = ayaml::parse_value(input).expect_err("default mode");
    assert!(
        error.to_string().contains("unknown anchor") || error.kind() == ErrorKind::Parser,
        "{error}"
    );
    assert!(ayaml::from_str::<Value>(input).is_err());
    assert!(ayaml::from_str_borrowed::<Value>(input).is_err());
    // The feature being on changes no root entry point.
    let value = ayaml::parse_value("&a: b\n").unwrap();
    assert_eq!(serde_json::to_value(&value).unwrap(), json!("b"));
    let value = ayaml::parse_value("x: &a: k\ny: *a:\n").unwrap();
    assert_eq!(
        serde_json::to_value(&value).unwrap(),
        json!({"x": "k", "y": "k"})
    );
    assert!(ayaml::parse_value("x: &a k\ny: *a:\n").is_err());
}

#[test]
fn an_alias_bomb_written_with_these_keys_hits_the_alias_limit() {
    // Each level names the last as a key and as a value, so the text stays small.
    let mut input = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
    for level in 1..12 {
        let previous = level - 1;
        input.push_str(&format!("a{level}: &a{level}\n"));
        for entry in 0..10 {
            input.push_str(&format!("  - {{*a{previous}: {entry}}}\n"));
        }
    }
    assert!(input.len() < 4096, "{}", input.len());
    let error = refused(&input);
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
}
