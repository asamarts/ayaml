#![allow(
    clippy::float_cmp,
    clippy::literal_string_with_formatting_args,
    reason = "test fixtures favor compact setup, generated inputs, and literal YAML braces"
)]

use super::*;

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

/// A single-document entry point stops at the second document's start, so a parser error later
/// in the stream does not hide the trailing document. The scanner reads the whole stream first,
/// so one of its errors there still does.
#[test]
fn single_document_entry_points_stop_at_the_second_document() {
    let stream = "a: 1\n---\nb: [1\n  c: 2\n";
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
        "more than one document in stream at line 2 column 1"
    );
}

/// YAML indents with spaces alone (production 63), so a tab that alone stands before a node on its
/// own line doesn't indent it under a key or an entry, in either mode, as libyaml refuses it too.
/// A space before the tab indents the node, and the tab then separates.
#[test]
fn a_tab_alone_indents_no_nested_node() {
    for (input, at) in [
        ("k:\n\tv", "2 column 2"),
        ("k:\n\t{a: b}", "2 column 2"),
        ("k:\n\t'a: b'", "2 column 2"),
        ("-\n\t[a]", "2 column 2"),
        ("k:\n\t|\n  x", "2 column 2"),
        ("a:\n  k:\n  \tv", "3 column 4"),
        ("- k:\n \t\tv", "2 column 4"),
    ] {
        for (mode, read) in readings(input) {
            assert_eq!(
                read,
                Err(format!(
                    "unexpected content after document at line {at}, indented by a tab, which \
                     YAML doesn't count as indentation"
                )),
                "{input:?} in {mode}"
            );
        }
    }
    for input in ["k: [a\n\tb]", "- k: [a\n  \tb]"] {
        for (mode, read) in readings(input) {
            assert!(read.is_err(), "{input:?} in {mode}: {read:?}");
        }
    }
    // Without a tab, or after a root that isn't a block collection, the error says nothing of one.
    for (input, at) in [
        ("k:\nv", "2 column 1"),
        ("[a]\n  \tb", "2 column 4"),
        ("\"x\"\n\tb", "2 column 2"),
    ] {
        for (mode, read) in readings(input) {
            assert_eq!(
                read,
                Err(format!("unexpected content after document at line {at}")),
                "{input:?} in {mode}"
            );
        }
    }
    // In compat a mark at a later line's start takes a column, as libyaml reads it, so the line
    // needs one more.
    let marked = "# c\n\u{feff}- k: [a\n   \tb]";
    assert_eq!(
        ayaml::parse_value(marked).expect(marked),
        ayaml::parse_value("- k: [a b]").expect("value")
    );
    #[cfg(feature = "libyaml-compat")]
    assert_eq!(
        ayaml::compat::from_str::<ayaml::Value>(marked)
            .expect_err(marked)
            .to_string(),
        "a tab cannot indent a flow collection's line under a block collection at line 3 column 4"
    );
    for (input, expected) in [
        ("k:\n \tv", "k: v"),
        ("k:\n \t{a: b}", "k: {a: b}"),
        ("k:\n \t'a: b'", "k: 'a: b'"),
        ("-\n \t[a]", "- [a]"),
        ("k: [a\n \tb]", "k: [a b]"),
        ("\tv", "v"),
        // A closing line or a comment line ends the scalar, so tabs alone may indent it.
        ("k: [a\n\t]", "k: [a]"),
        ("k: {a: b\n\t}", "k: {a: b}"),
        ("k: [a\n\t# c\n ]", "k: [a]"),
    ] {
        let expected = ayaml::parse_value(expected).expect(expected);
        for (mode, read) in readings(input) {
            assert_eq!(read.as_ref(), Ok(&expected), "{input:?} in {mode}");
        }
    }
}

/// By default a flow collection's lines under a block collection are indented past the innermost
/// one, whether a plain scalar goes on there or an entry starts there (YAML 1.2.2 production 69).
/// The error names where the collection opened, since a collection left open is the usual cause.
/// Compat reads such lines, as libyaml does, but for a key's `:` (see the next test).
#[test]
fn a_flow_collection_s_lines_are_indented_past_its_block_collection() {
    for (input, at, opened) in [
        (
            "k: [a\nb]",
            "2 column 1",
            "sequence opened at line 1 column 4",
        ),
        (
            "- [a\nb]",
            "2 column 1",
            "sequence opened at line 1 column 3",
        ),
        (
            "a:\n  b: [c\n  d]",
            "3 column 3",
            "sequence opened at line 2 column 6",
        ),
        (
            "a:\n  b: [c,\n  d]",
            "3 column 3",
            "sequence opened at line 2 column 6",
        ),
        (
            "- k: [a,\n  b]",
            "2 column 3",
            "sequence opened at line 1 column 6",
        ),
        (
            "- - [a,\n  b]",
            "2 column 3",
            "sequence opened at line 1 column 5",
        ),
        (
            "k: {a: [b,\nc]}",
            "2 column 1",
            "sequence opened at line 1 column 8",
        ),
    ] {
        let error = ayaml::parse_value(input).expect_err(input).to_string();
        assert_eq!(
            error,
            format!("flow collection entries must be indented at line {at}, in the flow {opened}"),
            "{input:?}"
        );
        #[cfg(feature = "libyaml-compat")]
        ayaml::compat::from_str::<ayaml::Value>(input).expect(input);
    }
    // A quoted scalar's lines are held to the innermost collection as well.
    for (input, line) in [("a:\n  b: 'c\n  d'", 3), ("a:\n  b: [ 'c\n  d' ]", 3)] {
        for (mode, read) in readings(input) {
            assert_eq!(
                read,
                Err(format!(
                    "quoted scalar continuation must be indented more than its parent at line \
                     {line} column 3"
                )),
                "{input:?} in {mode}"
            );
        }
    }
    for input in [
        "a:\n  b: 'c\n   d'",
        "a:\n  b: [ 'c\n   d' ]",
        "k: [a\n b]",
        "a:\n  b: [c\n   d]",
        "- k: [a,\n   b]",
        "k:\n  [a\n b]",
        "k: [a\n  b,\n c]",
        "k: [a\n]",
        "[a\nb]",
        "- [a\n  --- b]",
    ] {
        for (mode, read) in readings(input) {
            read.unwrap_or_else(|error| panic!("{input:?} in {mode}: {error}"));
        }
    }
}

/// An implicit flow key's `:` can't sit on a line that isn't indented past the block collection
/// around it, in compat too: the grammar refuses the line, and libyaml refuses it as well, as it
/// refuses a key that runs on to such a line, with a comment between them or not. Compat still reads
/// such a line that holds no key's `:`, and an explicit key's `:` there, as libyaml does.
#[test]
fn a_flow_key_s_colon_needs_an_indented_line() {
    for input in [
        "k: {x\n: y}",
        "k: {x\n:}",
        "k: {x,\n: y}",
        "k: {\"x\"\n: y}",
        "a:\n  {x\n: y}",
        "- {x\ny\n:}",
        "x: {s\nt: u}",
        "k: {x\ny\n : z}",
        "k: {x\ny # c\n : z}",
        "k: {x\ny\n # c\n : z}",
        // A `:` right after a quoted key or a flow collection is an indicator.
        "k: {\"x\"\n:y}",
        "k: {'x'\n:y}",
        "k: {[a]\n:y}",
        "k: {{a: b}\n:y}",
        // An implicit key in an explicit one is held to it, and so is one after an explicit entry.
        "k: {? [a\n: b]\n: c}",
        "k: {? a, x\n: y}",
    ] {
        for (mode, read) in readings(input) {
            let error = read.expect_err(input);
            assert!(
                error.starts_with("flow collection entries must be indented at line "),
                "{input:?} in {mode}: {error}"
            );
        }
    }
    #[cfg(feature = "libyaml-compat")]
    for (input, expected) in [
        ("k: {x\n:y}", "k: {x :y: }"),
        ("k: {a: 1,\nb: 2}", "k: {a: 1, b: 2}"),
        ("k: {x\ny,\nz: w}", "k: {x y: , z: w}"),
        ("k: {x\ny, \"z\": w}", "k: {x y: , z: w}"),
        // A `:` that isn't an indicator starts a plain scalar, which compat reads there.
        ("k: [a,\n:b]", "k: [a, ':b']"),
        // An explicit key's `:`, whatever line its key ends on.
        ("k: {? x\n: y}", "k: {x: y}"),
        ("k: {? \"x\"\n:y}", "k: {x: y}"),
        ("k: [? x\n: y]", "k: [{x: y}]"),
        ("k: {? x\n# c\n: y}", "k: {x: y}"),
        ("k: {? x\ny\n : z}", "k: {x y: z}"),
        ("k: {? x\ny : z}", "k: {x y: z}"),
        // A `,` in a collection in the key doesn't end the key.
        ("k: {? [a, b]\n: c}", "k: {[a, b]: c}"),
    ] {
        assert_eq!(
            ayaml::compat::from_str::<ayaml::Value>(input).expect(input),
            ayaml::parse_value(expected).expect(expected),
            "{input:?}"
        );
    }
}

/// Only a quoted scalar's own closing quote may start a line left of its indent. The other kind of
/// quote is text, and so is `''` in a single-quoted scalar, so their lines are held to it.
#[test]
fn a_quoted_line_is_held_to_the_indent_whatever_quote_starts_it() {
    for (input, line) in [
        ("k: \"a\n'b\"", 2),
        ("k: 'a\n\"b'", 2),
        ("k: 'a\n''b'", 2),
        ("k:\n  - \"a\n  'b\"", 3),
    ] {
        for (mode, read) in readings(input) {
            let error = read.expect_err(input);
            assert!(
                error.starts_with(&format!(
                    "quoted scalar continuation must be indented more than its parent at line \
                     {line} "
                )),
                "{input:?} in {mode}: {error}"
            );
        }
    }
    for (input, expected) in [
        ("k: 'a\n'", "a "),
        ("k: \"a\n\"", "a "),
        ("k: 'a\n ''b'", "a 'b"),
    ] {
        let expected = ayaml::Value::from(ayaml::Mapping::from_iter([(
            ayaml::Value::from("k"),
            ayaml::Value::from(expected),
        )]));
        for (mode, read) in readings(input) {
            assert_eq!(read.as_ref(), Ok(&expected), "{input:?} in {mode}");
        }
    }
}

/// After a compact key or entry, a quoted scalar's lines are held by default to the collection
/// that the key or entry starts, right of the line's indent, as a plain scalar's are: `- k: 'a\n b'`
/// needs three spaces. Compat holds them to the line's indent, as libyaml reads files that align
/// the lines with the key, and a mark that takes a column in compat doesn't make that stricter.
#[test]
fn a_quoted_scalar_after_a_compact_key_is_held_to_its_collection() {
    for (input, column, compat) in [
        ("- k: 'a\n b'", 2, "- k: a b"),
        ("- k: 'a\n  b'", 3, "- k: a b"),
        ("- - 'a\n  b'", 3, "- - a b"),
        ("- k: [ 'a\n b' ]", 2, "- k: [a b]"),
        ("- - k: \"a\n   b\"", 4, "- - k: a b"),
        ("- k: \"a\n  b\"\n  j: 1", 3, "- {k: a b, j: 1}"),
    ] {
        assert_eq!(
            ayaml::parse_value(input).map_err(|error| error.to_string()),
            Err(format!(
                "quoted scalar continuation must be indented more than its parent at line 2 \
                 column {column}"
            )),
            "{input:?}"
        );
        #[cfg(feature = "libyaml-compat")]
        assert_eq!(
            ayaml::compat::from_str::<ayaml::Value>(input).map_err(|error| error.to_string()),
            Ok(ayaml::parse_value(compat).expect(compat)),
            "{input:?} in compat"
        );
        let _ = compat;
    }
    // A line at the line's indent is short in each mode.
    for input in ["- k: 'a\nb'", "- - 'a\nb'", "- k: [ 'a\nb' ]"] {
        for (mode, read) in readings(input) {
            assert_eq!(
                read,
                Err(
                    "quoted scalar continuation must be indented more than its parent at line 2 \
                     column 1"
                        .to_owned()
                ),
                "{input:?} in {mode}"
            );
        }
    }
    for (input, expected) in [
        ("- k: 'a\n   b'", "- k: a b"),
        ("- - 'a\n   b'", "- - a b"),
        ("- k: [ 'a\n   b' ]", "- k: [a b]"),
        ("- - k: \"a\n     b\"", "- - k: a b"),
        ("k: 'a\n b'", "k: a b"),
    ] {
        let expected = ayaml::parse_value(expected).expect(expected);
        for (mode, read) in readings(input) {
            assert_eq!(read.as_ref(), Ok(&expected), "{input:?} in {mode}");
        }
    }
    #[cfg(feature = "libyaml-compat")]
    assert_eq!(
        ayaml::compat::from_str::<ayaml::Value>("# c\n\u{feff}- k: 'a\n   b'").expect("compat"),
        ayaml::parse_value("- k: a b").expect("value")
    );
    // A byte order mark that starts the stream takes no column.
    assert_eq!(
        ayaml::parse_value("\u{feff}- k: 'a\n b'").map_err(|error| error.to_string()),
        Err(
            "quoted scalar continuation must be indented more than its parent at line 2 column 2"
                .to_owned()
        )
    );
}

/// A document marker at column 0 ends a plain scalar in a flow collection, as it ends one anywhere,
/// and the flow collection then refuses it: no node holds one (YAML 1.2.2 production 206). A `---`
/// that isn't a marker, as before a `]` or indented, is text.
#[test]
fn a_document_marker_line_ends_a_plain_scalar_in_a_flow_collection() {
    for input in [
        "[a\n--- b]",
        "[a\n... b]",
        "{a: b\n--- c}",
        "[a\n---\n]",
        "[a\n...\n]",
    ] {
        for (mode, read) in readings(input) {
            assert_eq!(
                read,
                Err(
                    "document markers are not allowed inside flow collections at line 2 column 1"
                        .to_owned()
                ),
                "{input:?} in {mode}"
            );
        }
    }
    for (input, expected) in [("[a\n---]", "a ---"), ("[a\n --- b]", "a --- b")] {
        let expected = ayaml::Value::Sequence(vec![ayaml::Value::from(expected)]);
        for (mode, read) in readings(input) {
            assert_eq!(read.as_ref(), Ok(&expected), "{input:?} in {mode}");
        }
    }
}
