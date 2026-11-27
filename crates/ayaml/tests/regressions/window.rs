//! Regression tests for bounded `LineWindow` streaming, compat retired-parent diagnostics, and
//! SWAR/UTF-8 scanner boundaries.

/// Same-line `--- <flow>` lines, repeated `LineWindow` compactions, oversized flow release, and
/// medium-width flow lines agree across all public entry points and never drop trailing tokens.
#[test]
fn document_start_same_line_flow_and_window_compaction_parity() {
    use std::fmt::Write as _;

    for (plain, with_start) in [
        ("{a: [1], b: 2}\n", "--- {a: [1], b: 2}\n"),
        ("{[1]: 2}\n", "--- {[1]: 2}\n"),
        ("[[1]: 2]\n", "--- [[1]: 2]\n"),
        ("[[1], a: 2]\n", "--- [[1], a: 2]\n"),
    ] {
        let expected = ayaml::parse_value(plain).expect(plain);
        assert_eq!(ayaml::parse_value(with_start).expect(with_start), expected);
        assert_eq!(
            ayaml::from_str::<ayaml::Value>(with_start).expect(with_start),
            expected
        );
        assert!(ayaml::SpannedTree::parse(with_start).is_ok());
        assert!(
            ayaml::parse_events(with_start)
                .expect(with_start)
                .collect::<Result<Vec<_>, _>>()
                .is_ok()
        );
    }
    for invalid in [
        "--- {a: [1] x}\n",
        "--- {a: [1] x, b: 2}\n",
        "--- [1, [2] x]\n",
    ] {
        assert_eq!(
            ayaml::parse_value(invalid).expect_err(invalid).kind(),
            ayaml::ErrorKind::Parser
        );
        assert_eq!(
            ayaml::from_str::<ayaml::Value>(invalid)
                .expect_err(invalid)
                .kind(),
            ayaml::ErrorKind::Parser
        );
        assert!(ayaml::SpannedTree::parse(invalid).is_err());
        assert!(
            ayaml::parse_events(invalid)
                .expect(invalid)
                .collect::<Result<Vec<_>, _>>()
                .is_err()
        );
    }

    // 400+ mixed flow-in-block lines, large flow line (>1,024 tokens) + block tail, and
    // medium-width >4 KiB flow lines all agree across public APIs.
    let mut mixed = String::new();
    for i in 0..400 {
        let _ = writeln!(mixed, "k{i}: [1, {{a: [2, 3], b: {i}}}, {i}]");
    }
    let mut flow_then_block = format!("big: [1{}]\n", ",1".repeat(600));
    for i in 0..200 {
        let _ = writeln!(flow_then_block, "b{i}: {i}");
    }
    let mut medium = String::new();
    for row in 0..25 {
        let _ = write!(medium, "r{row}: [0");
        for col in 1..70 {
            let _ = write!(medium, ",{col}");
        }
        medium.push_str("]\n");
    }
    for doc in [&mixed, &flow_then_block, &medium] {
        let val = ayaml::parse_value(doc).expect("parse_value");
        let typed: ayaml::Value = ayaml::from_str(doc).expect("from_str");
        assert_eq!(val, typed);
        assert!(ayaml::SpannedTree::parse(doc).is_ok());
        assert!(
            ayaml::parse_events(doc)
                .expect("parse_events")
                .collect::<Result<Vec<_>, _>>()
                .is_ok()
        );
    }
}

/// Compat retired-parent indentation diagnostics remain exact past the 128-line batch window.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_retired_parent_indentation_diagnostics_survive_batch_boundary() {
    use std::fmt::Write as _;

    let mut retired_map = String::from("outer:\n");
    for i in 0..160 {
        let _ = writeln!(retired_map, "  k{i}: {i}");
    }
    retired_map.push_str(" bad: 1\n");
    for err in [
        ayaml::compat::from_str::<ayaml::Value>(&retired_map).expect_err("compat from_str map"),
        ayaml::compat::parse_events_with(&retired_map, &ayaml::compat::ParseOptions::new())
            .expect("compat events")
            .collect::<Result<Vec<_>, _>>()
            .expect_err("compat events map"),
    ] {
        assert!(
            err.message()
                .contains("this line is indented differently from the mapping at line 1 column 1"),
            "{err}"
        );
    }

    let mut retired_seq = String::from("-\n");
    for i in 0..160 {
        let _ = writeln!(retired_seq, "  k{i}: {i}");
    }
    retired_seq.push_str(" - bad\n");
    let seq_err = ayaml::compat::from_str::<ayaml::Value>(&retired_seq).expect_err("compat seq");
    assert!(
        seq_err
            .message()
            .contains("this line is indented differently from the sequence at line 1 column 1"),
        "{seq_err}"
    );

    let mut bare_doc_start = String::from("---\n");
    for i in 0..160 {
        let _ = writeln!(bare_doc_start, "  k{i}: {i}");
    }
    bare_doc_start.push_str(" bad: 1\n");
    let doc_err =
        ayaml::compat::from_str::<ayaml::Value>(&bare_doc_start).expect_err("compat bare ---");
    assert!(
        !doc_err.message().contains("mapping at line 1"),
        "{doc_err}"
    );
}

/// SWAR 1..=24-byte alignments, >32-byte SWAR-crossing non-printable rejections, and 4-byte UTF-8
/// 1024-char implicit keys / wide-indented compact sequences hold across all public APIs.
#[test]
fn swar_alignments_non_printables_and_utf8_implicit_key_boundaries() {
    // Sweep 1..=24-byte prefixes across plain, flow, quoted, block scalars, and comments.
    for len in 1..=24 {
        let p = "a".repeat(len);
        let doc = format!(
            "plain: {p}é:x#y # {p}comment\n\
             flow: [{p}v, \"{p}\\\"é\", '{p}''é']\n\
             blk: |\n  {p}lineé\n"
        );
        let val = ayaml::parse_value(&doc).expect("swar parse_value");
        let typed: ayaml::Value = ayaml::from_str_borrowed(&doc).expect("swar from_str_borrowed");
        assert_eq!(val, typed, "len={len}");
        assert!(ayaml::SpannedTree::parse(&doc).is_ok(), "len={len}");
        assert!(
            ayaml::parse_events(&doc)
                .expect("events")
                .collect::<Result<Vec<_>, _>>()
                .is_ok(),
            "len={len}"
        );
    }

    // >32-byte SWAR-crossing non-printable rejection across every forbidden context, plus NEL OK.
    let pad = "x".repeat(36);
    for ch in ['\x01', '\x7f', '\u{80}', '\u{9f}', '\u{fffe}', '\u{ffff}'] {
        for bad in [
            format!("{pad}plain{ch}tail\n"),
            format!("a: 1 # {pad}comment{ch}tail\n"),
            format!("|\n  {pad}block{ch}tail\n"),
            format!("| # {pad}hdr{ch}tail\n  body\n"),
            format!("&{pad}anc{ch} 1\n"),
            format!("!{pad}tag{ch} 1\n"),
            format!("%TAG !e! tag:{pad}{ch}\n---\n1\n"),
        ] {
            assert_eq!(
                ayaml::parse_value(&bad).expect_err(&bad).kind(),
                ayaml::ErrorKind::Scanner,
                "{bad:?}"
            );
        }
    }
    assert!(ayaml::parse_value(&format!("k: {pad}nel\u{85}ok # {pad}\u{85}\n")).is_ok());

    // 4-byte UTF-8 1024-char (4096 B) implicit key accepted vs 1025-char (4100 B) rejected, and
    // wide-indented compact block sequence.
    let ok_key = "🚀".repeat(1_024);
    let bad_key = "🚀".repeat(1_025);
    assert!(ayaml::parse_value(&format!("{ok_key}: 1\n")).is_ok());
    assert!(ayaml::parse_value(&format!("[{ok_key}: 1]\n")).is_ok());
    assert_eq!(
        ayaml::parse_value(&format!("{bad_key}: 1\n"))
            .expect_err("1025 4-byte chars")
            .code(),
        ayaml::ErrorCode::InvalidImplicitKey
    );
    assert!(ayaml::parse_value(&format!("[{bad_key}: 1]\n")).is_err());
    let wide_compact = format!("{}- - - x\n", " ".repeat(4_096));
    let parsed = ayaml::parse_value(&wide_compact).expect("wide compact sequence");
    assert_eq!(parsed[0][0][0].as_str(), Some("x"));
}
