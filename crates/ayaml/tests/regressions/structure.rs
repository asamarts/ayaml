use super::*;

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
    // `%`, `$`, `:`, and `?` are URI characters, so a name with a scheme may hold them.
    let value = ayaml::parse_value("!<tag:x$:?> x\n").expect("parse");
    assert!(value.is_tagged());
}

/// A tag has to name one, in each mode. A shorthand's handle needs a suffix (production 99), and
/// a verbatim tag needs a name, which `!` alone isn't (production 97, example 6.25).
#[test]
fn tag_names_are_checked() {
    const SUFFIX: &str = "a tag's handle needs a suffix after it";
    const VERBATIM: &str = "a verbatim tag needs a name";
    const NAME: &str = "a verbatim tag must name a local tag or a tag with a scheme";
    const ESCAPE: &str = "a `%` in a verbatim tag must start a two-digit escape";
    // What follows a tag is checked first, so `!!!` is a `!` the suffix can't hold.
    const ENDING: &str = "a tag must be followed by whitespace or a line break";
    for (input, message, line, column) in [
        ("!!!", ENDING, 1, 3),
        ("- !!!a", ENDING, 1, 5),
        ("!! a", SUFFIX, 1, 1),
        ("!!", SUFFIX, 1, 1),
        ("- !! a", SUFFIX, 1, 3),
        ("[!!, a]", SUFFIX, 1, 2),
        ("{!! : a}", SUFFIX, 1, 2),
        ("? !!\n: a", SUFFIX, 1, 3),
        ("%TAG !e! tag:x,2000:\n---\n!e! a", SUFFIX, 3, 1),
        ("!<> a", VERBATIM, 1, 1),
        ("!<!> a", VERBATIM, 1, 1),
        ("- !<!> a", VERBATIM, 1, 3),
        ("[!<>]", VERBATIM, 1, 2),
        ("!<]> a", NAME, 1, 1),
        ("!<b.> a", NAME, 1, 1),
        ("!<foo> a", NAME, 1, 1),
        ("!<$:?> a", NAME, 1, 1),
        ("!<1a:x> a", NAME, 1, 1),
        ("- !<]> a", NAME, 1, 3),
        ("!<tag:x:50%> a", ESCAPE, 1, 1),
        ("!<tag:x:%2> a", ESCAPE, 1, 1),
    ] {
        let expected = format!("{message} at line {line} column {column}");
        for (mode, read) in readings(input) {
            assert_eq!(read, Err(expected.clone()), "{input:?} in {mode}");
        }
    }
    // Each keeps its name as written.
    for (input, shown) in [
        ("!<!foo> a", "!foo"),
        ("!<!%41> a", "!%41"),
        ("!<tag:x> a", "tag:x"),
        ("!<tag:x#y> a", "tag:x#y"),
        ("!<a+b.c-d:x> a", "a+b.c-d:x"),
        ("!<Z9:> a", "Z9:"),
        ("!<tag:x%2C%41> a", "tag:x%2C%41"),
        ("!<tag:x:50%25> a", "tag:x:50%25"),
    ] {
        for (mode, read) in readings(input) {
            let value = read.unwrap_or_else(|error| panic!("{input:?} in {mode}: {error}"));
            let ayaml::Value::Tagged(tagged) = value else {
                panic!("{input:?} in {mode}: {value:?}");
            };
            assert_eq!(tagged.tag.to_string(), shown, "{input:?} in {mode}");
        }
    }
    // `!` alone is the non-specific tag, and a named handle still takes a suffix.
    for (mode, read) in readings("- ! a\n- !e!b c") {
        assert!(read.is_err(), "an undefined handle in {mode}");
    }
    for (mode, read) in readings("%TAG !e! tag:x:\n---\n- ! a\n- !e!b c") {
        read.unwrap_or_else(|error| panic!("{mode}: {error}"));
    }
}

/// A document gives each handle one prefix (YAML 1.2.2 section 6.8.2.2), even the same prefix
/// twice. The next document may give it another.
#[test]
fn a_tag_handle_has_one_prefix_in_a_document() {
    for (input, handle, line) in [
        ("%TAG !e! tag:a:\n%TAG !e! tag:b:\n---\n!e!x y", "!e!", 2),
        ("%TAG ! tag:a:\n%TAG ! tag:b:\n--- a", "!", 2),
        (
            "%TAG !! tag:a:\n%TAG !e! tag:b:\n%TAG !! tag:a:\n--- a",
            "!!",
            3,
        ),
    ] {
        let expected = format!("duplicate %TAG directive for {handle} at line {line} column 1");
        for (mode, read) in readings(input) {
            assert_eq!(read, Err(expected.clone()), "{input:?} in {mode}");
        }
    }
    for (mode, read) in readings("%TAG !! tag:a:\n%TAG ! tag:a:\n--- a") {
        assert_eq!(read, Ok(ayaml::Value::from("a")), "{mode}");
    }
    let values =
        ayaml::parse_documents("%TAG !e! tag:a:\n--- !e!x 1\n...\n%TAG !e! tag:b:\n--- !e!x 2")
            .expect("two documents");
    let tags: Vec<String> = values
        .iter()
        .map(|value| match value {
            ayaml::Value::Tagged(tagged) => tagged.tag.to_string(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(tags, ["tag:a:x", "tag:b:x"]);
}

/// A block scalar's header holds at most one chomping and one indentation indicator, in either
/// order, right after the `|` or `>` (YAML 1.2.2 production 162). Only blanks and a comment may
/// follow, in each mode.
#[test]
fn a_block_scalar_header_has_one_indicator_of_each_kind() {
    for (input, found, column) in [
        ("|++\n a", '+', 3),
        ("|--\n a", '-', 3),
        ("|+-\n a", '-', 3),
        (">-+\n a", '+', 3),
        ("|11\n  a", '1', 3),
        ("|1+1\n  a", '1', 4),
        ("|-2-\n  a", '-', 4),
        ("|2+2\n  a", '2', 4),
        ("| -\n a", '-', 3),
        ("| 1\n a", '1', 3),
        (">\t-\n a", '-', 3),
        ("|  +\n a", '+', 4),
        ("|2 -\n  a", '-', 4),
        ("k: |+ 2\n  a", '2', 7),
        ("- >1 1\n  a", '1', 6),
        ("|0\n a", '0', 2),
        ("|10\n  a", '0', 3),
        (">-0\n a", '0', 3),
    ] {
        let expected =
            format!("unexpected {found:?} in block scalar header at line 1 column {column}");
        for (mode, read) in readings(input) {
            assert_eq!(read, Err(expected.clone()), "{input:?} in {mode}");
        }
    }
    for (input, default, compat) in [
        ("|2+\n  a\n", " a\n", "a\n"),
        ("|+2\n  a\n", " a\n", "a\n"),
        ("|2-\n  a\n", " a", "a"),
        ("|-2\n   a\n", "  a", " a"),
        ("|- # c\n a\n", "a", "a"),
        ("|2 # c\n   a\n", "  a\n", " a\n"),
        (">+\t# c\n a\n", "a\n", "a\n"),
    ] {
        // `compat` is only read when the compatibility feature is on.
        let _ = compat;
        for (mode, read) in readings(input) {
            let value = if mode == "compat" { compat } else { default };
            assert_eq!(read, Ok(ayaml::Value::from(value)), "{input:?} in {mode}");
        }
    }
}

/// A comment after a block scalar's header holds no byte order mark by default, as any other
/// comment doesn't (production 75). Compat reads one there, as it does in any other comment and as
/// libyaml does.
#[test]
fn a_block_scalar_header_comment_holds_no_byte_order_mark() {
    for (input, line, column) in [
        ("| #\u{feff}\n a", 1, 4),
        ("|- #a\u{feff}\n a", 1, 6),
        ("k: > #\u{feff}\n  a", 1, 7),
    ] {
        assert_eq!(
            ayaml::parse_value(input).map_err(|error| error.to_string()),
            Err(format!(
                "a byte order mark can only start a document at line {line} column {column}"
            )),
            "{input:?}"
        );
        #[cfg(feature = "libyaml-compat")]
        {
            let read = ayaml::compat::from_str::<ayaml::Value>(input);
            read.unwrap_or_else(|error| panic!("{input:?} in compat: {error}"));
        }
    }
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

/// A whitespace-only line is not content when the scalar has no content lines: the scalar is
/// empty and the next node parses. A comment at or below the parent's indent ends it.
#[test]
fn a_whitespace_only_line_is_not_content_without_content_lines() {
    assert_eq!(parse("k: |\n  \nx: 1\n"), json!({"k": "", "x": 1}));
    assert_eq!(parse("k: |\n  \n# c\nx: 1\n"), json!({"k": "", "x": 1}));
    assert_eq!(parse("k: |\n  \n...\n"), json!({"k": ""}));
}

/// The leading-empty check measures against the first line with content, and a tab after the
/// indentation is content rather than an empty line.
#[test]
fn a_tab_after_the_indent_is_content_for_leading_lines() {
    assert_eq!(parse("k: |\n  \t\n"), json!({"k": "\t\n"}));
    rejects("k: |\n    \n  \t\n");
}

/// A root block scalar's explicit indicator counts from the spec's root indentation of -1, so
/// `|1` keeps the first space of a one-space line. Compat keeps libyaml's count from 0.
#[test]
fn a_root_indent_indicator_counts_from_minus_one() {
    for (input, default, compat) in [
        ("|1\n y\n", " y\n", "y\n"),
        ("|2\n  y\n", " y\n", "y\n"),
        ("|2\n    a\n", "   a\n", "  a\n"),
    ] {
        // `compat` is only read when the compatibility feature is on.
        let _ = compat;
        assert_eq!(parse(input), json!(default), "{input:?}");
        #[cfg(feature = "libyaml-compat")]
        assert_eq!(
            ayaml::compat::from_str::<ayaml::Value>(input)
                .expect("compat")
                .as_str(),
            Some(compat),
            "{input:?} in compat"
        );
    }
}

/// A tab at or past an explicit content indent is content, not the start of a comment.
#[test]
fn an_explicit_indent_makes_a_tab_content_not_a_comment() {
    assert_eq!(parse("|2\n  \t# c\n"), json!(" \t# c\n"));
    #[cfg(feature = "libyaml-compat")]
    assert_eq!(
        ayaml::compat::from_str::<ayaml::Value>("|2\n  \t# c\n")
            .expect("compat")
            .as_str(),
        Some("\t# c\n")
    );
}

/// A comment line that starts with a tab directly after a block scalar is not a comment. The
/// grammar wants the `#` at the line's first column or after spaces, not after a tab.
#[test]
fn a_tab_cannot_start_a_comment_after_a_block_scalar() {
    for input in ["|\n x\n\t# c\n", "k: |\n  x\n\t# c\n", "k: |\n  \n\t# c\n"] {
        let error = ayaml::parse_value(input).expect_err("a tabbed comment");
        assert!(error.message().contains("tab"), "{input:?}: {error}");
    }
}

/// A root block scalar's explicit indicator counts from the root's -1, so the writers add one to
/// the content indentation they use. A leading space or tab must read back.
#[test]
fn a_root_block_scalar_reads_back_after_an_indicator_from_minus_one() {
    for text in [" foo\n", "\n\t{\n", "  bar\n"] {
        let value = ayaml::Value::from(text);
        for written in [
            ayaml::to_string_value(&value).expect("value writer"),
            ayaml::to_string(&value).expect("serde writer"),
        ] {
            assert_eq!(
                ayaml::parse_value(&written).expect("reparse"),
                value,
                "{text:?} wrote {written:?}"
            );
        }
    }
}
