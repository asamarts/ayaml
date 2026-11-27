use super::Value;
use serde::Deserialize as _;

#[test]
fn serde_json_round_trips_generic_values() {
    let value = crate::parse_value("a: 1\nb:\n  - true\n  - text\n").expect("parse");
    let json = serde_json::to_string(&value).expect("serialize");
    let decoded: Value = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(decoded, value);
}

#[test]
fn serde_deserializes_scalars_and_sequences() {
    let value: Value = serde_json::from_str("[null, true, 3, 1.5, \"x\"]").expect("value");
    assert_eq!(value[0], Value::Null);
    assert_eq!(value[1], Value::Bool(true));
    assert_eq!(value[2].as_u64(), Some(3));
    assert_eq!(value[3].as_f64(), Some(1.5));
    assert_eq!(value[4], Value::from("x"));
}

#[test]
fn a_string_target_accepts_only_string_values() {
    assert_eq!(String::deserialize(Value::from("text")).unwrap(), "text");
    for value in [Value::from(false), Value::from(42_u64), Value::Null] {
        assert!(String::deserialize(value).is_err());
    }
}

#[test]
fn value_deserializer_accepts_plain_unit_enum_variants() {
    #[derive(Debug, PartialEq, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Mode {
        Fast,
        Careful,
    }

    let mode = Mode::deserialize(Value::from("careful")).expect("unit variant");
    assert_eq!(mode, Mode::Careful);
}

/// A key that is a null, a boolean or a number is kept as it is, and its text is written only
/// for an error's path. Tests can't count allocations, so this looks at what a read keeps.
#[test]
fn a_scalar_key_is_kept_without_its_text() {
    use super::{PlainKey, ValueMapAccess};
    use crate::tag::{Tag, TaggedValue};
    use serde::de::{IgnoredAny, MapAccess as _};

    let tagged = Value::Tagged(Box::new(TaggedValue::new(
        Tag::new("!", "t"),
        Value::from(7_u64),
    )));
    for (key, text) in [
        (Value::Null, "null"),
        (Value::from(true), "true"),
        (Value::from(42_u64), "42"),
        (Value::from(-3_i64), "-3"),
        (Value::from(1.5), "1.5"),
        (tagged, "7"),
    ] {
        let mut mapping = crate::Mapping::new();
        mapping.insert(key.clone(), Value::Null);
        let mut access = ValueMapAccess {
            iter: mapping.into_iter(),
            key: None,
            plain_key: None,
            value: None,
        };
        access.next_key::<IgnoredAny>().expect("a key");
        assert!(access.key.is_none(), "{key:?} kept text");
        let kept = access.plain_key.expect("the key is kept");
        assert!(
            matches!(
                kept,
                PlainKey::Null | PlainKey::Bool(_) | PlainKey::Number(_)
            ),
            "{key:?}"
        );
        assert_eq!(kept.to_string(), text, "{key:?}");
    }
    // A string key keeps its own text, which isn't copied, and a collection keeps nothing.
    let mut mapping = crate::Mapping::new();
    mapping.insert(Value::from("name"), Value::Null);
    mapping.insert(Value::Sequence(vec![]), Value::Null);
    let mut access = ValueMapAccess {
        iter: mapping.into_iter(),
        key: None,
        plain_key: None,
        value: None,
    };
    access.next_key::<IgnoredAny>().expect("a key");
    assert_eq!(access.key.as_deref(), Some("name"));
    access.next_value::<IgnoredAny>().expect("a value");
    access.next_key::<IgnoredAny>().expect("a key");
    assert!(access.key.is_none() && access.plain_key.is_none());
}

/// A variant's name that no path keeps is given to the visitor, and one that a path keeps is
/// lent to it.
#[test]
fn a_variant_s_name_is_moved_unless_a_path_keeps_it() {
    use super::ValueEnumAccess;
    use serde::de::{DeserializeSeed, Deserializer, EnumAccess as _, Visitor};
    use std::borrow::Cow;

    /// Where the text it was handed lives.
    struct Address;
    impl<'de> DeserializeSeed<'de> for Address {
        type Value = usize;
        fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<usize, D::Error> {
            deserializer.deserialize_identifier(self)
        }
    }
    impl Visitor<'_> for Address {
        type Value = usize;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a name")
        }
        fn visit_string<E>(self, text: String) -> Result<usize, E> {
            Ok(text.as_ptr().addr())
        }
        // The name is always handed over owned, so a visitor that takes only that form reads it.
        fn visit_str<E: serde::de::Error>(self, _text: &str) -> Result<usize, E> {
            Err(E::custom("the name was only lent"))
        }
    }

    let name = String::from("Right");
    let address = name.as_ptr().addr();
    let access = ValueEnumAccess {
        variant: Cow::Owned(name),
        value: Value::Null,
        in_path: false,
        bare: true,
        entered: None,
    };
    let (handed, variant) = access.variant_seed(Address).expect("a name");
    assert_eq!(handed, address, "the name was copied");
    assert!(variant.name.is_none());

    let key = String::from("Right");
    let access = ValueEnumAccess {
        variant: Cow::Borrowed(&key),
        value: Value::Null,
        in_path: true,
        bare: true,
        entered: None,
    };
    let (handed, variant) = access.variant_seed(Address).expect("a name");
    assert_ne!(handed, key.as_ptr().addr());
    assert!(matches!(variant.name, Some(Cow::Borrowed("Right"))));
}

/// A value read as an enum gives up its tag's suffix, or its string, as the variant's name,
/// without copying it.
#[test]
fn an_enum_s_name_is_moved_out_of_its_value() {
    use super::enum_parts;
    use crate::tag::{Tag, TaggedValue};

    let suffix = String::from("Right");
    let address = suffix.as_ptr().addr();
    let tagged = Value::Tagged(Box::new(TaggedValue::new(
        Tag::local(suffix),
        Value::from(5_u64),
    )));
    let (name, value) = enum_parts(tagged).expect("a variant");
    assert_eq!((name.as_str(), name.as_ptr().addr()), ("Right", address));
    assert_eq!(value, Value::from(5_u64));

    let text = String::from("Left");
    let address = text.as_ptr().addr();
    let (name, value) = enum_parts(Value::String(text)).expect("a variant");
    assert_eq!((name.as_str(), name.as_ptr().addr()), ("Left", address));
    assert!(value.is_null());

    let key = String::from("Both");
    let address = key.as_ptr().addr();
    let mut mapping = crate::Mapping::new();
    mapping.insert(Value::String(key), Value::from(1_u64));
    let (name, value) = enum_parts(Value::Mapping(mapping)).expect("a variant");
    assert_eq!((name.as_str(), name.as_ptr().addr()), ("Both", address));
    assert_eq!(value, Value::from(1_u64));
}

/// A mapping that is told its size reserves that room once, and keeps no more: it holds
/// what one made for that many entries holds. Grown from nothing it would hold more.
#[test]
fn a_mapping_that_is_told_its_size_reserves_it_once() {
    use serde::de::value::{Error as ValueError, MapDeserializer, SeqDeserializer};

    for entries in [3_usize, 100, 1_000] {
        let pairs = || (0..entries).map(|index| (format!("k{index}"), index as u64));
        let room = crate::Mapping::with_capacity(entries).capacity();
        let told = MapDeserializer::<_, ValueError>::new(pairs());
        let mapping = crate::Mapping::deserialize(told).expect("a mapping");
        assert_eq!((mapping.len(), mapping.capacity()), (entries, room));
        // A `Value` that is a mapping, or a sequence, does the same.
        let told = MapDeserializer::<_, ValueError>::new(pairs());
        let Value::Mapping(mapping) = Value::deserialize(told).expect("a mapping") else {
            panic!("a mapping");
        };
        assert_eq!((mapping.len(), mapping.capacity()), (entries, room));
        let told = SeqDeserializer::<_, ValueError>::new(0..entries as u64);
        let Value::Sequence(items) = Value::deserialize(told).expect("a sequence") else {
            panic!("a sequence");
        };
        assert_eq!(items.len(), entries);
        // A sequence takes the size for its room from 8 items on, up to 1,024.
        if entries >= 8 {
            assert_eq!(items.capacity(), entries);
        }
    }
}

#[test]
fn unhinted_small_and_spilled_collections_preserve_order_and_reject_duplicates() {
    use serde::de::value::{Error as ValueError, MapDeserializer, SeqDeserializer};
    use serde::de::{DeserializeSeed, MapAccess, SeqAccess};

    struct UnhintedMap<'de, I: Iterator<Item = (String, u64)>>(MapDeserializer<'de, I, ValueError>);
    impl<'de, I: Iterator<Item = (String, u64)>> MapAccess<'de> for UnhintedMap<'de, I> {
        type Error = ValueError;
        fn next_key_seed<K: DeserializeSeed<'de>>(
            &mut self,
            seed: K,
        ) -> Result<Option<K::Value>, Self::Error> {
            self.0.next_key_seed(seed)
        }
        fn next_value_seed<V: DeserializeSeed<'de>>(
            &mut self,
            seed: V,
        ) -> Result<V::Value, Self::Error> {
            self.0.next_value_seed(seed)
        }
        fn size_hint(&self) -> Option<usize> {
            None
        }
    }

    struct UnhintedSeq<I: Iterator<Item = u64>>(SeqDeserializer<I, ValueError>);
    impl<'de, I: Iterator<Item = u64>> SeqAccess<'de> for UnhintedSeq<I> {
        type Error = ValueError;
        fn next_element_seed<T: DeserializeSeed<'de>>(
            &mut self,
            seed: T,
        ) -> Result<Option<T::Value>, Self::Error> {
            self.0.next_element_seed(seed)
        }
        fn size_hint(&self) -> Option<usize> {
            None
        }
    }

    struct UnhintedValueMap(std::vec::IntoIter<(Value, Value)>, Option<Value>);
    impl<'de> MapAccess<'de> for UnhintedValueMap {
        type Error = ValueError;
        fn next_key_seed<K: DeserializeSeed<'de>>(
            &mut self,
            seed: K,
        ) -> Result<Option<K::Value>, Self::Error> {
            match self.0.next() {
                Some((k, v)) => {
                    self.1 = Some(v);
                    seed.deserialize(super::ValueDeserializer { value: k })
                        .map(Some)
                        .map_err(serde::de::Error::custom)
                }
                None => Ok(None),
            }
        }
        fn next_value_seed<V: DeserializeSeed<'de>>(
            &mut self,
            seed: V,
        ) -> Result<V::Value, Self::Error> {
            let v = self.1.take().expect("value after key");
            seed.deserialize(super::ValueDeserializer { value: v })
                .map_err(serde::de::Error::custom)
        }
        fn size_hint(&self) -> Option<usize> {
            None
        }
    }

    // Sizes straddling SMALL_LIMIT (8): empty, small, exact boundary, spill+1, larger.
    for count in [0_usize, 1, 7, 8, 9, 20] {
        let pairs: Vec<(String, u64)> = (0..count).map(|i| (format!("k{i}"), i as u64)).collect();
        let map = super::visit_mapping_entries(UnhintedMap(MapDeserializer::new(
            pairs.clone().into_iter(),
        )))
        .expect("unhinted mapping");
        assert_eq!(map.len(), count);
        for (idx, (k, v)) in map.iter().enumerate() {
            assert_eq!(k, &Value::from(pairs[idx].0.as_str()));
            assert_eq!(v, &Value::from(pairs[idx].1));
        }

        let items: Vec<u64> = (0..count as u64).collect();
        let Value::Sequence(seq) = serde::de::Visitor::visit_seq(
            super::ValueVisitor,
            UnhintedSeq(SeqDeserializer::new(items.clone().into_iter())),
        )
        .expect("unhinted sequence") else {
            panic!("expected sequence");
        };
        assert_eq!(
            seq,
            items.into_iter().map(Value::from).collect::<Vec<Value>>()
        );
    }

    // Duplicate detection inside small stack (<= 8), right at spill (9th key), and post-spill.
    for dup_pairs in [
        vec![
            ("a".to_owned(), 1_u64),
            ("b".to_owned(), 2_u64),
            ("a".to_owned(), 3_u64),
        ],
        {
            let mut v: Vec<(String, u64)> = (0_u64..8).map(|i| (format!("k{i}"), i)).collect();
            v.push(("k0".to_owned(), 99_u64));
            v
        },
        {
            let mut v: Vec<(String, u64)> = (0_u64..12).map(|i| (format!("k{i}"), i)).collect();
            v.push(("k9".to_owned(), 99_u64));
            v
        },
    ] {
        let err =
            super::visit_mapping_entries(UnhintedMap(MapDeserializer::new(dup_pairs.into_iter())))
                .expect_err("duplicate must fail");
        assert!(
            err.to_string().contains("duplicate entry with key"),
            "unexpected error: {err}"
        );
    }

    // Semantic Value equality duplicates (`-0.0 == 0.0`, `NaN == NaN`, and `Tagged`) in the
    // unhinted small-stack fast path (`<= 8` entries).
    for dup_vals in [
        vec![
            (
                Value::Number(crate::Number::from_f64(-0.0)),
                Value::from(1_u64),
            ),
            (
                Value::Number(crate::Number::from_f64(0.0)),
                Value::from(2_u64),
            ),
        ],
        vec![
            (
                Value::Number(crate::Number::from_f64(f64::NAN)),
                Value::from(1_u64),
            ),
            (
                Value::Number(crate::Number::from_f64(f64::NAN)),
                Value::from(2_u64),
            ),
        ],
        vec![
            (
                Value::Tagged(Box::new(crate::TaggedValue::new(
                    crate::Tag::local("t"),
                    Value::from("x"),
                ))),
                Value::from(1_u64),
            ),
            (
                Value::Tagged(Box::new(crate::TaggedValue::new(
                    crate::Tag::local("t"),
                    Value::from("x"),
                ))),
                Value::from(2_u64),
            ),
        ],
    ] {
        let err = super::visit_mapping_entries(UnhintedValueMap(dup_vals.into_iter(), None))
            .expect_err("semantic duplicate in unhinted map must fail");
        assert!(
            err.to_string().contains("duplicate entry with key"),
            "unexpected error: {err}"
        );
    }
}
