//! Regression tests for parser, composer, Serde, and emitter behavior.
//! Each test names its finding ID. Expected values follow YAML 1.2.2 and, where
//! the spec and yaml_serde agree, yaml_serde 0.10.7.

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

/// P1: a plain scalar must not continue into the next entry of a compact
/// nested sequence (suite 3ALJ, A2M4, 6BCT, M5DY).
#[test]
fn p1_compact_nested_sequence_entries_stay_separate() {
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

/// P2: explicit keys holding block collections, and compact mappings inside
/// explicit keys (suite M2N8).
#[test]
fn p2_explicit_keys_hold_block_collections() {
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

/// P3: a block-scalar indentation indicator counts from the owning
/// collection, not from the line (suite 4WA9).
#[test]
fn p3_indentation_indicator_is_relative_to_the_parent_node() {
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

/// P4: continuation and block-scalar content lines are measured against the
/// owning collection, which is both stricter and more permissive than the
/// line indentation.
#[test]
fn p4_continuation_indentation_follows_the_owner() {
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

/// P5: document markers end a root block scalar, also after `--- |`
/// (suite W4TN, M7A3).
#[test]
fn p5_document_markers_end_root_block_scalars() {
    assert_eq!(
        parse_documents("--- |\n%!PS-Adobe-2.0\n...\n--- x\n"),
        vec![json!("%!PS-Adobe-2.0\n"), json!("x")]
    );
    assert_eq!(
        parse_documents("--- |\na\n--- b\n"),
        vec![json!("a\n"), json!("b")]
    );
}

/// P7: an explicit key can start a compact mapping in a sequence entry
/// (suite 4FJ6, 9MMW, and V9D5 round trips).
#[test]
fn p7_explicit_key_in_sequence_entry() {
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

/// P6: `...` with no open document ends nothing (suite HWV9, QT73, M7A3).
#[test]
fn p6_document_end_markers_without_documents() {
    assert_eq!(parse_documents("...\n"), Vec::<Json>::new());
    assert_eq!(parse_documents("...\n...\n"), Vec::<Json>::new());
    assert_eq!(
        parse_documents("a\n...\n# c\n...\n|\n%x # not a comment\n"),
        vec![json!("a"), json!("%x # not a comment\n")]
    );
}

/// P10: the spec readings by default (suite Y2GN, HM87, 652Z, JEF9, L24T).
#[test]
fn p10_spec_readings_by_default() {
    assert_eq!(parse("key: &an:chor value\n"), json!({"key": "value"}));
    assert_eq!(parse("[?x]\n"), json!(["?x"]));
    assert_eq!(parse("{ ?foo: bar }\n"), json!({"?foo": "bar"}));
    assert_eq!(parse("- |+\n   "), json!(["\n"]));
    assert_eq!(parse("foo: |\n  x\n   "), json!({"foo": "x\n \n"}));
}

/// P11: `%XX` escapes in shorthand tags are decoded (suite 6CK3).
#[test]
fn p11_tag_escapes_are_decoded() {
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

/// P13: properties before `,`, `]`, or `}` belong to an empty node.
#[test]
fn p13_properties_before_flow_indicators() {
    let value = ayaml::parse_value("[!t , &a , b]\n").expect("parse");
    let items = value.as_sequence().expect("sequence");
    assert!(matches!(&items[0], ayaml::Value::Tagged(tagged) if tagged.value.is_null()));
    assert!(items[1].is_null());
    assert_eq!(items[2], ayaml::Value::from("b"));
    assert_eq!(parse("[&a ]\n"), json!([null]));
    assert_eq!(parse("{k: &a , j: x}\n"), json!({"k": null, "j": "x"}));
}

/// P18: only space and tab separate tokens; other Unicode spaces are content.
#[test]
fn p18_unicode_spaces_are_content() {
    assert_eq!(parse("a:\u{a0}0\n"), json!("a:\u{a0}0"));
    assert_eq!(parse("key: x\u{a0}\n"), json!({"key": "x\u{a0}"}));
    assert_eq!(parse("\u{3000}a: b\n"), json!({"\u{3000}a": "b"}));
    assert_eq!(parse("a\u{a0}#b\n"), json!("a\u{a0}#b"));
}

/// P19: tags may only hold URI characters, escaped as `%XX` otherwise.
#[test]
fn p19_tags_hold_uri_characters_only() {
    rejects("!x\u{a0}y z\n");
    rejects("!<a b> x\n");
    rejects("!foo,bar x\n");
    let value = ayaml::parse_value("!<$:?> x\n").expect("parse");
    assert!(value.is_tagged());
}

/// P20: in block context a flow indicator does not end `-` or `?`.
#[test]
fn p20_dash_and_question_mark_before_flow_indicators() {
    assert_eq!(parse("-,\n"), json!("-,"));
    assert_eq!(parse("- -]\n"), json!(["-]"]));
    assert_eq!(parse("a: ?}\n"), json!({"a": "?}"}));
}

/// P21: a `:` inside a tag or anchor is not a mapping value indicator when
/// the scanner decides which collection owns a block scalar.
#[test]
fn p21_colons_inside_properties() {
    let value = ayaml::parse_value("- !: |-\n  \n  0\n").expect("parse");
    let ayaml::Value::Tagged(tagged) = &value.as_sequence().expect("sequence")[0] else {
        panic!("expected a tagged item, got {value:?}");
    };
    assert_eq!(tagged.tag.suffix(), ":");
    assert_eq!(tagged.value, ayaml::Value::from("\n0"));
    assert_eq!(parse("- &a: |\n  x\n"), json!(["x\n"]));
}

/// P22: properties on the first key of a compact mapping do not move the
/// mapping's indentation.
#[test]
fn p22_properties_on_the_first_compact_key() {
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

/// P23: each empty line after an escaped line break in a double-quoted
/// scalar is a line feed (YAML 1.2.2 production 112). Found by the diff
/// fuzz target.
#[test]
fn p23_empty_lines_after_an_escaped_line_break() {
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

/// P24: a directive needs a name, and neither its name nor its parameters
/// may hold a byte order mark. Found by the emit fuzz target. Whatever
/// directive parses, the event writer writes back.
#[test]
fn p24_directives_need_a_name() {
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

/// P25: `%`, `@`, and `` ` `` are indicators and cannot start a plain
/// scalar, in either mode. Found by the borrowed fuzz target.
#[test]
fn p25_reserved_indicators_cannot_start_plain_scalars() {
    for input in [
        "a: @b\n", "a: `b\n", "a: %b\n", "[@a]\n", "- `a\n", "@a: b\n",
    ] {
        rejects(input);
    }
    assert_eq!(parse("a: b@c`d%e\n"), json!({"a": "b@c`d%e"}));
    assert_eq!(parse("a: '@b'\n"), json!({"a": "@b"}));
}

/// P27: an anchor or alias is separated from the content that follows
/// (production 96); libyaml rejects `&a[b]` too. A `:` before a blank or a
/// flow indicator stays in the name, as YAML 1.2.2 says (2SXE, W5VH). Found by
/// the diff fuzz target.
#[test]
fn p27_anchors_are_separated_from_their_content() {
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

/// P14 (second half): with an indentation indicator, a leading line holding
/// only spaces keeps the spaces past the indentation as content. Without
/// one, such a line is more indented than the content, which is an error.
#[test]
fn p14_explicit_indentation_keeps_leading_spaces() {
    assert_eq!(parse("a: |2\n    \n  x\n"), json!({"a": "  \nx\n"}));
    assert_eq!(parse("- >1\n  \n x\n"), json!([" \nx\n"]));
    assert_eq!(parse("a: |\n  x\n    \n"), json!({"a": "x\n  \n"}));
    assert_eq!(parse("a: |\n   \n"), json!({"a": ""}));
    rejects("a: |\n    \n  x\n");
}

/// P15: an implicit key is at most 1024 characters, up to its `:`, in block
/// mappings and in flow-sequence pairs; flow-mapping keys have no limit.
#[test]
fn p15_implicit_keys_are_at_most_1024_characters() {
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

/// P17: NEL, LS, and PS are content in YAML 1.2, not line breaks.
#[test]
fn p17_nel_ls_and_ps_are_content() {
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

/// P26: `%TAG` handles are `!`, `!!`, or word characters between `!`s, and
/// prefixes are URI characters that do not start with a flow indicator.
#[test]
fn p26_tag_directives_are_validated() {
    let document = |directive: &str| format!("{directive}\n--- !e!x a\n");
    // A tagged value serializes as `{"!<tag>": payload}` (V6).
    assert_eq!(
        parse(&document("%TAG !e! tag:x,2000:")),
        json!({ "!<tag:x,2000:x>": "a" })
    );
    // The prefix's escapes are decoded (P33).
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

/// P34: after a property-only line, the node may continue only within its
/// parent's bounds (YAML 1.2.2 section 8.2). A sequence entry's node cannot
/// take the next `-` at the entry's own column as its block sequence; a
/// mapping value's node can, since its sequence may sit at the key's column.
/// Content may sit left of the property line, down to the parent's bound.
/// Both modes; yaml_serde agrees on every case. Found by the `diff` fuzz
/// target's 10-minute campaign.
#[test]
fn p34_property_lines_stay_within_the_parent() {
    let json = |input: &str| {
        let value = ayaml::parse_value(input).unwrap_or_else(|error| panic!("{input:?}: {error}"));
        #[cfg(feature = "libyaml-compat")]
        assert_eq!(
            ayaml::compat::from_str::<ayaml::Value>(input).expect("compat"),
            value,
            "{input:?}"
        );
        let baseline: yaml_serde::Value = yaml_serde::from_str(input).expect("yaml_serde");
        let text = serde_json::to_string(&value).expect("json");
        assert_eq!(
            text,
            serde_json::to_string(&baseline).expect("json"),
            "{input:?}"
        );
        text
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

/// P33: `%TAG` prefixes have their %-escapes decoded, like tag suffixes
/// (P11), as libyaml does. Verbatim tags stay as written (YAML 1.2.2 section
/// 6.9.1).
#[test]
fn p33_tag_prefix_escapes_are_decoded() {
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
    let baseline: yaml_serde::Value = yaml_serde::from_str(local).expect("yaml_serde");
    let yaml_serde::Value::Tagged(tagged) = baseline else {
        panic!("yaml_serde did not keep the tag");
    };
    assert_eq!(tagged.tag.to_string(), "!my!pre-foo");
    #[cfg(feature = "libyaml-compat")]
    assert_eq!(
        ayaml::compat::from_str::<ayaml::Value>(local).expect("compat"),
        ayaml::parse_value(local).expect("default")
    );
}

/// P36: in compat, a column-0 byte order mark before any content ends a
/// root block scalar, as a column-0 `#` line does (P29): libyaml indents
/// root block scalars by at least one space and then skips the BOM. Default
/// mode reads these the same way here, since the BOM starts a document
/// prefix. Found by the `diff` fuzz target.
#[cfg(feature = "libyaml-compat")]
#[test]
fn p36_compat_column_zero_boms_end_root_block_scalars() {
    for input in [
        "|+\n\u{feff}\n\n\n\n ",
        "|+\n\u{feff}\n",
        "--- |+\n\u{feff}\n\n",
    ] {
        let compat = ayaml::compat::from_str::<ayaml::Value>(input).expect("compat");
        assert_eq!(compat, "", "{input:?}");
        let baseline: yaml_serde::Value = yaml_serde::from_str(input).expect("yaml_serde");
        assert_eq!(
            baseline,
            yaml_serde::Value::String(String::new()),
            "{input:?}"
        );
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

/// P32: YAML 1.2.2 section 5.1 requires every non-C0 character to be
/// allowed inside quoted scalars (`nb-json`), so a raw DEL, C1 control, or
/// U+FFFE is content there, in both modes. libyaml rejects them anywhere.
/// Outside quoted scalars they stay an error (P8), and C0 controls are an
/// error everywhere. Found in LLVM's `llvm-dlltool` test inputs (T4).
#[test]
fn p32_quoted_scalars_hold_non_c0_characters() {
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

/// P28: quotes, flow indicators, and properties inside a plain key are
/// content, so the block-scalar owner heuristic finds the key's `:`. Found
/// by the emit fuzz target.
#[test]
fn p28_flow_indicators_inside_plain_keys_are_content() {
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

/// P29: in a zero-indented root block scalar, a column-0 `#` line is content
/// in YAML 1.2.2 (DK3J), whether or not the header follows `---` or a BOM.
#[test]
fn p29_column_zero_hash_lines_in_root_block_scalars_are_content() {
    assert_eq!(parse(">\n#\n"), json!("#\n"));
    assert_eq!(parse("|\n#x\n"), json!("#x\n"));
    assert_eq!(parse("|\nfoo\n# c\n"), json!("foo\n# c\n"));
    assert_eq!(parse("--- |\n#x\n"), json!("#x\n"));
    assert_eq!(parse("\u{feff}>\n#\n"), json!("#\n"));
    assert_eq!(parse("--- |\n  x\n# c\n"), json!("x\n"));
}

/// P29 in compat: libyaml indents a root block scalar by at least one space,
/// so a column-0 `#` line before any content is a comment there.
#[cfg(feature = "libyaml-compat")]
#[test]
fn p29_compat_column_zero_hash_lines_end_root_block_scalars() {
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

/// P30: in compat, the block-scalar owner heuristic ends an anchor or alias
/// name where the compat scanner does, before a `?` or before a `:` followed
/// by more name text. `&-:: >` is then the key `:`, whose scalar must be
/// indented. Found by the diff fuzz target.
#[cfg(feature = "libyaml-compat")]
#[test]
fn p30_compat_owner_heuristic_ends_anchors_like_the_scanner() {
    assert_eq!(compat("&-:: >\n(0: ("), json!({ ":": "", "(0": "(" }));
    assert_eq!(compat("&-:: >\n x\n"), json!({ ":": "x\n" }));
    assert_eq!(compat("&a?b: |\n x\n"), json!({ "?b": "x\n" }));
    assert_eq!(compat("&a:b: >\n x\n"), json!({ ":b": "x\n" }));
}

/// P30 by default: anchor names hold `:` and `?`, so these are anchored
/// root scalars.
#[test]
fn p30_anchor_names_hold_colons_and_question_marks() {
    assert_eq!(parse("&-:: >\n(0: ("), json!("(0: ("));
    assert_eq!(parse("&a?b: |\nx\n"), json!("x\n"));
}

#[cfg(feature = "libyaml-compat")]
fn compat(input: &str) -> Json {
    let value = ayaml::compat::from_str::<ayaml::Value>(input)
        .unwrap_or_else(|error| panic!("{input:?}: {error}"));
    serde_json::to_value(&value).expect("JSON-compatible value")
}

/// V8: a custom tag keeps its scalar payload resolved, as in yaml_serde.
#[test]
fn v8_custom_tags_resolve_their_payloads() {
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

/// V17 and E7: tags compare by their full name, so verbatim and shorthand
/// spellings of a tag are the same tag.
#[test]
fn v17_e7_tags_compare_by_full_name() {
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

/// V2: a struct field is named by its key's text, so an integer key does not
/// bind a field by position, and a key naming no field is ignored, as in
/// yaml_serde.
#[test]
fn v2_integer_keys_do_not_bind_fields_by_position() {
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

/// V7: a sequence longer than a tuple or array is an error.
#[test]
fn v7_overlong_sequences_are_an_error() {
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

/// V9: an empty value reads as an empty sequence, an empty map, or a struct
/// with its defaults. On the typed path an explicit `~` does not, as in
/// yaml_serde; a `Value` cannot tell the two apart (fixed for `from_str` by
/// the typed path).
#[test]
fn v9_empty_values_are_empty_collections() {
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

/// V19: a `char` and a unit variant are read from the scalar's text, and an
/// empty value or `~` is an empty `Mapping`.
#[test]
fn v19_chars_variants_and_mappings_follow_the_text() {
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

/// V4: `Number`'s order is total and agrees with `Eq`: NaN sorts after
/// every other float, and equals itself, as in yaml_serde.
#[test]
fn v4_number_order_is_total() {
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

/// V12: an alias refers to the most recent anchor before it, so an anchor
/// redefined inside its own anchored collection wins over the collection.
#[test]
fn v12_inner_anchor_redefinitions_win() {
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

/// V13: indexing follows yaml_serde. `value[0]` finds an integer key and
/// looks through tags, an index past the end panics instead of growing the
/// sequence (or wrapping and clearing it at `usize::MAX`), and writing a key
/// into a null makes it a mapping.
#[test]
fn v13_indexing_matches_yaml_serde() {
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

/// V14: `as_f64` converts integers, so a `Value` integer equals the same
/// float, as in yaml_serde.
#[test]
fn v14_integers_read_as_floats() {
    let five = ayaml::Value::from(5_u64);
    assert_eq!(five.as_f64(), Some(5.0));
    assert_eq!(five, 5.0_f64);
    assert_eq!(ayaml::Value::from(-3_i64).as_f64(), Some(-3.0));
    assert!(!five.is_f64());
    assert_ne!(ayaml::Value::from(5.0_f64), 5_u64);
}

/// V5, V6: tags survive `to_value`, `from_value`, and serialization. A
/// tagged value is a one-entry map from `!` and the tag's Serde name to the
/// value, as in yaml_serde, and ayaml's serializers turn that map back into
/// the tag. Core, `%TAG`, and verbatim tags keep their full names.
#[test]
fn v5_v6_tags_round_trip_through_serde() {
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

/// V10: core collection tags restate the node kind, so every composer drops
/// them, as yaml_serde does; `!!seq [1, 2]` reads into a `Vec`.
#[test]
fn v10_core_collection_tags_are_dropped() {
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

/// V22 (tags): the typed path keeps tags, so `from_str_borrowed::<Value>`
/// and the multi-document cursor give the same values as `parse_value`.
#[test]
fn v22_the_typed_path_keeps_tags() {
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

/// V11: merging keeps the mapping's own keys in source order and appends the
/// merged keys, as yaml_serde does when `<<` is last. Chained merges flatten
/// fully, where yaml_serde leaves the inner `<<` behind.
#[test]
fn v11_merge_order_and_chains() {
    let base = "b: &b {y: 1, x: 1}\n";
    assert_eq!(
        merged_keys(&format!("{base}m:\n  z: 0\n  <<: *b\n  x: 2\n"), "m"),
        merged_keys("m: {z: 0, x: 2, y: 1}\n", "m")
    );
    // yaml_serde moves `w` into the `<<` slot here (`w, z, y, x`).
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

/// V15: integers up to 128 bits are exact. A typed target reads them as
/// yaml_serde does, and the value model keeps them.
#[test]
fn v15_wide_integers() {
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
    // path canonicalizes it (V3).
    assert_eq!(
        ayaml::from_str_borrowed::<String>("0x10000000000000000\n").expect("typed"),
        "0x10000000000000000"
    );
    let value =
        ayaml::parse_value("[0x10000000000000000, -0o2000000000000000000001]\n").expect("parse");
    assert_eq!(value[0], ayaml::Value::from(1_u128 << 64));
    assert_eq!(value[1], ayaml::Value::from(-(1_i128 << 64) - 1));
    // Past 128 bits a decimal integer is a float and a radix integer is a
    // string, as in yaml_serde.
    let value = ayaml::parse_value(
        "[340282366920938463463374607431768211456, 0x100000000000000000000000000000000]\n",
    )
    .expect("parse");
    assert!(value[0].as_number().is_some_and(ayaml::Number::is_f64));
    assert_eq!(value[1], "0x100000000000000000000000000000000");
}

/// V16: 128-bit integers serialize as plain integers and read back.
#[test]
fn v16_wide_integers_serialize() {
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

/// V18: explicit core tags follow the YAML 1.2.2 core schema. `!!float 01` is
/// 1.0, as in yaml_serde. `!!int 0123` is 123 and `!!null ''` is null, where
/// yaml_serde rejects both.
#[test]
fn v18_explicit_core_tags_follow_the_core_schema() {
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

/// E1: integer-like text outside the value model is quoted.
#[test]
fn e1_out_of_range_integers_are_quoted() {
    for text in [
        "18446744073709551616",
        "-9223372036854775809",
        "0xFFFFFFFFFFFFFFFFFF",
    ] {
        let output = emit_value(&ayaml::Value::from(text));
        assert_eq!(output, format!("\"{text}\"\n"));
    }
}

/// E2: `indent` must be between 2 and 9.
#[test]
fn e2_indent_is_validated() {
    let value = ayaml::parse_value("a:\n  b: 1\n").expect("parse");
    for indent in [0, 1, 10] {
        let options = ayaml::EmitterOptions::default().with_indent(indent);
        let error = ayaml::to_string_value_with_options(&value, &options).expect_err("indent");
        assert_eq!(error.kind(), ayaml::ErrorKind::Emitter);
    }
}

/// E3, E10: the event emitter keeps empty plain scalars and negative
/// numbers plain.
#[test]
fn e3_e10_event_emitter_keeps_plain_scalars_plain() {
    assert_eq!(
        emit_events("a:\nb: -1\nc: [-.inf, ~]\n"),
        "a:\nb: -1\nc: [-.inf, ~]\n"
    );
    assert_eq!(emit_events("- \n- -1\n"), "-\n- -1\n");
    assert_eq!(emit_events("{a: , : b}\n"), "{a: , ? : b}\n");
    assert_eq!(emit_events("---\n"), "---\n");
}

/// E4: keep chomping keeps every trailing line break.
#[test]
fn e4_event_emitter_keeps_trailing_line_breaks() {
    assert_eq!(emit_events("- |+\n  a\n\n\n- x\n"), "- |+\n  a\n\n\n- x\n");
    assert_eq!(emit_events("k: |+\n\n"), "k: |+\n\n");
}

/// E5, E12: multi-line single-quoted values are written double-quoted.
#[test]
fn e5_e12_single_quotes_never_hold_line_breaks() {
    assert_eq!(emit_events("'a\n\n  b'\n"), "\"a\\nb\"\n");
    let options =
        ayaml::EmitterOptions::default().with_scalar_style(Some(ayaml::ScalarStyle::SingleQuoted));
    let value = ayaml::Value::from("a\nb\u{1}");
    let output = ayaml::to_string_value_with_options(&value, &options).expect("emit");
    assert_eq!(output, "\"a\\nb\\x01\"\n");
    assert_eq!(ayaml::parse_value(&output).expect("reparse"), value);
}

/// E6: anchored keys and alias keys (suite E76Z, 26DV, F6MC, X38W).
#[test]
fn e6_alias_keys_round_trip() {
    assert_eq!(
        emit_events("&a a: &b b\n*b : *a\n"),
        "&a a: &b b\n*b : *a\n"
    );
    emit_events("? &a [x]\n: *a\n");
}

/// E9: the lazy writer lays out block collections like the collected one.
#[test]
fn e9_lazy_writer_layout() {
    assert_eq!(emit_events("- a\n- b\n"), "- a\n- b\n");
    assert_eq!(
        emit_events("a:\n  b: [1, 2]\n  c:\n  - x\n--- |\n  text\n"),
        "a:\n  b: [1, 2]\n  c:\n  - x\n---\n|\n  text\n"
    );
}

/// E11: block-scalar indentation indicators are kept.
#[test]
fn e11_indentation_indicators_are_kept() {
    assert_eq!(
        emit_events("key: |1\n  explicit\n"),
        "key: |2\n   explicit\n"
    );
    // Folding cannot keep a leading space, so the literal style is used.
    assert_eq!(emit_events("- >2\n   folded\n"), "- |2\n   folded\n");
    assert_eq!(emit_events("- >\n  a\n  b\n"), "- >\n  a b\n");
}

/// E13: compact entries pad the `-` to the indentation step.
#[test]
fn e13_compact_entries_follow_the_indent() {
    let value = ayaml::parse_value("- a: 1\n  b: |\n    x\n- - c\n  - d\n").expect("parse");
    let options = ayaml::EmitterOptions::default().with_indent(4);
    let output = ayaml::to_string_value_with_options(&value, &options).expect("emit");
    assert_eq!(output, "-   a: 1\n    b: |\n        x\n-   - c\n    - d\n");
    assert_eq!(ayaml::parse_value(&output).expect("reparse"), value);
}

/// E14: a tag on a tagged value cannot be written.
#[test]
fn e14_nested_tags_are_an_error() {
    let inner = ayaml::Value::from(ayaml::TaggedValue::new(
        ayaml::Tag::local("Y"),
        ayaml::Value::from(1_u64),
    ));
    let outer = ayaml::Value::from(ayaml::TaggedValue::new(ayaml::Tag::local("X"), inner));
    let error = ayaml::to_string_value(&outer).expect_err("nested tags");
    assert_eq!(error.kind(), ayaml::ErrorKind::Emitter);
}

/// E28: a directive is written with NEL, LS, or PS in it, since they are
/// content (P17) and a directive has no escapes. An anchor holding a byte
/// order mark, which the parser rejects, cannot be written. Found by the
/// emit fuzz target.
#[test]
fn e28_directives_keep_nel_ls_and_ps() {
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

/// E15: tag suffixes are %-escaped.
#[test]
fn e15_tag_suffixes_are_escaped() {
    let value = ayaml::Value::from(ayaml::TaggedValue::new(
        ayaml::Tag::local("my variant!%"),
        ayaml::Value::from(1_u64),
    ));
    assert_eq!(emit_value(&value), "!my%20variant%21%25 1\n");
}

/// E16: a tagged null before a flow indicator is followed by a space.
#[test]
fn e16_tagged_null_in_flow() {
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

/// E17: text starting with a document marker is quoted.
#[test]
fn e17_document_marker_lookalikes_are_quoted() {
    for text in ["... a", "--- a", "...", "---"] {
        assert_eq!(
            emit_value(&ayaml::Value::from(text)),
            format!("\"{text}\"\n")
        );
    }
}

/// E18, E19: characters outside YAML's printable set are escaped and never
/// written in block scalars.
#[test]
fn e18_e19_non_printable_characters_are_escaped() {
    let output = emit_value(&ayaml::Value::from("a\u{1}\nb\n"));
    assert_eq!(output, "\"a\\x01\\nb\\n\"\n");
    let output = emit_value(&ayaml::Value::from("\u{fffe}\u{ffff}\u{feff}"));
    assert_eq!(output, "\"\\uFFFE\\uFFFF\\uFEFF\"\n");
    assert_eq!(emit_events("\"\\x7f\\u0085\"\n"), "\"\\x7F\\N\"\n");
}

/// E20: long keys use the explicit form.
#[test]
fn e20_long_keys_are_explicit() {
    let key = "k".repeat(200);
    let mut mapping = ayaml::Mapping::new();
    mapping.insert(ayaml::Value::from(key.as_str()), ayaml::Value::from(1_u64));
    let output = emit_value(&ayaml::Value::Mapping(mapping));
    assert_eq!(output, format!("? {key}\n: 1\n"));
    let input = format!("{key}: 1\n");
    assert_eq!(emit_events(&input), format!("? {key}\n: 1\n"));
}

/// E21: a `:` inside a flow collection is quoted.
#[test]
fn e21_colons_are_quoted_in_flow() {
    let value = ayaml::Value::Sequence(vec![ayaml::Value::from("a:?"), ayaml::Value::from("?x")]);
    let options = ayaml::EmitterOptions::default().with_flow(true);
    let output = ayaml::to_string_value_with_options(&value, &options).expect("emit");
    assert_eq!(output, "[\"a:?\", \"?x\"]\n");
}

/// E22, E23: document streams charge the caller's limits, before writing.
#[test]
fn e22_e23_document_streams_use_the_callers_limits() {
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

/// E25: the emitters enforce `max_depth` instead of overflowing the stack.
#[test]
fn e25_emitters_enforce_the_depth_limit() {
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

/// E27: per-path styles apply to values, not to the keys of a mapping.
#[test]
fn e27_styles_do_not_leak_into_keys() {
    let value = ayaml::parse_value("a:\n  b: c\n").expect("parse");
    let mut styles = ayaml::EmitterStyles::new();
    styles.set(
        vec![ayaml::PathSegment::key("a")],
        ayaml::NodeStyle::default().with_scalar_style(Some(ayaml::ScalarStyle::DoubleQuoted)),
    );
    let output = ayaml::to_string_value_with_styles(&value, &styles).expect("emit");
    assert_eq!(output, "a:\n  b: c\n");
}

/// E26: `f32` values are written in their shortest form, as yaml_serde
/// writes them.
#[test]
fn e26_f32_is_written_in_its_shortest_form() {
    assert_eq!(ayaml::to_string(&0.1_f32).expect("emit"), "0.1\n");
    assert_eq!(
        ayaml::to_string(&[1.5_f32, f32::INFINITY]).expect("emit"),
        "- 1.5\n- .inf\n"
    );
    let decoded: f32 = ayaml::from_str("0.1\n").expect("decode");
    assert_eq!(decoded, 0.1_f32);
}

/// V22: the default limits read a stream of documents, while the
/// single-document entry points still reject a second document.
#[test]
fn v22_default_limits_read_document_streams() {
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
fn d26_cursor_alias_budget_covers_the_stream() {
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

/// V1: an alias finds its anchor wherever it sits in the document being
/// built: in an open collection, in a finished sibling, under a tag, or as a
/// key whose value is still open.
#[test]
fn v1_aliases_resolve_through_open_and_finished_collections() {
    let inputs = [
        "a: [[&x {k: v}]]\nb: *x\n",
        "- [&x 1, [*x, [*x]]]\n- *x\n",
        "x: !t [&a 1]\ny: *a\n",
        "{&k a: *k}\n",
        "? &k [x]\n: 1\nother: *k\n",
        "a: &outer\n  b: &inner [1, 2]\n  c: *inner\nd: *outer\ne: *inner\n",
        "&r [&e a, *e, {*e : *e}]\n",
    ];
    for input in inputs {
        let expected = yaml_serde::from_str::<yaml_serde::Value>(input)
            .unwrap_or_else(|error| panic!("{input:?}: yaml_serde: {error}"));
        let value = ayaml::parse_value(input).unwrap_or_else(|error| panic!("{input:?}: {error}"));
        // Some keys are collections, so compare through yaml_serde's `Value`.
        let output = ayaml::to_string_value(&value).expect("emit");
        let reread = yaml_serde::from_str::<yaml_serde::Value>(&output).expect("reread");
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

/// a single-document entry point stops at the second document's start,
/// as yaml_serde does, so an error later in the stream does not hide the
/// trailing document.
#[test]
fn d26_single_document_entry_points_stop_at_the_second_document() {
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
    assert!(yaml_serde::from_str::<yaml_serde::Value>(stream).is_err());
}
