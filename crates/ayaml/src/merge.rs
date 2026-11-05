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

use crate::error::{Error, ErrorKind};
use crate::mapping::Mapping;
use crate::value::Value;

impl Value {
    /// Apply YAML 1.1 merge keys recursively.
    ///
    /// A `<<` mapping entry is removed and the entries from its operand are
    /// appended after the mapping's own keys. Explicit keys on the same
    /// mapping override merged keys. A sequence operand is processed in order
    /// with the first mapping winning conflicts. An operand is merged before
    /// it is used, so `<<` chains flatten fully. Invalid operands return
    /// [`ErrorKind::Composition`].
    ///
    /// The generic parser keeps `<<` literal; this method is opt-in. Because the
    /// generic `Value` model does not retain scalar style, a quoted `"<<"` key
    /// is treated the same as a plain one here. Use the event stream when a
    /// caller needs to distinguish those forms.
    pub fn apply_merge_keys(&mut self) -> Result<(), Error> {
        apply_value(self)
    }
}

fn apply_value(value: &mut Value) -> Result<(), Error> {
    match value {
        Value::Sequence(sequence) => {
            for item in sequence {
                apply_value(item)?;
            }
        }
        Value::Mapping(mapping) => apply_mapping(mapping)?,
        Value::Tagged(tagged) => apply_value(&mut tagged.value)?,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

fn apply_mapping(mapping: &mut Mapping) -> Result<(), Error> {
    for (_, value) in mapping.iter_mut() {
        apply_value(value)?;
    }

    let Some(merge_operand) = mapping.shift_remove("<<") else {
        return Ok(());
    };
    merge_operand_into(merge_operand, mapping)
}

fn merge_operand_into(operand: Value, target: &mut Mapping) -> Result<(), Error> {
    fn merge_mapping(source: Mapping, target: &mut Mapping) {
        for (key, value) in source {
            if !target.contains_key(&key) {
                target.insert(key, value);
            }
        }
    }
    match operand {
        Value::Mapping(mapping) => {
            merge_mapping(mapping, target);
            Ok(())
        }
        Value::Sequence(sequence) => {
            for item in sequence {
                let Value::Mapping(mapping) = item else {
                    return Err(Error::new(
                        ErrorKind::Composition,
                        "merge sequence operands must be mappings",
                    ));
                };
                merge_mapping(mapping, target);
            }
            Ok(())
        }
        _ => Err(Error::new(
            ErrorKind::Composition,
            "merge operand must be a mapping or a sequence of mappings",
        )),
    }
}

#[cfg(test)]
mod tests {
    use crate::error::ErrorKind;
    use crate::value::Value;

    fn merged(input: &str) -> Value {
        let mut value = crate::parse_value(input).expect("parse");
        value.apply_merge_keys().expect("merge");
        value
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
    }
}
