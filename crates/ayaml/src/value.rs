//! The generic YAML value model.

use std::ops::{Index as OpsIndex, IndexMut};

use crate::mapping::Mapping;
use crate::number::Number;
use crate::tag::TaggedValue;

/// The null value used when indexing a missing key/sequence item.
pub(crate) static NULL_VALUE: Value = Value::Null;

/// A human-readable mapping key for diagnostics. Scalar keys are printed as
/// values, not as their Rust `Debug` representation; complex keys fall back to
/// `Debug`.
pub(crate) fn key_text(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => format!("{value:?}"),
        Value::Tagged(tagged) => format!("{} {}", tagged.tag, key_text(&tagged.value)),
        Value::Sequence(_) | Value::Mapping(_) => format!("{value:?}"),
    }
}

/// A key or sequence offset accepted by [`Value::get`], [`Value::get_mut`],
/// and `value[index]`.
///
/// This trait is sealed: it is implemented only for the key and offset types
/// that [`Value`] supports.
pub trait Index: private::Sealed {
    #[doc(hidden)]
    fn get<'a>(&self, value: &'a Value) -> Option<&'a Value>;
    #[doc(hidden)]
    fn get_mut<'a>(&self, value: &'a mut Value) -> Option<&'a mut Value>;
    /// The place `value[index] = ...` writes to (see [`IndexMut`] for
    /// [`Value`]).
    #[doc(hidden)]
    fn index_or_insert<'a>(&self, value: &'a mut Value) -> &'a mut Value;
}

/// A generic YAML value.
///
/// The `is_*` and `as_*` accessors, [`Value::get`], and indexing look through
/// tags: `!Celsius 21` is a `u64`. [`Value::is_tagged`]
/// and [`Value::as_tagged`] see the tag.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Value {
    /// YAML null.
    Null,
    /// Boolean.
    Bool(bool),
    /// Integer or floating-point number.
    Number(Number),
    /// String.
    String(String),
    /// Ordered sequence.
    Sequence(Vec<Value>),
    /// Ordered mapping.
    Mapping(Mapping),
    /// Tagged value.
    Tagged(Box<TaggedValue>),
}

impl Value {
    /// Look up a mapping key or sequence offset without panicking.
    #[must_use]
    pub fn get<I: Index>(&self, index: I) -> Option<&Value> {
        Index::get(&index, self)
    }

    /// Mutably look up a mapping key or sequence offset without inserting.
    #[must_use]
    pub fn get_mut<I: Index>(&mut self, index: I) -> Option<&mut Value> {
        Index::get_mut(&index, self)
    }

    /// The value under any tags: `!t 5` gives `5`.
    #[must_use]
    pub fn untag(self) -> Value {
        let mut value = self;
        while let Self::Tagged(tagged) = value {
            value = tagged.value;
        }
        value
    }

    /// Borrow the value under any tags.
    #[must_use]
    pub fn untag_ref(&self) -> &Value {
        let mut value = self;
        while let Self::Tagged(tagged) = value {
            value = &tagged.value;
        }
        value
    }

    /// Mutably borrow the value under any tags.
    #[must_use]
    pub fn untag_mut(&mut self) -> &mut Value {
        let mut value = self;
        loop {
            match value {
                Self::Tagged(tagged) => value = &mut tagged.value,
                other => return other,
            }
        }
    }

    /// Whether this is null.
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self.untag_ref(), Self::Null)
    }

    /// Whether this is a boolean.
    #[must_use]
    pub fn is_bool(&self) -> bool {
        matches!(self.untag_ref(), Self::Bool(_))
    }

    /// Whether this is a number.
    #[must_use]
    pub fn is_number(&self) -> bool {
        matches!(self.untag_ref(), Self::Number(_))
    }

    /// Whether this is a non-negative integer.
    #[must_use]
    pub fn is_u64(&self) -> bool {
        self.as_number().is_some_and(Number::is_u64)
    }

    /// Whether this is an `i64` integer.
    #[must_use]
    pub fn is_i64(&self) -> bool {
        self.as_number().is_some_and(Number::is_i64)
    }

    /// Whether this is a float.
    #[must_use]
    pub fn is_f64(&self) -> bool {
        self.as_number().is_some_and(Number::is_f64)
    }

    /// Whether this is a string.
    #[must_use]
    pub fn is_string(&self) -> bool {
        matches!(self.untag_ref(), Self::String(_))
    }

    /// Whether this is a sequence.
    #[must_use]
    pub fn is_sequence(&self) -> bool {
        matches!(self.untag_ref(), Self::Sequence(_))
    }

    /// Whether this is a mapping.
    #[must_use]
    pub fn is_mapping(&self) -> bool {
        matches!(self.untag_ref(), Self::Mapping(_))
    }

    /// Whether this is tagged.
    #[must_use]
    pub const fn is_tagged(&self) -> bool {
        matches!(self, Self::Tagged(_))
    }

    /// Borrow as a bool.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self.untag_ref() {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// Borrow as a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self.untag_ref() {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    /// Borrow as a number.
    #[must_use]
    pub fn as_number(&self) -> Option<&Number> {
        match self.untag_ref() {
            Self::Number(value) => Some(value),
            _ => None,
        }
    }

    /// Borrow as `u64`.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        self.as_number().and_then(Number::as_u64)
    }

    /// Borrow as `i64`.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        self.as_number().and_then(Number::as_i64)
    }

    /// Borrow as `f64`. Integers convert.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        self.as_number().and_then(Number::as_f64)
    }

    /// Borrow as a sequence.
    #[must_use]
    pub fn as_sequence(&self) -> Option<&Vec<Value>> {
        match self.untag_ref() {
            Self::Sequence(value) => Some(value),
            _ => None,
        }
    }

    /// Mutably borrow as a sequence.
    #[must_use]
    pub fn as_sequence_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self.untag_mut() {
            Self::Sequence(value) => Some(value),
            _ => None,
        }
    }

    /// Borrow as a mapping.
    #[must_use]
    pub fn as_mapping(&self) -> Option<&Mapping> {
        match self.untag_ref() {
            Self::Mapping(value) => Some(value),
            _ => None,
        }
    }

    /// Mutably borrow as a mapping.
    #[must_use]
    pub fn as_mapping_mut(&mut self) -> Option<&mut Mapping> {
        match self.untag_mut() {
            Self::Mapping(value) => Some(value),
            _ => None,
        }
    }

    /// Borrow as a tagged value.
    #[must_use]
    pub const fn as_tagged(&self) -> Option<&TaggedValue> {
        match self {
            Self::Tagged(value) => Some(value),
            _ => None,
        }
    }
}

mod private {
    pub trait Sealed {}
}

impl private::Sealed for usize {}

impl Index for usize {
    fn get<'a>(&self, value: &'a Value) -> Option<&'a Value> {
        match value.untag_ref() {
            Value::Sequence(sequence) => sequence.get(*self),
            Value::Mapping(mapping) => mapping.get(Value::from(*self)),
            _ => None,
        }
    }

    fn get_mut<'a>(&self, value: &'a mut Value) -> Option<&'a mut Value> {
        match value.untag_mut() {
            Value::Sequence(sequence) => sequence.get_mut(*self),
            Value::Mapping(mapping) => mapping.get_mut(Value::from(*self)),
            _ => None,
        }
    }

    /// A sequence entry must exist; a sequence never grows. A mapping
    /// gains the integer key.
    fn index_or_insert<'a>(&self, value: &'a mut Value) -> &'a mut Value {
        match value.untag_mut() {
            Value::Sequence(sequence) => {
                let len = sequence.len();
                sequence.get_mut(*self).unwrap_or_else(|| {
                    panic!("cannot access index {self} of a YAML sequence of length {len}")
                })
            }
            Value::Mapping(mapping) => mapping.entry(Value::from(*self)).or_insert(Value::Null),
            other => panic!("cannot access index {self} of a YAML {}", kind_name(other)),
        }
    }
}

/// The kind of `value`, for index panics.
fn kind_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Sequence(_) => "sequence",
        Value::Mapping(_) => "mapping",
        Value::Tagged(_) => "tagged value",
    }
}

macro_rules! mapping_index {
    ($type:ty, $key:expr) => {
        impl private::Sealed for $type {}

        impl Index for $type {
            fn get<'a>(&self, value: &'a Value) -> Option<&'a Value> {
                match value.untag_ref() {
                    Value::Mapping(mapping) => mapping.get(self),
                    _ => None,
                }
            }

            fn get_mut<'a>(&self, value: &'a mut Value) -> Option<&'a mut Value> {
                match value.untag_mut() {
                    Value::Mapping(mapping) => mapping.get_mut(self),
                    _ => None,
                }
            }

            /// A null becomes an empty mapping, and a missing key is added
            /// with a null value.
            fn index_or_insert<'a>(&self, value: &'a mut Value) -> &'a mut Value {
                if matches!(value, Value::Null) {
                    *value = Value::Mapping(Mapping::new());
                }
                match value.untag_mut() {
                    Value::Mapping(mapping) => {
                        let key: fn(&$type) -> Value = $key;
                        mapping.entry(key(self)).or_insert(Value::Null)
                    }
                    other => panic!("cannot access key {self:?} in a YAML {}", kind_name(other)),
                }
            }
        }
    };
}

mapping_index!(Value, Value::clone);
mapping_index!(str, |key| Value::from(key));
mapping_index!(String, |key| Value::from(key.as_str()));

impl<T> private::Sealed for &T where T: ?Sized + Index {}

impl<T> Index for &T
where
    T: ?Sized + Index,
{
    fn get<'a>(&self, value: &'a Value) -> Option<&'a Value> {
        T::get(*self, value)
    }

    fn get_mut<'a>(&self, value: &'a mut Value) -> Option<&'a mut Value> {
        T::get_mut(*self, value)
    }

    fn index_or_insert<'a>(&self, value: &'a mut Value) -> &'a mut Value {
        T::index_or_insert(*self, value)
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<u64> for Value {
    fn from(value: u64) -> Self {
        Self::Number(Number::from(value))
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Number(Number::from(value))
    }
}

impl From<u128> for Value {
    fn from(value: u128) -> Self {
        Self::Number(Number::from(value))
    }
}

impl From<i128> for Value {
    fn from(value: i128) -> Self {
        Self::Number(Number::from(value))
    }
}

impl From<usize> for Value {
    fn from(value: usize) -> Self {
        Self::Number(Number::from(u64::try_from(value).unwrap_or(u64::MAX)))
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::Number(Number::from(value))
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<Vec<Value>> for Value {
    fn from(value: Vec<Value>) -> Self {
        Self::Sequence(value)
    }
}

impl From<Mapping> for Value {
    fn from(value: Mapping) -> Self {
        Self::Mapping(value)
    }
}

impl From<Number> for Value {
    fn from(value: Number) -> Self {
        Self::Number(value)
    }
}

impl From<TaggedValue> for Value {
    fn from(value: TaggedValue) -> Self {
        Self::Tagged(Box::new(value))
    }
}

impl PartialEq<str> for Value {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == Some(other)
    }
}

impl PartialEq<&str> for Value {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == Some(*other)
    }
}

impl PartialEq<String> for Value {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == Some(other.as_str())
    }
}

impl PartialEq<bool> for Value {
    fn eq(&self, other: &bool) -> bool {
        self.as_bool() == Some(*other)
    }
}

macro_rules! partial_eq_numeric {
    ($method:ident, $base:ty, $($type:ty),+ $(,)?) => {
        $(
            impl PartialEq<$type> for Value {
                fn eq(&self, other: &$type) -> bool {
                    self.$method().is_some_and(|value| value == *other as $base)
                }
            }
        )+
    };
}

partial_eq_numeric!(as_i64, i64, i8, i16, i32, i64, isize);
partial_eq_numeric!(as_u64, u64, u8, u16, u32, u64, usize);
partial_eq_numeric!(as_f64, f64, f32, f64);

/// `value[index]` is the entry, or null when there is none: a missing key, an
/// offset past the end, or a value of another kind. It looks through tags, so `value[0]` on `{0: zero}` is `zero`.
impl<I: Index> OpsIndex<I> for Value {
    type Output = Value;

    fn index(&self, index: I) -> &Self::Output {
        Index::get(&index, self).unwrap_or(&NULL_VALUE)
    }
}

/// `value[index] = ...` writes the entry. A mapping gains
/// a missing key, and a null becomes a mapping first. A sequence offset
/// must exist, since a sequence never grows. Panics on any other kind, or
/// on an offset past the end.
impl<I: Index> IndexMut<I> for Value {
    fn index_mut(&mut self, index: I) -> &mut Self::Output {
        index.index_or_insert(self)
    }
}

#[cfg(test)]
mod tests {
    use super::Value;
    use crate::mapping::Mapping;
    use crate::number::Number;
    use crate::tag::{Tag, TaggedValue};

    #[test]
    fn string_and_integer_indexing() {
        let mut map = Mapping::new();
        map.insert("name".into(), "ayaml".into());
        let value = Value::Mapping(map);
        assert_eq!(value["name"], Value::from("ayaml"));
        assert_eq!(value["missing"], Value::Null);

        let sequence = Value::Sequence(vec![Value::from(1_u64), Value::from(2_u64)]);
        assert_eq!(sequence[0], Value::from(1_u64));
        assert_eq!(sequence[9], Value::Null);
    }

    #[test]
    fn value_key_indexing() {
        let mut map = Mapping::new();
        map.insert(Value::from(1_u64), Value::from("one"));
        let value = Value::Mapping(map);
        assert_eq!(value[&Value::from(1_u64)], Value::from("one"));
        assert_eq!(value[&Value::from(1.0_f64)], Value::Null);
    }

    #[test]
    fn accessors_report_the_right_variant() {
        assert!(Value::Null.is_null());
        assert!(Value::from(true).is_bool());
        assert_eq!(Value::from(3_u64).as_u64(), Some(3));
        assert_eq!(Value::from(3.5_f64).as_f64(), Some(3.5));
        assert_eq!(Value::from("s").as_str(), Some("s"));
        assert!(Value::Sequence(vec![]).is_sequence());
        assert!(Value::Mapping(Mapping::new()).is_mapping());
    }

    #[test]
    fn tagged_values_keep_the_tag() {
        let tagged = Value::from(TaggedValue::new(Tag::local("Custom"), Value::from(1_u64)));
        assert!(tagged.is_tagged());
        assert_eq!(tagged.as_tagged().unwrap().tag.to_string(), "!Custom");
    }

    #[test]
    fn mut_indexing_inserts_without_clobbering() {
        let mut value = Value::Mapping(Mapping::new());
        value["a"] = Value::from(1_u64);
        value["a"] = Value::from(2_u64);
        assert_eq!(value["a"], Value::from(2_u64));

        let mut sequence = Value::Sequence(vec![Value::Null; 3]);
        sequence[2] = Value::from(3_u64);
        assert_eq!(sequence[0], Value::Null);
        assert_eq!(sequence[2], Value::from(3_u64));

        let mut null = Value::Null;
        null["a"]["b"] = Value::from(1_u64);
        assert_eq!(null["a"]["b"], Value::from(1_u64));
    }

    #[test]
    #[should_panic(expected = "cannot access index 3 of a YAML sequence of length 3")]
    fn mut_indexing_never_grows_a_sequence() {
        let mut sequence = Value::Sequence(vec![Value::Null; 3]);
        sequence[3] = Value::from(3_u64);
    }

    #[test]
    fn numeric_keys_are_distinct() {
        let mut map = Mapping::new();
        map.insert(Value::from(1_u64), Value::from("int"));
        map.insert(Value::Number(Number::from(1.0)), Value::from("float"));
        assert_eq!(map.len(), 2);
    }
}
