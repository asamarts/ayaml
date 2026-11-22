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
    /// is treated the same as a plain one here. Use the event stream when a
    /// caller needs to distinguish those forms.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::Composition`] when a merge key operand cannot be
    /// merged, or [`ErrorKind::Limit`] when collection nesting depth or merged
    /// collection entries exceed the default limits.
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
    /// depth or merged entry counts exceed `limits`, or
    /// [`ErrorKind::Composition`] when a merge operand cannot be merged.
    pub fn apply_merge_keys_with_limits(&mut self, limits: &Limits) -> Result<(), Error> {
        limits.validate()?;
        apply_value(self, 0, limits)
    }
}

fn apply_value(mut value: &mut Value, depth: usize, limits: &Limits) -> Result<(), Error> {
    loop {
        match value {
            Value::Tagged(tagged) => {
                value = &mut tagged.value;
            }
            Value::Sequence(sequence) => {
                let next_depth = depth.saturating_add(1);
                limits.check_depth(next_depth)?;
                for item in sequence {
                    apply_value(item, next_depth, limits)?;
                }
                return Ok(());
            }
            Value::Mapping(mapping) => return apply_mapping(mapping, depth, limits),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => return Ok(()),
        }
    }
}

fn apply_mapping(mapping: &mut Mapping, depth: usize, limits: &Limits) -> Result<(), Error> {
    let next_depth = depth.saturating_add(1);
    limits.check_depth(next_depth)?;
    for (_, value) in mapping.iter_mut() {
        apply_value(value, next_depth, limits)?;
    }

    let Some(merge_operand) = mapping.shift_remove("<<") else {
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
        let exceeded = error.limit().expect("structured limit");
        assert_eq!(exceeded.kind(), LimitKind::Depth);
        assert_eq!(exceeded.limit(), MAX_DEPTH as u64);
        assert_eq!(exceeded.observed(), (MAX_DEPTH + 1) as u64);
        dismantle_nested_mapping(over_limit);

        let mut deep_custom = nested_mapping(5);
        let tight = Limits::default().with_max_depth(4);
        let error = deep_custom
            .apply_merge_keys_with_limits(&tight)
            .expect_err("over custom max depth");
        assert_eq!(error.kind(), ErrorKind::Limit);
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
        let exceeded = error.limit().expect("structured limit");
        assert_eq!(exceeded.kind(), LimitKind::CollectionEntries);
        assert_eq!(exceeded.limit(), 3);
        assert_eq!(exceeded.observed(), 4);
    }

    #[test]
    fn apply_merge_keys_with_limits_rejects_invalid_limits() {
        let mut value = Value::Null;
        let over_cap = Limits::default().with_max_depth(MAX_DEPTH_CAP + 1);
        let error = value
            .apply_merge_keys_with_limits(&over_cap)
            .expect_err("max_depth above cap");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert!(error.limit().is_none());

        let zero_cost = Limits::default().with_node_cost_bytes(0);
        let error = value
            .apply_merge_keys_with_limits(&zero_cost)
            .expect_err("zero node cost");
        assert_eq!(error.kind(), ErrorKind::Limit);
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
}
