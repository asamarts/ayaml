//! Expanded denial-of-service regression tests (AY-134), executed inside the
//! bounded `in_child!` harness of `crates/ayaml/tests/adversarial.rs`.

use ayaml::{ErrorCode, ErrorKind, LimitKind, Limits, MAX_DEPTH, ParseOptions, SpannedTree, Value};

use super::{entry_points, in_child_named};

fn assert_limit_or_compat_rejection(name: &str, err: &ayaml::Error, expected_kind: LimitKind) {
    if let Some(limit) = err.limit() {
        assert_eq!(limit.kind(), expected_kind, "{name}: {err:?}");
        return;
    }
    assert!(
        err.kind() == ErrorKind::Limit
            || (name.starts_with("compat::") && err.kind() == ErrorKind::Serde),
        "{name}: expected limit error ({expected_kind:?}), got {err:?}"
    );
}

/// Deep alternating flow collections `[ { ... } ]` accept at `MAX_DEPTH` and are rejected cleanly
/// with `LimitKind::Depth` just above `MAX_DEPTH` and at extreme nesting (`10_000`) across every
/// reader including `SpannedTree`.
#[test]
fn deep_flow_collections_stay_bounded() {
    in_child!(|| {
        let make_nested = |depth: usize| -> String {
            let mut out = String::with_capacity(depth * 6 + 4);
            for level in 0..depth {
                if level % 2 == 0 {
                    out.push('[');
                } else {
                    out.push_str("{k: ");
                }
            }
            out.push('0');
            for level in (0..depth).rev() {
                if level % 2 == 0 {
                    out.push(']');
                } else {
                    out.push('}');
                }
            }
            out.push('\n');
            out
        };

        let at_limit = make_nested(MAX_DEPTH);
        assert!(ayaml::parse_value(&at_limit).is_ok());
        assert!(SpannedTree::parse(&at_limit).is_ok());

        for depth in [MAX_DEPTH + 1, 10_000] {
            let deep = make_nested(depth);
            for (name, result) in entry_points(&deep) {
                let err = result.expect_err(&format!("{name} depth={depth}"));
                assert_limit_or_compat_rejection(name, &err, LimitKind::Depth);
            }
            let spanned_err = SpannedTree::parse(&deep).expect_err("SpannedTree depth");
            assert_eq!(spanned_err.code(), ErrorCode::DepthLimit);
        }
    });
}

/// Large single-line flow mappings stay linear in memory and time and enforce
/// `max_collection_entries` / `max_events` cleanly without quadratic scanning.
#[test]
fn large_single_line_flow_mapping_stays_bounded() {
    in_child!(|| {
        use std::fmt::Write as _;

        // Moderate single-line flow mapping succeeds across all readers.
        let mut moderate = String::from("{");
        for i in 0..4_000 {
            if i > 0 {
                moderate.push_str(", ");
            }
            let _ = write!(moderate, "k{i}: {i}");
        }
        moderate.push_str("}\n");
        let parsed = ayaml::parse_value(&moderate).expect("moderate single-line flow mapping");
        assert_eq!(parsed.as_mapping().map(ayaml::Mapping::len), Some(4_000));
        assert!(SpannedTree::parse(&moderate).is_ok());

        // Single-line flow mapping exceeding a configured collection-entry limit fails cleanly.
        let tight =
            ParseOptions::new().with_limits(Limits::default().with_max_collection_entries(256));
        let err = ayaml::parse_value_with(&moderate, &tight).expect_err("entry limit");
        assert_eq!(err.kind(), ErrorKind::Limit);
        assert_eq!(err.code(), ErrorCode::CollectionEntriesLimit);

        // Exceeding default `max_events` (1,000,000 tokens/events) on a single line stops
        // promptly across every entry point (~4 tokens per `kN: 1,` entry).
        let mut huge = String::from("{");
        for i in 0..260_000 {
            if i > 0 {
                huge.push_str(", ");
            }
            let _ = write!(huge, "k{i}: 1");
        }
        huge.push_str("}\n");
        for (name, result) in entry_points(&huge) {
            let err = result.expect_err(name);
            assert_eq!(err.kind(), ErrorKind::Limit, "{name}: {err:?}");
        }
        let spanned_err = SpannedTree::parse(&huge).expect_err("SpannedTree huge flow mapping");
        assert_eq!(spanned_err.kind(), ErrorKind::Limit);
    });
}

/// Chained merge-and-alias inputs cannot bypass either the parser's alias budget or
/// `Value::apply_merge_keys_with_limits`.
#[test]
fn chained_merge_and_alias_inputs_stay_bounded() {
    in_child!(|| {
        use std::fmt::Write as _;

        // 1. Exponential merge-chain alias bomb (`&m0` -> `&m1 { <<: [*m0, *m0, ...], ... }`)
        //    is stopped by the parser's alias budget across every entry point.
        let mut bomb = String::from("m0: &m0 {a0: 1, b0: 2, c0: 3, d0: 4}\n");
        for level in 1..14 {
            let prev = format!("*m{}", level - 1);
            let _ = writeln!(
                bomb,
                "m{level}: &m{level} {{<<: [{prev}, {prev}, {prev}, {prev}], k{level}: {level}}}"
            );
        }
        for (name, result) in entry_points(&bomb) {
            let err = result.expect_err(name);
            assert_eq!(err.kind(), ErrorKind::Limit, "{name}: {err:?}");
        }
        // `SpannedTree` preserves alias syntax nodes without expanding them, so it stays compact.
        assert!(SpannedTree::parse(&bomb).is_ok());

        // 2. A smaller chained merge graph that fits parse limits is still bounded when
        //    `apply_merge_keys_with_limits` runs with tight entry/depth limits.
        let mut merge_chain = String::from("m0: &m0 {a: 1, b: 2, c: 3, d: 4}\n");
        for level in 1..6 {
            let prev = format!("*m{}", level - 1);
            let _ = writeln!(
                merge_chain,
                "m{level}: &m{level} {{<<: [{prev}, {prev}], x{level}: {level}}}"
            );
        }
        let parsed = ayaml::parse_value(&merge_chain).expect("small merge chain parses");
        let mut bounded_entries = parsed.clone();
        let entry_err = bounded_entries
            .apply_merge_keys_with_limits(&Limits::default().with_max_collection_entries(3))
            .expect_err("tight entry limit must reject chained merge");
        assert_eq!(entry_err.kind(), ErrorKind::Limit);
        assert_eq!(entry_err.code(), ErrorCode::CollectionEntriesLimit);

        let mut bounded_depth = parsed.clone();
        let depth_err = bounded_depth
            .apply_merge_keys_with_limits(&Limits::default().with_max_depth(1))
            .expect_err("tight depth limit must reject nested merge");
        assert_eq!(depth_err.kind(), ErrorKind::Limit);
        assert_eq!(depth_err.code(), ErrorCode::DepthLimit);

        let mut full = parsed;
        full.apply_merge_keys()
            .expect("small merge chain succeeds under default limits");
    });
}

/// Oversized `%TAG` handle prefixes repeated across many nodes hit `LimitKind::TagExpansion`
/// before memory can grow disproportionately to the input size.
#[test]
fn oversized_tag_handles_stay_bounded() {
    in_child!(|| {
        let prefix = format!("tag:example.com,2026:{}/", "a".repeat(8_000));
        let mut input = format!("%TAG !p! {prefix}\n---\n");
        // ~9,000 short `!p!x` uses (~54 KiB input) expand past the 64 MiB default tag cap.
        for _ in 0..9_000 {
            input.push_str("- !p!x 1\n");
        }
        assert!(
            input.len() < 128 * 1024,
            "input stayed small: {}",
            input.len()
        );

        for (name, result) in entry_points(&input) {
            let err = result.expect_err(name);
            assert_eq!(err.kind(), ErrorKind::Limit, "{name}: {err:?}");
            assert_eq!(err.code(), ErrorCode::TagExpansionLimit, "{name}: {err:?}");
        }
        let spanned_err = SpannedTree::parse(&input).expect_err("SpannedTree tag expansion");
        assert_eq!(spanned_err.code(), ErrorCode::TagExpansionLimit);
    });
}

/// Repeated `...` / `---\n...` document-end markers stay bounded in time and memory and enforce
/// `max_documents` and `max_events` cleanly.
#[test]
fn repeated_document_end_markers_stay_bounded() {
    in_child!(|| {
        // 1. Many consecutive bare `...` markers in a single valid document stay linear and fast.
        let mut repeated_end = String::from("---\nkey: value\n");
        for _ in 0..20_000 {
            repeated_end.push_str("...\n");
        }
        let parsed = ayaml::parse_value(&repeated_end).expect("repeated document-end markers");
        assert_eq!(parsed["key"], Value::from("value"));
        let docs = ayaml::parse_documents(&repeated_end).expect("parse_documents repeated end");
        assert_eq!(docs.len(), 1);
        assert!(SpannedTree::parse(&repeated_end).is_ok());

        // 2. Repeated `---\n...\n` empty documents exceed `DEFAULT_MAX_DOCUMENTS` (10,000) and
        //    fail with `DocumentCountLimit` across multi-document readers (and `TrailingDocuments`
        //    on single-document readers).
        let many_docs = "---\n...\n".repeat(10_005);
        let docs_err = ayaml::parse_documents(&many_docs).expect_err("must enforce max_documents");
        assert_eq!(docs_err.kind(), ErrorKind::Limit);
        assert_eq!(docs_err.code(), ErrorCode::DocumentCountLimit);

        let single_err = ayaml::parse_value(&many_docs).expect_err("trailing documents");
        assert_eq!(single_err.code(), ErrorCode::TrailingDocuments);

        let mut cursor = ayaml::Deserializer::from_str(&many_docs).expect("cursor init");
        let mut cursor_err = None;
        while let Some(res) = cursor.next::<Value>().transpose() {
            if let Err(err) = res {
                cursor_err = Some(err);
                break;
            }
        }
        let err = cursor_err.expect("cursor must enforce max_documents");
        assert_eq!(err.kind(), ErrorKind::Limit);
        assert_eq!(err.code(), ErrorCode::DocumentCountLimit);

        let spanned_err =
            SpannedTree::parse(&many_docs).expect_err("SpannedTree must enforce max_documents");
        assert_eq!(spanned_err.kind(), ErrorKind::Limit);
        assert_eq!(spanned_err.code(), ErrorCode::DocumentCountLimit);
    });
}

/// Duplicate-key collision stress (large unique maps, numeric-equivalent duplicates, and
/// complex-key collisions) stays bounded and detects duplicates cleanly without quadratic stalls.
#[test]
fn duplicate_key_collision_stress_stays_bounded() {
    in_child!(|| {
        use std::fmt::Write as _;

        // 1. High-cardinality mapping with a duplicate at the very end.
        let mut trailing_dup = String::with_capacity(64 * 1024);
        for i in 0..5_000 {
            let _ = writeln!(trailing_dup, "key_{i:05}: {i}");
        }
        trailing_dup.push_str("key_00000: 99999\n");

        for (name, result) in entry_points(&trailing_dup) {
            if name.contains("seed") || name.contains("deserialize_any") {
                continue;
            }
            let err = result.expect_err(name);
            assert!(
                err.kind() == ErrorKind::DuplicateKey || err.to_string().contains("duplicate"),
                "{name}: {err:?}"
            );
        }
        // `SpannedTree` records duplicate mapping keys as syntax entries without quadratic stalls.
        assert!(SpannedTree::parse(&trailing_dup).is_ok());

        // 2. Semantic numeric collision across decimal / hex / octal spellings in a wide map.
        let mut numeric_dup = String::with_capacity(32 * 1024);
        for i in 0..2_000 {
            let _ = writeln!(numeric_dup, "{i}: v{i}");
        }
        numeric_dup.push_str("0x10: duplicate_sixteen\n");
        let err = ayaml::parse_value(&numeric_dup).expect_err("0x10 duplicates 16");
        assert_eq!(err.kind(), ErrorKind::DuplicateKey);
        assert_eq!(err.code(), ErrorCode::DuplicateKey);
        assert!(SpannedTree::parse(&numeric_dup).is_ok());

        // 3. Many complex mapping keys (distinct flow sequences/maps) ending in a reordered
        //    equivalent composite key.
        let mut complex_dup = String::with_capacity(64 * 1024);
        for i in 0..1_000 {
            let _ = writeln!(complex_dup, "? {{a: {i}, b: [{i}, {}]}}\n: {i}", i + 1);
        }
        complex_dup.push_str("? {b: [0, 1], a: 0}\n: duplicate\n");
        let err = ayaml::parse_value(&complex_dup).expect_err("reordered complex key duplicate");
        assert_eq!(err.kind(), ErrorKind::DuplicateKey);
        assert_eq!(err.code(), ErrorCode::DuplicateKey);
    });
}

/// Frontmatter helpers enforce alias, depth, and input-size limits boundedly on hostile headers
/// without scanning past `max_input_bytes` or failing on large non-YAML bodies.
#[test]
fn frontmatter_enforces_limits_and_alias_bombs_boundedly() {
    in_child!(|| {
        // Alias bomb embedded in frontmatter is rejected by both typed and value parsers.
        let bomb_doc = format!("---\n{}---\n# Markdown body\n", super::alias_bomb());
        for err in [
            ayaml::parse_frontmatter::<Value>(&bomb_doc).expect_err("typed bomb"),
            ayaml::parse_frontmatter_value(&bomb_doc).expect_err("value bomb"),
        ] {
            assert_eq!(err.kind(), ErrorKind::Limit);
        }

        // Unclosed oversized frontmatter fails with InputBytes without scanning past the limit.
        let tight = ParseOptions::new().with_limits(Limits::new().with_max_input_bytes(256));
        let mut huge_unclosed = String::from("---\n");
        huge_unclosed.push_str(&"a".repeat(64 * 1024));
        let err =
            ayaml::split_frontmatter_with(&huge_unclosed, &tight).expect_err("oversized unclosed");
        assert_eq!(err.kind(), ErrorKind::Limit);
        assert_eq!(
            err.limit().map(ayaml::LimitExceeded::kind),
            Some(LimitKind::InputBytes)
        );

        // Deep nesting in frontmatter is rejected with DepthLimit.
        let deep = format!(
            "---\n{}{}\n---\nbody\n",
            "[".repeat(MAX_DEPTH + 1),
            "]".repeat(MAX_DEPTH + 1)
        );
        let err = ayaml::parse_frontmatter_value(&deep).expect_err("deep frontmatter");
        assert_eq!(err.code(), ErrorCode::DepthLimit);
    });
}
