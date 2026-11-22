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
