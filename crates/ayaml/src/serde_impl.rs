//! Serde interoperability for the generic value model.
//!
//! It covers `Value`, `Number`, `Mapping`, `Tag`, and `TaggedValue`
//! serialization, `Value` deserialization, `from_value`, and the value-model
//! serializer. Event-backed borrowed-string streaming remains later work.

use serde::de::{
    Deserialize, Deserializer, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor,
};
use serde::ser::{
    Serialize, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
    SerializeTupleStruct, SerializeTupleVariant, Serializer,
};
use std::borrow::Cow;
use std::fmt;

use crate::mapping::Mapping;
use crate::number::Number;
use crate::tag::{Tag, TaggedValue};
use crate::value::Value;

impl Serialize for Number {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.serialize_to(serializer)
    }
}

impl Serialize for Tag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

/// A tagged value is a one-entry map from the tag to the value: `{"!Foo": 1}`
/// to a generic serializer such as serde_json. ayaml's serializers turn it
/// back into a tag.
impl Serialize for TaggedValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(&TagKey(&self.tag), &self.value)?;
        map.end()
    }
}

/// A tag as a map key: `!` and then the tag's Serde name
/// ([`Tag::serde_name`]), written in two parts so that [`check_for_tag`] can
/// tell it from a string key that starts with `!`.
struct TagKey<'a>(&'a Tag);

impl Serialize for TagKey<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl fmt::Display for TagKey<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("!")?;
        formatter.write_str(&self.0.serde_name())
    }
}

enum MaybeTag {
    Tag(String),
    NotTag(String),
}

/// Whether `value` displays as a [`TagKey`]: exactly `!` and then a name, in
/// two writes.
fn check_for_tag<T: ?Sized + fmt::Display>(value: &T) -> MaybeTag {
    enum State {
        Empty,
        Bang,
        Tag(String),
        NotTag(String),
    }
    impl fmt::Write for State {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            if text.is_empty() {
                return Ok(());
            }
            *self = match std::mem::replace(self, State::Empty) {
                State::Empty if text == "!" => State::Bang,
                State::Empty => State::NotTag(text.to_owned()),
                State::Bang => State::Tag(text.to_owned()),
                State::Tag(name) => State::NotTag(format!("!{name}{text}")),
                State::NotTag(mut string) => {
                    string.push_str(text);
                    State::NotTag(string)
                }
            };
            Ok(())
        }
    }
    let mut state = State::Empty;
    // Writing to a `State` cannot fail; a failing `Display` leaves a prefix.
    let _ = fmt::write(&mut state, format_args!("{value}"));
    match state {
        State::Empty => MaybeTag::NotTag(String::new()),
        State::Bang => MaybeTag::NotTag("!".to_owned()),
        State::Tag(name) => MaybeTag::Tag(name),
        State::NotTag(string) => MaybeTag::NotTag(string),
    }
}

impl<'de> Deserialize<'de> for Mapping {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            Value::Mapping(mapping) => Ok(mapping),
            // An empty value or `~` is an empty mapping.
            Value::Null => Ok(Mapping::new()),
            _ => Err(serde::de::Error::custom("expected a YAML mapping")),
        }
    }
}

impl Serialize for Mapping {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (key, value) in self.iter() {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Null => serializer.serialize_unit(),
            Value::Bool(value) => serializer.serialize_bool(*value),
            Value::Number(value) => value.serialize(serializer),
            Value::String(value) => serializer.serialize_str(value),
            Value::Sequence(sequence) => {
                let mut seq = serializer.serialize_seq(Some(sequence.len()))?;
                for item in sequence {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Value::Mapping(mapping) => mapping.serialize(serializer),
            Value::Tagged(tagged) => tagged.serialize(serializer),
        }
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a YAML-compatible Serde value")
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from_i64(value)))
    }

    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from_u64(value)))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from_f64(value)))
    }

    fn visit_i128<E: serde::de::Error>(self, value: i128) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from_i128(value)))
    }

    fn visit_u128<E: serde::de::Error>(self, value: u128) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from_u128(value)))
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(Value::String(value))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
        let mut sequence = Vec::new();
        while let Some(value) = access.next_element()? {
            sequence.push(value);
        }
        Ok(Value::Sequence(sequence))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
        let mut mapping = Mapping::new();
        while let Some((key, value)) = access.next_entry()? {
            if mapping.contains_key(&key) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate entry with key {}",
                    crate::value::key_text(&key)
                )));
            }
            mapping.insert(key, value);
        }
        Ok(Value::Mapping(mapping))
    }

    /// A tagged node arrives as an enum whose variant is the tag's Serde
    /// name, so `!!set` and `%TAG` tags survive.
    fn visit_enum<A: EnumAccess<'de>>(self, access: A) -> Result<Self::Value, A::Error> {
        let (variant, value) = access.variant::<String>()?;
        let value = value.newtype_variant::<Value>()?;
        Ok(Value::from(TaggedValue::new(
            Tag::from_serde_name(&variant),
            value,
        )))
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ValueVisitor)
    }
}

/// Deserialize a value from an already-composed generic value.
pub fn from_value<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, crate::Error> {
    T::deserialize(ValueDeserializer { value })
}

/// Deserialize using a caller-supplied Serde seed.
pub fn from_value_seed<'de, T: serde::de::DeserializeSeed<'de>>(
    value: Value,
    seed: T,
) -> Result<T::Value, crate::Error> {
    seed.deserialize(ValueDeserializer { value })
}

struct ValueDeserializer {
    value: Value,
}

impl<'de> Deserializer<'de> for ValueDeserializer {
    type Error = crate::Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        match self.value {
            Value::Null => visitor.visit_unit(),
            Value::Bool(value) => visitor.visit_bool(value),
            Value::Number(value) => value.visit(visitor),
            Value::String(value) => visitor.visit_string(value),
            Value::Sequence(sequence) => {
                let total = sequence.len();
                let mut access = ValueSeqAccess {
                    iter: sequence.into_iter(),
                    len: total,
                };
                let value = visitor.visit_seq(&mut access)?;
                // A visitor that stops early, such as a tuple's, leaves
                // elements behind.
                check_consumed(access.iter.len(), total, "sequence")?;
                Ok(value)
            }
            Value::Mapping(mapping) => {
                let total = mapping.len();
                let mut access = ValueMapAccess {
                    iter: mapping.into_iter(),
                    key: None,
                    plain_key: None,
                    value: None,
                };
                let value = visitor.visit_map(&mut access)?;
                check_consumed(access.iter.len(), total, "map")?;
                Ok(value)
            }
            Value::Tagged(tagged) => visitor.visit_enum(ValueEnumAccess {
                variant: Cow::Owned(tagged.tag.serde_name()),
                value: tagged.value,
                in_path: false,
            }),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        if matches!(self.value, Value::Null) {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_string(visitor)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        match self.value {
            Value::Null => visitor.visit_string("null".to_owned()),
            Value::Bool(value) => visitor.visit_string(value.to_string()),
            Value::Number(value) => visitor.visit_string(value.to_string()),
            Value::String(value) => visitor.visit_string(value),
            Value::Tagged(tagged) => ValueDeserializer {
                value: tagged.value,
            }
            .deserialize_string(visitor),
            value => ValueDeserializer { value }.deserialize_any(visitor),
        }
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        match self.value {
            Value::String(value) => visitor.visit_byte_buf(value.into_bytes()),
            Value::Tagged(tagged) => ValueDeserializer {
                value: tagged.value,
            }
            .deserialize_bytes(visitor),
            value => ValueDeserializer { value }.deserialize_any(visitor),
        }
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_bytes(visitor)
    }

    /// A null reads as an empty sequence. A `Value` cannot tell an empty
    /// scalar from `~`, so both are accepted.
    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        match self.value {
            Value::Null => visitor.visit_seq(ValueSeqAccess {
                iter: Vec::new().into_iter(),
                len: 0,
            }),
            value => ValueDeserializer { value }.deserialize_any(visitor),
        }
    }

    /// A null reads as an empty map.
    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        match self.value {
            Value::Null => visitor.visit_map(ValueMapAccess {
                iter: crate::mapping::Mapping::new().into_iter(),
                key: None,
                plain_key: None,
                value: None,
            }),
            value => ValueDeserializer { value }.deserialize_any(visitor),
        }
    }

    /// A null reads as a struct with no fields, so defaults apply.
    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_map(visitor)
    }

    /// A field name is the key's text: `1: x` names the field `"1"`, not
    /// the second field.
    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_string(visitor)
    }

    /// A character is read from the scalar's text, so `5` is `'5'`.
    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_string(visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let (variant, value) = enum_parts(self.value)?;
        visitor.visit_enum(ValueEnumAccess {
            variant: Cow::Owned(variant),
            value,
            in_path: true,
        })
    }

    /// Serde's float visitors take 64-bit integers but not wider ones, so a
    /// wide integer is handed over as the nearest float.
    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        match self.value {
            Value::Number(number) if number.is_wide() => {
                visitor.visit_f64(number.as_f64().expect("an integer converts to f64"))
            }
            value => ValueDeserializer { value }.deserialize_any(visitor),
        }
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_f64(visitor)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128
        unit unit_struct tuple tuple_struct ignored_any
    }
}

/// Fail when a visitor left `remaining` of `total` elements unread.
fn check_consumed(remaining: usize, total: usize, kind: &str) -> Result<(), crate::Error> {
    if remaining == 0 {
        Ok(())
    } else {
        Err(serde::de::Error::invalid_length(
            total,
            &format!("fewer elements in the {kind}").as_str(),
        ))
    }
}

impl<'de> Deserializer<'de> for Value {
    type Error = crate::Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self }.deserialize_any(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self }.deserialize_option(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self }.deserialize_newtype_struct(name, visitor)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self }.deserialize_bytes(visitor)
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self }.deserialize_byte_buf(visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self }.deserialize_enum(name, variants, visitor)
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self }.deserialize_str(visitor)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self }.deserialize_string(visitor)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self }.deserialize_seq(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self }.deserialize_map(visitor)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self }.deserialize_struct(name, fields, visitor)
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self }.deserialize_identifier(visitor)
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self }.deserialize_char(visitor)
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self }.deserialize_f64(visitor)
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self }.deserialize_f32(visitor)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128
        unit unit_struct tuple tuple_struct ignored_any
    }
}

struct ValueSeqAccess {
    iter: std::vec::IntoIter<Value>,
    len: usize,
}

impl<'de> SeqAccess<'de> for ValueSeqAccess {
    type Error = crate::Error;

    fn next_element_seed<T: serde::de::DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Self::Error> {
        let Some(value) = self.iter.next() else {
            return Ok(None);
        };
        seed.deserialize(ValueDeserializer { value })
            .map(Some)
            .map_err(|error| error.inside_index(self.len - self.iter.len() - 1))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

struct ValueMapAccess {
    iter: indexmap::map::IntoIter<Value, Value>,
    /// The text of the key just read, for a path, if it is a string.
    key: Option<String>,
    /// The key just read, if it is another scalar. Its text is written only for a path. A key
    /// that is neither is a collection.
    plain_key: Option<PlainKey>,
    value: Option<Value>,
}

impl<'de> MapAccess<'de> for ValueMapAccess {
    type Error = crate::Error;

    fn next_key_seed<K: serde::de::DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        let Some((key, value)) = self.iter.next() else {
            return Ok(None);
        };
        self.value = Some(value);
        match key {
            // The map keeps the text, and a target copies it only if it needs its own.
            Value::String(text) => seed
                .deserialize(KeyDeserializer {
                    text: Cow::Borrowed(self.key.insert(text)),
                })
                .map(Some),
            key => {
                (self.key, self.plain_key) = key_name(&key);
                seed.deserialize(ValueDeserializer { value: key }).map(Some)
            }
        }
    }

    fn next_value_seed<V: serde::de::DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        let value = self.value.take().ok_or_else(|| {
            crate::Error::new(crate::ErrorKind::Serde, "map value requested before key")
        })?;
        seed.deserialize(ValueDeserializer { value })
            .map_err(|error| match (&self.key, self.plain_key) {
                (Some(key), _) => error.inside_key(key),
                (None, Some(key)) => error.inside_key(&key.to_string()),
                (None, None) => error.inside_collection_key(),
            })
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

/// A key that is a null, a boolean or a number, kept without writing its text.
#[derive(Clone, Copy)]
enum PlainKey {
    Null,
    Bool(bool),
    Number(Number),
}

impl fmt::Display for PlainKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => formatter.write_str("null"),
            Self::Bool(value) => fmt::Display::fmt(value, formatter),
            Self::Number(value) => fmt::Display::fmt(value, formatter),
        }
    }
}

/// What a path says of a key that isn't a string: the text of a tagged string, or the scalar it
/// is. Neither, for a collection.
fn key_name(key: &Value) -> (Option<String>, Option<PlainKey>) {
    match key {
        Value::Null => (None, Some(PlainKey::Null)),
        Value::Bool(value) => (None, Some(PlainKey::Bool(*value))),
        Value::Number(value) => (None, Some(PlainKey::Number(*value))),
        Value::String(text) => (Some(text.clone()), None),
        Value::Tagged(tagged) => key_name(&tagged.value),
        Value::Sequence(_) | Value::Mapping(_) => (None, None),
    }
}

/// A string key or a variant's name. Text that a path may need is borrowed, and a visitor gets a
/// copy of it in whatever form it asks for. Text that nothing else keeps is the visitor's.
struct KeyDeserializer<'a> {
    text: Cow<'a, str>,
}

impl<'de> Deserializer<'de> for KeyDeserializer<'_> {
    type Error = crate::Error;

    // A visitor may implement only the owned form, and the default for it reads the borrowed one.
    // That holds for an identifier too, as `from_value` read one before it kept keys for paths.
    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_string(self.text.into_owned())
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_byte_buf(self.text.into_owned().into_bytes())
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_bytes(visitor)
    }

    /// A key read as an enum names a variant with no content, as a string value does. An error
    /// inside it names the variant, from the key's own text.
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_enum(ValueEnumAccess {
            variant: self.text,
            value: Value::Null,
            in_path: true,
        })
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        unit unit_struct seq tuple tuple_struct map struct identifier ignored_any
    }
}

/// A value read as an enum: the variant's name and what it holds. A tag's suffix and a string are
/// moved, not copied.
fn enum_parts(value: Value) -> Result<(String, Value), crate::Error> {
    Ok(match value {
        Value::Tagged(tagged) => {
            let TaggedValue { tag, value } = *tagged;
            (tag.into_suffix(), value)
        }
        Value::String(variant) => (variant, Value::Null),
        // `true` and `1` name variants renamed to them.
        Value::Bool(value) => (value.to_string(), Value::Null),
        Value::Number(value) => (value.to_string(), Value::Null),
        Value::Mapping(mapping) => {
            let mut iter = mapping.into_iter();
            let Some((key, value)) = iter.next() else {
                return Err(crate::Error::new(
                    crate::ErrorKind::Serde,
                    "enum mapping must contain one variant",
                ));
            };
            if iter.next().is_some() {
                return Err(crate::Error::new(
                    crate::ErrorKind::Serde,
                    "enum mapping must contain one variant",
                ));
            }
            let Value::String(variant) = key else {
                return Err(crate::Error::new(
                    crate::ErrorKind::Serde,
                    "enum variant key must be a string",
                ));
            };
            (variant, value)
        }
        _ => {
            return Err(crate::Error::new(
                crate::ErrorKind::Serde,
                "enum value must be tagged or a single-entry mapping",
            ));
        }
    })
}

struct ValueEnumAccess<'a> {
    variant: Cow<'a, str>,
    value: Value,
    /// Whether an error inside names the variant. A target that isn't an enum takes the tag as a
    /// tag, and the document has no key by that name.
    in_path: bool,
}

impl<'de, 'a> serde::de::EnumAccess<'de> for ValueEnumAccess<'a> {
    type Error = crate::Error;
    type Variant = ValueVariantAccess<'a>;

    fn variant_seed<V: serde::de::DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Self::Variant), Self::Error> {
        // A name kept for a path is lent to the visitor, and one that isn't is given to it.
        let (variant, name) = if self.in_path {
            let variant = seed.deserialize(KeyDeserializer {
                text: Cow::Borrowed(&self.variant),
            })?;
            (variant, Some(self.variant))
        } else {
            let variant = seed.deserialize(KeyDeserializer { text: self.variant })?;
            (variant, None)
        };
        Ok((
            variant,
            ValueVariantAccess {
                name,
                value: self.value,
            },
        ))
    }
}

struct ValueVariantAccess<'a> {
    /// The variant's name, if an error inside gives it.
    name: Option<Cow<'a, str>>,
    value: Value,
}

impl ValueVariantAccess<'_> {
    #[cold]
    fn inside_variant(name: Option<&str>, error: crate::Error) -> crate::Error {
        match name {
            Some(name) => error.inside_key(name),
            None => error,
        }
    }
}

impl<'de> serde::de::VariantAccess<'de> for ValueVariantAccess<'_> {
    type Error = crate::Error;

    fn unit_variant(self) -> Result<(), Self::Error> {
        if matches!(self.value, Value::Null) {
            Ok(())
        } else {
            Err(Self::inside_variant(
                self.name.as_deref(),
                crate::Error::new(crate::ErrorKind::Serde, "expected a unit enum variant"),
            ))
        }
    }

    fn newtype_variant_seed<T: serde::de::DeserializeSeed<'de>>(
        self,
        seed: T,
    ) -> Result<T::Value, Self::Error> {
        seed.deserialize(ValueDeserializer { value: self.value })
            .map_err(|error| Self::inside_variant(self.name.as_deref(), error))
    }

    fn tuple_variant<V: Visitor<'de>>(
        self,
        _length: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self.value }
            .deserialize_any(visitor)
            .map_err(|error| Self::inside_variant(self.name.as_deref(), error))
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        ValueDeserializer { value: self.value }
            .deserialize_any(visitor)
            .map_err(|error| Self::inside_variant(self.name.as_deref(), error))
    }
}

/// Serialize a Rust value into the generic YAML value model.
pub fn to_value<T: Serialize>(value: T) -> Result<Value, crate::Error> {
    value.serialize(ValueSerializer)
}

struct ValueSerializer;

struct ValueSeqSerializer {
    values: Vec<Value>,
}

struct ValueMapSerializer {
    mapping: Mapping,
    key: Option<Value>,
    /// A one-entry map may be a [`TaggedValue`] (see [`KeySerializer`]).
    check_tag: bool,
    tag_key: Option<Tag>,
    tagged: Option<TaggedValue>,
}

struct ValueStructSerializer {
    mapping: Mapping,
}

struct ValueTupleVariantSerializer {
    variant: &'static str,
    values: Vec<Value>,
}

struct ValueStructVariantSerializer {
    variant: &'static str,
    mapping: Mapping,
}

impl Serializer for ValueSerializer {
    type Ok = Value;
    type Error = crate::Error;
    type SerializeSeq = ValueSeqSerializer;
    type SerializeTuple = ValueSeqSerializer;
    type SerializeTupleStruct = ValueSeqSerializer;
    type SerializeTupleVariant = ValueTupleVariantSerializer;
    type SerializeMap = ValueMapSerializer;
    type SerializeStruct = ValueStructSerializer;
    type SerializeStructVariant = ValueStructVariantSerializer;

    fn serialize_bool(self, value: bool) -> Result<Self::Ok, Self::Error> {
        Ok(Value::Bool(value))
    }
    fn serialize_i8(self, value: i8) -> Result<Self::Ok, Self::Error> {
        self.serialize_i64(i64::from(value))
    }
    fn serialize_i16(self, value: i16) -> Result<Self::Ok, Self::Error> {
        self.serialize_i64(i64::from(value))
    }
    fn serialize_i32(self, value: i32) -> Result<Self::Ok, Self::Error> {
        self.serialize_i64(i64::from(value))
    }
    fn serialize_i64(self, value: i64) -> Result<Self::Ok, Self::Error> {
        Ok(Value::Number(Number::from_i64(value)))
    }
    fn serialize_i128(self, value: i128) -> Result<Self::Ok, Self::Error> {
        Ok(Value::Number(Number::from_i128(value)))
    }
    fn serialize_u8(self, value: u8) -> Result<Self::Ok, Self::Error> {
        self.serialize_u64(u64::from(value))
    }
    fn serialize_u16(self, value: u16) -> Result<Self::Ok, Self::Error> {
        self.serialize_u64(u64::from(value))
    }
    fn serialize_u32(self, value: u32) -> Result<Self::Ok, Self::Error> {
        self.serialize_u64(u64::from(value))
    }
    fn serialize_u64(self, value: u64) -> Result<Self::Ok, Self::Error> {
        Ok(Value::Number(Number::from_u64(value)))
    }
    fn serialize_u128(self, value: u128) -> Result<Self::Ok, Self::Error> {
        Ok(Value::Number(Number::from_u128(value)))
    }
    fn serialize_f32(self, value: f32) -> Result<Self::Ok, Self::Error> {
        // Widen through the shortest `f32` text, so `0.1_f32` is written
        // `0.1` and not `0.10000000149011612`.
        let widened = value.to_string().parse::<f64>().unwrap_or(f64::from(value));
        self.serialize_f64(widened)
    }
    fn serialize_f64(self, value: f64) -> Result<Self::Ok, Self::Error> {
        Ok(Value::Number(Number::from_f64(value)))
    }
    fn serialize_char(self, value: char) -> Result<Self::Ok, Self::Error> {
        Ok(Value::String(value.to_string()))
    }
    fn serialize_str(self, value: &str) -> Result<Self::Ok, Self::Error> {
        Ok(Value::String(value.to_owned()))
    }
    fn serialize_bytes(self, value: &[u8]) -> Result<Self::Ok, Self::Error> {
        Ok(Value::Sequence(
            value
                .iter()
                .map(|byte| Value::Number(Number::from_u64(u64::from(*byte))))
                .collect(),
        ))
    }
    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        Ok(Value::Null)
    }
    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<Self::Ok, Self::Error> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        Ok(Value::Null)
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        Ok(Value::Null)
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        // A unit variant is rendered as its scalar name. Newtype, tuple, and
        // struct variants remain tagged.
        Ok(Value::String(variant.to_owned()))
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        Ok(Value::from(TaggedValue::new(
            Tag::local(variant),
            value.serialize(ValueSerializer)?,
        )))
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Ok(ValueSeqSerializer {
            values: Vec::with_capacity(len.unwrap_or(0)),
        })
    }
    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        Ok(ValueSeqSerializer {
            values: Vec::with_capacity(len),
        })
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        Ok(ValueSeqSerializer {
            values: Vec::with_capacity(len),
        })
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Ok(ValueTupleVariantSerializer {
            variant,
            values: Vec::with_capacity(len),
        })
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Ok(ValueMapSerializer {
            mapping: Mapping::with_capacity(len.unwrap_or(0)),
            key: None,
            check_tag: len == Some(1),
            tag_key: None,
            tagged: None,
        })
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        Ok(ValueStructSerializer {
            mapping: Mapping::with_capacity(len),
        })
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Ok(ValueStructVariantSerializer {
            variant,
            mapping: Mapping::with_capacity(len),
        })
    }
}

macro_rules! finish_sequence {
    ($name:ident, $trait:ident, $finish:ident, $method:ident) => {
        impl $trait for $name {
            type Ok = Value;
            type Error = crate::Error;
            fn $method<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
                self.values.push(value.serialize(ValueSerializer)?);
                Ok(())
            }
            fn $finish(self) -> Result<Self::Ok, Self::Error> {
                Ok(Value::Sequence(self.values))
            }
        }
    };
}

finish_sequence!(ValueSeqSerializer, SerializeSeq, end, serialize_element);
finish_sequence!(ValueSeqSerializer, SerializeTuple, end, serialize_element);
finish_sequence!(
    ValueSeqSerializer,
    SerializeTupleStruct,
    end,
    serialize_field
);

impl SerializeTupleVariant for ValueTupleVariantSerializer {
    type Ok = Value;
    type Error = crate::Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.values.push(value.serialize(ValueSerializer)?);
        Ok(())
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(Value::from(TaggedValue::new(
            Tag::local(self.variant),
            Value::Sequence(self.values),
        )))
    }
}

impl SerializeMap for ValueMapSerializer {
    type Ok = Value;
    type Error = crate::Error;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), Self::Error> {
        if let Some(tagged) = self.tagged.take() {
            // A second entry: the first key only looked like a tag.
            self.mapping
                .insert(Value::String(TagKey(&tagged.tag).to_string()), tagged.value);
        }
        if self.check_tag && self.mapping.is_empty() {
            self.check_tag = false;
            match key.serialize(KeySerializer)? {
                Value::Tagged(tagged) if tagged.tag.handle() == TAG_KEY_HANDLE => {
                    self.tag_key = Some(Tag::from_serde_name(tagged.tag.suffix()));
                }
                key => self.key = Some(key),
            }
            return Ok(());
        }
        self.key = Some(key.serialize(ValueSerializer)?);
        Ok(())
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        if let Some(tag) = self.tag_key.take() {
            self.tagged = Some(TaggedValue::new(tag, value.serialize(ValueSerializer)?));
            return Ok(());
        }
        let key = self.key.take().ok_or_else(|| {
            crate::Error::new(crate::ErrorKind::Serde, "map value serialized before key")
        })?;
        self.mapping.insert(key, value.serialize(ValueSerializer)?);
        Ok(())
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        match self.tagged {
            Some(tagged) => Ok(Value::from(tagged)),
            None => Ok(Value::Mapping(self.mapping)),
        }
    }
}

/// The handle of the placeholder tag [`KeySerializer`] returns for a
/// [`TagKey`]. No parsed or constructed tag has it, and the placeholder
/// never leaves [`ValueMapSerializer`].
const TAG_KEY_HANDLE: &str = "\u{0}ayaml:tag-key:";

/// Serializes the first key of a one-entry map like [`ValueSerializer`], but
/// recognizes a [`TagKey`].
struct KeySerializer;

macro_rules! delegate_to_value_serializer {
    ($($method:ident($($arg:ident: $type:ty),*) -> $output:ty;)*) => {
        $(
            fn $method(self, $($arg: $type),*) -> Result<$output, Self::Error> {
                ValueSerializer.$method($($arg),*)
            }
        )*
    };
}

impl Serializer for KeySerializer {
    type Ok = Value;
    type Error = crate::Error;
    type SerializeSeq = ValueSeqSerializer;
    type SerializeTuple = ValueSeqSerializer;
    type SerializeTupleStruct = ValueSeqSerializer;
    type SerializeTupleVariant = ValueTupleVariantSerializer;
    type SerializeMap = ValueMapSerializer;
    type SerializeStruct = ValueStructSerializer;
    type SerializeStructVariant = ValueStructVariantSerializer;

    delegate_to_value_serializer! {
        serialize_bool(value: bool) -> Value;
        serialize_i8(value: i8) -> Value;
        serialize_i16(value: i16) -> Value;
        serialize_i32(value: i32) -> Value;
        serialize_i64(value: i64) -> Value;
        serialize_i128(value: i128) -> Value;
        serialize_u8(value: u8) -> Value;
        serialize_u16(value: u16) -> Value;
        serialize_u32(value: u32) -> Value;
        serialize_u64(value: u64) -> Value;
        serialize_u128(value: u128) -> Value;
        serialize_f32(value: f32) -> Value;
        serialize_f64(value: f64) -> Value;
        serialize_char(value: char) -> Value;
        serialize_str(value: &str) -> Value;
        serialize_bytes(value: &[u8]) -> Value;
        serialize_none() -> Value;
        serialize_unit() -> Value;
        serialize_unit_struct(name: &'static str) -> Value;
        serialize_unit_variant(name: &'static str, index: u32, variant: &'static str) -> Value;
        serialize_seq(len: Option<usize>) -> ValueSeqSerializer;
        serialize_tuple(len: usize) -> ValueSeqSerializer;
        serialize_tuple_struct(name: &'static str, len: usize) -> ValueSeqSerializer;
        serialize_tuple_variant(
            name: &'static str,
            index: u32,
            variant: &'static str,
            len: usize
        ) -> ValueTupleVariantSerializer;
        serialize_map(len: Option<usize>) -> ValueMapSerializer;
        serialize_struct(name: &'static str, len: usize) -> ValueStructSerializer;
        serialize_struct_variant(
            name: &'static str,
            index: u32,
            variant: &'static str,
            len: usize
        ) -> ValueStructVariantSerializer;
    }

    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<Value, Self::Error> {
        ValueSerializer.serialize_some(value)
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<Value, Self::Error> {
        ValueSerializer.serialize_newtype_struct(name, value)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Value, Self::Error> {
        ValueSerializer.serialize_newtype_variant(name, index, variant, value)
    }
    fn collect_str<T: ?Sized + fmt::Display>(self, value: &T) -> Result<Value, Self::Error> {
        Ok(match check_for_tag(value) {
            MaybeTag::Tag(name) => Value::from(TaggedValue::new(
                Tag::new(TAG_KEY_HANDLE, name),
                Value::Null,
            )),
            MaybeTag::NotTag(string) => Value::String(string),
        })
    }
}

impl SerializeStruct for ValueStructSerializer {
    type Ok = Value;
    type Error = crate::Error;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.mapping.insert(
            Value::String(key.to_owned()),
            value.serialize(ValueSerializer)?,
        );
        Ok(())
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(Value::Mapping(self.mapping))
    }
}

impl SerializeStructVariant for ValueStructVariantSerializer {
    type Ok = Value;
    type Error = crate::Error;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.mapping.insert(
            Value::String(key.to_owned()),
            value.serialize(ValueSerializer)?,
        );
        Ok(())
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(Value::from(TaggedValue::new(
            Tag::local(self.variant),
            Value::Mapping(self.mapping),
        )))
    }
}

#[cfg(test)]
mod tests {
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
    fn scalar_values_deserialize_as_strings_with_their_text() {
        use serde::Deserialize as _;
        assert_eq!(String::deserialize(Value::from(false)).unwrap(), "false");
        assert_eq!(String::deserialize(Value::from(42_u64)).unwrap(), "42");
        assert_eq!(String::deserialize(Value::Null).unwrap(), "null");
        assert_eq!(String::deserialize(Value::from("text")).unwrap(), "text");
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
        };
        let (handed, variant) = access.variant_seed(Address).expect("a name");
        assert_eq!(handed, address, "the name was copied");
        assert!(variant.name.is_none());

        let key = String::from("Right");
        let access = ValueEnumAccess {
            variant: Cow::Borrowed(&key),
            value: Value::Null,
            in_path: true,
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
}
