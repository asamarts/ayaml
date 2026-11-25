//! Unit and property tests for frontmatter / prefix-document splitting and parsing (AY-146).

use ayaml::{
    ColumnEncoding, ErrorCode, ErrorKind, FrontmatterDelimiter, LimitKind, Limits, LineIndex,
    MAX_DEPTH_CAP, ParseOptions, PathSegment, Position, SpannedTree, Value,
};
use proptest::prelude::*;
use serde::Deserialize;

#[derive(Debug, PartialEq, Eq, Deserialize)]
struct BorrowedHeader<'a> {
    title: &'a str,
    tags: Vec<&'a str>,
}

#[test]
fn handles_no_frontmatter_and_non_delimiter_prefixes() {
    for input in [
        "",
        "\u{feff}",
        "# Plain Markdown\n\nBody text.\n",
        "----\nnot frontmatter\n----\n",
        "--- inline scalar\n---\nbody\n",
        "---#no-space-comment\n---\nbody\n",
    ] {
        assert_eq!(
            ayaml::split_frontmatter(input).expect("split"),
            None,
            "{input:?}"
        );
        assert_eq!(
            ayaml::parse_frontmatter_value(input).expect("parse value"),
            None,
            "{input:?}"
        );
    }
}

#[test]
fn handles_bom_crlf_delimiters_empty_and_block_scalars_with_dashes() {
    // Leading BOM + CRLF + block scalar containing indented `---` and `...` + `...` closer
    // + arbitrary non-YAML body (including form feed and unbalanced brackets).
    let input = "\u{feff}--- # header\r\ntitle: Hello\r\nnote: |\r\n  line 1\r\n  ---\r\n  ...\r\n  line 2\r\n...   # end\r\n# Body\r\n\x0c[unclosed\r\n";
    let split = ayaml::split_frontmatter(input)
        .expect("split")
        .expect("has frontmatter");
    assert_eq!(split.closing_delimiter(), FrontmatterDelimiter::DocumentEnd);
    assert_eq!(split.yaml_span().start(), 0);
    assert_eq!(&input[split.yaml_span().range()], split.yaml());
    assert_eq!(&input[split.delimiter_span().range()], "...");
    assert_eq!(split.body(), "# Body\r\n\x0c[unclosed\r\n");
    assert_eq!(&input[split.body_offset()..], split.body());

    // LineIndex aligns spans from parsing `split.yaml()` directly with `input`.
    let index = LineIndex::new(input);
    let tree = SpannedTree::parse(split.yaml()).expect("spanned");
    let title_span = tree
        .span_at(&[PathSegment::key("title")])
        .expect("title span");
    assert_eq!(&input[title_span.range()], "Hello");
    assert_eq!(
        index
            .to_position(title_span.start(), ColumnEncoding::Utf8)
            .expect("title pos"),
        Position::new(1, 7)
    );
    assert_eq!(
        index
            .to_position(split.body_offset(), ColumnEncoding::Utf8)
            .expect("body pos"),
        Position::new(8, 0)
    );

    let parsed = ayaml::parse_frontmatter_value(input)
        .expect("parse value")
        .expect("has frontmatter");
    assert_eq!(parsed.frontmatter["title"].as_str(), Some("Hello"));
    assert_eq!(
        parsed.frontmatter["note"].as_str(),
        Some("line 1\n---\n...\nline 2\n")
    );
    assert_eq!(parsed.body, split.body());
    assert_eq!(parsed.body_offset, split.body_offset());

    // Empty frontmatter with `---` and `...`, with and without trailing newline.
    for (doc, delim, expected_body) in [
        (
            "---\n---\nbody\n",
            FrontmatterDelimiter::DocumentStart,
            "body\n",
        ),
        (
            "---\r\n...\r\nbody",
            FrontmatterDelimiter::DocumentEnd,
            "body",
        ),
        ("---\n---", FrontmatterDelimiter::DocumentStart, ""),
        ("---\n...", FrontmatterDelimiter::DocumentEnd, ""),
    ] {
        let parsed = ayaml::parse_frontmatter_value(doc)
            .expect("empty frontmatter")
            .expect("some");
        assert_eq!(parsed.frontmatter, Value::Null);
        assert_eq!(parsed.closing_delimiter, delim);
        assert_eq!(parsed.body, expected_body);
        assert_eq!(&doc[parsed.body_offset..], parsed.body);
    }

    // Multiline double-quoted scalar containing an indented `---` line stays inside frontmatter.
    let quoted_doc = "---\nmsg: \"hello\n  ---\n  world\"\n---\nBody\n";
    let parsed = ayaml::parse_frontmatter_value(quoted_doc)
        .expect("quoted dashes")
        .expect("some");
    assert_eq!(parsed.frontmatter["msg"].as_str(), Some("hello --- world"));
    assert_eq!(parsed.body, "Body\n");

    // Syntax errors inside frontmatter preserve exact spans and locations in the original input.
    let bad_yaml = "\u{feff}---\na: 1\nb: \"\\z\"\n---\n# Body\n";
    let err = ayaml::parse_frontmatter_value(bad_yaml).expect_err("syntax error");
    let err_span = err.span().expect("error span");
    let bad_index = LineIndex::new(bad_yaml);
    let err_pos = bad_index
        .to_position(err_span.start(), ColumnEncoding::Utf8)
        .expect("error pos");
    assert_eq!(err_pos.line, 2);
    assert_eq!(err.location().map(ayaml::Location::line), Some(3));

    // Borrowed deserialization works directly from the original input.
    let typed_doc = "---\ntitle: Borrowed\ntags: [a, b]\n---\nParagraph\n";
    let parsed: ayaml::ParsedFrontmatter<'_, BorrowedHeader<'_>> =
        ayaml::parse_frontmatter(typed_doc)
            .expect("typed")
            .expect("some");
    assert_eq!(
        parsed.frontmatter,
        BorrowedHeader {
            title: "Borrowed",
            tags: vec!["a", "b"],
        }
    );
    assert_eq!(parsed.body, "Paragraph\n");
}

#[test]
fn rejects_missing_closing_delimiter_and_enforces_limits() {
    for unclosed in [
        "---",
        "\u{feff}---",
        "---\n",
        "---\ntitle: hi\n",
        "---\na: 1\n--- extra text\n",
    ] {
        let error = ayaml::split_frontmatter(unclosed).expect_err(unclosed);
        assert_eq!(error.kind(), ErrorKind::Parser, "{unclosed:?}: {error}");
        assert_eq!(
            error.code(),
            ErrorCode::UnexpectedToken,
            "{unclosed:?}: {error}"
        );
        assert_eq!(
            error.location().map(|loc| (loc.line(), loc.column())),
            Some((1, 1)),
            "{unclosed:?}: {error}"
        );
    }

    // Invalid limits fail fast even when the input has no frontmatter.
    for bad in [
        Limits::new().with_max_depth(MAX_DEPTH_CAP + 1),
        Limits::new().with_node_cost_bytes(0),
    ] {
        let opts = ParseOptions::new().with_limits(bad);
        for error in [
            ayaml::split_frontmatter_with("# plain\n", &opts).unwrap_err(),
            ayaml::parse_frontmatter_with::<Value>("# plain\n", &opts).unwrap_err(),
            ayaml::parse_frontmatter_value_with("# plain\n", &opts).unwrap_err(),
        ] {
            assert_eq!(error.kind(), ErrorKind::Limit);
            assert_eq!(error.limit(), None);
        }
    }

    // Oversized body after a small frontmatter is accepted even when total input > max_input_bytes.
    let small_opts = ParseOptions::new().with_limits(Limits::new().with_max_input_bytes(32));
    let mut big_body = String::from("---\na: 1\n---\n");
    big_body.push_str(&"x".repeat(4096));
    let parsed = ayaml::parse_frontmatter_value_with(&big_body, &small_opts)
        .expect("small header with large body")
        .expect("some");
    assert_eq!(parsed.frontmatter["a"].as_u64(), Some(1));
    assert_eq!(parsed.body.len(), 4096);

    // Oversized frontmatter (closed or unclosed single long line) fails with LimitKind::InputBytes.
    let big_header = format!("---\nk: {}\n---\nbody\n", "v".repeat(64));
    let error = ayaml::split_frontmatter_with(&big_header, &small_opts).expect_err("big header");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(
        error.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::InputBytes)
    );

    let unclosed_long_line = format!("---\n{}", "z".repeat(4096));
    let error = ayaml::split_frontmatter_with(&unclosed_long_line, &small_opts)
        .expect_err("unclosed long line");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(
        error.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::InputBytes)
    );

    // Oversized comment on the opening `---` line fails boundedly with LimitKind::InputBytes.
    let long_open_comment = format!("--- #{}\na: 1\n---\nbody\n", "c".repeat(4096));
    let error = ayaml::split_frontmatter_with(&long_open_comment, &small_opts)
        .expect_err("long opening comment");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(
        error.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::InputBytes)
    );

    // Oversized comment on the closing delimiter line also fails boundedly with InputBytes.
    let long_close_comment = format!("---\na: 1\n--- #{}\nbody\n", "c".repeat(4096));
    let error = ayaml::split_frontmatter_with(&long_close_comment, &small_opts)
        .expect_err("long closing comment");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(
        error.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::InputBytes)
    );

    // Bare CR (`\r`) line breaks work consistently with YAML 1.2.2 and LineIndex.
    for (doc, expected_body) in [
        ("---\ra: 1\r---\rbody", "body"),
        ("---\na: 1\r---\nbody\n", "body\n"),
        ("---\na: 1\n---\rbody", "body"),
    ] {
        let parsed = ayaml::parse_frontmatter_value(doc)
            .expect("cr frontmatter")
            .expect("some");
        assert_eq!(parsed.frontmatter["a"].as_u64(), Some(1));
        assert_eq!(parsed.body, expected_body);
        assert_eq!(&doc[parsed.body_offset..], parsed.body);
    }

    // Depth and other parser limits are enforced when parsing frontmatter.
    let depth_opts = ParseOptions::new().with_limits(Limits::new().with_max_depth(1));
    let error = ayaml::parse_frontmatter_value_with("---\na: [1, 2]\n---\nbody\n", &depth_opts)
        .expect_err("depth limit");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(
        error.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::Depth)
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn round_trips_frontmatter_and_aligns_line_index_spans(
        bom in any::<bool>(),
        crlf in any::<bool>(),
        use_doc_end in any::<bool>(),
        title in "[a-zA-Z0-9 _.-]{1,24}",
        block_mid in "[a-zA-Z0-9 _.-]{0,16}",
        body in "[\\PC\n]{0,64}",
    ) {
        let nl = if crlf { "\r\n" } else { "\n" };
        let close = if use_doc_end { "..." } else { "---" };
        let expected_delim = if use_doc_end {
            FrontmatterDelimiter::DocumentEnd
        } else {
            FrontmatterDelimiter::DocumentStart
        };

        let mut doc = String::new();
        if bom {
            doc.push('\u{feff}');
        }
        doc.push_str("---");
        doc.push_str(nl);
        doc.push_str("title: '");
        doc.push_str(&title.replace('\'', "''"));
        doc.push('\'');
        doc.push_str(nl);
        doc.push_str("note: |2");
        doc.push_str(nl);
        doc.push_str("  ");
        doc.push_str(&block_mid);
        doc.push_str(nl);
        doc.push_str("  ---");
        doc.push_str(nl);
        doc.push_str("  ...");
        doc.push_str(nl);
        let expected_yaml_end = doc.len();
        doc.push_str(close);
        doc.push_str(nl);
        let expected_body_offset = doc.len();
        doc.push_str(&body);

        let split = ayaml::split_frontmatter(&doc)
            .expect("valid split")
            .expect("has frontmatter");
        prop_assert_eq!(split.closing_delimiter(), expected_delim);
        prop_assert_eq!(split.yaml_span().start(), 0);
        prop_assert_eq!(split.yaml_span().end(), expected_yaml_end);
        prop_assert_eq!(split.body_offset(), expected_body_offset);
        prop_assert_eq!(split.body(), body.as_str());
        prop_assert_eq!(&doc[split.body_offset()..], split.body());

        // SpannedTree spans over split.yaml() align identically with LineIndex::new(&doc).
        let index = LineIndex::new(&doc);
        let tree = SpannedTree::parse(split.yaml()).expect("spanned");
        let title_span = tree
            .span_at(&[PathSegment::key("title")])
            .expect("title span");
        prop_assert_eq!(&doc[title_span.range()], &split.yaml()[title_span.range()]);
        prop_assert_eq!(
            index
                .to_position(title_span.start(), ColumnEncoding::Utf8)
                .expect("title pos")
                .line,
            1
        );
        prop_assert_eq!(
            index
                .to_position(split.body_offset(), ColumnEncoding::Utf8)
                .expect("body pos")
                .line,
            7
        );

        // Decoded Value matches expected fields.
        let parsed = ayaml::parse_frontmatter_value(&doc)
            .expect("parse value")
            .expect("has frontmatter");
        prop_assert_eq!(parsed.frontmatter["title"].as_str(), Some(title.as_str()));
        let expected_note = format!("{block_mid}\n---\n...\n");
        prop_assert_eq!(
            parsed.frontmatter["note"].as_str(),
            Some(expected_note.as_str())
        );
        prop_assert_eq!(parsed.body, body.as_str());
        prop_assert_eq!(parsed.body_offset, expected_body_offset);
    }
}
