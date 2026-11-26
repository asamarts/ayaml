use ayaml::{ErrorKind, LimitExceeded, LimitKind, Limits, MAX_DEPTH, ParseOptions, Value};

use super::in_child_named;

/// `Value::apply_merge_keys` stops at `max_depth` on deep programmatic `Value` trees instead of
/// overflowing the stack.
#[test]
fn apply_merge_keys_rejects_deep_programmatic_trees() {
    in_child!(|| {
        let mut value = Value::from(1_u64);
        for _ in 0..4_000 {
            let mut mapping = ayaml::Mapping::new();
            mapping.insert(Value::from("k"), value);
            value = Value::Mapping(mapping);
        }
        let error = value
            .apply_merge_keys()
            .expect_err("4000-deep programmatic mapping must fail with depth limit");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(
            error.limit(),
            Some(LimitExceeded::new(
                LimitKind::Depth,
                MAX_DEPTH as u64,
                (MAX_DEPTH + 1) as u64,
            ))
        );
        while let Value::Mapping(mut mapping) = value {
            let Some(next) = mapping.remove("k") else {
                break;
            };
            value = next;
        }
    });
}

/// Chained merge-and-alias amplification stays bounded across parsing and `apply_merge_keys`.
#[test]
fn chained_merge_and_alias_amplification_is_bounded() {
    in_child!(|| {
        let mut yaml = String::from("l0: &l0 {k0: 0, k1: 1, k2: 2, k3: 3}\n");
        for level in 1..=20 {
            yaml.push_str(&format!(
                "l{level}: &l{level}\n  <<: [*l{}, *l{}]\n  extra{level}: {level}\n",
                level - 1,
                level - 1,
            ));
        }
        let limits = Limits::default().with_max_alias_expansion_bytes(64 * 1024);
        let parsed = ayaml::parse_value_with(&yaml, &ParseOptions::new().with_limits(limits));
        let error = parsed.expect_err("exponential alias+merge chain must hit alias budget");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(
            error.limit().map(LimitExceeded::kind),
            Some(LimitKind::AliasExpansion)
        );
    });
}

/// Wide overlapping merge sequences preserve precedence, stay linear, and enforce
/// `max_collection_entries`.
#[test]
fn wide_merge_sequences_stay_linear_and_enforce_entry_limits() {
    in_child!(|| {
        let operands = 10_usize;
        let keys_per_operand = 2_000_usize;
        let mut yaml = String::new();
        for op in 0..operands {
            yaml.push_str(&format!("b{op}: &b{op}\n"));
            let base = op * (keys_per_operand / 2);
            for offset in 0..keys_per_operand {
                let key = base + offset;
                yaml.push_str(&format!("  k{key}: {op}\n"));
            }
        }
        yaml.push_str("merged:\n  own: 99\n  k0: 777\n  <<: [");
        for op in 0..operands {
            if op != 0 {
                yaml.push_str(", ");
            }
            yaml.push_str(&format!("*b{op}"));
        }
        yaml.push_str("]\n");

        let expected_keys = 1 + (operands - 1) * (keys_per_operand / 2) + keys_per_operand;
        let mut value = ayaml::parse_value(&yaml).expect("wide merge fixture parses");
        value
            .apply_merge_keys_with_limits(
                &Limits::default().with_max_collection_entries(expected_keys),
            )
            .expect("exact collection entry count fits");
        let merged = value["merged"].as_mapping().expect("merged mapping");
        assert_eq!(merged.len(), expected_keys);
        assert_eq!(merged.get("own").and_then(Value::as_u64), Some(99));
        assert_eq!(merged.get("k0").and_then(Value::as_u64), Some(777));
        assert_eq!(merged.get("k1000").and_then(Value::as_u64), Some(0));
        assert_eq!(
            merged
                .get(&*format!("k{}", expected_keys - 2))
                .and_then(Value::as_u64),
            Some((operands - 1) as u64)
        );

        let mut tight = ayaml::parse_value(&yaml).expect("wide merge fixture parses");
        let error = tight
            .apply_merge_keys_with_limits(
                &Limits::default().with_max_collection_entries(expected_keys - 1),
            )
            .expect_err("one entry below merged size must fail");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(
            error.limit(),
            Some(LimitExceeded::new(
                LimitKind::CollectionEntries,
                (expected_keys - 1) as u64,
                expected_keys as u64,
            ))
        );
    });
}

/// Opt-in YAML 1.1 merge-key expansion (`ParseOptions::with_merge_keys(true)`) rejects hostile
/// chained merge bombs and oversized merged mappings across event, value, and typed readers
/// without unbounded allocation.
#[test]
fn opt_in_merge_keys_rejects_merge_bombs_on_typed_and_event_readers() {
    in_child!(|| {
        use std::fmt::Write as _;

        // Chained exponential merge bomb: each level merges the previous level twice into
        // nested mappings (`x: { <<: *m{prev} }`, `y: { <<: *m{prev} }`), doubling the expanded
        // node count at every level (2^20 subtrees) so event, value, and typed readers all stop
        // cleanly at resource limits.
        let mut bomb = String::from("m0: &m0\n  a: x\n  b: y\n");
        for level in 1..20 {
            let prev = level - 1;
            let _ = writeln!(
                bomb,
                "m{level}: &m{level}\n  x:\n    <<: *m{prev}\n  y:\n    <<: *m{prev}"
            );
        }

        let opts = ParseOptions::new().with_merge_keys(true);
        let events_err = ayaml::parse_events_with(&bomb, &opts)
            .and_then(Iterator::collect::<Result<Vec<_>, _>>)
            .expect_err("merge bomb events");
        assert_eq!(events_err.kind(), ErrorKind::Limit);

        let value_err = ayaml::parse_value_with(&bomb, &opts).expect_err("merge bomb value");
        assert_eq!(value_err.kind(), ErrorKind::Limit);

        let typed_err = ayaml::from_str_with::<Value>(&bomb, &opts).expect_err("merge bomb typed");
        assert_eq!(typed_err.kind(), ErrorKind::Limit);

        // Chained depth bomb via merge: merging a nested mapping past max_depth is rejected.
        let deep_merge = "base: &b\n  inner:\n    leaf: 1\ntop:\n  <<: *b\n";
        let shallow = ParseOptions::new()
            .with_limits(Limits::new().with_max_depth(2))
            .with_merge_keys(true);
        let depth_err =
            ayaml::from_str_with::<Value>(deep_merge, &shallow).expect_err("merge depth limit");
        assert_eq!(depth_err.kind(), ErrorKind::Limit);
        assert_eq!(
            depth_err.limit().map(LimitExceeded::kind),
            Some(LimitKind::Depth)
        );
    });
}

/// Alias-bomb mapping keys (`? *a8 : 1` beside `<<: {}`) cannot bypass `AliasBudget` during
/// merge-key deduplication, and deeply nested anchors without aliases cannot amplify memory.
#[test]
fn opt_in_merge_keys_blocks_alias_bomb_keys_and_nested_anchor_amplification() {
    in_child!(|| {
        use std::fmt::Write as _;

        // 1. Billion-laughs alias bomb inside an explicit mapping key beside an empty merge.
        let mut key_bomb = String::from("a0: &a0 [1,1,1,1,1,1,1,1,1,1]\n");
        for level in 1..=8 {
            let prev = level - 1;
            let _ = writeln!(
                key_bomb,
                "a{level}: &a{level} [*a{prev},*a{prev},*a{prev},*a{prev},*a{prev},*a{prev},*a{prev},*a{prev},*a{prev},*a{prev}]"
            );
        }
        key_bomb.push_str("t:\n  ? *a8\n  : 1\n  <<: {}\n");

        let opts = ParseOptions::new().with_merge_keys(true);
        for err in [
            ayaml::parse_events_with(&key_bomb, &opts)
                .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                .expect_err("key bomb events"),
            ayaml::parse_value_with(&key_bomb, &opts).expect_err("key bomb value"),
            ayaml::from_str_with::<Value>(&key_bomb, &opts).expect_err("key bomb typed"),
            ayaml::DocumentDeserializer::from_str_with(&key_bomb, &opts)
                .expect_err("key bomb doc_de"),
            ayaml::SpannedTree::parse_with(&key_bomb, &opts).expect_err("key bomb spanned"),
        ] {
            assert_eq!(err.kind(), ErrorKind::Limit, "{err:?}");
        }

        // 2. 120 nested anchors wrapping a 100k-item sequence with zero aliases/merges must share
        //    subtrees via Arc without 120x cloning amplification.
        let mut nested = String::with_capacity(260_000);
        for level in 0..120 {
            let _ = write!(nested, "&a{level} [");
        }
        for i in 0..100_000 {
            if i > 0 {
                nested.push(',');
            }
            nested.push('1');
        }
        for _ in 0..120 {
            nested.push(']');
        }
        nested.push('\n');
        let events: Vec<_> = ayaml::parse_events_with(&nested, &opts)
            .expect("nested anchors stream")
            .collect::<Result<_, _>>()
            .expect("nested anchors events");
        assert!(events.len() > 100_000);
    });
}

/// Repeated sibling mappings with alias keys beside `<<: {}` must charge `node_key_value`
/// against a stream-cumulative `AliasBudget` so sibling mappings cannot reset alias expansion
/// or dereference budgets back to zero (AY-169).
#[test]
fn opt_in_merge_keys_enforces_cumulative_alias_budget_across_sibling_mappings() {
    in_child!(|| {
        use std::fmt::Write as _;

        // `*a1` expands to 100 scalars + 11 sequence nodes (~31.3 KiB per key inspection, ~91 KiB
        // total including value composition), which fits comfortably inside one mapping under a
        // 128 KiB expansion budget, whereas repeating it across 20 sibling merge mappings
        // (~626 KiB) must exceed the cumulative expansion budget.
        let header = "a0: &a0 [1,1,1,1,1,1,1,1,1,1]\n\
                      a1: &a1 [*a0,*a0,*a0,*a0,*a0,*a0,*a0,*a0,*a0,*a0]\n";
        let exp_opts = ParseOptions::new()
            .with_limits(Limits::default().with_max_alias_expansion_bytes(128 * 1024))
            .with_merge_keys(true);

        // Positive control: a single sibling mapping stays well under the expansion budget.
        let single_yaml = format!("{header}m0: {{ ? *a1 : 1, <<: {{}} }}\n");
        let events: Vec<_> = ayaml::parse_events_with(&single_yaml, &exp_opts)
            .expect("single mapping events stream")
            .collect::<Result<_, _>>()
            .expect("single mapping must fit expansion budget");
        assert!(!events.is_empty());
        assert!(ayaml::SpannedTree::parse_with(&single_yaml, &exp_opts).is_ok());
        assert!(ayaml::parse_value_with(&single_yaml, &exp_opts).is_ok());
        assert!(ayaml::from_str_with::<Value>(&single_yaml, &exp_opts).is_ok());

        // Cumulative check: 20 sibling mappings must trip `LimitKind::AliasExpansion`.
        let mut yaml = String::from(header);
        for i in 0..20 {
            let _ = writeln!(yaml, "m{i}: {{ ? *a1 : 1, <<: {{}} }}");
        }
        for (label, err) in [
            (
                "parse_events_with",
                ayaml::parse_events_with(&yaml, &exp_opts)
                    .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                    .expect_err("cumulative alias expansion events"),
            ),
            (
                "SpannedTree::parse_with",
                ayaml::SpannedTree::parse_with(&yaml, &exp_opts)
                    .expect_err("cumulative alias expansion spanned"),
            ),
            (
                "parse_value_with",
                ayaml::parse_value_with(&yaml, &exp_opts)
                    .expect_err("cumulative alias expansion value"),
            ),
            (
                "from_str_with",
                ayaml::from_str_with::<Value>(&yaml, &exp_opts)
                    .expect_err("cumulative alias expansion typed"),
            ),
        ] {
            assert_eq!(err.kind(), ErrorKind::Limit, "{label}: {err:?}");
            assert_eq!(
                err.limit().map(LimitExceeded::kind),
                Some(LimitKind::AliasExpansion),
                "{label}: {err:?}"
            );
        }

        // Also enforce cumulative `max_alias_dereferences` across sibling mappings with simple
        // scalar alias keys (`? *k : 1` beside `<<: {}`).
        let deref_opts = ParseOptions::new()
            .with_limits(Limits::default().with_max_alias_dereferences(20))
            .with_merge_keys(true);

        // Positive control: 10 sibling mappings (10 derefs <= 20) succeed.
        let mut ok_deref = String::from("k: &k key\n");
        for i in 0..10 {
            let _ = writeln!(ok_deref, "m{i}: {{ ? *k : 1, <<: {{}} }}");
        }
        assert!(
            ayaml::parse_events_with(&ok_deref, &deref_opts)
                .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                .is_ok()
        );
        assert!(ayaml::SpannedTree::parse_with(&ok_deref, &deref_opts).is_ok());

        let mut deref_yaml = String::from("k: &k key\n");
        for i in 0..40 {
            let _ = writeln!(deref_yaml, "m{i}: {{ ? *k : 1, <<: {{}} }}");
        }
        for (label, err) in [
            (
                "parse_events_with",
                ayaml::parse_events_with(&deref_yaml, &deref_opts)
                    .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                    .expect_err("cumulative alias derefs events"),
            ),
            (
                "SpannedTree::parse_with",
                ayaml::SpannedTree::parse_with(&deref_yaml, &deref_opts)
                    .expect_err("cumulative alias derefs spanned"),
            ),
        ] {
            assert_eq!(err.kind(), ErrorKind::Limit, "{label}: {err:?}");
            assert_eq!(
                err.limit().map(LimitExceeded::kind),
                Some(LimitKind::AliasDereferences),
                "{label}: {err:?}"
            );
        }
    });
}
