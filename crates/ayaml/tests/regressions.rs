//! Regression tests for parser, composer, Serde, and emitter behavior.
//! Expected values follow YAML 1.2.2 and, in compat mode, libyaml.

use serde_json::{Value as Json, json};

fn parse(input: &str) -> Json {
    let value = ayaml::parse_value(input).unwrap_or_else(|error| panic!("{input:?}: {error}"));
    serde_json::to_value(&value).expect("JSON-compatible value")
}

fn parse_documents(input: &str) -> Vec<Json> {
    let limits = ayaml::Limits::default().with_max_documents(16);
    ayaml::parse_documents_with_limits(input, &limits)
        .unwrap_or_else(|error| panic!("{input:?}: {error}"))
        .iter()
        .map(|value| serde_json::to_value(value).expect("JSON-compatible value"))
        .collect()
}

fn rejects(input: &str) {
    assert!(
        ayaml::parse_value(input).is_err(),
        "{input:?} should be rejected, got {:?}",
        ayaml::parse_value(input)
    );
}

/// A plain scalar must not continue into the next entry of a compact
/// nested sequence (suite 3ALJ, A2M4, 6BCT, M5DY).
#[test]
fn compact_nested_sequence_entries_stay_separate() {
    assert_eq!(parse("- - a\n  - b\n"), json!([["a", "b"]]));
    assert_eq!(parse("-  - a\n   - b\n"), json!([["a", "b"]]));
    assert_eq!(
        parse("- - - a\n    - b\n  - c\n"),
        json!([[["a", "b"], "c"]])
    );
    assert_eq!(
        parse("key:\n  - - a\n    - b\n"),
        json!({"key": [["a", "b"]]})
    );
    // A deeper line is still a continuation of the inner entry.
    assert_eq!(parse("- - a\n     b\n"), json!([["a b"]]));
    assert_eq!(parse("- - a\n    - b\n"), json!([["a - b"]]));
}

/// Explicit keys holding block collections, and compact mappings inside
/// explicit keys (suite M2N8).
#[test]
fn explicit_keys_hold_block_collections() {
    let value = ayaml::parse_value("? - a\n  - b\n: c\n").expect("parse");
    let mapping = value.as_mapping().expect("mapping");
    let key = ayaml::Value::Sequence(vec!["a".into(), "b".into()]);
    assert_eq!(mapping.get(&key), Some(&ayaml::Value::from("c")));

    let value = ayaml::parse_value("? a: 1\n  b: 2\n: c\n").expect("parse");
    let (key, entry) = value
        .as_mapping()
        .expect("mapping")
        .iter()
        .next()
        .expect("entry");
    assert_eq!(
        serde_json::to_value(key).expect("json"),
        json!({"a": 1, "b": 2})
    );
    assert_eq!(entry, &ayaml::Value::from("c"));

    // `? a: b` on one line: the `:` belongs to a compact mapping in the key.
    let value = ayaml::parse_value("? a: b\n").expect("parse");
    let (key, entry) = value
        .as_mapping()
        .expect("mapping")
        .iter()
        .next()
        .expect("entry");
    assert_eq!(serde_json::to_value(key).expect("json"), json!({"a": "b"}));
    assert!(entry.is_null());
}

/// A block-scalar indentation indicator counts from the owning
/// collection, not from the line (suite 4WA9).
#[test]
fn indentation_indicator_is_relative_to_the_parent_node() {
    assert_eq!(
        parse("- aaa: |2\n    xxx\n  bbb: |\n    xxx\n"),
        json!([{"aaa": "xxx\n", "bbb": "xxx\n"}])
    );
    assert_eq!(parse("- |1\n  explicit\n"), json!([" explicit\n"]));
    assert_eq!(parse("key: |2\n   x\n"), json!({"key": " x\n"}));
    assert_eq!(
        parse("- k: |+\n\n  k2: v\n"),
        json!([{"k": "\n", "k2": "v"}])
    );
}

/// Continuation and block-scalar content lines are measured against the
/// owning collection, which is both stricter and more permissive than the
/// line indentation.
#[test]
fn continuation_indentation_follows_the_owner() {
    // Valid: more indented than the owning mapping.
    assert_eq!(parse("key: a\n  - b\n"), json!({"key": "a - b"}));
    assert_eq!(parse("key:\n    a\n  b\n"), json!({"key": "a b"}));
    assert_eq!(parse("k:\n  &x\n    a\n   b\n"), json!({"k": "a b"}));
    assert_eq!(parse("  a\nb\n"), json!("a b"));
    assert_eq!(parse("--- a\n- b\n"), json!("a - b"));
    assert_eq!(parse("- key: a\n   b\n"), json!([{"key": "a b"}]));
    // Invalid: not more indented than the owning mapping at column 2.
    rejects("- key: a\n  b\n");
    rejects("- a: |\n  x\n");
}

/// Document markers end a root block scalar, also after `--- |`
/// (suite W4TN, M7A3).
#[test]
fn document_markers_end_root_block_scalars() {
    assert_eq!(
        parse_documents("--- |\n%!PS-Adobe-2.0\n...\n--- x\n"),
        vec![json!("%!PS-Adobe-2.0\n"), json!("x")]
    );
    assert_eq!(
        parse_documents("--- |\na\n--- b\n"),
        vec![json!("a\n"), json!("b")]
    );
}

/// An explicit key can start a compact mapping in a sequence entry
/// (suite 4FJ6, 9MMW, and V9D5 round trips).
#[test]
fn explicit_key_in_sequence_entry() {
    assert_eq!(parse("- ? a\n  : b\n"), json!([{"a": "b"}]));
    assert_eq!(parse("- ? a\n"), json!([{"a": null}]));
    assert_eq!(
        parse("- ? a\n  : b\n  c: d\n"),
        json!([{"a": "b", "c": "d"}])
    );
    let value = ayaml::parse_value("- ? - x\n  : y\n").expect("parse");
    let entry = &value.as_sequence().expect("sequence")[0];
    let key = ayaml::Value::Sequence(vec!["x".into()]);
    assert_eq!(
        entry.as_mapping().expect("mapping").get(&key),
        Some(&ayaml::Value::from("y"))
    );
}

/// `...` with no open document ends nothing (suite HWV9, QT73, M7A3).
#[test]
fn document_end_markers_without_documents() {
    assert_eq!(parse_documents("...\n"), Vec::<Json>::new());
    assert_eq!(parse_documents("...\n...\n"), Vec::<Json>::new());
    assert_eq!(
        parse_documents("a\n...\n# c\n...\n|\n%x # not a comment\n"),
        vec![json!("a"), json!("%x # not a comment\n")]
    );
}

/// The spec readings by default (suite Y2GN, HM87, 652Z, JEF9, L24T).
#[test]
fn spec_readings_by_default() {
    assert_eq!(parse("key: &an:chor value\n"), json!({"key": "value"}));
    assert_eq!(parse("[?x]\n"), json!(["?x"]));
    assert_eq!(parse("{ ?foo: bar }\n"), json!({"?foo": "bar"}));
    assert_eq!(parse("- |+\n   "), json!(["\n"]));
    assert_eq!(parse("foo: |\n  x\n   "), json!({"foo": "x\n \n"}));
}

/// Compat ends a block scalar on a last line of spaces at the end of the input as libyaml does, in
/// each chomping mode: spaces past the indentation are content without a line break after them, and
/// a shorter line adds no break. The values are libyaml's.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_ends_a_block_scalar_on_a_last_line_of_spaces_as_libyaml_does() {
    for (input, value) in [
        ("k: |\n  a\n ", "a\n"),
        ("k: |\n  a\n    ", "a\n  "),
        ("k: |\n  a\n\n    ", "a\n\n  "),
        ("k: |-\n  a\n ", "a"),
        ("k: |-\n  a\n    ", "a\n  "),
        ("k: |-\n  a\n\n    ", "a\n\n  "),
        ("k: |+\n  a\n ", "a\n"),
        ("k: |+\n  a\n\n ", "a\n\n"),
        ("k: |+\n  a\n    ", "a\n  "),
        ("k: |+\n  a\n\n    ", "a\n\n  "),
        ("k: >\n  a\n ", "a\n"),
        ("k: >\n  a\n    ", "a\n  "),
        ("k: >-\n  a\n ", "a"),
        ("k: >-\n  a\n    ", "a\n  "),
        ("k: >+\n  a\n ", "a\n"),
        ("k: >+\n  a\n    ", "a\n  "),
        ("k: >+\n  a\n\n    ", "a\n\n  "),
    ] {
        assert_eq!(compat(input), json!({ "k": value }), "{input:?}");
    }
}

/// `%XX` escapes in shorthand tags are decoded (suite 6CK3).
#[test]
fn tag_escapes_are_decoded() {
    let value = ayaml::parse_value("%TAG !e! tag:example.com,2000:app/\n---\n!e!tag%21 baz\n")
        .expect("parse");
    let ayaml::Value::Tagged(tagged) = value else {
        panic!("expected a tagged value, got {value:?}");
    };
    assert_eq!(
        format!("{}{}", tagged.tag.handle(), tagged.tag.suffix()),
        "tag:example.com,2000:app/tag!"
    );
    assert!(ayaml::parse_value("!a%zz b\n").is_err());
    assert!(ayaml::parse_value("!a%ff b\n").is_err());
}

/// Properties before `,`, `]`, or `}` belong to an empty node.
#[test]
fn properties_before_flow_indicators() {
    let value = ayaml::parse_value("[!t , &a , b]\n").expect("parse");
    let items = value.as_sequence().expect("sequence");
    assert!(matches!(&items[0], ayaml::Value::Tagged(tagged) if tagged.value.is_null()));
    assert!(items[1].is_null());
    assert_eq!(items[2], ayaml::Value::from("b"));
    assert_eq!(parse("[&a ]\n"), json!([null]));
    assert_eq!(parse("{k: &a , j: x}\n"), json!({"k": null, "j": "x"}));
}

/// Only space and tab separate tokens; other Unicode spaces are content.
#[test]
fn unicode_spaces_are_content() {
    assert_eq!(parse("a:\u{a0}0\n"), json!("a:\u{a0}0"));
    assert_eq!(parse("key: x\u{a0}\n"), json!({"key": "x\u{a0}"}));
    assert_eq!(parse("\u{3000}a: b\n"), json!({"\u{3000}a": "b"}));
    assert_eq!(parse("a\u{a0}#b\n"), json!("a\u{a0}#b"));
}

/// Tags may only hold URI characters, escaped as `%XX` otherwise.
#[test]
fn tags_hold_uri_characters_only() {
    rejects("!x\u{a0}y z\n");
    rejects("!<a b> x\n");
    rejects("!foo,bar x\n");
    let value = ayaml::parse_value("!<$:?> x\n").expect("parse");
    assert!(value.is_tagged());
}

/// In block context a flow indicator does not end `-` or `?`.
#[test]
fn dash_and_question_mark_before_flow_indicators() {
    assert_eq!(parse("-,\n"), json!("-,"));
    assert_eq!(parse("- -]\n"), json!(["-]"]));
    assert_eq!(parse("a: ?}\n"), json!({"a": "?}"}));
}

/// A `:` inside a tag or anchor is not a mapping value indicator when
/// the scanner decides which collection owns a block scalar.
#[test]
fn colons_inside_properties() {
    let value = ayaml::parse_value("- !: |-\n  \n  0\n").expect("parse");
    let ayaml::Value::Tagged(tagged) = &value.as_sequence().expect("sequence")[0] else {
        panic!("expected a tagged item, got {value:?}");
    };
    assert_eq!(tagged.tag.suffix(), ":");
    assert_eq!(tagged.value, ayaml::Value::from("\n0"));
    assert_eq!(parse("- &a: |\n  x\n"), json!(["x\n"]));
}

/// Properties on the first key of a compact mapping do not move the
/// mapping's indentation.
#[test]
fn properties_on_the_first_compact_key() {
    assert_eq!(parse("- &x a: b\n  c: d\n"), json!([{"a": "b", "c": "d"}]));
    assert_eq!(
        parse("? a\n: &x b: c\n  d: e\n"),
        json!({"a": {"b": "c", "d": "e"}})
    );
    let value = ayaml::parse_value("- !k 1: b\n  c: d\n").expect("parse");
    let mapping = value.as_sequence().expect("sequence")[0]
        .as_mapping()
        .expect("mapping");
    assert_eq!(mapping.len(), 2);
    rejects("? a\n: &x - b\n");
}

/// Each empty line after an escaped line break in a double-quoted
/// scalar is a line feed (YAML 1.2.2 production 112). Found by the diff
/// fuzz target.
#[test]
fn empty_lines_after_an_escaped_line_break() {
    assert_eq!(parse("\"a\\\n\nb\"\n"), json!("a\nb"));
    assert_eq!(parse("\"a\\\n  \n\n b\"\n"), json!("a\n\nb"));
    assert_eq!(parse("\"\n\\\n\n\n\"\n"), json!(" \n\n"));
    assert_eq!(parse("\"\r\\\r\r\r\"\r"), json!(" \n\n"));
    // White space before the escaped break is content; without empty
    // lines, the break is simply removed.
    assert_eq!(parse("\"a \\\n b\"\n"), json!("a b"));
    assert_eq!(parse("\"a\\\n  b\"\n"), json!("ab"));
    // The continuation after an escaped break follows the usual rules.
    rejects("\"a\\\n---\n\"\n");
    rejects("k:\n  \"a\\\nb\"\n");
}

/// A directive needs a name, and neither its name nor its parameters
/// may hold a byte order mark. Found by the emit fuzz target. Whatever
/// directive parses, the event writer writes back.
#[test]
fn directives_need_a_name() {
    rejects("%\n--- a\n");
    rejects("% \n--- a\n");
    rejects("% x\n--- a\n");
    rejects("%A\u{feff}B\n--- a\n");
    rejects("%A B\u{feff}\n--- a\n");
    let output = emit_events("%A\u{a0}B C\u{a0}D\n--- a\n");
    assert!(output.starts_with("%A\u{a0}B C\u{a0}D\n"), "{output:?}");
    let output = emit_events("%TAG\t!e!\ttag:x,2000:\n--- !e!y a\n");
    assert!(output.contains("%TAG !e!\ttag:x,2000:"), "{output:?}");
}

/// `%`, `@`, and `` ` `` are indicators and cannot start a plain
/// scalar, in either mode. Found by the borrowed fuzz target.
#[test]
fn reserved_indicators_cannot_start_plain_scalars() {
    for input in [
        "a: @b\n", "a: `b\n", "a: %b\n", "[@a]\n", "- `a\n", "@a: b\n",
    ] {
        rejects(input);
    }
    assert_eq!(parse("a: b@c`d%e\n"), json!({"a": "b@c`d%e"}));
    assert_eq!(parse("a: '@b'\n"), json!({"a": "@b"}));
}

/// An anchor or alias is separated from the content that follows
/// (production 96); libyaml rejects `&a[b]` too. A `:` before a blank or a
/// flow indicator stays in the name, as YAML 1.2.2 says (2SXE, W5VH). Found by
/// fuzzing.
#[test]
fn anchors_are_separated_from_their_content() {
    for input in [
        "&a[b]\n",
        "&a{b: c}\n",
        "[&0:[a]]\n",
        "- *a[b]\n",
        "&0:[a]\n",
    ] {
        rejects(input);
    }
    assert_eq!(parse("&a [b]\n"), json!(["b"]));
    assert_eq!(parse("&a: b\n"), json!("b"));
    assert_eq!(parse("[&a:, b]\n"), json!([null, "b"]));
}

/// With an indentation indicator, a leading line holding
/// only spaces keeps the spaces past the indentation as content. Without
/// one, such a line is more indented than the content, which is an error.
#[test]
fn explicit_indentation_keeps_leading_spaces() {
    assert_eq!(parse("a: |2\n    \n  x\n"), json!({"a": "  \nx\n"}));
    assert_eq!(parse("- >1\n  \n x\n"), json!([" \nx\n"]));
    assert_eq!(parse("a: |\n  x\n    \n"), json!({"a": "x\n  \n"}));
    assert_eq!(parse("a: |\n   \n"), json!({"a": ""}));
    rejects("a: |\n    \n  x\n");
}

/// An implicit key is at most 1024 characters, up to its `:`, in block
/// mappings and in flow-sequence pairs; flow-mapping keys have no limit.
#[test]
fn implicit_keys_are_at_most_1024_characters() {
    let fits = "k".repeat(1024);
    let long = "k".repeat(1025);
    assert_eq!(parse(&format!("{fits}: v\n")), json!({ fits.clone(): "v" }));
    rejects(&format!("{long}: v\n"));
    rejects(&format!("{} : v\n", "k".repeat(1024)));
    rejects(&format!("- {long}: v\n"));
    assert_eq!(
        parse(&format!("[{fits}: v]\n")),
        json!([{ fits.clone(): "v" }])
    );
    rejects(&format!("[{long}: v]\n"));
    assert_eq!(parse(&format!("{{{long}: v}}\n")), json!({ long: "v" }));
}

/// NEL, LS, and PS are content in YAML 1.2, not line breaks.
#[test]
fn nel_ls_and_ps_are_content() {
    for separator in ['\u{85}', '\u{2028}', '\u{2029}'] {
        assert_eq!(
            parse(&format!("a: b{separator}c\n")),
            json!({ "a": format!("b{separator}c") })
        );
        assert_eq!(
            parse(&format!("\"b{separator}c\"\n")),
            json!(format!("b{separator}c"))
        );
        let output = emit_events(&format!("- b{separator}c\n"));
        assert!(!output.contains(separator), "{output:?}");
    }
}

/// `%TAG` handles are `!`, `!!`, or word characters between `!`s, and
/// prefixes are URI characters that do not start with a flow indicator.
#[test]
fn tag_directives_are_validated() {
    let document = |directive: &str| format!("{directive}\n--- !e!x a\n");
    // A tagged value serializes as `{"!<tag>": payload}`.
    assert_eq!(
        parse(&document("%TAG !e! tag:x,2000:")),
        json!({ "!<tag:x,2000:x>": "a" })
    );
    // The prefix's escapes are decoded.
    assert_eq!(
        parse(&document("%TAG !e! tag:x%21y")),
        json!({ "!<tag:x!yx>": "a" })
    );
    for directive in [
        "%TAG !e_f! tag:x",
        "%TAG e tag:x",
        "%TAG !\u{e9}! tag:x",
        "%TAG !e! [x",
        "%TAG !e! ,x",
        "%TAG !e! tag:x%2",
        "%TAG !e! tag:\u{e9}",
    ] {
        rejects(&document(directive));
    }
}

/// After a property-only line, the node may continue only within its
/// parent's bounds (YAML 1.2.2 section 8.2). A sequence entry's node cannot
/// take the next `-` at the entry's own column as its block sequence; a
/// mapping value's node can, since its sequence may sit at the key's column.
/// Content may sit left of the property line, down to the parent's bound.
/// Both modes. Found by fuzzing.
#[test]
fn property_lines_stay_within_the_parent() {
    let json = |input: &str| {
        let value = ayaml::parse_value(input).unwrap_or_else(|error| panic!("{input:?}: {error}"));
        #[cfg(feature = "libyaml-compat")]
        assert_eq!(
            ayaml::compat::from_str::<ayaml::Value>(input).expect("compat"),
            value,
            "{input:?}"
        );
        serde_json::to_string(&value).expect("json")
    };
    assert_eq!(
        json("!A\r-\r  !-\t\r \r-  \r-"),
        r#"{"!A":[{"!-":null},null,null]}"#
    );
    assert_eq!(json("-\n  !t\n-\n"), r#"[{"!t":null},null]"#);
    assert_eq!(json("-\n  &a\n- x\n"), r#"[null,"x"]"#);
    assert_eq!(json("-\n  !t\n  - x\n"), r#"[{"!t":["x"]}]"#);
    assert_eq!(json("-\n    !t\n  - x\n"), r#"[{"!t":["x"]}]"#);
    assert_eq!(json("-\n    !t\n  x\n"), r#"[{"!t":"x"}]"#);
    assert_eq!(json("a:\n  !t\n- x\n"), r#"{"a":{"!t":["x"]}}"#);
    assert_eq!(json("a:\n    !t\n b: 1\n"), r#"{"a":{"!t":{"b":1}}}"#);
    assert_eq!(
        json("- a:\n    !t\n  b: 1\n"),
        r#"[{"a":{"!t":null},"b":1}]"#
    );
    assert_eq!(json("  !t\n- x\n"), r#"{"!t":["x"]}"#);
}

/// `%TAG` prefixes have their %-escapes decoded, like tag suffixes, as
/// libyaml does. Verbatim tags stay as written (YAML 1.2.2 section
/// 6.9.1).
#[test]
fn tag_prefix_escapes_are_decoded() {
    let tag = |input: &str| {
        let value = ayaml::parse_value(input).unwrap_or_else(|error| panic!("{input:?}: {error}"));
        value.as_tagged().expect("tagged").tag.to_string()
    };
    let local = "%TAG !e! !my%21pre-\n--- !e!foo 1\n";
    assert_eq!(tag(local), "!my!pre-foo");
    assert_eq!(
        tag("%TAG !e! tag:ex.com,2000:a%2Fb-\n--- !e!foo 1\n"),
        "tag:ex.com,2000:a/b-foo"
    );
    assert_eq!(tag("--- !<!my%21pre-foo> 1\n"), "!my%21pre-foo");
    let error = ayaml::parse_value("%TAG !e! !%FF\n--- !e!x 1\n").expect_err("bad UTF-8");
    assert_eq!(error.kind(), ayaml::ErrorKind::Scanner);
    #[cfg(feature = "libyaml-compat")]
    assert_eq!(
        ayaml::compat::from_str::<ayaml::Value>(local).expect("compat"),
        ayaml::parse_value(local).expect("default")
    );
}

/// In compat, a column-0 byte order mark before any content ends a
/// root block scalar, as a column-0 `#` line does: libyaml indents
/// root block scalars by at least one space and then skips the BOM. Default
/// mode reads these the same way here, since the BOM starts a document
/// prefix. Found by fuzzing.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_column_zero_boms_end_root_block_scalars() {
    for input in [
        "|+\n\u{feff}\n\n\n\n ",
        "|+\n\u{feff}\n",
        "--- |+\n\u{feff}\n\n",
    ] {
        let compat = ayaml::compat::from_str::<ayaml::Value>(input).expect("compat");
        assert_eq!(compat, "", "{input:?}");
        assert_eq!(
            ayaml::parse_value(input).expect("default"),
            compat,
            "{input:?}"
        );
    }
    // After content, the BOM line is outside the scalar too.
    assert_eq!(
        ayaml::compat::from_str::<ayaml::Value>("|+\n x\n\u{feff}\n").expect("compat"),
        "x\n"
    );
}

/// YAML 1.2.2 section 5.1 requires every JSON-compatible character to be
/// allowed inside quoted scalars (`nb-json`), so a raw DEL, C1 control, or
/// U+FFFE is content there, in both modes. libyaml rejects them anywhere.
/// Outside quoted scalars they stay an error, and controls below U+0020 other
/// than tab, LF, and CR are an error everywhere. Found in LLVM's
/// `llvm-dlltool` test inputs.
#[test]
fn quoted_scalars_hold_non_c0_characters() {
    let single = |key: &str, value: &str| {
        let mut mapping = ayaml::Mapping::new();
        mapping.insert(ayaml::Value::from(key), ayaml::Value::from(value));
        ayaml::Value::Mapping(mapping)
    };
    let input = "- Name: \"\u{7f}foo_NULL_THUNK_DATA\"\n- '\u{9b}\u{fffe}'\n- {\"\u{7f}k\": v}\n";
    let expected = ayaml::Value::Sequence(vec![
        single("Name", "\u{7f}foo_NULL_THUNK_DATA"),
        ayaml::Value::from("\u{9b}\u{fffe}"),
        single("\u{7f}k", "v"),
    ]);
    assert_eq!(ayaml::parse_value(input).expect("default"), expected);
    #[cfg(feature = "libyaml-compat")]
    assert_eq!(
        ayaml::compat::from_str::<ayaml::Value>(input).expect("compat"),
        expected
    );
    assert_eq!(
        ayaml::from_str_borrowed::<ayaml::Value>(input).expect("typed"),
        expected
    );
    // A multi-line quoted scalar holds them on any line.
    assert_eq!(
        ayaml::parse_value("\"a\n  \u{7f}b\"\n").expect("multi-line"),
        "a \u{7f}b"
    );
    for input in [
        "a\u{7f}\n",
        "a: b # \u{7f}\n",
        "a: |\n  \u{7f}\n",
        "&a\u{7f} b\n",
        "\"a\" \u{9b}\n",
        "\"a\"\u{7f}: b\n",
        "a: \"b\"\n# \u{fffe}",
        "\"a\u{1}\"\n",
        "'a\u{0}'\n",
    ] {
        let error = ayaml::parse_value(input).expect_err(input);
        assert_eq!(error.kind(), ayaml::ErrorKind::Scanner, "{input:?}");
        assert!(
            error.to_string().contains("non-printable"),
            "{input:?}: {error}"
        );
    }
    // The emitter escapes them on output.
    let text = ayaml::to_string_value(&expected).expect("emit");
    assert!(!text.contains(['\u{7f}', '\u{9b}', '\u{fffe}']), "{text:?}");
    assert_eq!(ayaml::parse_value(&text).expect("reparse"), expected);
}

/// Quotes, flow indicators, and properties inside a plain key are
/// content, so the block-scalar owner heuristic finds the key's `:`. Found
/// by the emit fuzz target.
#[test]
fn flow_indicators_inside_plain_keys_are_content() {
    let single = |key: ayaml::Value, value: ayaml::Value| {
        let mut mapping = ayaml::Mapping::new();
        mapping.insert(key, value);
        ayaml::Value::Mapping(mapping)
    };
    let inner = single(ayaml::Value::from("a [b"), ayaml::Value::from("\n"));
    let expected = single(single(inner, ayaml::Value::Null), ayaml::Value::Null);
    assert_eq!(
        ayaml::parse_value("? ? a [b: |+\n\n  :\n:\n").expect("parse"),
        expected
    );
    assert_eq!(parse("a {b: |\n  x\n"), json!({ "a {b": "x\n" }));
    assert_eq!(parse("- a 'b: |\n   x\n"), json!([{ "a 'b": "x\n" }]));
    assert_eq!(parse("a &b: |\n  x\n"), json!({ "a &b": "x\n" }));
}

/// In a zero-indented root block scalar, a column-0 `#` line is content
/// in YAML 1.2.2 (DK3J), whether or not the header follows `---` or a BOM.
#[test]
fn column_zero_hash_lines_in_root_block_scalars_are_content() {
    assert_eq!(parse(">\n#\n"), json!("#\n"));
    assert_eq!(parse("|\n#x\n"), json!("#x\n"));
    assert_eq!(parse("|\nfoo\n# c\n"), json!("foo\n# c\n"));
    assert_eq!(parse("--- |\n#x\n"), json!("#x\n"));
    assert_eq!(parse("\u{feff}>\n#\n"), json!("#\n"));
    assert_eq!(parse("--- |\n  x\n# c\n"), json!("x\n"));
}

/// libyaml indents a root block scalar by at least one space, so in compat a
/// column-0 `#` line before any content is a comment.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_column_zero_hash_lines_end_root_block_scalars() {
    for input in [
        ">\n#\n",
        "|\n#x\n",
        "--- |\n#x\n",
        "--- |\n\n#x\n",
        "--- |\n#a\n#b\n",
        "\u{feff}>\n#\n",
        "\u{feff}>\r#",
    ] {
        assert_eq!(compat(input), json!(""), "{input:?}");
    }
    // After zero-indented content, which libyaml rejects, the spec reading
    // stays (DK3J).
    assert_eq!(
        compat("--- >\nline1\n# no comment\nline3\n"),
        json!("line1 # no comment line3\n")
    );
}

/// In compat, the block-scalar owner heuristic ends an anchor or alias
/// name where the compat scanner does, before a `?` or before a `:` followed
/// by more name text. `&-:: >` is then the key `:`, whose scalar must be
/// indented. Found by fuzzing.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_owner_heuristic_ends_anchors_like_the_scanner() {
    assert_eq!(compat("&-:: >\n(0: ("), json!({ ":": "", "(0": "(" }));
    assert_eq!(compat("&-:: >\n x\n"), json!({ ":": "x\n" }));
    assert_eq!(compat("&a?b: |\n x\n"), json!({ "?b": "x\n" }));
    assert_eq!(compat("&a:b: >\n x\n"), json!({ ":b": "x\n" }));
}

/// What a block scalar in a one-entry mapping under a sequence reads as, in both modes.
#[cfg(feature = "libyaml-compat")]
fn block_scalar_under_key(input: &str) -> [ayaml::Value; 2] {
    let read = |value: ayaml::Value| {
        value[0]
            .as_mapping()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .clone()
    };
    [
        read(ayaml::parse_value(input).unwrap_or_else(|error| panic!("{input:?}: {error}"))),
        read(ayaml::compat::from_str(input).unwrap_or_else(|error| panic!("{input:?}: {error}"))),
    ]
}

#[cfg(feature = "libyaml-compat")]
fn rejected_in_both_modes(input: &str) {
    rejects(input);
    assert!(
        ayaml::compat::from_str::<ayaml::Value>(input).is_err(),
        "{input:?} should be rejected in compat mode"
    );
}

/// In a flow collection a `:` doesn't end a name, as it does in block context, so the check on a
/// block scalar's parent has to read the name to the same place.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_owner_heuristic_keeps_a_colon_in_a_flow_name() {
    let text = ayaml::Value::String("x\n".into());
    assert_eq!(
        block_scalar_under_key("- [&a:\"b]: |1\n   x\n"),
        [text.clone(), text]
    );
    rejected_in_both_modes("- [&a:\"b]: |\n  x\n");
}

/// Past the first plain text in a flow collection, a quote is content, so it can't hide the key
/// that a block scalar belongs to.
#[cfg(feature = "libyaml-compat")]
#[test]
fn a_quote_inside_flow_plain_text_opens_nothing() {
    let text = ayaml::Value::String("x\n".into());
    let keys = [
        "[a\"b]",
        "[a'b]",
        "[a b\"c]",
        "[a, b\"c]",
        "{a\"b: c}",
        "[? \"\": :\"]",
        // A quoted scalar in a flow collection still hides what it holds.
        "{\"a\":\"b]c\"}",
        "['a]']",
        "[a, \"b]c\"]",
        // And so does one after a `:`, a `,`, a `?`, or the start or end of a collection, past
        // plain text.
        "{\"a\":\"]: \"}",
        "[a, \"]: \"]",
        "{a: [\"]: \"]}",
        "{[a]: \"]: \"}",
        "[a: \"]: \"]",
        "{a: \"b # c\"}",
        "{a: 'b # c'}",
        "{a: \"[x\"}",
        "{a: \"x, \"}",
        "{? \"]: \": b}",
        // A property between them changes nothing.
        "{\"a\":!t \"]: \"}",
        "{\"a\":&b \"]: \"}",
        "{a: !t \"b # c\"}",
        // A blank may come before the `:` of a key that takes an adjacent value.
        "{\"a\" :\",\"}",
        "{\"a\" :\"]: \"}",
        "{\"a\" :\"[x\"}",
        "{\"a\"\t:\"]: \"}",
        "{[a] :\"]: \"}",
        "['a' :\"]: \"]",
        "{[a]:\"]: \"}",
        // A plain scalar ends at a `:` before a blank, even one that looks like part of a name.
        "[x &a: \"]: \"]",
        "[x !a: \"]: \"]",
        "{x *a: \"]: \"}",
        "[x &a?: \"]: \"]",
        "{a:\"b: c}",
        "{a:!x: \"]: \"}",
        // In a name, a `:` before more text doesn't end it, and nothing but a blank or a flow
        // indicator does.
        "[&a:b \"]: \"]",
        // A verbatim tag holds what would otherwise end it.
        "[[!<t:]> \"]: \"]]",
        "{[!<t:]> \"}: \"]}",
        "{!<t:]> : }",
        "[!<t:]> : ]",
        // A second `:` right after the first starts plain text, so a quote after it opens nothing.
        "{\"a\"::\"b}",
        "{','::'b}",
        // A tab ends what a blank ends.
        "{a:\t\"]: \"}",
        "[&a\t\"]: \"]",
        // So does what ends a flow entry, for a name.
        "[&a,\"]: \"]",
        "{&a}",
        // A quoted scalar ends at its own closing quote.
        "\"a\\\": b\"",
        "[\"\\\"]: \"]",
        "'\\'",
        "['\\']",
        "'a'': b'",
        "['a'']: ']",
    ];
    for key in keys {
        assert_eq!(
            block_scalar_under_key(&format!("- {key}: |1\n   x\n")),
            [text.clone(), text.clone()],
            "{key}"
        );
        let deeper = format!("- - {key}: |1\n     x\n");
        for value in [
            ayaml::parse_value(&deeper).unwrap_or_else(|error| panic!("{deeper:?}: {error}")),
            ayaml::compat::from_str(&deeper).unwrap_or_else(|error| panic!("{deeper:?}: {error}")),
        ] {
            let read = value[0][0].as_mapping().unwrap().values().next().unwrap();
            assert_eq!(*read, text, "{deeper:?}");
        }
        rejected_in_both_modes(&format!("- {key}: |\n  x\n"));
    }
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn a_block_scalar_finds_its_key_past_a_name_or_a_tab() {
    let text = ayaml::Value::String("x\n".into());
    assert_eq!(
        block_scalar_under_key("- a:\t|1\n   x\n"),
        [text.clone(), text.clone()]
    );
    // Only compat ends these names, so only there is what follows them a key.
    for input in [
        "- &a:b: |1\n   x\n",
        "- &a?b: |1\n   x\n",
        "- &a?b \"c: |1\n   x\n",
    ] {
        let value: ayaml::Value =
            ayaml::compat::from_str(input).unwrap_or_else(|error| panic!("{input:?}: {error}"));
        let read = value[0].as_mapping().unwrap().values().next();
        assert_eq!(read, Some(&text), "{input:?}");
    }
    for input in ["- &a:b: |1\n   x\n", "- &a?b: |1\n   x\n"] {
        assert_eq!(
            ayaml::parse_value(input).unwrap()[0],
            ayaml::Value::String("  x\n".into()),
            "{input:?}"
        );
    }
    // No mode ends a tag there.
    for input in ["- !a:b: |1\n   x\n", "- !a?b: |1\n   x\n"] {
        for value in [
            ayaml::parse_value(input).unwrap(),
            ayaml::compat::from_str(input).unwrap(),
        ] {
            let ayaml::Value::Tagged(tagged) = &value[0] else {
                panic!("{input:?}: {value:?}");
            };
            assert_eq!(
                tagged.value,
                ayaml::Value::String("  x\n".into()),
                "{input:?}"
            );
        }
    }
}

/// NEL, LS, and PS are ordinary characters in both modes, and these forms would read wrongly, or
/// not at all, if either mode took one for a line break or for the end of a name.
#[cfg(feature = "libyaml-compat")]
#[test]
fn a_line_break_character_is_content_where_it_is_written() {
    for (character, escape) in [('\u{85}', "\\N"), ('\u{2028}', "\\L"), ('\u{2029}', "\\P")] {
        for (input, meaning) in [
            (format!("k: a{character}b"), format!("k: \"a{escape}b\"")),
            (
                format!("k: a b{character}c"),
                format!("k: \"a b{escape}c\""),
            ),
            (format!("|\n a{character}b"), format!("\"a{escape}b\"")),
            (format!("[a{character}b]"), format!("[\"a{escape}b\"]")),
            (format!("{character}a"), format!("\"{escape}a\"")),
            (format!("a{character}\nb"), format!("\"a{escape} b\"")),
            (format!("a{character}b: c"), format!("\"a{escape}b\": c")),
            (
                format!("- a b{character}c: d"),
                format!("- \"a b{escape}c\": d"),
            ),
            // The name doesn't end there, so the alias names the anchor, and no text is left.
            (
                format!("k: &a{character}b x\nj: *a{character}b"),
                "k: x\nj: x".to_owned(),
            ),
            (
                format!("[&a{character}b x, *a{character}b]"),
                "[x, x]".to_owned(),
            ),
        ] {
            let expected = ayaml::parse_value(&meaning).unwrap();
            let read =
                ayaml::parse_value(&input).unwrap_or_else(|error| panic!("{input:?}: {error}"));
            assert_eq!(read, expected, "{input:?} by default");
            let read: ayaml::Value = ayaml::compat::from_str(&input)
                .unwrap_or_else(|error| panic!("{input:?}: {error}"));
            assert_eq!(read, expected, "{input:?} in compat");
        }
    }
}

/// A name may end in `:` before any separator, and a verbatim tag keeps its text as written.
#[cfg(feature = "libyaml-compat")]
#[test]
fn a_name_before_a_separator_and_a_verbatim_tag_read_alike_in_both_modes() {
    for (input, meaning) in [
        ("&a:\tb", "b"),
        ("&a:\rb", "b"),
        ("[&a:]", "[~]"),
        // A byte order mark that starts the stream changes nothing about the name.
        ("\u{feff}&a: b", "b"),
    ] {
        let expected = ayaml::parse_value(meaning).unwrap();
        assert_eq!(ayaml::parse_value(input).unwrap(), expected, "{input:?}");
        let read: ayaml::Value = ayaml::compat::from_str(input).unwrap();
        assert_eq!(read, expected, "{input:?} in compat");
    }
    // Compat reads a byte order mark in a comment, and the name before it still ends in `:`.
    let read: ayaml::Value = ayaml::compat::from_str("&a: b\n# \u{feff}").unwrap();
    assert_eq!(read, ayaml::Value::String("b".into()));
    let tag_of = |value: ayaml::Value| match value {
        ayaml::Value::Tagged(tagged) => tagged.tag.to_string(),
        other => panic!("{other:?}"),
    };
    // Valid forms only: AY-025 will reject a verbatim tag that is neither a local nor a global
    // tag, such as `!<Foo>`.
    for (input, tag) in [
        ("!<tag:Example.com,1:Foo> v", "tag:Example.com,1:Foo"),
        ("!<!A> x", "!A"),
        ("!<!a#b> x", "!a#b"),
        ("!<tag:x,1:y#z> v", "tag:x,1:y#z"),
    ] {
        assert_eq!(tag_of(ayaml::parse_value(input).unwrap()), tag, "{input:?}");
        assert_eq!(
            tag_of(ayaml::compat::from_str(input).unwrap()),
            tag,
            "{input:?} in compat"
        );
    }
}

/// In compat a flow `?` is an indicator whatever follows it, so a quote right after it opens, and
/// by default `?` followed by more text starts plain text, so one there doesn't.
#[cfg(feature = "libyaml-compat")]
#[test]
fn a_question_mark_before_a_quote_is_read_as_each_mode_reads_it() {
    let text = ayaml::Value::String("x\n".into());
    let compat = |input: &str| -> ayaml::Value {
        let value: ayaml::Value =
            ayaml::compat::from_str(input).unwrap_or_else(|error| panic!("{input:?}: {error}"));
        value[0]
            .as_mapping()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .clone()
    };
    for key in ["[?\"]: \"]", "{?\"a]: b\"}", "{?\"]: \": b}"] {
        assert_eq!(compat(&format!("- {key}: |1\n   x\n")), text, "{key}");
        let shallow = format!("- {key}: |\n  x\n");
        assert!(
            ayaml::compat::from_str::<ayaml::Value>(&shallow).is_err(),
            "{shallow:?}"
        );
        rejects(&format!("- {key}: |1\n   x\n"));
    }
    let input = "- {?\"a: b}: |1\n   x\n";
    let value = ayaml::parse_value(input).unwrap_or_else(|error| panic!("{input:?}: {error}"));
    assert_eq!(value[0].as_mapping().unwrap().values().next(), Some(&text));
    rejects("- {?\"a: b}: |\n  x\n");
}

/// The node can start the next line, so what ends the key on this one takes reading its comment
/// from where the scanner does.
#[cfg(feature = "libyaml-compat")]
#[test]
fn a_comment_after_a_key_is_found_as_the_scanner_finds_it() {
    let text = ayaml::Value::String("x\n".into());
    for line in [
        "- [' # ']:",
        "- [\" # \"]:",
        "- [\"]: \"]: # c \"",
        "- {\", \"}: # c \"",
        "- {a: ' # '}: # c",
        "- [a]: # c ]",
    ] {
        let input = format!("{line}\n    |1\n   x\n");
        assert_eq!(
            block_scalar_under_key(&input),
            [text.clone(), text.clone()],
            "{line}"
        );
    }
}

/// Only a blank before a `#` starts a comment, and only outside a quoted scalar, so what comes
/// next is still on the line that says who owns it.
#[test]
fn a_line_that_only_looks_commented_still_owns_what_follows() {
    let key = ayaml::Value::String("x\n".into());
    for (input, value) in [
        ("- 'a#b':\n    |1\n   x\n", &key),
        ("- \"a # b\":\n    |1\n   x\n", &key),
        ("- a#b:\n    |1\n   x\n", &key),
    ] {
        let read = ayaml::parse_value(input).unwrap_or_else(|error| panic!("{input:?}: {error}"));
        assert_eq!(
            read[0].as_mapping().unwrap().values().next(),
            Some(value),
            "{input:?}"
        );
    }
}

/// Valid forms next to known wrong rejections, each read as what it means in both modes. A fix that
/// goes too far, or a new check that catches one of these, fails here.
#[cfg(feature = "libyaml-compat")]
#[test]
fn valid_forms_beside_wrong_rejections_keep_their_reading() {
    for (input, meaning) in [
        // A block scalar past its parent.
        ("a:\n |\n  x", "a: \"x\""),
        ("b: 1\na:\n >\n  x", "{b: 1, a: \"x\"}"),
        // A tab before a plain key's `:`, or with no `: ` after it.
        ("a\t: 1", "{a: 1}"),
        ("b: 1\na\t: 1", "{b: 1, a: 1}"),
        ("k:\t\"a:b\"", "{k: \"a:b\"}"),
        ("k:\t[a:b]", "{k: [\"a:b\"]}"),
        ("k:\t{a:b}", "{k: {\"a:b\": ~}}"),
        ("---\tb", "b"),
        ("--- a\t# c", "a"),
        // A flow mapping's empty key.
        ("{? ,}", "{~: ~}"),
        // A comment's colon on a flow scalar's line.
        ("[a\n b # c: d\n]", "[\"a b\"]"),
        ("{a: b\n c # d: e\n}", "{a: \"b c\"}"),
        // Text like a marker on a flow scalar's line.
        ("[a\n---b]", "[\"a ---b\"]"),
        ("k: [a\n  ...b]", "{k: [\"a ...b\"]}"),
        ("{a\n  ---b\n}", "{\"a ---b\": ~}"),
        // These lines are past the scalar's parent.
        ("- \"a\n b\"", "[\"a b\"]"),
        ("k: \"a\n b\"", "{k: \"a b\"}"),
        // Characters that YAML 1.2 doesn't break a line on.
        ("'a\u{2028}b'", "\"a\\Lb\""),
        ("'a\u{2029}b'", "\"a\\Pb\""),
        ("'a\u{85}b'", "\"a\\Nb\""),
        ("k: 'a\u{2028}b'", "{k: \"a\\Lb\"}"),
        ("k: \"a\u{2028}b\"", "{k: \"a\\Lb\"}"),
        ("# a\u{85}b\nk: 1", "{k: 1}"),
        ("k: 1 # a\u{85}b", "{k: 1}"),
        // A comment with nothing before it.
        ("# c\na: 1", "{a: 1}"),
        ("#", "~"),
        // A name may end in `:`, and a comment may follow it.
        ("&a: # c\nb", "b"),
        ("x: &a: # c\n  b", "{x: b}"),
        // A core tag however it's written.
        ("!<tag:yaml.org,2002:str> 1", "\"1\""),
        ("!<tag:yaml.org,2002:int> 1", "1"),
        ("!!%69nt 1", "1"),
        // A verbatim tag may hold a `!`.
        ("!<!a> x", "!a x"),
        ("!<!a!b> x", "%TAG !e! !a!\n---\n!e!b x"),
    ] {
        let expected =
            ayaml::parse_value(meaning).unwrap_or_else(|error| panic!("{meaning:?}: {error}"));
        assert_eq!(
            ayaml::parse_value(input).unwrap_or_else(|error| panic!("{input:?}: {error}")),
            expected,
            "{input:?}"
        );
        assert_eq!(
            ayaml::compat::from_str::<ayaml::Value>(input)
                .unwrap_or_else(|error| panic!("{input:?} in compat: {error}")),
            expected,
            "{input:?} in compat"
        );
    }
}

/// Each mode's reading of `input`: the value, or the error's text.
fn readings(input: &str) -> Vec<(&'static str, Result<ayaml::Value, String>)> {
    vec![
        (
            "default",
            ayaml::parse_value(input).map_err(|error| error.to_string()),
        ),
        #[cfg(feature = "libyaml-compat")]
        (
            "compat",
            ayaml::compat::from_str::<ayaml::Value>(input).map_err(|error| error.to_string()),
        ),
    ]
}

/// A tab separates wherever it doesn't indent a block collection (YAML 1.2.2 section 6.1): after a
/// key or a node property, and before a flow collection, a quoted scalar, a block scalar's header,
/// or a comment, though a `: ` follows in one of them. Each reads as it does with a space, in
/// each mode.
#[test]
fn a_tab_separates_where_it_does_not_indent() {
    for (input, with_spaces) in [
        // After `key:`, before a flow collection or a quoted value holding a colon, and after a
        // quoted value before a comment holding one.
        ("key:\t\"a: b\"", "key: \"a: b\""),
        ("key:\t'a: b'", "key: 'a: b'"),
        ("key:\t{a: 1}", "key: {a: 1}"),
        ("key:\t[a: 1]", "key: [a: 1]"),
        ("a: \"1\"\t# note: x", "a: \"1\" # note: x"),
        // Before `:` after a quoted, empty, alias, or flow key, or a node property alone.
        ("\"a\"\t: 1", "\"a\" : 1"),
        ("''\t: 1", "'' : 1"),
        ("- &a x\n- *a\t: 2", "- &a x\n- *a : 2"),
        ("[1]\t: x", "[1] : x"),
        ("{a: 1}\t: x", "{a: 1} : x"),
        ("&x\t: 1", "&x : 1"),
        ("!t\t: 1", "!t : 1"),
        // Between a node property and a key or a flow collection, and after `---` before a flow
        // collection or a property.
        ("!t\ta: 1", "!t a: 1"),
        ("- &x\ta: 1", "- &x a: 1"),
        ("&x\t[1]", "&x [1]"),
        ("---\t[1]", "--- [1]"),
        ("---\t{a: 1}", "--- {a: 1}"),
        ("---\t!:", "--- !:"),
        // Where the only later `: ` is in a comment, whatever comes before the tab.
        ("- a\t# c: d", "- a # c: d"),
        ("k:\t# c: d\n  v", "k: # c: d\n  v"),
        ("-\t# c: d\n  x", "- # c: d\n  x"),
        ("\t# c: d\nx", " # c: d\nx"),
        ("a: 1\n\t# c: d\nb: 2", "a: 1\n # c: d\nb: 2"),
        ("a\n...\t# c: d", "a\n... # c: d"),
        // After `---`, `-`, `?`, or `:`, or at a line's start, before a quoted scalar holding `: `
        // or a flow collection.
        ("---\t'a: b'", "--- 'a: b'"),
        ("---\t\"a: b\"", "--- \"a: b\""),
        ("-\t[1]", "- [1]"),
        ("-\t'a: b'", "- 'a: b'"),
        ("-\t\"a: b\"", "- \"a: b\""),
        ("\t[1]", " [1]"),
        ("\t{a: b}", " {a: b}"),
        ("\t'a: b'", " 'a: b'"),
        ("? a\n:\t'b: c'", "? a\n: 'b: c'"),
        ("?\t[a]\n:\tb", "? [a]\n: b"),
        // Or one that goes on to the next line, so it can't be a key.
        ("-\t\"a: b\n  c\"", "- \"a: b\n  c\""),
        ("k:\t'a: b\n  c'", "k: 'a: b\n  c'"),
        ("-\t[a: b,\n  c]", "- [a: b,\n  c]"),
        // Before a block scalar's header, with a `: ` in its comment.
        ("---\t>\n a", "--- >\n a"),
        ("-\t| # c: d\n  a", "- | # c: d\n  a"),
        // A property's name may end in `:`.
        ("-\t&a: b", "- &a: b"),
        ("&x:\ta: b", "&x: a: b"),
    ] {
        for ((mode, read), (_, expected)) in readings(input).into_iter().zip(readings(with_spaces))
        {
            let expected = expected.unwrap_or_else(|error| panic!("{with_spaces:?}: {error}"));
            assert_eq!(read, Ok(expected), "{input:?} in {mode}");
        }
    }
    // A block scalar's header is judged on its own after a tab too, so a comment with no blank
    // before it is the error, though a `: ` follows.
    for (input, with_spaces) in [
        ("---\t|#: c\n x", "--- |#: c\n x"),
        ("-\t>#: c\n  x", "- >#: c\n  x"),
    ] {
        for ((mode, read), (_, expected)) in readings(input).into_iter().zip(readings(with_spaces))
        {
            assert!(
                expected
                    .as_ref()
                    .is_err_and(|error| error.contains("comment")),
                "{with_spaces:?} in {mode}: {expected:?}"
            );
            assert_eq!(read, expected, "{input:?} in {mode}");
        }
    }
}

/// A tab still can't indent a block collection: at a line's start, or after a `-`, `?`, or `:`
/// indicator or `---`, before a block indicator or an implicit key, whichever blank follows that
/// indicator.
#[test]
fn a_tab_cannot_indent_a_block_collection() {
    for (input, line, column) in [
        ("\ta: 1", 1, 1),
        ("\t- a", 1, 1),
        ("\t? a", 1, 1),
        ("\t: a", 1, 1),
        ("\t[1]: x", 1, 1),
        ("-\ta: 1", 1, 2),
        ("-\t- a", 1, 2),
        ("- \t- a", 1, 3),
        ("?\ta: 1", 1, 2),
        ("-\t[1]: x", 1, 2),
        ("-\t'a': b", 1, 2),
        ("-\t\"a\"\t: b", 1, 2),
        ("-\t&a b: c", 1, 2),
        ("-\t!t : c", 1, 2),
        ("- a\n-\t\"b\": c", 2, 2),
        ("---\ta: 1", 1, 4),
        ("key:\ta: b", 1, 5),
        ("key:\t- a", 1, 5),
        ("\u{feff}\t- a", 1, 1),
        // A tab after the indicator, too.
        ("\t-\tb", 1, 1),
        ("\t?\tb", 1, 1),
        ("-\t-\tb", 1, 2),
        ("k:\n\t-\tb", 2, 1),
        // After a document end marker, as after `---`.
        ("...\ta: 1", 1, 4),
        ("--- a\n...\ta: 1", 2, 4),
    ] {
        let expected = format!(
            "tab cannot separate a block structural indicator at line {line} column {column}"
        );
        for (mode, read) in readings(input) {
            assert_eq!(read, Err(expected.clone()), "{input:?} in {mode}");
        }
    }
    // A line's leading byte order marks are counted again on each line, after a tab on an earlier
    // one. The modes count the mark's column differently, so only the line is checked.
    for input in ["a:\tb\n...\n\u{feff}\t- c", "-\ta\n...\n\u{feff}\tc: d"] {
        for (mode, read) in readings(input) {
            let error = read.expect_err(input);
            assert!(
                error.starts_with("tab cannot separate a block structural indicator at line 3 "),
                "{input:?} in {mode}: {error}"
            );
        }
    }
    // In compat, an anchor's name ends at a `?`, or at a `:` with more text after it, and an alias
    // gives up a `:` that ends its name. Then what follows is a key, so the tab indents a mapping.
    // By default the name runs on, so the tab only separates it.
    #[cfg(feature = "libyaml-compat")]
    for (input, line, column) in [
        ("-\t&a:b: c", 1, 2),
        ("-\t&a?b: c", 1, 2),
        ("- &a x\n-\t*a: b", 2, 2),
    ] {
        let expected = format!(
            "tab cannot separate a block structural indicator at line {line} column {column}"
        );
        let read =
            ayaml::compat::from_str::<ayaml::Value>(input).map_err(|error| error.to_string());
        assert_eq!(read, Err(expected), "{input:?}");
    }
    assert_eq!(
        ayaml::parse_value("-\t&a:b: c").expect("a name"),
        ayaml::parse_value("- c").expect("a sequence")
    );
}

/// A closing quote may follow tabs alone on its line, as a closing `]` or `}` may. Only the
/// scalar's own quote closes it, and `''` is a quote in a single-quoted scalar's text.
#[test]
fn a_closing_quote_may_follow_tabs_alone() {
    for (input, with_spaces) in [
        ("k: 'a\n\t'", "k: 'a\n '"),
        ("k: \"a\n\t\"", "k: \"a\n \""),
        ("- 'a\n\t\t'", "- 'a\n '"),
        ("k: 'a\n\n\t'", "k: 'a\n\n '"),
        ("k: \"a\\\n\t\"", "k: \"a\\\n \""),
    ] {
        for ((mode, read), (_, expected)) in readings(input).into_iter().zip(readings(with_spaces))
        {
            let expected = expected.unwrap_or_else(|error| panic!("{with_spaces:?}: {error}"));
            assert_eq!(read, Ok(expected), "{input:?} in {mode}");
        }
    }
    for input in [
        "k: 'a\n\tb'",
        "k: 'a\n\t''b'",
        "k: 'a\n\t\"'",
        "k: \"a\n\t'\"",
    ] {
        for (mode, read) in readings(input) {
            let error = read.expect_err(input);
            assert!(
                error.starts_with("tabs cannot be the only quoted-scalar indentation at line 2"),
                "{input:?} in {mode}: {error}"
            );
        }
    }
}

/// A comment may follow a `%TAG` directive's prefix, as it may follow `%YAML`'s version, and
/// the directive then reads as it does without it. Only a comment may follow the prefix.
#[test]
fn a_comment_may_follow_a_tag_directive() {
    for (input, without) in [
        ("%TAG ! tag:x: # c\n--- !a 1", "%TAG ! tag:x:\n--- !a 1"),
        (
            "%TAG !e! tag:e: #c\n---\n!e!a b",
            "%TAG !e! tag:e:\n---\n!e!a b",
        ),
        (
            "%TAG !! tag:y:\t#: c\n--- !!a b",
            "%TAG !! tag:y:\n--- !!a b",
        ),
        ("%TAG ! tag:x: #\n--- !a 1", "%TAG ! tag:x:\n--- !a 1"),
    ] {
        for ((mode, read), (_, expected)) in readings(input).into_iter().zip(readings(without)) {
            let expected = expected.unwrap_or_else(|error| panic!("{without:?}: {error}"));
            assert_eq!(read, Ok(expected), "{input:?} in {mode}");
        }
    }
    // A comment may hold a byte order mark in compat, as any comment may there, and the directive
    // still may not. By default no comment may hold one.
    #[cfg(feature = "libyaml-compat")]
    for (input, without) in [
        (
            "%TAG ! tag:x: #\u{feff}\n--- !a 1",
            "%TAG ! tag:x:\n--- !a 1",
        ),
        ("%YAML 1.2 # a\u{feff}\n--- a", "%YAML 1.2\n--- a"),
        ("%FOO bar\t#\u{feff}\n--- a", "%FOO bar\n--- a"),
    ] {
        let read =
            ayaml::compat::from_str::<ayaml::Value>(input).map_err(|error| error.to_string());
        let expected = ayaml::compat::from_str::<ayaml::Value>(without).expect("no mark");
        assert_eq!(read, Ok(expected), "{input:?}");
        let error = ayaml::parse_value(input).expect_err(input);
        assert!(
            error
                .to_string()
                .starts_with("a directive cannot contain a byte order mark"),
            "{input:?}: {error}"
        );
    }
    for input in [
        "%TAG ! tag:x:\u{feff}\n--- a",
        "%TAG ! tag:x:\u{feff} # c\n--- a",
        "%FOO b#\u{feff}\n--- a",
    ] {
        for (mode, read) in readings(input) {
            let error = read.expect_err(input);
            assert!(
                error.starts_with("a directive cannot contain a byte order mark"),
                "{input:?} in {mode}: {error}"
            );
        }
    }
    for input in [
        "%TAG ! tag:x: x # c\n--- a",
        "%TAG ! tag:x: x\n--- a",
        "%TAG ! tag:x: x#c\n--- a",
        "%TAG ! # c\n--- a",
    ] {
        for (mode, read) in readings(input) {
            let error = read.expect_err(input);
            assert!(
                error.starts_with("%TAG requires a handle and a prefix at line 1"),
                "{input:?} in {mode}: {error}"
            );
        }
    }
}

/// Each input reads as its `equal` does, in each mode.
fn reads_as(cases: &[(&str, &str)]) {
    for &(input, equal) in cases {
        for ((mode, read), (_, expected)) in readings(input).into_iter().zip(readings(equal)) {
            let expected = expected.unwrap_or_else(|error| panic!("{equal:?}: {error}"));
            assert_eq!(read, Ok(expected), "{input:?} in {mode}");
        }
    }
}

/// Each input is refused with an error that starts with its message, in each mode.
fn refused_with(cases: &[(&str, &str)]) {
    for &(input, message) in cases {
        for (mode, read) in readings(input) {
            let error = read.expect_err(input);
            assert!(error.starts_with(message), "{input:?} in {mode}: {error}");
        }
    }
}

/// A plain scalar's line may start with `---` or `...` unless that is a document marker: at
/// column 0, with whitespace or the line's end after it (YAML 1.2.2 production 206).
#[test]
fn a_plain_scalar_line_may_start_with_marker_text() {
    reads_as(&[
        ("a\n ---", "'a ---'"),
        ("a\n ---x", "'a ---x'"),
        ("a\n---b", "'a ---b'"),
        ("a\n  ...", "'a ...'"),
        ("a\n ...\nb", "'a ... b'"),
        ("1\n...)", "'1 ...)'"),
        ("a\n\t---", "'a ---'"),
        ("key: a\n  ---", "key: 'a ---'"),
        ("k: a\n  ...and so on", "k: 'a ...and so on'"),
        ("- a\n  --- b", "- 'a --- b'"),
        // A marker still ends it.
        ("a\n...", "a"),
        ("a\n... # c", "a"),
    ]);
    for input in ["a\n---", "a\n--- b", "a\n---\tb", "a\n---\n b"] {
        let documents = ayaml::parse_documents(input).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(documents.len(), 2, "{input:?}");
    }
    // `...` and then a mark isn't a marker either. Compat reads the mark as text, as libyaml does,
    // and default mode refuses a mark there.
    #[cfg(feature = "libyaml-compat")]
    assert_eq!(
        ayaml::compat::from_str::<ayaml::Value>("0\n...\u{feff}").unwrap(),
        ayaml::Value::from("0 ...\u{feff}")
    );
    assert_eq!(
        ayaml::parse_value("0\n...\u{feff}")
            .unwrap_err()
            .to_string(),
        "a byte order mark can only start a document at line 2 column 4"
    );
}

/// A `:` in a comment isn't a mapping indicator, so a plain scalar's line may hold one there.
#[test]
fn a_comment_colon_does_not_end_a_plain_scalar() {
    reads_as(&[
        ("a\nb #c: d", "'a b'"),
        ("a\n b\t# c: d", "'a b'"),
        ("k: a\n  b # note: d", "k: 'a b'"),
        ("- a\n  b # c: d", "- 'a b'"),
        ("c\n| #:", "'c |'"),
        ("$\n? #:", "'$ ?'"),
        ("a\n ... # c: d", "'a ...'"),
        // A `:` before the comment still does.
        ("k: a\nb: c # d: e", "{k: a, b: c}"),
    ]);
    // Without a blank before it, `#` is text, and the `: ` after it an indicator, so the line
    // isn't the scalar's.
    let after = "unexpected content after document";
    refused_with(&[
        ("a\nb#c: d", &format!("{after} at line 2 column 1")),
        ("k: a\n  b#c: d", &format!("{after} at line 2 column 3")),
    ]);
}

/// A node property may end in `:`, as in `&a:`, and that `:` isn't an indicator. So a plain
/// scalar after one, on a line that the node starts or after `---`, goes on at its line's
/// indentation in default mode, as it does in compat.
#[test]
fn a_plain_scalar_after_a_property_ending_in_a_colon_goes_on() {
    reads_as(&[
        ("&a: b\nc", "&a: 'b c'"),
        ("--- &a: b\nc", "--- &a: 'b c'"),
        ("!t: b\nc", "!t: 'b c'"),
        ("&a: b\n c", "&a: 'b c'"),
        ("- &a: b\n  c", "- &a: 'b c'"),
    ]);
    // A block indicator or a key before the scalar still holds its lines past them.
    refused_with(&[
        (
            "- &a: b\nc",
            "unexpected content after document at line 2 column 1",
        ),
        (
            "k: b\nc",
            "unexpected content after document at line 2 column 1",
        ),
        (
            "&a k: b\nc",
            "unexpected content after document at line 2 column 1",
        ),
    ]);
}

/// A quoted scalar on its own line, or in a flow collection that is, is measured against the
/// block collection that holds it, not the line it opens on (YAML 1.2.2 production 69). Its
/// lines need only pass that collection's column, as a flow collection's do.
#[test]
fn a_quoted_scalar_on_its_own_line_is_measured_against_its_parent() {
    reads_as(&[
        ("k:\n  'a\n b'", "k: 'a b'"),
        ("k:\n  \"a\n b\"", "k: 'a b'"),
        ("- \n    'a\n  b'", "- 'a b'"),
        ("  k:\n      'a\n     b'", "k: 'a b'"),
        ("'k':\n  'a\n b'", "k: 'a b'"),
        ("k:\n  !t 'a\n b'", "k: !t 'a b'"),
        ("k: !t\n  'a\n b'", "k: !t 'a b'"),
        ("k:\n  &x\n  'a\n b'", "k: 'a b'"),
        ("k:\n  'a\n \tb'", "k: 'a b'"),
        ("k: [\n    'a\n  b']", "k: ['a b']"),
        ("k: [\n    \"a\n b\"]", "k: ['a b']"),
        ("k: [[\n    'a\n b']]", "k: [['a b']]"),
        ("k: {\n    a: 'b\n  c'}", "k: {a: 'b c'}"),
        ("k: [{\n    a: 'b\n  c'}]", "k: [{a: 'b c'}]"),
        ("k:\n  [? \"x\n y\"]", "k: [? 'x y']"),
        ("k:\n  [&a \"x\n y\"]", "k: ['x y']"),
        ("k:\n  [a,\n b]", "k: [a, b]"),
        ("- k:\n    [a,\n   b]", "- k: [a, b]"),
    ]);
    // The lines still have to pass it, and only spaces indent them.
    let below = "quoted scalar continuation must be indented more than its parent";
    let tabs = "tabs cannot be the only quoted-scalar indentation";
    refused_with(&[
        ("k:\n  'a\nb'", &format!("{below} at line 3 column 1")),
        (
            "  k:\n      'a\n  b'",
            &format!("{below} at line 3 column 3"),
        ),
        ("k: [\n    'a\nb']", &format!("{below} at line 3 column 1")),
        ("k:\n  'a\n\tb'", &format!("{tabs} at line 3 column 1")),
        ("k:\n  'a\n\t\tb'", &format!("{tabs} at line 3 column 1")),
        // Each outermost flow collection has its own owner, so a later one isn't held to an
        // earlier one's.
        (
            "a: [\n    'x\n  y']\nb:\n  - [\n      'x\n  y']",
            &format!("{below} at line 7 column 3"),
        ),
    ]);
}

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

#[cfg(feature = "libyaml-compat")]
fn compat(input: &str) -> Json {
    let value = ayaml::compat::from_str::<ayaml::Value>(input)
        .unwrap_or_else(|error| panic!("{input:?}: {error}"));
    serde_json::to_value(&value).expect("JSON-compatible value")
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

/// Deserialize `input` through `from_str`, `from_str_borrowed`, and
/// `from_value`, in that order.
fn every_path<T>(input: &str) -> [Result<T, String>; 3]
where
    T: for<'de> serde::Deserialize<'de>,
{
    [
        ayaml::from_str::<T>(input).map_err(|error| error.to_string()),
        ayaml::from_str_borrowed::<T>(input).map_err(|error| error.to_string()),
        ayaml::parse_value(input)
            .and_then(ayaml::from_value::<T>)
            .map_err(|error| error.to_string()),
    ]
}

#[derive(Debug, PartialEq, serde::Deserialize)]
struct FieldA {
    a: u32,
    #[serde(default)]
    b: u32,
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

#[derive(Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
struct Defaults {
    a: u32,
    v: Vec<u32>,
    m: std::collections::BTreeMap<String, u32>,
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

#[derive(Debug, PartialEq, serde::Deserialize)]
enum Renamed {
    #[serde(rename = "true")]
    Yes,
    #[serde(rename = "1")]
    One,
    Plain,
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

fn merged_keys(input: &str, path: &str) -> Vec<String> {
    let mut value = ayaml::parse_value(input).expect("parse");
    value.apply_merge_keys().expect("merge");
    value[path]
        .as_mapping()
        .expect("mapping")
        .iter()
        .map(|(key, value)| format!("{}={value:?}", key.as_str().expect("string key")))
        .collect()
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

// ---------------------------------------------------------------------------
// Emitters
// ---------------------------------------------------------------------------

fn stream_limits() -> ayaml::Limits {
    ayaml::Limits::default().with_max_documents(16)
}

fn values(input: &str) -> Vec<ayaml::Value> {
    ayaml::parse_documents_with_limits(input, &stream_limits())
        .unwrap_or_else(|error| panic!("{input:?}: {error}"))
}

/// Emit the events of `input` with the collected and the lazy writer; both
/// must agree, and the output must describe the same documents.
fn emit_events(input: &str) -> String {
    let limits = stream_limits();
    let events = ayaml::parse_events_with_limits(input, &limits)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .unwrap_or_else(|error| panic!("{input:?}: {error}"));
    let output = ayaml::to_string_events_with_limits(&events, &limits).expect("emit events");
    let mut lazy = Vec::new();
    let stream = ayaml::parse_events_with_limits(input, &limits).expect("stream");
    ayaml::to_writer_event_stream_with_limits(&mut lazy, stream, &limits).expect("lazy emit");
    assert_eq!(String::from_utf8(lazy).expect("utf8"), output);
    assert_eq!(
        values(&output),
        values(input),
        "{input:?} became {output:?}"
    );
    output
}

fn emit_value(value: &ayaml::Value) -> String {
    let output = ayaml::to_string_value(value).expect("emit");
    assert_eq!(
        &ayaml::parse_value(&output).unwrap_or_else(|error| panic!("{output:?}: {error}")),
        value,
        "{output:?}"
    );
    output
}

/// Integer-like text outside the value model is quoted.
#[test]
fn out_of_range_integers_are_quoted() {
    for text in [
        "18446744073709551616",
        "-9223372036854775809",
        "0xFFFFFFFFFFFFFFFFFF",
    ] {
        let output = emit_value(&ayaml::Value::from(text));
        assert_eq!(output, format!("\"{text}\"\n"));
    }
}

/// `indent` must be between 2 and 9.
#[test]
fn indent_is_validated() {
    let value = ayaml::parse_value("a:\n  b: 1\n").expect("parse");
    for indent in [0, 1, 10] {
        let options = ayaml::EmitterOptions::default().with_indent(indent);
        let error = ayaml::to_string_value_with_options(&value, &options).expect_err("indent");
        assert_eq!(error.kind(), ayaml::ErrorKind::Emitter);
    }
}

/// The event emitter keeps empty plain scalars and negative
/// numbers plain.
#[test]
fn event_emitter_keeps_plain_scalars_plain() {
    assert_eq!(
        emit_events("a:\nb: -1\nc: [-.inf, ~]\n"),
        "a:\nb: -1\nc: [-.inf, ~]\n"
    );
    assert_eq!(emit_events("- \n- -1\n"), "-\n- -1\n");
    assert_eq!(emit_events("{a: , : b}\n"), "{a: , ? : b}\n");
    assert_eq!(emit_events("---\n"), "---\n");
}

/// Keep chomping keeps every trailing line break.
#[test]
fn event_emitter_keeps_trailing_line_breaks() {
    assert_eq!(emit_events("- |+\n  a\n\n\n- x\n"), "- |+\n  a\n\n\n- x\n");
    assert_eq!(emit_events("k: |+\n\n"), "k: |+\n\n");
}

/// Multi-line single-quoted values are written double-quoted.
#[test]
fn single_quotes_never_hold_line_breaks() {
    assert_eq!(emit_events("'a\n\n  b'\n"), "\"a\\nb\"\n");
    let options =
        ayaml::EmitterOptions::default().with_scalar_style(Some(ayaml::ScalarStyle::SingleQuoted));
    let value = ayaml::Value::from("a\nb\u{1}");
    let output = ayaml::to_string_value_with_options(&value, &options).expect("emit");
    assert_eq!(output, "\"a\\nb\\x01\"\n");
    assert_eq!(ayaml::parse_value(&output).expect("reparse"), value);
}

/// Anchored keys and alias keys (suite E76Z, 26DV, F6MC, X38W).
#[test]
fn alias_keys_round_trip() {
    assert_eq!(
        emit_events("&a a: &b b\n*b : *a\n"),
        "&a a: &b b\n*b : *a\n"
    );
    emit_events("? &a [x]\n: *a\n");
}

/// The lazy writer lays out block collections like the collected one.
#[test]
fn lazy_writer_layout() {
    assert_eq!(emit_events("- a\n- b\n"), "- a\n- b\n");
    assert_eq!(
        emit_events("a:\n  b: [1, 2]\n  c:\n  - x\n--- |\n  text\n"),
        "a:\n  b: [1, 2]\n  c:\n  - x\n---\n|\n  text\n"
    );
}

/// Block-scalar indentation indicators are kept.
#[test]
fn indentation_indicators_are_kept() {
    assert_eq!(
        emit_events("key: |1\n  explicit\n"),
        "key: |2\n   explicit\n"
    );
    // Folding cannot keep a leading space, so the literal style is used.
    assert_eq!(emit_events("- >2\n   folded\n"), "- |2\n   folded\n");
    assert_eq!(emit_events("- >\n  a\n  b\n"), "- >\n  a b\n");
}

/// Compact entries pad the `-` to the indentation step.
#[test]
fn compact_entries_follow_the_indent() {
    let value = ayaml::parse_value("- a: 1\n  b: |\n    x\n- - c\n  - d\n").expect("parse");
    let options = ayaml::EmitterOptions::default().with_indent(4);
    let output = ayaml::to_string_value_with_options(&value, &options).expect("emit");
    assert_eq!(output, "-   a: 1\n    b: |\n        x\n-   - c\n    - d\n");
    assert_eq!(ayaml::parse_value(&output).expect("reparse"), value);
}

/// A tag on a tagged value cannot be written.
#[test]
fn nested_tags_are_an_error() {
    let inner = ayaml::Value::from(ayaml::TaggedValue::new(
        ayaml::Tag::local("Y"),
        ayaml::Value::from(1_u64),
    ));
    let outer = ayaml::Value::from(ayaml::TaggedValue::new(ayaml::Tag::local("X"), inner));
    let error = ayaml::to_string_value(&outer).expect_err("nested tags");
    assert_eq!(error.kind(), ayaml::ErrorKind::Emitter);
}

/// A directive is written with NEL, LS, or PS in it, since they are
/// content and a directive has no escapes. An anchor holding a byte
/// order mark, which the parser rejects, cannot be written. Found by the
/// emit fuzz target.
#[test]
fn directives_keep_nel_ls_and_ps() {
    for separator in ['\u{85}', '\u{2028}', '\u{2029}'] {
        let output = emit_events(&format!("%FOO a{separator}b\n--- x\n"));
        assert!(
            output.starts_with(&format!("%FOO a{separator}b\n")),
            "{output:?}"
        );
    }
    let mut events: Vec<ayaml::event::Event<'_>> =
        ayaml::parse_events_with_limits("&a x\n", &stream_limits())
            .and_then(Iterator::collect)
            .expect("events");
    for event in &mut events {
        if let ayaml::event::Event::Scalar { anchor, .. } = event {
            *anchor = Some(ayaml::event::Anchor::new("a\u{feff}"));
        }
    }
    let error = ayaml::to_string_events_with_limits(&events, &stream_limits())
        .expect_err("an anchor with a byte order mark");
    assert_eq!(error.kind(), ayaml::ErrorKind::Emitter);
}

/// Tag suffixes are %-escaped.
#[test]
fn tag_suffixes_are_escaped() {
    let value = ayaml::Value::from(ayaml::TaggedValue::new(
        ayaml::Tag::local("my variant!%"),
        ayaml::Value::from(1_u64),
    ));
    assert_eq!(emit_value(&value), "!my%20variant%21%25 1\n");
}

/// A tagged null before a flow indicator is followed by a space.
#[test]
fn tagged_null_in_flow() {
    let value = ayaml::Value::Sequence(vec![
        ayaml::Value::from(ayaml::TaggedValue::new(
            ayaml::Tag::local("t"),
            ayaml::Value::Null,
        )),
        ayaml::Value::from("x"),
    ]);
    let options = ayaml::EmitterOptions::default().with_flow(true);
    let output = ayaml::to_string_value_with_options(&value, &options).expect("emit");
    assert_eq!(output, "[!t , x]\n");
    assert_eq!(ayaml::parse_value(&output).expect("reparse"), value);
    assert_eq!(emit_events("[!t , &a ]\n"), "[!t , &a ]\n");
}

/// Text starting with a document marker is quoted.
#[test]
fn document_marker_lookalikes_are_quoted() {
    for text in ["... a", "--- a", "...", "---"] {
        assert_eq!(
            emit_value(&ayaml::Value::from(text)),
            format!("\"{text}\"\n")
        );
    }
}

/// Characters outside YAML's printable set are escaped and never
/// written in block scalars.
#[test]
fn non_printable_characters_are_escaped() {
    let output = emit_value(&ayaml::Value::from("a\u{1}\nb\n"));
    assert_eq!(output, "\"a\\x01\\nb\\n\"\n");
    let output = emit_value(&ayaml::Value::from("\u{fffe}\u{ffff}\u{feff}"));
    assert_eq!(output, "\"\\uFFFE\\uFFFF\\uFEFF\"\n");
    assert_eq!(emit_events("\"\\x7f\\u0085\"\n"), "\"\\x7F\\N\"\n");
}

/// Long keys use the explicit form.
#[test]
fn long_keys_are_explicit() {
    let key = "k".repeat(200);
    let mut mapping = ayaml::Mapping::new();
    mapping.insert(ayaml::Value::from(key.as_str()), ayaml::Value::from(1_u64));
    let output = emit_value(&ayaml::Value::Mapping(mapping));
    assert_eq!(output, format!("? {key}\n: 1\n"));
    let input = format!("{key}: 1\n");
    assert_eq!(emit_events(&input), format!("? {key}\n: 1\n"));
}

/// A `:` inside a flow collection is quoted.
#[test]
fn colons_are_quoted_in_flow() {
    let value = ayaml::Value::Sequence(vec![ayaml::Value::from("a:?"), ayaml::Value::from("?x")]);
    let options = ayaml::EmitterOptions::default().with_flow(true);
    let output = ayaml::to_string_value_with_options(&value, &options).expect("emit");
    assert_eq!(output, "[\"a:?\", \"?x\"]\n");
}

/// Document streams charge the caller's limits, before writing.
#[test]
fn document_streams_use_the_callers_limits() {
    let nested = ayaml::parse_value("a: [[1]]\n").expect("parse");
    let shallow = ayaml::Limits::default().with_max_depth(2);
    let error =
        ayaml::to_string_documents_with_limits(&[nested], &shallow).expect_err("depth limit");
    assert_eq!(error.kind(), ayaml::ErrorKind::Limit);

    let small = ayaml::Limits::default().with_max_output_bytes(5);
    let documents = [ayaml::Value::from("a"), ayaml::Value::from("b")];
    let mut output = Vec::new();
    let error = ayaml::to_writer_documents_with_limits(&mut output, &documents, &small)
        .expect_err("output limit");
    assert_eq!(error.kind(), ayaml::ErrorKind::Limit);
    assert!(output.len() <= 5, "{output:?}");
}

/// The emitters enforce `max_depth` instead of overflowing the stack.
#[test]
fn emitters_enforce_the_depth_limit() {
    let mut value = ayaml::Value::Null;
    for _ in 0..1_000 {
        value = ayaml::Value::Sequence(vec![value]);
    }
    let error = ayaml::to_string_value(&value).expect_err("depth");
    assert_eq!(error.kind(), ayaml::ErrorKind::Limit);

    let span = ayaml::Span::new(ayaml::Marker::new(0, 1, 1), ayaml::Marker::new(0, 1, 1));
    let mut events = vec![
        ayaml::Event::StreamStart { span },
        ayaml::Event::DocumentStart {
            explicit: false,
            span,
        },
    ];
    for _ in 0..1_000 {
        events.push(ayaml::Event::SequenceStart {
            tag: None,
            anchor: None,
            style: ayaml::CollectionStyle::Block,
            span,
        });
    }
    let error = ayaml::to_string_events(&events).expect_err("depth");
    assert_eq!(error.kind(), ayaml::ErrorKind::Limit);
}

/// Per-path styles apply to values, not to the keys of a mapping.
#[test]
fn styles_do_not_leak_into_keys() {
    let value = ayaml::parse_value("a:\n  b: c\n").expect("parse");
    let mut styles = ayaml::EmitterStyles::new();
    styles.set(
        vec![ayaml::PathSegment::key("a")],
        ayaml::NodeStyle::default().with_scalar_style(Some(ayaml::ScalarStyle::DoubleQuoted)),
    );
    let output = ayaml::to_string_value_with_styles(&value, &styles).expect("emit");
    assert_eq!(output, "a:\n  b: c\n");
}

/// `f32` values are written in their shortest form.
#[test]
fn f32_is_written_in_its_shortest_form() {
    assert_eq!(ayaml::to_string(&0.1_f32).expect("emit"), "0.1\n");
    assert_eq!(
        ayaml::to_string(&[1.5_f32, f32::INFINITY]).expect("emit"),
        "- 1.5\n- .inf\n"
    );
    let decoded: f32 = ayaml::from_str("0.1\n").expect("decode");
    assert_eq!(decoded, 0.1_f32);
}

/// The default limits read a stream of documents, while the
/// single-document entry points still reject a second document.
#[test]
fn default_limits_read_document_streams() {
    let stream = "--- a\n--- b\n--- c\n";
    let documents = ayaml::parse_documents(stream).expect("stream");
    assert_eq!(documents, ["a", "b", "c"].map(ayaml::Value::from));

    let mut cursor = ayaml::Deserializer::from_str(stream).expect("cursor");
    let mut borrowed = Vec::new();
    while let Some(document) = cursor.next_borrowed::<String>().expect("next") {
        borrowed.push(document);
    }
    assert_eq!(borrowed, ["a", "b", "c"]);

    for error in [
        ayaml::parse_value(stream).expect_err("parse_value"),
        ayaml::from_str::<ayaml::Value>(stream).expect_err("from_str"),
        ayaml::from_str_borrowed::<ayaml::Value>(stream).expect_err("from_str_borrowed"),
    ] {
        assert!(
            error.to_string().contains("trailing documents")
                || error.to_string().contains("more than one document"),
            "{error}"
        );
    }
    let compat = ayaml::Limits::compat();
    assert_eq!(
        ayaml::parse_documents_with_limits(stream, &compat)
            .expect_err("compatibility profile")
            .kind(),
        ayaml::ErrorKind::Limit
    );
}

/// the document cursor charges every document's aliases to one budget
/// for the stream, as `parse_documents` does.
#[test]
fn cursor_alias_budget_covers_the_stream() {
    let stream = "--- [&a 1, *a]\n--- [&b 2, *b]\n--- [&c 3, *c]\n";
    let limits = ayaml::Limits::default().with_max_alias_dereferences(2);
    assert_eq!(
        ayaml::parse_documents_with_limits(stream, &limits)
            .expect_err("stream budget")
            .kind(),
        ayaml::ErrorKind::Limit
    );

    let mut borrowed = ayaml::Deserializer::from_str_with_limits(stream, &limits).expect("cursor");
    let mut owned = ayaml::Deserializer::from_str_with_limits(stream, &limits).expect("cursor");
    for _ in 0..2 {
        assert!(
            borrowed
                .next_borrowed::<Vec<u8>>()
                .expect("within budget")
                .is_some()
        );
        assert!(owned.next::<Vec<u8>>().expect("within budget").is_some());
    }
    assert_eq!(
        borrowed
            .next_borrowed::<Vec<u8>>()
            .expect_err("third alias")
            .kind(),
        ayaml::ErrorKind::Limit
    );
    assert_eq!(
        owned.next::<Vec<u8>>().expect_err("third alias").kind(),
        ayaml::ErrorKind::Limit
    );
}

/// An alias finds its anchor wherever it sits in the document being
/// built: in an open collection, in a finished sibling, under a tag, or as a
/// key whose value is still open.
#[test]
fn aliases_resolve_through_open_and_finished_collections() {
    // Each input, then the same document with its aliases written out.
    let cases = [
        ("a: [[&x {k: v}]]\nb: *x\n", "a: [[{k: v}]]\nb: {k: v}\n"),
        ("- [&x 1, [*x, [*x]]]\n- *x\n", "- [1, [1, [1]]]\n- 1\n"),
        ("x: !t [&a 1]\ny: *a\n", "x: !t [1]\ny: 1\n"),
        ("{&k a: *k}\n", "{a: a}\n"),
        ("? &k [x]\n: 1\nother: *k\n", "? [x]\n: 1\nother: [x]\n"),
        (
            "a: &outer\n  b: &inner [1, 2]\n  c: *inner\nd: *outer\ne: *inner\n",
            "a:\n  b: [1, 2]\n  c: [1, 2]\nd:\n  b: [1, 2]\n  c: [1, 2]\ne: [1, 2]\n",
        ),
        ("&r [&e a, *e, {*e : *e}]\n", "[a, a, {a: a}]\n"),
    ];
    // The written-out forms have no aliases, so they don't depend on alias handling.
    // These pin the reading of a few of them to literal values too.
    assert_eq!(
        parse("a: [[&x {k: v}]]\nb: *x\n"),
        json!({"a": [[{"k": "v"}]], "b": {"k": "v"}})
    );
    assert_eq!(
        parse("- [&x 1, [*x, [*x]]]\n- *x\n"),
        json!([[1, [1, [1]]], 1])
    );
    assert_eq!(
        parse("a: &outer\n  b: &inner [1, 2]\n  c: *inner\nd: *outer\ne: *inner\n"),
        json!({
            "a": {"b": [1, 2], "c": [1, 2]},
            "d": {"b": [1, 2], "c": [1, 2]},
            "e": [1, 2],
        })
    );
    for (input, written_out) in cases {
        let expected = ayaml::parse_value(written_out).expect("written out");
        let value = ayaml::parse_value(input).unwrap_or_else(|error| panic!("{input:?}: {error}"));
        assert_eq!(value, expected, "{input:?}");
        let output = ayaml::to_string_value(&value).expect("emit");
        let reread = ayaml::parse_value(&output).expect("reread");
        assert_eq!(reread, expected, "{input:?}: {output}");
        let typed: ayaml::Value =
            ayaml::from_str_borrowed(input).unwrap_or_else(|error| panic!("{input:?}: {error}"));
        assert_eq!(typed, value, "{input:?}: typed path");
    }
}

/// An alias key equals the key its anchor names, on the typed path too.
#[test]
fn alias_keys_duplicate_their_anchor_on_every_path() {
    let input = "&a k: 1\n*a : 2\n";
    assert_eq!(
        ayaml::parse_value(input).expect_err("value").kind(),
        ayaml::ErrorKind::DuplicateKey
    );
    let typed = ayaml::from_str_borrowed::<std::collections::BTreeMap<String, u8>>(input)
        .expect_err("typed");
    assert_eq!(typed.kind(), ayaml::ErrorKind::DuplicateKey, "{typed}");
}

/// A single-document entry point stops at the second document's start, so an
/// error later in the stream does not hide the trailing document.
#[test]
fn single_document_entry_points_stop_at_the_second_document() {
    let stream = "a: 1\n---\nb: [1\nc: 2\n";
    for error in [
        ayaml::parse_value(stream).expect_err("parse_value"),
        ayaml::from_str_borrowed::<ayaml::Value>(stream).expect_err("from_str_borrowed"),
    ] {
        assert_eq!(error.kind(), ayaml::ErrorKind::Parser, "{error}");
        assert!(error.to_string().contains("trailing documents"), "{error}");
    }
    #[cfg(feature = "libyaml-compat")]
    assert_eq!(
        ayaml::compat::from_str::<ayaml::Value>(stream)
            .expect_err("compat")
            .to_string(),
        "more than one document in stream"
    );
}
