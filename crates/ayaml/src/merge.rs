//! Explicit YAML 1.1 merge-key adapter.
//!
//! Generic parsing deliberately keeps `<<` as a literal mapping key. This
//! module applies the merge behavior only when a caller asks for it, so
//! duplicate-aware and source-aware consumers still see the original pairs.
//! The adapter accepts a mapping or a sequence of mappings. For a sequence,
//! earlier mappings take precedence, matching the YAML 1.1 merge rules.
//!
//! The result keeps the mapping's own keys in source order and appends the
//! merged keys after them, in operand order. A merge operand that itself holds
//! `<<` is merged first, so chained merges flatten fully.

use crate::error::{Error, ErrorCode, ErrorKind};
use crate::limits::Limits;
use crate::mapping::Mapping;
use crate::value::Value;

pub(crate) mod events;

impl Value {
    /// Apply YAML 1.1 merge keys recursively using the default resource limits.
    ///
    /// A `<<` mapping entry is removed and the entries from its operand are
    /// appended after the mapping's own keys. Explicit keys on the same
    /// mapping override merged keys. A sequence operand is processed in order
    /// with the first mapping winning conflicts. An operand is merged before
    /// it is used, so `<<` chains flatten fully. Invalid operands return
    /// [`ErrorKind::Composition`].
    ///
    /// Collection nesting depth and merged mapping sizes are bounded by
    /// [`Limits::default()`]. Use [`Value::apply_merge_keys_with_limits`] to
    /// supply custom bounds.
    ///
    /// The generic parser keeps `<<` literal; this method is opt-in. Because the
    /// generic `Value` model does not retain scalar style, a quoted `"<<"` key
    /// is treated the same as a plain one here. Use
    /// [`ParseOptions::with_merge_keys`](crate::ParseOptions::with_merge_keys)
    /// when you want event-level or typed merge expansion that keeps quoted
    /// `"<<"` keys literal and preserves source spans.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::Composition`] when a merge key operand cannot be
    /// merged, [`ErrorKind::DuplicateKey`] when expanding merges inside complex
    /// mapping keys makes two keys in the same mapping equal, or
    /// [`ErrorKind::Limit`] when collection nesting depth or merged collection
    /// entries exceed the default limits.
    pub fn apply_merge_keys(&mut self) -> Result<(), Error> {
        self.apply_merge_keys_with_limits(&Limits::default())
    }

    /// Apply YAML 1.1 merge keys recursively under explicit resource limits.
    ///
    /// Enforces `limits.max_depth` on collection nesting (at most
    /// [`MAX_DEPTH_CAP`](crate::MAX_DEPTH_CAP)) and
    /// `limits.max_collection_entries` whenever merging adds a new entry to a
    /// mapping.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::Limit`] when `limits` is invalid or when collection
    /// depth or merged entry counts exceed `limits`, [`ErrorKind::DuplicateKey`]
    /// when expanding merges inside complex mapping keys makes two keys in the
    /// same mapping equal, or [`ErrorKind::Composition`] when a merge operand
    /// cannot be merged.
    pub fn apply_merge_keys_with_limits(&mut self, limits: &Limits) -> Result<(), Error> {
        limits.validate()?;
        apply_value(self, 0, limits, false)
    }
}

fn apply_value(
    mut value: &mut Value,
    depth: usize,
    limits: &Limits,
    key_depth_checked: bool,
) -> Result<(), Error> {
    loop {
        match value {
            Value::Tagged(tagged) => {
                value = &mut tagged.value;
            }
            Value::Sequence(sequence) => {
                let next_depth = depth.saturating_add(1);
                limits.check_depth(next_depth)?;
                for item in sequence {
                    apply_value(item, next_depth, limits, key_depth_checked)?;
                }
                return Ok(());
            }
            Value::Mapping(mapping) => {
                return apply_mapping(mapping, depth, limits, key_depth_checked);
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => return Ok(()),
        }
    }
}

fn is_complex_key(key: &Value) -> bool {
    matches!(
        key,
        Value::Mapping(_) | Value::Sequence(_) | Value::Tagged(_)
    )
}

/// Validate `limits.max_depth` read-only inside a complex mapping key and report whether any
/// nested mapping inside it contains a `"<<"` merge key that requires rebuilding the parent map.
fn check_key_depth_and_has_merge(
    mut value: &Value,
    depth: usize,
    limits: &Limits,
) -> Result<bool, Error> {
    loop {
        match value {
            Value::Tagged(tagged) => {
                value = &tagged.value;
            }
            Value::Sequence(sequence) => {
                let next_depth = depth.saturating_add(1);
                limits.check_depth(next_depth)?;
                let mut has_merge = false;
                for item in sequence {
                    has_merge |= check_key_depth_and_has_merge(item, next_depth, limits)?;
                }
                return Ok(has_merge);
            }
            Value::Mapping(mapping) => {
                let next_depth = depth.saturating_add(1);
                limits.check_depth(next_depth)?;
                let mut has_merge = false;
                for (k, v) in mapping {
                    if matches!(k, Value::String(text) if text == "<<") {
                        has_merge = true;
                    }
                    has_merge |= check_key_depth_and_has_merge(k, next_depth, limits)?;
                    has_merge |= check_key_depth_and_has_merge(v, next_depth, limits)?;
                }
                return Ok(has_merge);
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => return Ok(false),
        }
    }
}

fn apply_mapping(
    mapping: &mut Mapping,
    depth: usize,
    limits: &Limits,
    key_depth_checked: bool,
) -> Result<(), Error> {
    let next_depth = depth.saturating_add(1);
    limits.check_depth(next_depth)?;

    let rebuild_for_complex_merges = if key_depth_checked {
        // Depth for every complex key inside this subtree was already verified by an enclosing
        // `check_key_depth_and_has_merge` pass, keeping deep complex-key chains strictly O(N).
        mapping.keys().any(is_complex_key)
    } else {
        let mut has_merge = false;
        for key in mapping.keys() {
            if is_complex_key(key) {
                has_merge |= check_key_depth_and_has_merge(key, next_depth, limits)?;
            }
        }
        has_merge
    };

    let merge_operand = if rebuild_for_complex_merges {
        let old = std::mem::take(mapping);
        let mut rebuilt = Mapping::with_capacity(old.len());
        let mut operand = None;
        for (mut key, mut value) in old {
            if matches!(&key, Value::String(text) if text == "<<") {
                apply_value(&mut value, next_depth, limits, key_depth_checked)?;
                operand = Some(value);
                continue;
            }
            apply_value(&mut key, next_depth, limits, true)?;
            apply_value(&mut value, next_depth, limits, key_depth_checked)?;
            if let Err(duplicate) = rebuilt.insert_new(key, value) {
                return Err(Error::coded(
                    ErrorKind::DuplicateKey,
                    ErrorCode::DuplicateKey,
                    format!("duplicate mapping key: {duplicate}"),
                ));
            }
        }
        *mapping = rebuilt;
        operand
    } else {
        for (_, value) in mapping.iter_mut() {
            apply_value(value, next_depth, limits, key_depth_checked)?;
        }
        mapping.shift_remove("<<")
    };

    let Some(merge_operand) = merge_operand else {
        return Ok(());
    };
    merge_operand_into(merge_operand, mapping, limits)
}

fn merge_operand_into(operand: Value, target: &mut Mapping, limits: &Limits) -> Result<(), Error> {
    fn merge_mapping(source: Mapping, target: &mut Mapping, limits: &Limits) -> Result<(), Error> {
        for (key, value) in source {
            target.insert_if_absent_with_limit(key, value, limits)?;
        }
        Ok(())
    }
    match operand {
        Value::Mapping(mapping) => merge_mapping(mapping, target, limits),
        Value::Sequence(sequence) => {
            for item in sequence {
                let Value::Mapping(mapping) = item else {
                    return Err(Error::coded(
                        ErrorKind::Composition,
                        ErrorCode::InvalidMergeOperand,
                        "merge sequence operands must be mappings",
                    ));
                };
                merge_mapping(mapping, target, limits)?;
            }
            Ok(())
        }
        _ => Err(Error::coded(
            ErrorKind::Composition,
            ErrorCode::InvalidMergeOperand,
            "merge operand must be a mapping or a sequence of mappings",
        )),
    }
}

#[cfg(test)]
mod tests {
    use crate::error::{ErrorCode, ErrorKind};
    use crate::limits::{LimitKind, Limits, MAX_DEPTH, MAX_DEPTH_CAP};
    use crate::mapping::Mapping;
    use crate::tag::{Tag, TaggedValue};
    use crate::value::Value;

    fn merged(input: &str) -> Value {
        let mut value = crate::parse_value(input).expect("parse");
        value.apply_merge_keys().expect("merge");
        value
    }

    fn nested_mapping(depth: usize) -> Value {
        let mut value = Value::from(1_u64);
        for _ in 0..depth {
            let mut mapping = Mapping::new();
            mapping.insert(Value::from("k"), value);
            value = Value::Mapping(mapping);
        }
        value
    }

    fn dismantle_nested_mapping(mut value: Value) {
        while let Value::Mapping(mut mapping) = value {
            let Some(next) = mapping.remove("k") else {
                break;
            };
            value = next;
        }
    }

    fn nested_sequence(depth: usize) -> Value {
        let mut value = Value::from(1_u64);
        for _ in 0..depth {
            value = Value::Sequence(vec![value]);
        }
        value
    }

    fn dismantle_nested_sequence(mut value: Value) {
        while let Value::Sequence(mut seq) = value {
            let Some(next) = seq.pop() else {
                break;
            };
            value = next;
        }
    }

    #[test]
    fn mapping_operand_merges_and_explicit_keys_win() {
        let value = merged("base: &b {x: 1, y: 1}\nmerged:\n  <<: *b\n  x: 2\n");
        assert_eq!(value["merged"]["x"].as_u64(), Some(2));
        assert_eq!(value["merged"]["y"].as_u64(), Some(1));
        assert!(
            !value["merged"]
                .as_mapping()
                .unwrap()
                .contains_key(Value::from("<<"))
        );
    }

    #[test]
    fn sequence_operands_use_first_mapping_precedence() {
        let value = merged("a: &a {x: 1, y: 1}\nb: &b {x: 2, z: 2}\nmerged:\n  <<: [*a, *b]\n");
        assert_eq!(value["merged"]["x"].as_u64(), Some(1));
        assert_eq!(value["merged"]["y"].as_u64(), Some(1));
        assert_eq!(value["merged"]["z"].as_u64(), Some(2));
    }

    #[test]
    fn merge_keys_are_applied_recursively() {
        let value = merged("base: &b {x: 1}\nouter:\n  inner:\n    <<: *b\n    y: 2\n");
        assert_eq!(value["outer"]["inner"]["x"].as_u64(), Some(1));
        assert_eq!(value["outer"]["inner"]["y"].as_u64(), Some(2));
    }

    #[test]
    fn own_keys_keep_their_order_and_merged_keys_follow() {
        let value = merged("b: &b {y: 1, x: 1}\nm:\n  z: 0\n  <<: *b\n  x: 2\n");
        let keys: Vec<_> = value["m"]
            .as_mapping()
            .unwrap()
            .keys()
            .map(|key| key.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(keys, ["z", "x", "y"]);
        assert_eq!(value["m"]["x"].as_u64(), Some(2));
    }

    #[test]
    fn chained_merges_flatten() {
        let value = merged("a: &a {x: 1}\nb: &b {<<: *a, y: 2}\nc:\n  <<: *b\n  z: 3\n");
        let expected = merged("c: {z: 3, y: 2, x: 1}\n");
        assert_eq!(value["c"], expected["c"]);
        assert_eq!(
            value["c"].as_mapping().unwrap().keys().collect::<Vec<_>>(),
            expected["c"]
                .as_mapping()
                .unwrap()
                .keys()
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn invalid_merge_operands_are_errors() {
        let mut value = crate::parse_value("merged:\n  <<: 1\n").expect("parse");
        let error = value.apply_merge_keys().expect_err("invalid operand");
        assert_eq!(error.kind(), ErrorKind::Composition);
        assert_eq!(error.code(), ErrorCode::InvalidMergeOperand);
    }

    #[test]
    fn apply_merge_keys_enforces_depth_limit() {
        let mut at_limit = nested_mapping(MAX_DEPTH);
        at_limit.apply_merge_keys().expect("at default max depth");
        dismantle_nested_mapping(at_limit);

        let mut over_limit = nested_mapping(MAX_DEPTH + 1);
        let error = over_limit
            .apply_merge_keys()
            .expect_err("over default max depth");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(error.code(), ErrorCode::DepthLimit);
        let exceeded = error.limit().expect("structured limit");
        assert_eq!(exceeded.kind(), LimitKind::Depth);
        assert_eq!(exceeded.limit(), MAX_DEPTH as u64);
        assert_eq!(exceeded.observed(), (MAX_DEPTH + 1) as u64);
        dismantle_nested_mapping(over_limit);

        let mut seq_at_limit = nested_sequence(MAX_DEPTH);
        seq_at_limit
            .apply_merge_keys()
            .expect("sequence at default max depth");
        dismantle_nested_sequence(seq_at_limit);

        let mut seq_over_limit = nested_sequence(MAX_DEPTH + 1);
        let error = seq_over_limit
            .apply_merge_keys()
            .expect_err("sequence over default max depth");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(error.code(), ErrorCode::DepthLimit);
        assert_eq!(
            error.limit().map(crate::LimitExceeded::kind),
            Some(LimitKind::Depth)
        );
        dismantle_nested_sequence(seq_over_limit);

        let mut deep_custom = nested_mapping(5);
        let tight = Limits::default().with_max_depth(4);
        let error = deep_custom
            .apply_merge_keys_with_limits(&tight)
            .expect_err("over custom max depth");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(error.code(), ErrorCode::DepthLimit);
        assert_eq!(
            error.limit().map(crate::LimitExceeded::kind),
            Some(LimitKind::Depth)
        );
        dismantle_nested_mapping(deep_custom);

        let mut scalar = Value::from("plain");
        scalar
            .apply_merge_keys_with_limits(&Limits::default().with_max_depth(0))
            .expect("root scalar has depth 0");
    }

    #[test]
    fn apply_merge_keys_with_limits_enforces_collection_entries_and_overlap() {
        let input = "base: &b {a: 1, b: 2, c: 3}\nm:\n  a: 10\n  d: 4\n  <<: *b\n";
        let mut exact = crate::parse_value(input).expect("parse");
        exact
            .apply_merge_keys_with_limits(&Limits::default().with_max_collection_entries(4))
            .expect("four entries fit inclusive limit");
        assert_eq!(exact["m"]["a"].as_u64(), Some(10));
        assert_eq!(exact["m"]["b"].as_u64(), Some(2));
        assert_eq!(exact["m"]["c"].as_u64(), Some(3));
        assert_eq!(exact["m"]["d"].as_u64(), Some(4));

        let mut too_tight = crate::parse_value(input).expect("parse");
        let error = too_tight
            .apply_merge_keys_with_limits(&Limits::default().with_max_collection_entries(3))
            .expect_err("fourth entry exceeds limit");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(error.code(), ErrorCode::CollectionEntriesLimit);
        let exceeded = error.limit().expect("structured limit");
        assert_eq!(exceeded.kind(), LimitKind::CollectionEntries);
        assert_eq!(exceeded.limit(), 3);
        assert_eq!(exceeded.observed(), 4);

        // When every merged key already exists on the target, merging adds zero entries.
        let mut overlap_only =
            crate::parse_value("base: &b {a: 1, b: 2}\nm:\n  a: 10\n  b: 20\n  <<: *b\n")
                .expect("parse");
        overlap_only
            .apply_merge_keys_with_limits(&Limits::default().with_max_collection_entries(2))
            .expect("overlapping keys do not grow the mapping");
        assert_eq!(overlap_only["m"]["a"].as_u64(), Some(10));
        assert_eq!(overlap_only["m"]["b"].as_u64(), Some(20));
    }

    #[test]
    fn apply_merge_keys_with_limits_rejects_invalid_limits() {
        let mut value = Value::Null;
        let over_cap = Limits::default().with_max_depth(MAX_DEPTH_CAP + 1);
        let error = value
            .apply_merge_keys_with_limits(&over_cap)
            .expect_err("max_depth above cap");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(error.code(), ErrorCode::InvalidLimits);
        assert!(error.limit().is_none());

        let zero_cost = Limits::default().with_node_cost_bytes(0);
        let error = value
            .apply_merge_keys_with_limits(&zero_cost)
            .expect_err("zero node cost");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(error.code(), ErrorCode::InvalidLimits);
        assert!(error.limit().is_none());
    }

    #[test]
    fn apply_merge_keys_handles_deep_tagged_chain_without_recursing() {
        let mut value =
            crate::parse_value("base: &b {x: 1}\nm:\n  <<: *b\n  y: 2\n").expect("parse");
        for _ in 0..5_000 {
            value = Value::from(TaggedValue::new(Tag::local("wrap"), value));
        }
        value
            .apply_merge_keys()
            .expect("tagged wrappers do not add recursion frames");
        let mut current = value;
        while let Value::Tagged(tagged) = current {
            current = tagged.value;
        }
        assert_eq!(current["m"]["x"].as_u64(), Some(1));
        assert_eq!(current["m"]["y"].as_u64(), Some(2));
    }

    #[test]
    fn apply_merge_keys_recurses_into_complex_keys_and_enforces_depth_and_duplicates() {
        let mut value = crate::parse_value("b: &b {x: 1}\nout:\n  ? { <<: *b, y: 2 }\n  : ok\n")
            .expect("parse");
        value.apply_merge_keys().expect("merge inside complex key");
        let mut expected_key = Mapping::new();
        expected_key.insert(Value::from("y"), Value::from(2_u64));
        expected_key.insert(Value::from("x"), Value::from(1_u64));
        assert_eq!(
            value["out"]
                .as_mapping()
                .unwrap()
                .get(Value::Mapping(expected_key)),
            Some(&Value::from("ok"))
        );

        // Complex key nesting counts toward `max_depth`, and failing the limit leaves the
        // mapping intact (non-destructive check before taking/rebuilding the mapping).
        let mut key_value = Value::from(1_u64);
        for _ in 0..4 {
            let mut inner = Mapping::new();
            inner.insert(key_value, Value::from(0_u64));
            key_value = Value::Mapping(inner);
        }
        let mut over_limit = key_value.clone();
        let err = over_limit
            .apply_merge_keys_with_limits(&Limits::default().with_max_depth(3))
            .expect_err("deep complex key must exceed max_depth");
        assert_eq!(err.kind(), ErrorKind::Limit);
        assert_eq!(err.code(), ErrorCode::DepthLimit);
        assert_eq!(
            err.limit().map(crate::LimitExceeded::kind),
            Some(LimitKind::Depth)
        );
        assert_eq!(over_limit, key_value);
        key_value
            .apply_merge_keys_with_limits(&Limits::default().with_max_depth(4))
            .expect("depth 4 fits inclusive limit");
        assert_ne!(key_value.as_mapping().unwrap().len(), 0);

        // Expanding a complex key to match an existing key returns DuplicateKey.
        let mut dup = crate::parse_value(
            "b: &b {x: 1}\nout:\n  ? { <<: *b }\n  : first\n  ? { x: 1 }\n  : second\n",
        )
        .expect("parse");
        let err = dup.apply_merge_keys().expect_err("colliding complex keys");
        assert_eq!(err.kind(), ErrorKind::DuplicateKey);
        assert_eq!(err.code(), ErrorCode::DuplicateKey);
    }
}
