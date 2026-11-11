use super::*;

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

/// A BOM may only start a document (YAML 1.2.2 section 5.2); libyaml
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
    // c-printable excludes control characters other than tab, LF, CR, and
    // NEL, and also U+FFFE and U+FFFF. Escapes can still produce them inside
    // double-quoted scalars, and a quoted scalar may hold anything in
    // `nb-json` raw (`tests/regressions.rs`).
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
/// anywhere else in a document they are an error.
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

/// A byte order mark at the start of a line never costs the text after it in compat:
/// libyaml either keeps that text or rejects the input, whether or not the line holds a
/// comment or a colon. Default mode rejects all of these or ends the scalar at the mark.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_never_drops_text_after_a_line_start_bom() {
    let compat = ayaml::compat::from_str::<Value>;

    // After a root block scalar, the mark's line is outside the scalar.
    for input in [
        "|\nx\n\u{feff}y",
        "|\nx\n\u{feff}y\n",
        ">\nx\n\u{feff}y\n",
        "--- |\nx\n\u{feff}y\n",
        "|\nx\n\u{feff}\ny\n",
    ] {
        assert!(compat(input).is_err(), "{input:?}");
        assert!(ayaml::parse_value(input).is_err(), "{input:?}");
    }
    for input in [
        "|\nx\n\u{feff}\n",
        "|+\nx\n\u{feff}\n",
        "|\nx\n\u{feff}#\n",
        "|\nx\n\u{feff}\n# c\n",
    ] {
        assert_eq!(compat(input).unwrap(), Value::from("x\n"), "{input:?}");
        assert_eq!(
            ayaml::parse_value(input).unwrap(),
            Value::from("x\n"),
            "{input:?}"
        );
    }

    // A root plain scalar continues onto the mark's line, whatever the line holds, so a
    // colon there makes a multi-line key.
    for input in [
        "n\n\u{feff}#:",
        "n\n\u{feff}#: x",
        "\u{feff}n\n\u{feff}#:",
        "n\n\n\u{feff}#:",
        "n\n\u{feff}a: b",
        "n\n\u{feff}# c:\nm",
    ] {
        assert!(compat(input).is_err(), "{input:?}");
    }
    for (input, expected) in [
        ("n\n\u{feff}#", "n \u{feff}#"),
        ("n\n\u{feff}a", "n \u{feff}a"),
        ("n\n\u{feff}", "n \u{feff}"),
        ("n\n\u{feff} #:", "n \u{feff}"),
        ("n\n\u{feff}\n#:", "n \u{feff}"),
    ] {
        assert_eq!(compat(input).unwrap(), Value::from(expected), "{input:?}");
    }

    // Past the end of a value, the mark's line is still a comment.
    for input in ["'n'\n\u{feff}#:", "'n'\n\u{feff}#\n"] {
        assert_eq!(compat(input).unwrap(), Value::from("n"), "{input:?}");
    }
    let mapping = compat("n: 1\n\u{feff}# m: 2\n").unwrap();
    assert_eq!(mapping["n"], Value::from(1_i64));
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
/// error.
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

/// An alias inside a collection refers to that collection when the collection has the
/// alias's anchor, whether or not an earlier node has the anchor too. A finished value
/// can't hold itself, so all of these are errors, not a read of the earlier node.
#[test]
fn an_alias_inside_a_collection_with_its_anchor_is_rejected() {
    for input in [
        "b: &x [*x]\n",
        "a: &x 1\nb: &x [*x]\n",
        "b: &x {k: *x}\n",
        "a: &x 1\nb: &x {k: *x}\n",
        "a: &x 1\nb: &x {*x : v}\n",
        "a: &x 1\nb: &x [[*x]]\n",
        "a: &x 1\nb: &x\n  - *x\n",
        "a: &x 1\nb: &x\n  k: *x\n",
        "a: &x 1\nb: &x\n  ? *x\n",
        "a: &x 1\nb: &x [*x, &x 2, *x]\n",
        "&x [*x]\n",
    ] {
        for (reader, read) in value_readers() {
            let error = read(input).expect_err(&format!("{reader}: {input:?}"));
            assert_eq!(
                error.message(),
                "an alias cannot refer to a collection that contains it",
                "{reader}: {input:?}"
            );
        }
    }
}

/// An alias refers to the nearest node before it with its anchor, which may be a sibling
/// or a node inside a collection with the same anchor.
#[test]
fn an_alias_refers_to_the_nearest_preceding_anchor() {
    let cases = [
        (
            "a: &x 1\nb: *x\nc: &x 2\nd: *x\n",
            "{a: 1, b: 1, c: 2, d: 2}\n",
        ),
        (
            "a: &x [1]\nb: &x [2]\nc: *x\n",
            "{a: [1], b: [2], c: [2]}\n",
        ),
        ("a: &x 1\nb: [*x, &x 2, *x]\n", "{a: 1, b: [1, 2, 2]}\n"),
        ("a: &x {k: 1}\nb: *x\n", "{a: {k: 1}, b: {k: 1}}\n"),
        ("b: &x [&x 2, *x]\nc: *x\n", "{b: [2, 2], c: 2}\n"),
        ("&x {k: &x v, j: *x}\n", "{k: v, j: v}\n"),
        ("&x\n- &x\n- *x\n", "[null, null]\n"),
        ("&x [&x [1], *x]\n", "[[1], [1]]\n"),
        (
            "a: &x [&y 1, *y]\nb: *x\nc: *y\n",
            "{a: [1, 1], b: [1, 1], c: 1}\n",
        ),
    ];
    for (input, expected) in cases {
        let expected = ayaml::parse_value(expected).unwrap();
        for (reader, read) in value_readers() {
            assert_eq!(read(input).unwrap(), expected, "{reader}: {input:?}");
        }
    }
}

/// Line breaks between a plain flow key and its `:` aren't part of the key.
#[test]
fn flow_keys_keep_no_line_breaks_before_their_colon() {
    for (_, read) in value_readers() {
        for (input, expected) in [
            ("{x\n\n: y}\n", "{x: y}\n"),
            ("{x\n\n\n: y}\n", "{x: y}\n"),
            ("{x \n \n : y}\n", "{x: y}\n"),
            ("{? x\n\n: y}\n", "{x: y}\n"),
            ("{x\n\n:}\n", "{x: null}\n"),
            ("{x\n\n:\n}\n", "{x: null}\n"),
            ("{x\ny\n\n: z}\n", "{x y: z}\n"),
            ("{x\n\ny\n\n: z}\n", "{\"x\\ny\": z}\n"),
            ("{k: {x\n\n : y}}\n", "{k: {x: y}}\n"),
            // One line break was already right, and a line break inside a key still folds.
            ("{x\n: y}\n", "{x: y}\n"),
            ("{? x\n\n  y\n: z}\n", "{\"x\\ny\": z}\n"),
            // A colon that isn't an indicator continues the scalar.
            ("{x\n\n:y: z}\n", "{\"x\\n:y\": z}\n"),
        ] {
            let expected = ayaml::parse_value(expected).unwrap();
            assert_eq!(read(input).unwrap(), expected, "{input:?}");
        }
    }
    // A block mapping key can't span lines at all.
    assert!(ayaml::parse_value("x\n\n: y\n").is_err());
    assert_eq!(
        ayaml::parse_value("? x\n\n: y\n").unwrap(),
        ayaml::parse_value("{x: y}\n").unwrap()
    );
}

#[test]
fn only_a_root_flow_node_may_continue_at_column_zero() {
    for (name, read) in value_readers() {
        for (input, expected) in [
            // Nested in a root flow collection.
            ("[{x\n: y}]\n", "[{x: y}]\n"),
            ("{k: {x\n: y}}\n", "{k: {x: y}}\n"),
            ("[[x\n, y]]\n", "[[x, y]]\n"),
            ("[[\nx\n]]\n", "[[x]]\n"),
            // With properties, past column 0, or on a `---` line.
            ("!!map {x\n: y}\n", "{x: y}\n"),
            ("&a {x\n: y}\n", "{x: y}\n"),
            ("!!map &a\n{x\n: y}\n", "{x: y}\n"),
            ("  {x\n: y}\n", "{x: y}\n"),
            ("  !!seq [x\n, y]\n", "[x, y]\n"),
            ("--- !!map {x\n: y}\n", "{x: y}\n"),
            ("%YAML 1.2\n---\n[\nx\n]\n", "[x]\n"),
            // Tabs alone.
            ("{x\n\t: y}\n", "{x: y}\n"),
            ("{x\n\t, y}\n", "{x: null, y: null}\n"),
            ("[\n\ta,\n\tb\n]\n", "[a, b]\n"),
            ("[[\n\ta\n]]\n", "[[a]]\n"),
            ("!!str 'a\n\tb'\n", "a b\n"),
            // Quoted scalars.
            ("'a\nb'\n", "a b\n"),
            ("  'a\nb'\n", "a b\n"),
            ("  \"a\nb\"\n", "a b\n"),
            ("!!str 'a\nb'\n", "a b\n"),
            ("--- 'a\nb'\n", "a b\n"),
            ("[ 'a\nb' ]\n", "[a b]\n"),
            ("{k: \"a\nb\"}\n", "{k: a b}\n"),
            // Under a block collection, an indented line is still right.
            ("k: {x\n : y}\n", "{k: {x: y}}\n"),
            ("k:\n  [\n a\n  ]\n", "{k: [a]}\n"),
            ("k: [[x\n , y]]\n", "{k: [[x, y]]}\n"),
            ("k:\n  'a\n  b'\n", "{k: a b}\n"),
            ("k: 'a\n b'\n", "{k: a b}\n"),
            // A comment line may start anywhere.
            ("k:\n  [\n# c\n    a\n  ]\n", "{k: [a]}\n"),
            ("k:\n  {\n# c\n    a: b\n  }\n", "{k: {a: b}}\n"),
            ("- \n  [\n# c\n    a\n  ]\n", "[[a]]\n"),
            ("k:\n  [\n    a\n# c\n  ]\n", "{k: [a]}\n"),
            ("k:\n  [\n    a, # c\n# d\n    b\n  ]\n", "{k: [a, b]}\n"),
            ("k: [\n# c\n  a\n  ]\n", "{k: [a]}\n"),
            ("k: [\n\t# c\n  a\n  ]\n", "{k: [a]}\n"),
        ] {
            let expected = ayaml::parse_value(expected).unwrap();
            assert_eq!(read(input).unwrap(), expected, "{name}: {input:?}");
        }
        // Compat keeps libyaml's looser rule for these.
        if name.starts_with("compat") {
            continue;
        }
        for input in [
            "k: {x\n: y}\n",
            "k: [{x\n: y}]\n",
            "k: !!map {x\n: y}\n",
            "- {x\n: y}\n",
            "? {x\n: y}\n",
            "k:\n  [\na\n  ]\n",
            "k:\n  {x\n, y}\n",
            "k:\n  !!seq [\na\n  ]\n",
            "k: [\n\ta\n]\n",
            "- [\n\ta\n]\n",
            "k: 'a\nb'\n",
            "---k: 'a\nb'\n",
            "...k: 'a\nb'\n",
        ] {
            assert!(read(input).is_err(), "{name}: {input:?}");
        }
    }
    // A document marker starts the rule again, and a later document's root node has it too.
    let documents =
        ayaml::parse_documents("k: v\n--- [\nx\n]\n...\n[\ny\n]\n--- 'a\nb'\n").unwrap();
    assert_eq!(documents.len(), 4, "{documents:?}");
    assert!(ayaml::parse_documents("[a]\n--- k: {x\n: y}\n").is_err());
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_keeps_reading_flow_lines_at_column_zero() {
    for (input, expected) in [
        ("k:\n  [\na\n  ]\n", "{k: [a]}\n"),
        ("[[x\n, y]]\n", "[[x, y]]\n"),
    ] {
        let read: Value = ayaml::compat::from_str(input).unwrap();
        assert_eq!(read, ayaml::parse_value(expected).unwrap(), "{input:?}");
    }
}

/// A node has at most one tag and one anchor, wherever they're written. A second one
/// used to replace the first without an error.
#[test]
fn a_second_tag_or_anchor_on_a_node_is_an_error() {
    for input in [
        "! !\n",
        "! ! a\n",
        "!a !b c\n",
        "!a !a c\n",
        "&a &b c\n",
        "&a &a c\n",
        "&a !t &b c\n",
        "!t &a !u c\n",
        "&a &b\n",
        "--- &a &b\nc\n",
        "&a &b\nc: d\n",
        "&a &b [c]\n",
        "&a &b {c: d}\n",
        "[&a &b c]\n",
        "[!a !b c]\n",
        "[&a &b c: d]\n",
        "[? &a &b c : d]\n",
        "{&a &b c: d}\n",
        "{k: &a &b c}\n",
        "{? &a &b c : d}\n",
        "- &a &b c\n",
        "- &a &b\n  - c\n",
        "k: &a &b c\n",
        "k: !a !b c\n",
        "k: &a &b\n  c\n",
        "? &a &b c\n: d\n",
        "&a &b c: d\n",
        "!a !b c: d\n",
        "&a\n&b c\n",
        "k: &a\n  &b [c]\n",
        "!\n!\n",
        "&a\n&b\n",
        "!a\n&b\n!c d\n",
        "&a\n&b\nc: d\n",
        "k: !a\n  !b\n  c\n",
        "- !a\n  !b\n  c\n",
        "? !a\n  !b\n  c\n",
    ] {
        for (reader, read) in value_readers() {
            let error = read(input).expect_err(&format!("{reader}: {input:?}"));
            assert_eq!(
                error.message(),
                "a node cannot have duplicate tag or anchor properties",
                "{reader}: {input:?}"
            );
        }
    }
    // Each of these has one tag and one anchor per node.
    for input in [
        "&a !t c\n",
        "!t &a c\n",
        "&a\n!t c\n",
        "&a\n&b c: 1\n",
        "!a\n!b c: 1\n",
        "k: &a\n  &b c: d\n",
        "- &a\n  &b c: d\n",
        "[&a a, &b b]\n",
    ] {
        for (reader, read) in value_readers() {
            read(input).unwrap_or_else(|error| panic!("{reader}: {input:?}: {error}"));
        }
    }
}

/// An explicit key's properties may come before an indentless sequence, as a mapping
/// value's may.
#[test]
fn an_explicit_key_with_properties_may_hold_an_indentless_sequence() {
    let sequence_key = |value: &Value| {
        let Value::Mapping(mapping) = value else {
            panic!("{value:?} isn't a mapping");
        };
        let (key, value) = mapping.iter().next().expect("one entry");
        (key.clone(), value.clone())
    };
    let items = ayaml::parse_value("[a]\n").unwrap();
    let tagged = Value::from(ayaml::TaggedValue::new(
        ayaml::Tag::local("t"),
        items.clone(),
    ));
    for (input, key, value) in [
        ("? &x\n- a\n", &items, Value::Null),
        ("? &x\n- a\n: b\n", &items, Value::from("b")),
        ("? !t\n- a\n", &tagged, Value::Null),
        ("? !t\n- a\n: b\n", &tagged, Value::from("b")),
        ("? !t &x\n- a\n: b\n", &tagged, Value::from("b")),
        ("? &x !t\n- a\n: b\n", &tagged, Value::from("b")),
        ("? !t\n  - a\n: b\n", &tagged, Value::from("b")),
    ] {
        for (reader, read) in value_readers() {
            let read = read(input).unwrap_or_else(|error| panic!("{reader}: {input:?}: {error}"));
            assert_eq!(
                sequence_key(&read),
                (key.clone(), value.clone()),
                "{input:?}"
            );
        }
    }
    // The sequence ends at the next key, and its anchor can be used there.
    let value = ayaml::parse_value("? &x\n- a\n: b\nc: *x\n").unwrap();
    assert_eq!(value["c"], items);
    // It works inside a compact mapping too.
    let value = ayaml::parse_value("- ? &x\n  - a\n  : b\n").unwrap();
    assert_eq!(sequence_key(&value[0]), (items, Value::from("b")));
}

/// A byte order mark takes up no column by default, so a marker or directive after one
/// starts a document. libyaml counts a column for it, so compat reads `---` as text there.
#[test]
fn a_bom_before_a_marker_or_directive_starts_a_document() {
    let limits = ayaml::Limits::default().with_max_documents(8);
    let documents = |input| ayaml::parse_documents_with_limits(input, &limits);
    let a = Value::from("a");
    let b = Value::from("b");
    let cases = [
        ("a\n...\n\u{feff}---\nb\n", vec![a.clone(), b.clone()]),
        ("a\n...\n\u{feff}--- b\n", vec![a.clone(), b.clone()]),
        (
            "a\n---\n\u{feff}---\nb\n",
            vec![a.clone(), Value::Null, b.clone()],
        ),
        ("a\n\u{feff}---\nb\n", vec![a.clone(), b.clone()]),
        ("a\n...\n\u{feff}...\n", vec![a.clone()]),
        ("a\n\u{feff}...\n", vec![a.clone()]),
        ("a\n\u{feff}...\nb\n", vec![a.clone(), b.clone()]),
        ("a\n\u{feff}\n...\nb\n", vec![a.clone(), b.clone()]),
        ("a\n\u{feff}# c\n...\nb\n", vec![a.clone(), b.clone()]),
        ("--- a\n\u{feff}...\n", vec![a.clone()]),
        ("---\n\u{feff}...\n", vec![Value::Null]),
        ("|\n a\n\u{feff}...\n", vec![Value::from("a\n")]),
        (
            "a\n...\n\u{feff}%YAML 1.2\n---\nb\n",
            vec![a.clone(), b.clone()],
        ),
        ("a\n...\n\u{feff}# c\n---\nb\n", vec![a.clone(), b.clone()]),
        ("a\n...\n\u{feff}\n---\nb\n", vec![a.clone(), b.clone()]),
        ("a\n...\n\u{feff}b\n", vec![a.clone(), b.clone()]),
        ("\u{feff}---\na\n", vec![a.clone()]),
        // The mark takes up no column, wherever it is.
        (
            "a\n...\n\u{feff}--- 'b\nc'\n",
            vec![a.clone(), Value::from("b c")],
        ),
        ("\u{feff}--- 'b\nc'\n", vec![Value::from("b c")]),
        ("\u{feff}'b\nc'\n", vec![Value::from("b c")]),
        ("\u{feff}# c\n  b\n  c\n", vec![Value::from("b c")]),
        // Its text is the same with any number of marks.
        ("\u{feff}\u{feff}--- b\nc\n", vec![Value::from("b c")]),
        ("\u{feff}\u{feff}--- |\nb\n", vec![Value::from("b\n")]),
        ("\u{feff}\u{feff}---\n |\nb\n", vec![Value::from("b\n")]),
        (
            "a\n...\n\u{feff}...\n |\nb\n",
            vec![a.clone(), Value::from("b\n")],
        ),
        ("\u{feff}...\n    b\n   c\n", vec![Value::from("b c")]),
        ("\u{feff}# c\n  b\n c\n", vec![Value::from("b c")]),
        ("\u{feff}\n\u{feff}\u{feff}# c\n", vec![]),
        // A prefix may repeat its mark.
        ("\u{feff}\u{feff}---\na\n", vec![a.clone()]),
        ("\u{feff}\n\u{feff}---\na\n", vec![a.clone()]),
        ("a\n\u{feff}\u{feff}---\nb\n", vec![a.clone(), b.clone()]),
        (
            "a\n...\n\u{feff}--- [\nb\n]\n",
            vec![a.clone(), Value::Sequence(vec![b.clone()])],
        ),
        (
            "k:\n\u{feff}---\nb\n",
            vec![ayaml::parse_value("{k: null}\n").unwrap(), b.clone()],
        ),
        (
            "-\n\u{feff}---\nb\n",
            vec![Value::Sequence(vec![Value::Null]), b.clone()],
        ),
        (
            "a\n...\n\u{feff}b: 1\nc: 2\n",
            vec![a.clone(), ayaml::parse_value("{b: 1, c: 2}\n").unwrap()],
        ),
        (
            "a\n...\n\u{feff}  b: 1\n  c: 2\n",
            vec![a.clone(), ayaml::parse_value("{b: 1, c: 2}\n").unwrap()],
        ),
        (
            "a\n...\n\u{feff}- b\n- c\n",
            vec![a.clone(), ayaml::parse_value("[b, c]\n").unwrap()],
        ),
        (
            "\u{feff}\u{feff}b: 1\nc: 2\n",
            vec![ayaml::parse_value("{b: 1, c: 2}\n").unwrap()],
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(documents(input).unwrap(), expected, "{input:?}");
        // Each way to read a stream agrees.
        let mut stream = ayaml::Deserializer::from_str(input).unwrap();
        let mut read = vec![];
        while let Some(value) = stream.next::<Value>().unwrap() {
            read.push(value);
        }
        assert_eq!(read, expected, "{input:?}");
    }
    // Indentation is the same with any number of marks.
    for (with_marks, without) in [
        ("\u{feff}  k: 'a\n b'\n", "  k: 'a\n b'\n"),
        ("\u{feff}\u{feff}  k: 'a\n b'\n", "  k: 'a\n b'\n"),
        ("  \n\u{feff}\u{feff} b: 1\n a]\n", "  \n b: 1\n a]\n"),
        ("...\r\u{feff}\u{feff}  k: a\r  b", "...\r  k: a\r  b"),
        (
            "--- >\n...\n\u{feff}\u{feff}  - |\n a\n",
            "--- >\n...\n  - |\n a\n",
        ),
    ] {
        assert!(documents(without).is_err(), "{without:?}");
        assert!(documents(with_marks).is_err(), "{with_marks:?}");
    }
    // A column doesn't count them either, in default mode.
    let error = ayaml::parse_value("# c\n\u{feff}k: [\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "unterminated flow sequence at line 3 column 1, opened at line 2 column 4"
    );
    // A directive's document has no room for a mark before its `---`.
    for input in [
        "%YAML 1.2\n\u{feff}---\na\n",
        "%YAML 1.2\n# c\n\u{feff}---\na\n",
        "%YAML 1.2\n\u{feff}# c\n---\na\n",
        "%YAML 1.2\n\u{feff}%TAG !e! tag:e.com,2000:\n---\na\n",
    ] {
        assert!(documents(input).is_err(), "{input:?}");
        assert!(
            ayaml::parse_events(input)
                .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                .is_err(),
            "{input:?}"
        );
    }
    // Inside a flow collection, a marker is still an error.
    assert!(ayaml::parse_value("[\n\u{feff}---\n]\n").is_err());
    // A mark that isn't first on its line isn't a document prefix.
    assert!(documents("a\n...\n \u{feff}---\nb\n").is_err());
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_counts_a_column_for_a_line_start_bom() {
    for (input, column) in [
        ("# c\n\u{feff}[a b", 6),
        ("# c\n\u{feff}k: [", 6),
        ("# c\n\u{feff}\u{1}", 2),
        ("# c\n\u{feff}a\u{1}", 3),
        ("# c\n\u{feff}a: b: c", 6),
        ("# c\n\u{feff}[a]]", 5),
    ] {
        let error = ayaml::compat::from_str::<Value>(input).unwrap_err();
        let start = error
            .span()
            .unwrap_or_else(|| panic!("{input:?}: {error}"))
            .start();
        assert_eq!(
            (start.line(), start.column()),
            (2, column),
            "{input:?}: {error}"
        );
    }
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_counts_that_column_for_block_structure() {
    let read = ayaml::compat::from_str::<Value>;
    // The mark makes the block collection one column deeper, so an indent of 1 is its level.
    assert_eq!(
        read("# c\n\u{feff}- 1\n - 2\n").expect("two entries"),
        ayaml::parse_value("- 1\n- 2\n").expect("two entries"),
    );
    for input in [
        "# c\n\u{feff}k: a\n b",
        "# c\n\u{feff}- a\n b",
        "# c\n\u{feff}- |\n x",
        "# c\n\u{feff}k: |\n x",
        "# c\n\u{feff} k: a\n  b",
    ] {
        assert!(read(input).is_err(), "{input:?}");
    }
    // A line deeper than that is still a continuation.
    for (input, expected) in [
        ("# c\n\u{feff}k: a\n  b", "k: a b\n"),
        ("# c\n\u{feff}- a\n  b", "- a b\n"),
        ("# c\n\u{feff}k: |\n  x", "k: \"x\"\n"),
    ] {
        let expected = ayaml::parse_value(expected).expect("control");
        assert_eq!(read(input).expect(input), expected, "{input:?}");
    }
    // A comment after a mark on an earlier line owns nothing, so a root scalar stays at the root.
    for input in ["\u{feff}# c\n\u{feff}a\nb", "\u{feff} # c\n\u{feff}a\nb"] {
        assert_eq!(read(input).expect(input), Value::from("a b"), "{input:?}");
    }
    // The first line's mark takes up no column, and the default mode counts none for any.
    assert_eq!(
        read("\u{feff}- 1\n  - 2\n").expect("one entry"),
        Value::Sequence(vec![Value::from("1 - 2")]),
    );
    assert_eq!(
        ayaml::parse_value("# c\n\u{feff}- 1\n - 2\n").expect("one entry"),
        Value::Sequence(vec![Value::from("1 - 2")]),
    );
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_reads_a_marker_after_a_bom_as_text() {
    let events = ayaml::compat::parse_events_with_limits(
        "a\n...\n\u{feff}---\nb\n",
        &ayaml::Limits::default(),
    )
    .unwrap();
    let scalars: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            ayaml::Event::Scalar { value, .. } => Some(value.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(scalars, ["a", "--- b"]);
}

/// The non-specific tag `!` isn't a shorthand, so `%TAG !` leaves it alone.
#[test]
fn the_non_specific_tag_ignores_a_primary_handle_directive() {
    let non_specific = |text: &str| {
        Value::from(ayaml::TaggedValue::new(
            ayaml::Tag::local(""),
            Value::from(text),
        ))
    };
    for (reader, read) in value_readers() {
        for input in ["%TAG ! tag:example.com,2000:\n---\n! 1\n", "! 1\n"] {
            assert_eq!(
                read(input).unwrap(),
                non_specific("1"),
                "{reader}: {input:?}"
            );
        }
        // A shorthand with the primary handle does expand.
        let value = read("%TAG ! tag:example.com,2000:\n---\n!x 1\n").unwrap();
        let Value::Tagged(tagged) = value else {
            panic!("{reader}: not tagged");
        };
        assert_eq!(tagged.tag, ayaml::Tag::new("tag:example.com,2000:", "x"));
    }
}

/// A tag is a core tag by its full name, however a handle splits that name.
#[test]
fn a_handle_expanding_to_a_core_name_is_that_core_tag() {
    for (reader, read) in value_readers() {
        // A name the core schema doesn't define reads as it does when written with `!!`.
        for (input, same) in [
            (
                "%TAG !y! tag:yaml.org,2002:app/\n---\n!y!Foo 1\n",
                "!!app/Foo 1\n",
            ),
            (
                "%TAG !y! tag:yaml.org,2002:app/\n---\n!y!Foo [1]\n",
                "!!app/Foo [1]\n",
            ),
        ] {
            assert_eq!(
                read(input).unwrap(),
                read(same).unwrap(),
                "{reader}: {input:?}"
            );
        }
        for input in [
            "%TAG !y! tag:yaml.org,2002:i\n---\n!y!nt 1\n",
            "%TAG !y! tag:yaml.org,2002:\n---\n!y!int 1\n",
            "%TAG ! tag:yaml.org,2002:i\n---\n!nt 1\n",
            "!<tag:yaml.org,2002:int> 1\n",
        ] {
            assert_eq!(
                read(input).unwrap(),
                Value::from(1_i64),
                "{reader}: {input:?}"
            );
        }
        for input in [
            "%TAG !y! tag:yaml.org,2002:i\n---\n!y!nt x\n",
            "%TAG !y! tag:yaml.org,2002:\n---\n!y!int x\n",
            "%TAG ! tag:yaml.org,2002:i\n---\n!nt x\n",
        ] {
            let error = read(input).expect_err(&format!("{reader}: {input:?}"));
            assert!(
                error.message().contains("invalid !!int"),
                "{reader}: {input:?}: {error}"
            );
        }
    }
    let tag = ayaml::Tag::new("tag:yaml.org,2002:s", "tr");
    assert!(tag.is_core());
    assert_eq!((tag.handle(), tag.suffix()), ("tag:yaml.org,2002:", "str"));
    assert_eq!(tag, ayaml::Tag::core("str"));
    // Near the core prefix isn't the core prefix.
    assert!(!ayaml::Tag::new("tag:yaml.org,", "2003:str").is_core());
    assert!(!ayaml::Tag::new("tag:", "example.com:x").is_core());
    assert_eq!(ayaml::Tag::new("", "!x"), ayaml::Tag::local("x"));
}

#[test]
fn tag_handles_count_against_their_limit_at_each_use() {
    let prefix = "tag:e.com,2000:";
    let tags = |count: usize| format!("%TAG !e! {prefix}\n---\n{}", "- !e!x a\n".repeat(count));
    let at_most = |bytes: usize| Limits::default().with_max_tag_expansion_bytes(bytes);
    let limit_of = |error: ayaml::Error| {
        assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
        assert!(error.span().is_some(), "{error}");
        let limit = error.limit().expect("a limit");
        assert_eq!(limit.kind(), ayaml::LimitKind::TagExpansion);
        (limit.limit(), limit.observed())
    };
    let over = |input: &str, limits: &Limits| {
        limit_of(ayaml::parse_value_with_limits(input, limits).expect_err(input))
    };

    // Below and at the limit, then one use above it.
    let three = 3 * prefix.len();
    ayaml::parse_value_with_limits(&tags(3), &at_most(three + 1)).expect("below");
    ayaml::parse_value_with_limits(&tags(3), &at_most(three)).expect("at");
    assert_eq!(
        over(&tags(4), &at_most(three)),
        (three as u64, (three + prefix.len()) as u64)
    );

    // The built-in handles count too, and a verbatim tag doesn't.
    let core = "tag:yaml.org,2002:".len();
    over("!!str a\n", &at_most(core - 1));
    ayaml::parse_value_with_limits("!!str a\n", &at_most(core)).expect("at");
    over("!x a\n", &at_most(0));
    ayaml::parse_value_with_limits("!<tag:e.com,2000:x> a\n", &at_most(0)).expect("verbatim");
    ayaml::parse_value_with_limits("! a\n", &at_most(0)).expect("non-specific");

    // The count covers the stream, not one document.
    let stream = format!("%TAG !e! {prefix}\n--- !e!x a\n...\n%TAG !e! {prefix}\n--- !e!x a\n");
    ayaml::parse_documents_with_limits(&stream, &at_most(2 * prefix.len())).expect("at");
    limit_of(
        ayaml::parse_documents_with_limits(&stream, &at_most(2 * prefix.len() - 1))
            .expect_err("over the limit"),
    );

    // The typed cursor sees it too, and the default leaves room for ordinary use.
    let input = tags(4);
    let mut cursor =
        ayaml::Deserializer::from_str_with_limits(&input, &at_most(three)).expect("start");
    limit_of(cursor.next::<Value>().expect_err("over the limit"));
    ayaml::parse_value(&tags(10_000)).expect("default");
}

/// An error elsewhere in the input isn't a limit because of what the input's text happens to hold.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_reports_a_scanner_error_as_itself() {
    for (input, message) in [
        ("&a *a\n@", "'@' is a reserved indicator"),
        ("!~!&-*-", "undefined tag handle"),
    ] {
        let error = ayaml::compat::from_str::<Value>(input).expect_err(input);
        assert_eq!(error.kind(), ayaml::ErrorKind::Scanner, "{input:?}");
        assert!(error.message().contains(message), "{input:?}: {error}");
    }
}

/// A tag that an alias copies counts with the rest of the copy, however the tag is written and
/// wherever it sits in the node the alias names.
#[test]
fn an_alias_is_charged_for_the_tags_it_copies() {
    let long = "e".repeat(1_000);
    let tight = Limits::default()
        .with_max_alias_expansion_bytes(100)
        .with_alias_bytes_per_input_byte(0);
    for input in [
        format!("%TAG !e! tag:{long}:\n---\n- &x !e!v a\n- *x\n"),
        format!("- &x !<tag:{long}:v> a\n- *x\n"),
        format!("- &x !<tag:{long}:v> [a]\n- *x\n"),
        format!("- &x !<tag:{long}:v> {{a: b}}\n- *x\n"),
        format!("- &x {{a: !<tag:{long}:v> b}}\n- *x\n"),
        format!("- &x [!<tag:{long}:v> [a]]\n- *x\n"),
        format!("- &x {{!<tag:{long}:v> k: v}}\n- {{<<: *x}}\n"),
    ] {
        let cursor = ayaml::Deserializer::from_str_with_limits(&input, &tight)
            .and_then(|mut cursor| cursor.next::<Value>().map(drop));
        for error in [
            ayaml::parse_value_with_limits(&input, &tight).map(drop),
            ayaml::from_str_borrowed_with_limits::<Value>(&input, &tight).map(drop),
            cursor,
        ]
        .map(|result| result.expect_err(&input))
        {
            let limit = error.limit().expect("a limit");
            assert_eq!(limit.kind(), ayaml::LimitKind::AliasExpansion, "{error}");
            assert!(limit.observed() > 1_000, "{error}");
        }
        ayaml::parse_value(&input).expect("the default leaves room for this");
    }
    assert_eq!(
        ayaml::MAX_TAG_EXPANSION_BYTES,
        Limits::default().max_tag_expansion_bytes
    );
}

/// A `---` or `...` after a mark is text in compat, and the rest of its line reads that way. A
/// quoted scalar's lines aren't held to a mark's column, because libyaml has no such check.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_reads_a_marks_line_as_libyaml_does() {
    let read = ayaml::compat::from_str::<Value>;
    for (input, expected) in [
        ("\n\u{feff}--- k: |\n  # c\n", "'--- k': \"# c\\n\""),
        ("\n\u{feff}--- k: |1\n       b\n", "'--- k': \"     b\\n\""),
        ("\n\u{feff}--- k: |\n  b\n", "'--- k': \"b\\n\""),
        ("\n\u{feff}--- k: |1\n  b\n", "'--- k': \"b\\n\""),
        ("\n\u{feff}--- k: a\n  b", "'--- k': a b"),
        ("\u{feff}--- k: |\n  # c\n", "'--- k': \"# c\\n\""),
        ("\u{feff}--- k: |1\n  b\n", "'--- k': \"b\\n\""),
        ("\u{feff}--- k: a\n  b", "'--- k': a b"),
        ("\u{feff}... k: |1\n  b\n", "'... k': \"b\\n\""),
        ("k:\n\u{feff}'a\n b'", "k: a b"),
        ("k:\n\u{feff}\"a\n b\"", "k: a b"),
        ("k:\n\u{feff} 'a\n  b'", "k: a b"),
        ("k:\n\u{feff}'a\nb'", "k: a b"),
        ("-\n\u{feff}'a\n b'", "- a b"),
        ("?\n\u{feff}'a\n b'", "? a b"),
        ("k: [\n\u{feff}'a\n b']", "k: [a b]"),
        ("# c\n\u{feff}k: 'a\n b'", "k: a b"),
        ("# c\n\u{feff}- 'a\n b'", "- a b"),
        ("\n\u{feff}- \"a\n b\"", "- a b"),
        ("\n\u{feff}k: 'a\n b'", "k: a b"),
    ] {
        assert_eq!(
            read(input).expect(input),
            ayaml::parse_value(expected).expect(expected),
            "{input:?}"
        );
    }
    for input in [
        "\u{feff}--- k: a\n b",
        "\u{feff}--- k: a\nz: 1",
        "\u{feff}... k: a\n b",
        "\u{feff}... k: a\nz: 1",
        "\n\u{feff}--- k: a\nz: 1",
    ] {
        read(input).expect_err(input);
    }
    // Without a marker, the first mark still takes up no column.
    assert_eq!(
        read("\u{feff}k: a\n b").expect("no column"),
        ayaml::parse_value("k: a b").expect("spelled out"),
    );
}
