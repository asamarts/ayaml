use super::*;

#[test]
fn complex_keys_detect_duplicates_and_ambiguity_in_linear_time() {
    // Duplicate mapping keys make lookup return None, while unique complex keys resolve.
    let unique = "? {a: 1, b: [2, 3]}\n: ok\nother: yes\n";
    let tree = SpannedTree::parse(unique).expect("parse unique complex key");
    let mut expected_key = Mapping::new();
    expected_key.insert(Value::String("a".into()), Value::from(1_i64));
    expected_key.insert(
        Value::String("b".into()),
        Value::Sequence(vec![Value::from(2_i64), Value::from(3_i64)]),
    );
    let found = tree
        .lookup(&[PathSegment::Key(Value::Mapping(expected_key.clone()))])
        .expect("unique complex key should be found");
    assert_eq!(found.scalar_text(), Some("ok"));
    assert_eq!(
        tree.lookup(&[PathSegment::Key(Value::String("other".into()))])
            .and_then(SpannedNode::scalar_text),
        Some("yes")
    );

    let dup = "? {a: 1, b: [2, 3]}\n: first\n? {b: [2, 3], a: 1}\n: second\nother: yes\n";
    let dup_tree = SpannedTree::parse(dup).expect("parse duplicate complex keys");
    assert!(
        dup_tree
            .lookup(&[PathSegment::Key(Value::Mapping(expected_key))])
            .is_none(),
        "duplicate complex keys must be marked ambiguous"
    );
    assert_eq!(
        dup_tree
            .lookup(&[PathSegment::Key(Value::String("other".into()))])
            .and_then(SpannedNode::scalar_text),
        Some("yes")
    );

    // An inner duplicate, merge key, or alias inside a complex key marks the enclosing
    // mapping ambiguous so lookups do not guess.
    for ambiguous_input in [
        "? {a: 1, a: 2}\n: v\nother: yes\n",
        "? {<<: {a: 1}}\n: v\nother: yes\n",
        "anchor: &x 1\n? {a: *x}\n: v\nother: yes\n",
        "? [*x]\n: v\nother: yes\n",
    ] {
        let t = SpannedTree::parse(ambiguous_input).expect("parse ambiguous complex key");
        assert!(
            t.lookup(&[PathSegment::Key(Value::String("other".into()))])
                .is_none(),
            "enclosing mapping must not guess for {ambiguous_input:?}"
        );
    }

    // Deeply nested complex keys stay linear.
    let mut nested = "{k0: 0, k1: 1, k2: 2}".to_owned();
    for _ in 0..48 {
        nested = format!("{{? {nested}: null}}");
    }
    nested.push('\n');
    assert!(SpannedTree::parse(&nested).is_ok());
}

#[test]
fn spanned_tree_line_scans_stay_linear_deterministically() {
    use std::fmt::Write as _;

    fn build_and_count(input: &str, merge_keys: bool) -> (SpannedTree<'_>, usize) {
        let options = ParseOptions::new().with_merge_keys(merge_keys);
        let events = crate::parse_events_with(input, &options).expect("events");
        let mut builder = Builder::new(input, merge_keys);
        for event in events {
            builder.event(event.expect("event")).expect("build");
        }
        let scanned = builder.scan_cache.blank_run_lines_scanned;
        (builder.finish(), scanned)
    }

    // 1. Dense single-line empty-value flow mapping scans zero blank-run lines.
    let mut flow = String::from("{");
    for i in 0..2_000 {
        if i > 0 {
            flow.push(',');
        }
        let _ = write!(flow, "k{i}:");
    }
    flow.push_str("}\n");
    let (_, scanned) = build_and_count(&flow, false);
    assert_eq!(scanned, 0, "single-line flow map must scan 0 blank lines");

    // 2. 80-deep block mapping followed by 1,000 blank/comment lines scans each line at most once.
    let mut block = String::from("hdr: |\n  keep\n");
    for level in 0..80 {
        for _ in 0..level {
            block.push_str("  ");
        }
        let _ = writeln!(block, "k{level}:");
    }
    for _ in 0..80 {
        block.push_str("  ");
    }
    block.push_str("leaf:\n");
    for i in 0..1_000 {
        if i % 2 == 0 {
            block.push('\n');
        } else {
            block.push_str("# comment\n");
        }
    }
    block.push_str("tail: 1\n");
    let (_, scanned) = build_and_count(&block, false);
    assert!(
        scanned <= 1_200,
        "80-deep block + 1,000 trailing blanks scanned {scanned} lines (expected linear)"
    );

    // 3. 40-deep nested compact mappings + 1,000 trailing blanks do not evict blank-run cache.
    let mut compact = String::new();
    for level in 0..40 {
        let pad = "    ".repeat(level);
        let _ = writeln!(compact, "{pad}- a{level}: 1\n\n{pad}  b{level}:");
    }
    let leaf_pad = "    ".repeat(40);
    let _ = writeln!(compact, "{leaf_pad}end: 1");
    for i in 0..1_000 {
        if i % 2 == 0 {
            compact.push('\n');
        } else {
            compact.push_str("# trailing\n");
        }
    }
    compact.push_str("- after: 2\n");
    let (_, scanned) = build_and_count(&compact, false);
    assert!(
        scanned <= 1_300,
        "40-deep compact + 1,000 trailing blanks scanned {scanned} lines (expected linear)"
    );

    // 4. Ancestor + sibling merge replays over 1,000 blanks scan the blank run once and
    //    preserve the exact originating missing-value span.
    let mut anc = String::new();
    for level in 0..40 {
        let pad = "  ".repeat(level);
        let _ = writeln!(anc, "{pad}m{level}:");
    }
    let base_pad = "  ".repeat(40);
    let _ = writeln!(anc, "{base_pad}base: &b");
    let _ = writeln!(anc, "{base_pad}  k0:");
    for _ in 0..1_000 {
        anc.push('\n');
    }
    let _ = writeln!(anc, "{base_pad}  k1: 1");
    for level in (0..40).rev() {
        let pad = "  ".repeat(level + 1);
        let _ = writeln!(anc, "{pad}<<: *b");
        let _ = writeln!(anc, "{pad}x{level}:");
    }
    for i in 0..40 {
        let _ = writeln!(anc, "s{i}: {{ <<: *b }}");
    }
    let (tree, scanned) = build_and_count(&anc, true);
    assert!(
        scanned <= 1_200,
        "ancestor + sibling merge replays scanned {scanned} lines (expected linear)"
    );
    let mut base_path: Vec<PathSegment> = (0..40)
        .map(|l| PathSegment::Key(Value::from(format!("m{l}"))))
        .collect();
    base_path.push(PathSegment::Key(Value::from("base")));
    base_path.push(PathSegment::Key(Value::from("k0")));
    let base_k0 = tree.span_at(&base_path).expect("base k0");
    let outer_k0 = tree
        .span_at(&[
            PathSegment::Key(Value::from("m0")),
            PathSegment::Key(Value::from("k0")),
        ])
        .expect("outer merged k0");
    let sibling_k0 = tree
        .span_at(&[
            PathSegment::Key(Value::from("s0")),
            PathSegment::Key(Value::from("k0")),
        ])
        .expect("sibling merged k0");
    assert_eq!(outer_k0, base_k0);
    assert_eq!(sibling_k0, base_k0);
}

#[test]
fn compact_first_entry_span_refuses_intervening_comments_and_merge_suppresses_entries() {
    let with_comment = "- a: 1\n  # comment about b\n  b: 2\n";
    let tree = SpannedTree::parse(with_comment).expect("tree");
    assert_eq!(
        tree.entry_span_at(&[PathSegment::Index(0), PathSegment::Key(Value::from("a"))]),
        None,
        "compact first entry followed by comment must not produce an entry_span"
    );
    let without_comment = "- a: 1\n  b: 2\n";
    let clean_tree = SpannedTree::parse(without_comment).expect("clean tree");
    assert_eq!(
        clean_tree
            .entry_span_at(&[PathSegment::Index(0), PathSegment::Key(Value::from("a"))])
            .and_then(|s| s.slice(without_comment)),
        Some("a: 1\n  ")
    );

    let merged = "base: &b\n  k: 1\nout:\n  <<: *b\n  own: 2\n";
    let opts = ParseOptions::new().with_merge_keys(true);
    let merged_tree = SpannedTree::parse_with(merged, &opts).expect("merged tree");
    assert!(
        merged_tree
            .span_at(&[
                PathSegment::Key(Value::from("out")),
                PathSegment::Key(Value::from("k")),
            ])
            .is_some()
    );
    assert_eq!(
        merged_tree.entry_span_at(&[
            PathSegment::Key(Value::from("out")),
            PathSegment::Key(Value::from("own")),
        ]),
        None,
        "merge_keys=true must suppress entry_span conservatively"
    );
}

#[test]
fn spanned_node_and_entry_layout_stay_compact() {
    assert!(
        size_of::<SpannedNode<'_>>() <= 64,
        "SpannedNode grew to {} bytes",
        size_of::<SpannedNode<'_>>()
    );
    assert!(
        size_of::<Entry<'_>>() <= 128,
        "Entry grew to {} bytes",
        size_of::<Entry<'_>>()
    );
}

#[test]
fn spanned_key_small_to_large_promotion_and_zero_alloc_lookup() {
    use std::fmt::Write as _;

    // 1. Small mapping (8 keys) stays in linear KeyTracker::Small and distinguishes string vs typed keys.
    let small_yaml = "1: int_one\n\"1\": str_one\n!!str 2: str_two\ntrue: bool_true\n\"true\": str_true\nnull: null_val\n\"null\": str_null\n01: str_01\n";
    let tree = SpannedTree::parse(small_yaml).expect("parse small disjoint keys");
    assert_eq!(
        tree.lookup(&[PathSegment::Key(Value::from(1_i64))])
            .and_then(SpannedNode::scalar_text),
        Some("int_one")
    );
    assert_eq!(
        tree.lookup(&[PathSegment::Key(Value::from("1"))])
            .and_then(SpannedNode::scalar_text),
        Some("str_one")
    );
    assert_eq!(
        tree.lookup(&[PathSegment::Key(Value::from("2"))])
            .and_then(SpannedNode::scalar_text),
        Some("str_two")
    );
    assert_eq!(
        tree.lookup(&[PathSegment::Key(Value::Bool(true))])
            .and_then(SpannedNode::scalar_text),
        Some("bool_true")
    );
    assert_eq!(
        tree.lookup(&[PathSegment::Key(Value::from("true"))])
            .and_then(SpannedNode::scalar_text),
        Some("str_true")
    );
    assert_eq!(
        tree.lookup(&[PathSegment::Key(Value::Null)])
            .and_then(SpannedNode::scalar_text),
        Some("null_val")
    );
    assert_eq!(
        tree.lookup(&[PathSegment::Key(Value::from("null"))])
            .and_then(SpannedNode::scalar_text),
        Some("str_null")
    );
    assert_eq!(
        tree.lookup(&[PathSegment::Key(Value::from("01"))])
            .and_then(SpannedNode::scalar_text),
        Some("str_01")
    );

    // 2. Duplicate detection before promotion (<= 8 keys) and across plain/quoted/!!str spellings.
    let dup_small = "a: 1\nb: 2\n\"a\": 3\n!!str b: 4\nc: 5\n";
    let dup_small_tree = SpannedTree::parse(dup_small).expect("parse small duplicate");
    assert!(
        dup_small_tree
            .lookup(&[PathSegment::Key(Value::from("a"))])
            .is_none()
    );
    assert!(
        dup_small_tree
            .lookup(&[PathSegment::Key(Value::from("b"))])
            .is_none()
    );
    assert_eq!(
        dup_small_tree
            .lookup(&[PathSegment::Key(Value::from("c"))])
            .and_then(SpannedNode::scalar_text),
        Some("5")
    );

    // 3. Promotion past SMALL_LIMIT (8) into KeyTracker::Large with duplicates before/after spill.
    let mut large = String::new();
    for i in 0..20 {
        let _ = writeln!(large, "k{i}: v{i}");
    }
    let _ = writeln!(large, "\"k0\": dup0");
    let _ = writeln!(large, "k15: dup15");
    let _ = writeln!(large, "1: int1");
    let _ = writeln!(large, "\"1\": str1");
    let _ = writeln!(large, "1: int1_dup");
    let large_tree = SpannedTree::parse(&large).expect("parse promoted mapping");
    assert!(
        large_tree
            .lookup(&[PathSegment::Key(Value::from("k0"))])
            .is_none()
    );
    assert!(
        large_tree
            .lookup(&[PathSegment::Key(Value::from("k15"))])
            .is_none()
    );
    assert!(
        large_tree
            .lookup(&[PathSegment::Key(Value::from(1_i64))])
            .is_none()
    );
    assert_eq!(
        large_tree
            .lookup(&[PathSegment::Key(Value::from("1"))])
            .and_then(SpannedNode::scalar_text),
        Some("str1")
    );
    assert_eq!(
        large_tree
            .lookup(&[PathSegment::Key(Value::from("k19"))])
            .and_then(SpannedNode::scalar_text),
        Some("v19")
    );
}

#[test]
fn builder_scratch_spill_preserves_spans_and_anchors() {
    use std::fmt::Write as _;

    // 1. Sequence exceeding SCRATCH_LIMIT (64) with nested empty/anchored items.
    let mut seq = String::new();
    for i in 0..72 {
        if i == 10 {
            let _ = writeln!(seq, "- &a item{i}");
        } else if i == 20 {
            seq.push_str("- []\n");
        } else if i == 30 {
            seq.push_str("- {}\n");
        } else {
            let _ = writeln!(seq, "- item{i}");
        }
    }
    let seq_tree = SpannedTree::parse(&seq).expect("parse spilled seq");
    assert_eq!(
        seq_tree
            .lookup(&[PathSegment::Index(0)])
            .and_then(SpannedNode::scalar_text),
        Some("item0")
    );
    assert_eq!(
        seq_tree
            .lookup(&[PathSegment::Index(71)])
            .and_then(SpannedNode::scalar_text),
        Some("item71")
    );
    assert_eq!(
        seq_tree.entry_span_at(&[PathSegment::Index(10)]),
        None,
        "anchored item in spilled sequence must suppress entry_span"
    );
    assert_eq!(
        seq_tree
            .entry_span_at(&[PathSegment::Index(70)])
            .and_then(|s| s.slice(&seq)),
        Some("- item70\n")
    );

    // 2. Compact block mapping exceeding SCRATCH_LIMIT (64) preserves compact first entry_span.
    let mut map = String::from("- k0: v0\n");
    for i in 1..72 {
        let _ = writeln!(map, "  k{i}: v{i}");
    }
    let map_tree = SpannedTree::parse(&map).expect("parse spilled compact map");
    assert_eq!(
        map_tree
            .entry_span_at(&[PathSegment::Index(0), PathSegment::Key(Value::from("k0"))])
            .and_then(|s| s.slice(&map)),
        Some("k0: v0\n  ")
    );
    assert_eq!(
        map_tree
            .entry_span_at(&[PathSegment::Index(0), PathSegment::Key(Value::from("k70"))])
            .and_then(|s| s.slice(&map)),
        Some("  k70: v70\n")
    );
    assert_eq!(
        map_tree
            .lookup(&[PathSegment::Index(0), PathSegment::Key(Value::from("k71"))])
            .and_then(SpannedNode::scalar_text),
        Some("v71")
    );
}

#[test]
fn parse_spanned_matches_event_stream_in_default_and_compat_modes() {
    let cases = [
        "",
        "\u{feff}a: 1\r\nb: |\r\n  hello\r\n",
        "a: 1\n---\nb: [2, {c: 3}]\n",
        "base: &b\n  k: 1\nuse: *b\n",
        "? {x: [1, 2]}\n: complex\nplain: ok\n",
        "- first: 1\n  second: \"escaped\\nval\"\n  third: 'single''quote'\n",
        "bad: [1, 2\n",
    ];
    let opts = ParseOptions::new();
    for input in cases {
        let direct = SpannedTree::parse_with(input, &opts);
        let via_events = crate::parse_events_with(input, &opts).and_then(|events| {
            let mut builder = Builder::new(input, false);
            for event in events {
                builder.event(event?)?;
            }
            Ok(builder.finish())
        });
        match (direct, via_events) {
            (Ok(d), Ok(e)) => {
                assert_eq!(format!("{d:?}"), format!("{e:?}"), "{input:?}");
            }
            (Err(de), Err(ee)) => {
                assert_eq!(de.code(), ee.code(), "{input:?}");
                assert_eq!(de.span(), ee.span(), "{input:?}");
                assert_eq!(de.location(), ee.location(), "{input:?}");
            }
            (d, e) => panic!("mismatch on {input:?}: direct={d:?} vs events={e:?}"),
        }

        #[cfg(feature = "libyaml-compat")]
        {
            let compat_opts = crate::compat::ParseOptions::new();
            let direct_c = SpannedTree::parse_compat_with(input, &compat_opts);
            let via_events_c =
                crate::compat::parse_events_with(input, &compat_opts).and_then(|events| {
                    let mut builder = Builder::new(input, false);
                    for event in events {
                        builder.event(event?)?;
                    }
                    Ok(builder.finish())
                });
            match (direct_c, via_events_c) {
                (Ok(d), Ok(e)) => {
                    assert_eq!(format!("{d:?}"), format!("{e:?}"), "compat {input:?}");
                }
                (Err(de), Err(ee)) => {
                    assert_eq!(de.code(), ee.code(), "compat {input:?}");
                    assert_eq!(de.span(), ee.span(), "compat {input:?}");
                    assert_eq!(de.location(), ee.location(), "compat {input:?}");
                }
                (d, e) => panic!("compat mismatch on {input:?}: direct={d:?} vs events={e:?}"),
            }
        }
    }

    // Tight `max_events` error matches between direct `EventSink::Spanned` and event-stream path.
    let tight = ParseOptions::new().with_limits(Limits::new().with_max_events(5));
    let tight_input = "a: 1\nb: 2\nc: 3\n";
    let direct_err = SpannedTree::parse_with(tight_input, &tight).expect_err("direct max_events");
    let events_err = crate::parse_events_with(tight_input, &tight)
        .and_then(|events| {
            let mut builder = Builder::new(tight_input, false);
            for event in events {
                builder.event(event?)?;
            }
            Ok(builder.finish())
        })
        .expect_err("events max_events");
    assert_eq!(direct_err.code(), events_err.code());
    assert_eq!(direct_err.span(), events_err.span());
    assert_eq!(direct_err.location(), events_err.location());
}
