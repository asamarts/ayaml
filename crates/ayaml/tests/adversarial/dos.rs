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

        // Oversized comment or whitespace on the closing delimiter line cannot bypass
        // `max_input_bytes`.
        let huge_close_comment = format!("---\na: 1\n--- #{}\nbody\n", "c".repeat(1_000_000));
        let err = ayaml::split_frontmatter_with(&huge_close_comment, &tight)
            .expect_err("oversized closing comment");
        assert_eq!(err.kind(), ErrorKind::Limit);
        assert_eq!(
            err.limit().map(ayaml::LimitExceeded::kind),
            Some(LimitKind::InputBytes)
        );

        let huge_close_ws = format!("---\na: 1\n---{}x\nbody\n", " ".repeat(1_000_000));
        let err = ayaml::split_frontmatter_with(&huge_close_ws, &tight)
            .expect_err("oversized closing whitespace");
        assert_eq!(err.kind(), ErrorKind::Limit);
        assert_eq!(
            err.limit().map(ayaml::LimitExceeded::kind),
            Some(LimitKind::InputBytes)
        );
    });
}

/// Dense empty-value flow mappings and deeply nested block closings followed by long blank/comment
/// runs parse in linear time in `SpannedTree::parse` without quadratic rescanning (AY-169).
#[test]
fn spanned_tree_empty_flow_entries_and_trailing_blanks_stay_linear() {
    in_child!(|| {
        use ayaml::PathSegment;
        use std::fmt::Write as _;
        use std::time::{Duration, Instant};

        // 1. Single-line dense empty-value flow mapping (`{k0:,k1:,...,kN:}`).
        let entries = if cfg!(debug_assertions) {
            8_000_usize
        } else {
            40_000_usize
        };
        let mut flow = String::with_capacity(entries * 9 + 4);
        flow.push('{');
        for i in 0..entries {
            if i > 0 {
                flow.push(',');
            }
            let _ = write!(flow, "k{i}:");
        }
        flow.push_str("}\n");

        let started = Instant::now();
        let tree = SpannedTree::parse(&flow).expect("dense empty-value flow mapping parses");
        let elapsed = started.elapsed();
        let first_span = tree
            .span_at(&[PathSegment::Key(Value::from("k0"))])
            .expect("first key span");
        assert!(first_span.is_empty());
        assert_eq!(flow.as_bytes()[first_span.start() - 1], b':');
        let last_key = format!("k{}", entries - 1);
        let last_span = tree
            .span_at(&[PathSegment::Key(Value::from(last_key))])
            .expect("last key span");
        assert!(last_span.is_empty());
        assert_eq!(flow.as_bytes()[last_span.start() - 1], b':');
        assert!(
            elapsed < Duration::from_secs(10),
            "dense flow mapping ({entries} entries) took {elapsed:?}"
        );

        // 2. Deeply nested block mapping after a block scalar followed by 100k blank/comment lines.
        let depth = 100_usize;
        let trailing_lines = if cfg!(debug_assertions) {
            25_000_usize
        } else {
            100_000_usize
        };
        let mut block = String::from("hdr: |\n  keep\n");
        for level in 0..depth {
            for _ in 0..level {
                block.push_str("  ");
            }
            let _ = writeln!(block, "k{level}:");
        }
        for _ in 0..depth {
            block.push_str("  ");
        }
        block.push_str("leaf:\n");
        for i in 0..trailing_lines {
            if i % 2 == 0 {
                block.push('\n');
            } else {
                block.push_str("# comment\n");
            }
        }
        block.push_str("tail: 1\n");

        let started = Instant::now();
        let tree = SpannedTree::parse(&block).expect("deep block with trailing blank run parses");
        let elapsed = started.elapsed();
        let mut leaf_path = Vec::with_capacity(depth + 1);
        for level in 0..depth {
            leaf_path.push(PathSegment::Key(Value::from(format!("k{level}"))));
        }
        leaf_path.push(PathSegment::Key(Value::from("leaf")));
        let leaf_span = tree.span_at(&leaf_path).expect("leaf span");
        assert!(leaf_span.is_empty());
        assert_eq!(block.as_bytes()[leaf_span.start() - 1], b':');
        assert_eq!(
            tree.scalar_text_at(&[PathSegment::Key(Value::from("tail"))]),
            Some("1")
        );
        assert!(
            tree.entry_span_at(&[PathSegment::Key(Value::from("k0"))])
                .is_some(),
            "k0 after clip block scalar followed by content must keep its entry_span"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "deep block + {trailing_lines} trailing blank/comment lines took {elapsed:?}"
        );

        // 3. Nested compact mappings (`- a: 1\n\n  b: ...`) followed by long trailing blanks must
        //    not evict the blank-run cache on each closing level.
        let compact_depth = 50_usize;
        let mut compact = String::new();
        for level in 0..compact_depth {
            let pad = "    ".repeat(level);
            let _ = writeln!(compact, "{pad}- a{level}: 1\n\n{pad}  b{level}:");
        }
        let leaf_pad = "    ".repeat(compact_depth);
        let _ = writeln!(compact, "{leaf_pad}end: 1");
        for i in 0..trailing_lines {
            if i % 2 == 0 {
                compact.push('\n');
            } else {
                compact.push_str("# trailing\n");
            }
        }
        compact.push_str("- after: 2\n");
        let started = Instant::now();
        let compact_tree = SpannedTree::parse(&compact).expect("nested compact mappings parse");
        let elapsed = started.elapsed();
        assert!(
            compact_tree
                .entry_span_at(&[PathSegment::Index(0), PathSegment::Key(Value::from("a0")),])
                .is_some(),
            "compact first entry span must remain exact"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "nested compact mappings + {trailing_lines} trailing blanks took {elapsed:?}"
        );

        assert_merge_replay_spanned_tree_stays_linear();
    });
}

fn assert_merge_replay_spanned_tree_stays_linear() {
    use ayaml::PathSegment;
    use std::fmt::Write as _;
    use std::time::{Duration, Instant};

    // 4. `SpannedTree::parse_with(..., merge_keys(true))` replaying an anchored missing-value
    //    entry followed by many blank lines across many sibling flow/block merge mappings
    //    must stay linear without rescanning the source blank run per replay.
    let merge_blanks = if cfg!(debug_assertions) {
        10_000_usize
    } else {
        50_000_usize
    };
    let merge_replays = if cfg!(debug_assertions) {
        500_usize
    } else {
        2_000_usize
    };
    let mut merge_yaml = String::from("base: &base\n  k0:\n");
    for _ in 0..merge_blanks {
        merge_yaml.push('\n');
    }
    merge_yaml.push_str("  k1: 1\n");
    for i in 0..merge_replays {
        if i % 2 == 0 {
            let _ = writeln!(merge_yaml, "m{i}: {{ <<: *base }}");
        } else {
            let _ = writeln!(merge_yaml, "m{i}:\n  <<: *base\n  x:");
        }
    }
    let started = Instant::now();
    let merge_tree =
        SpannedTree::parse_with(&merge_yaml, &ParseOptions::new().with_merge_keys(true))
            .expect("merge replay spanned tree parses");
    let elapsed = started.elapsed();
    assert!(
        merge_tree
            .lookup(&[
                PathSegment::Key(Value::from("m0")),
                PathSegment::Key(Value::from("k1")),
            ])
            .is_some()
    );
    // Replayed sibling missing-value entry keeps the exact originating source span.
    let base_k0_span = merge_tree
        .span_at(&[
            PathSegment::Key(Value::from("base")),
            PathSegment::Key(Value::from("k0")),
        ])
        .expect("base k0 span");
    let m0_k0_span = merge_tree
        .span_at(&[
            PathSegment::Key(Value::from("m0")),
            PathSegment::Key(Value::from("k0")),
        ])
        .expect("m0 merged k0 span");
    assert_eq!(m0_k0_span, base_k0_span);
    assert!(
        elapsed < Duration::from_secs(10),
        "merge-key replay ({merge_replays}x over {merge_blanks} blanks) took {elapsed:?}"
    );

    // 5. Ancestor merge replays + replayed `|-` empty-value block scalar with many blank lines
    //    must stay linear without rescanning blank runs or `AfterBlockScalar::from_raw`.
    let ancestor_depth = 80_usize;
    let mut ancestor_yaml = String::new();
    for level in 0..ancestor_depth {
        let pad = "  ".repeat(level);
        let _ = writeln!(ancestor_yaml, "{pad}m{level}:");
    }
    let base_pad = "  ".repeat(ancestor_depth);
    let _ = writeln!(ancestor_yaml, "{base_pad}base: &anc");
    let _ = writeln!(ancestor_yaml, "{base_pad}  k0:");
    for _ in 0..merge_blanks {
        ancestor_yaml.push('\n');
    }
    let _ = writeln!(ancestor_yaml, "{base_pad}  k1: |-");
    for _ in 0..merge_blanks {
        ancestor_yaml.push('\n');
    }
    let _ = writeln!(ancestor_yaml, "{base_pad}  k2: 1");
    for level in (0..ancestor_depth).rev() {
        let pad = "  ".repeat(level + 1);
        let _ = writeln!(ancestor_yaml, "{pad}<<: *anc");
        let _ = writeln!(ancestor_yaml, "{pad}x{level}:");
    }
    let started = Instant::now();
    let ancestor_tree =
        SpannedTree::parse_with(&ancestor_yaml, &ParseOptions::new().with_merge_keys(true))
            .expect("ancestor merge replay parses");
    let elapsed = started.elapsed();
    let outer_k0 = ancestor_tree
        .span_at(&[
            PathSegment::Key(Value::from("m0")),
            PathSegment::Key(Value::from("k0")),
        ])
        .expect("outer merged k0 span");
    assert!(outer_k0.is_empty());
    assert_eq!(ancestor_yaml.as_bytes()[outer_k0.start() - 1], b':');
    assert!(
        elapsed < Duration::from_secs(10),
        "ancestor merge replay + |- blank scalar ({ancestor_depth} deep) took {elapsed:?}"
    );
}

/// `ValueWriter::emit_flow` stops immediately after hitting `max_output_bytes` or an `io::Write`
/// failure without formatting or allocating remaining sequence/mapping entries (AY-169).
#[test]
fn flow_emitter_stops_immediately_on_limit_or_writer_error() {
    in_child!(|| {
        use ayaml::{EmitOptions, LimitExceeded};
        use std::io::Write;
        use std::time::{Duration, Instant};

        struct CountingFailWriter {
            writes: usize,
            fail_after_bytes: usize,
            written: usize,
        }

        impl Write for CountingFailWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.writes += 1;
                if self.written + buf.len() > self.fail_after_bytes {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "simulated writer failure",
                    ));
                }
                self.written += buf.len();
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let count = if cfg!(debug_assertions) {
            10_000_usize
        } else {
            40_000_usize
        };
        // Strings containing quotes/control characters force double-quoted escaping if visited.
        let payload = "line1\n\"quoted\"\t\\slash\\".repeat(4);
        let seq = Value::Sequence((0..count).map(|_| Value::String(payload.clone())).collect());
        let mut map = ayaml::Mapping::with_capacity(count);
        for i in 0..count {
            map.insert(
                Value::String(format!("k{i}")),
                Value::String(payload.clone()),
            );
        }
        let map_val = Value::Mapping(map);

        // 1. Tight `max_output_bytes` must fail immediately on both flow sequence and flow mapping.
        let tight = EmitOptions::flow().with_limits(Limits::default().with_max_output_bytes(64));
        for (label, value) in [("flow_seq", &seq), ("flow_map", &map_val)] {
            let started = Instant::now();
            let err = ayaml::to_string_value_with(value, &tight)
                .expect_err("tight max_output_bytes must reject large flow collection");
            assert_eq!(err.kind(), ErrorKind::Limit, "{label}: {err:?}");
            assert_eq!(
                err.limit().map(LimitExceeded::kind),
                Some(LimitKind::OutputBytes),
                "{label}: {err:?}"
            );
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "{label} took {:?} after hitting max_output_bytes",
                started.elapsed()
            );
        }

        // 2. Failing `io::Write` must stop after the very first failing write flush (never retry).
        let flow_opts = EmitOptions::flow();
        for (label, value) in [("flow_seq_io", &seq), ("flow_map_io", &map_val)] {
            let mut writer = CountingFailWriter {
                writes: 0,
                fail_after_bytes: 0,
                written: 0,
            };
            let started = Instant::now();
            let err = ayaml::to_writer_value_with(&mut writer, value, &flow_opts)
                .expect_err("failing writer must abort flow emission");
            assert_eq!(err.kind(), ErrorKind::Io, "{label}: {err:?}");
            assert_eq!(
                writer.writes, 1,
                "{label}: writer must not be invoked again after first I/O error"
            );
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "{label} took {:?} after first I/O failure",
                started.elapsed()
            );
        }

        // 3. Large literal, folded, single-quoted, and double-quoted scalars must fail immediately
        //    under tight `max_output_bytes` and on the first `io::Write` failure.
        let big_plain = format!("{}\n{}", "word ".repeat(8_000), "tail ".repeat(8_000));
        let big_single = "it's ".repeat(16_000);
        let big_double = "line\x01\"quote\"\\slash\t".repeat(8_000);
        for (label, style, text) in [
            ("literal", ayaml::ScalarStyle::Literal, &big_plain),
            ("folded", ayaml::ScalarStyle::Folded, &big_plain),
            ("single", ayaml::ScalarStyle::SingleQuoted, &big_single),
            ("double", ayaml::ScalarStyle::DoubleQuoted, &big_double),
        ] {
            let val = Value::String(text.clone());
            let tight_scalar = EmitOptions::new()
                .with_scalar_style(Some(style))
                .with_limits(Limits::default().with_max_output_bytes(64));
            let err = ayaml::to_string_value_with(&val, &tight_scalar)
                .expect_err("tight output limit must reject large scalar");
            assert_eq!(err.kind(), ErrorKind::Limit, "{label}: {err:?}");
            assert_eq!(
                err.limit().map(LimitExceeded::kind),
                Some(LimitKind::OutputBytes),
                "{label}: {err:?}"
            );

            let io_opts = EmitOptions::new().with_scalar_style(Some(style));
            let mut writer = CountingFailWriter {
                writes: 0,
                fail_after_bytes: 0,
                written: 0,
            };
            let io_err = ayaml::to_writer_value_with(&mut writer, &val, &io_opts)
                .expect_err("failing writer must abort scalar emission");
            assert_eq!(io_err.kind(), ErrorKind::Io, "{label}: {io_err:?}");
            assert_eq!(
                writer.writes, 1,
                "{label}: writer must not be retried after first I/O error"
            );
        }
    });
}

/// Oversized quoted, block, and multiline plain scalars fail incrementally under tight
/// `max_scalar_bytes` without allocating full decoded buffers (AY-176).
#[test]
fn oversized_scalars_fail_incrementally_without_allocating_decoded_buffers() {
    in_child!(|| {
        use ayaml::LimitExceeded;

        const SIZE: usize = 8 << 20;
        let single = format!("'{}'", "a''".repeat(SIZE / 3));
        let double_expand = format!("\"{}\"", "\\L".repeat(SIZE / 2));
        let double_blanks = format!("\"a{}  b\"", "\n".repeat(SIZE));
        let literal = format!("k: |\n{}", "  abcdefgh\n".repeat(SIZE / 11));
        let folded = format!("k: >\n{}", "  abcdefgh\n".repeat(SIZE / 11));
        let multiline_plain = format!("a\n{}", "  bcdefgh\n".repeat(SIZE / 10));

        let tight = ParseOptions::new().with_limits(Limits::default().with_max_scalar_bytes(64));
        let check = || {
            for (label, input) in [
                ("single_quoted", single.as_str()),
                ("double_expand", double_expand.as_str()),
                ("double_blanks", double_blanks.as_str()),
                ("literal_block", literal.as_str()),
                ("folded_block", folded.as_str()),
                ("multiline_plain", multiline_plain.as_str()),
            ] {
                for (reader, err) in [
                    (
                        "parse_value_with",
                        ayaml::parse_value_with(input, &tight).expect_err(label),
                    ),
                    (
                        "parse_events_with",
                        ayaml::parse_events_with(input, &tight)
                            .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                            .expect_err(label),
                    ),
                    (
                        "from_str_with",
                        ayaml::from_str_with::<Value>(input, &tight).expect_err(label),
                    ),
                    (
                        "SpannedTree::parse_with",
                        SpannedTree::parse_with(input, &tight).expect_err(label),
                    ),
                ] {
                    assert_eq!(err.kind(), ErrorKind::Limit, "{label}/{reader}: {err:?}");
                    assert_eq!(
                        err.code(),
                        ErrorCode::ScalarSizeLimit,
                        "{label}/{reader}: {err:?}"
                    );
                    assert_eq!(
                        err.limit().map(LimitExceeded::kind),
                        Some(LimitKind::ScalarBytes),
                        "{label}/{reader}: {err:?}"
                    );
                }
            }
        };

        if super::MEASURES_MEMORY {
            let growth = super::peak_growth(check);
            assert!(
                growth <= 2 << 20,
                "tight max_scalar_bytes rejection grew memory by {growth} bytes (expected <= 2 MiB)"
            );
        } else {
            check();
        }
    });
}

fn wrap_event_doc<'a>(
    directives: Vec<ayaml::Event<'a>>,
    root: Vec<ayaml::Event<'a>>,
) -> Vec<ayaml::Event<'a>> {
    let mut events = vec![ayaml::Event::StreamStart {
        span: ayaml::Span::default(),
    }];
    events.extend(directives);
    events.push(ayaml::Event::DocumentStart {
        explicit: false,
        span: ayaml::Span::default(),
    });
    events.extend(root);
    events.push(ayaml::Event::DocumentEnd {
        explicit: false,
        span: ayaml::Span::default(),
    });
    events.push(ayaml::Event::StreamEnd {
        span: ayaml::Span::default(),
    });
    events
}

fn event_scalar<'a>(
    value: &'a str,
    style: ayaml::ScalarStyle,
    tag: Option<ayaml::Tag>,
    anchor: Option<ayaml::Anchor<'a>>,
) -> ayaml::Event<'a> {
    ayaml::Event::Scalar {
        value: std::borrow::Cow::Borrowed(value),
        raw: std::borrow::Cow::Borrowed(""),
        style,
        tag,
        anchor,
        span: ayaml::Span::default(),
    }
}

fn event_kv_map(key: ayaml::Event<'_>) -> Vec<ayaml::Event<'_>> {
    vec![
        ayaml::Event::MappingStart {
            tag: None,
            anchor: None,
            style: ayaml::CollectionStyle::Block,
            span: ayaml::Span::default(),
        },
        key,
        event_scalar("1", ayaml::ScalarStyle::Plain, None, None),
        ayaml::Event::MappingEnd {
            span: ayaml::Span::default(),
        },
    ]
}

/// Both `ValueWriter` and `EventWriter` enforce `max_output_bytes` incrementally on oversized
/// escaped/folded scalars, long mapping keys, tags, directives, and anchors without allocating
/// full temporary strings (AY-176).
#[test]
fn emitters_enforce_max_output_bytes_incrementally_without_temporary_allocations() {
    in_child!(|| {
        use std::borrow::Cow;

        use ayaml::{
            Anchor, EmitOptions, Emitter, EmitterOptions, EmitterStyles, Event, LimitExceeded,
            Mapping, NodeStyle, PathSegment, ScalarStyle, Span, Tag, TaggedValue,
        };

        const SIZE: usize = 8 << 20;
        let big_double = "a\"b\\c\n\x01".repeat(SIZE / 7);
        let big_single = "a'".repeat(SIZE / 2);
        let big_folded = "ab\ncd\n".repeat(SIZE / 6);
        let big_tag = Tag::new(format!("custom {}:", "p".repeat(SIZE / 2)), "s");

        let double_val = Value::String(big_double.clone());
        let single_val = Value::String(big_single.clone());
        let folded_val = Value::String(big_folded.clone());
        let mut long_key_map = Mapping::new();
        long_key_map.insert(Value::String(big_double.clone()), Value::from(1_u64));
        let long_key_val = Value::Mapping(long_key_map);
        let tagged_val = Value::Tagged(Box::new(TaggedValue::new(
            big_tag.clone(),
            Value::from(1_u64),
        )));

        let double_events = wrap_event_doc(
            vec![],
            vec![event_scalar(
                &big_double,
                ScalarStyle::DoubleQuoted,
                None,
                None,
            )],
        );
        let single_events = wrap_event_doc(
            vec![],
            vec![event_scalar(
                &big_single,
                ScalarStyle::SingleQuoted,
                None,
                None,
            )],
        );
        let folded_events = wrap_event_doc(
            vec![],
            vec![event_scalar(&big_folded, ScalarStyle::Folded, None, None)],
        );
        let long_key_events = wrap_event_doc(
            vec![],
            event_kv_map(event_scalar(
                &big_double,
                ScalarStyle::DoubleQuoted,
                None,
                None,
            )),
        );
        let huge_dir_value = format!("!e! tag:example.com,2026:{}:", "a".repeat(SIZE / 2));
        let directive_events = wrap_event_doc(
            vec![Event::Directive {
                name: Cow::Borrowed("TAG"),
                value: Cow::Borrowed(&huge_dir_value),
                span: Span::default(),
            }],
            vec![event_scalar("1", ScalarStyle::Plain, None, None)],
        );
        let tagged_events = wrap_event_doc(
            vec![],
            vec![event_scalar("1", ScalarStyle::Plain, Some(big_tag), None)],
        );
        let huge_anchor_str = "a".repeat(SIZE / 2);
        let anchor_events = wrap_event_doc(
            vec![],
            vec![event_scalar(
                "1",
                ScalarStyle::Plain,
                None,
                Some(Anchor::new(huge_anchor_str.as_str())),
            )],
        );
        let anchor_key_events = wrap_event_doc(
            vec![],
            event_kv_map(event_scalar(
                "k",
                ScalarStyle::Plain,
                None,
                Some(Anchor::new(huge_anchor_str.as_str())),
            )),
        );

        let tight_limits = Limits::default().with_max_output_bytes(128);
        let tight = EmitOptions::new().with_limits(tight_limits.clone());
        let tight_sq = tight
            .clone()
            .with_scalar_style(Some(ScalarStyle::SingleQuoted));
        let tight_folded = tight.clone().with_scalar_style(Some(ScalarStyle::Folded));
        let tight_flow = tight.clone().with_flow(true);
        let mut active_styles = EmitterStyles::new();
        active_styles.set(
            vec![PathSegment::key("other")],
            NodeStyle::new().with_scalar_style(Some(ScalarStyle::DoubleQuoted)),
        );
        let styled_emitter_opts = EmitterOptions::new();
        let styled_emitter =
            Emitter::new(&tight_limits, &styled_emitter_opts).with_styles(&active_styles);

        let check = || {
            for (label, err) in [
                (
                    "value_double",
                    ayaml::to_string_value_with(&double_val, &tight).expect_err("double"),
                ),
                (
                    "value_single",
                    ayaml::to_string_value_with(&single_val, &tight_sq).expect_err("single"),
                ),
                (
                    "value_folded",
                    ayaml::to_string_value_with(&folded_val, &tight_folded).expect_err("folded"),
                ),
                (
                    "value_block_key",
                    ayaml::to_string_value_with(&long_key_val, &tight).expect_err("block key"),
                ),
                (
                    "value_block_key_styled",
                    styled_emitter
                        .value_to_string(&long_key_val)
                        .expect_err("styled block key"),
                ),
                (
                    "value_flow_key",
                    ayaml::to_string_value_with(&long_key_val, &tight_flow).expect_err("flow key"),
                ),
                (
                    "value_tagged",
                    ayaml::to_string_value_with(&tagged_val, &tight).expect_err("tagged"),
                ),
                (
                    "events_double",
                    ayaml::to_string_events_with(&double_events, &tight).expect_err("ev double"),
                ),
                (
                    "events_single",
                    ayaml::to_string_events_with(&single_events, &tight).expect_err("ev single"),
                ),
                (
                    "events_folded",
                    ayaml::to_string_events_with(&folded_events, &tight).expect_err("ev folded"),
                ),
                (
                    "events_key",
                    ayaml::to_string_events_with(&long_key_events, &tight).expect_err("ev key"),
                ),
                (
                    "events_directive",
                    ayaml::to_string_events_with(&directive_events, &tight).expect_err("ev dir"),
                ),
                (
                    "events_tagged",
                    ayaml::to_string_events_with(&tagged_events, &tight).expect_err("ev tagged"),
                ),
                (
                    "events_anchor",
                    ayaml::to_string_events_with(&anchor_events, &tight).expect_err("ev anchor"),
                ),
                (
                    "events_anchor_key",
                    ayaml::to_string_events_with(&anchor_key_events, &tight)
                        .expect_err("ev anchor key"),
                ),
            ] {
                assert_eq!(err.kind(), ErrorKind::Limit, "{label}: {err:?}");
                assert_eq!(err.code(), ErrorCode::OutputSizeLimit, "{label}: {err:?}");
                assert_eq!(
                    err.limit().map(LimitExceeded::kind),
                    Some(LimitKind::OutputBytes),
                    "{label}: {err:?}"
                );
            }
        };

        if super::MEASURES_MEMORY {
            let growth = super::peak_growth(check);
            assert!(
                growth <= 2 << 20,
                "tight max_output_bytes rejection grew memory by {growth} bytes (expected <= 2 MiB)"
            );
        } else {
            check();
        }
    });
}

/// Iterative `ValueWriter` and `TagDirectives::scan` emit `MAX_DEPTH_CAP` (128) nested trees on a
/// 512 KiB thread stack (4x smaller than Rust's 2 MiB default, where recursive emission previously
/// overflowed in debug builds) (AY-176).
#[test]
fn value_emitter_handles_max_depth_cap_on_tiny_stack() {
    in_child!(|| {
        use ayaml::{EmitOptions, Emitter, EmitterOptions, MAX_DEPTH_CAP};

        let limits = Limits::default().with_max_depth(MAX_DEPTH_CAP);
        let parse_opts = ParseOptions::new().with_limits(limits.clone());
        let values: Vec<(&str, Value)> = [
            "flow sequence",
            "flow mapping",
            "block sequence",
            "block mapping",
            "tagged flow mapping",
        ]
        .into_iter()
        .map(|shape| {
            let input = super::nested(shape, MAX_DEPTH_CAP);
            let value = ayaml::parse_value_with(&input, &parse_opts).expect(shape);
            (shape, value)
        })
        .collect();

        std::thread::Builder::new()
            .stack_size(512 << 10)
            .spawn(move || {
                let emit_opts = EmitOptions::new().with_limits(limits.clone());
                let flow_opts = emit_opts.clone().with_flow(true);
                let emitter_opts = EmitterOptions::new();
                let emitter = Emitter::new(&limits, &emitter_opts);
                for (shape, value) in &values {
                    let block = ayaml::to_string_value_with(value, &emit_opts)
                        .unwrap_or_else(|err| panic!("{shape} block: {err}"));
                    let flow = ayaml::to_string_value_with(value, &flow_opts)
                        .unwrap_or_else(|err| panic!("{shape} flow: {err}"));
                    let mut buf = Vec::new();
                    ayaml::to_writer_value_with(&mut buf, value, &emit_opts)
                        .unwrap_or_else(|err| panic!("{shape} writer: {err}"));
                    assert_eq!(buf, block.as_bytes(), "{shape} writer parity");
                    let doc = emitter
                        .value_to_string(value)
                        .unwrap_or_else(|err| panic!("{shape} emitter: {err}"));
                    assert_eq!(doc, block, "{shape} emitter parity");
                    assert_eq!(
                        ayaml::parse_value_with(&block, &parse_opts).expect("reparse block"),
                        *value,
                        "{shape} block round-trip"
                    );
                    assert_eq!(
                        ayaml::parse_value_with(&flow, &parse_opts).expect("reparse flow"),
                        *value,
                        "{shape} flow round-trip"
                    );
                }
            })
            .expect("spawn 512 KiB stack thread")
            .join()
            .expect("512 KiB stack emission succeeded");
    });
}
