//! A YAML number with compatibility semantics.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

/// A YAML number: an integer in the `i128` or `u128` range, or a float.
///
/// Integers wider than 64 bits are kept exactly. The 64-bit
/// accessors return `None` for them; use [`Number::as_i128`] and
/// [`Number::as_u128`].
#[derive(Clone, Copy, Debug)]
pub struct Number(Repr);

/// Each integer has one representation: the 64-bit variants hold every
/// value that fits them, so derived equality is exact.
///
/// Wide integers are stored as two `u64` limbs rather than a `u128`. A
/// `u128` field would align `Number` to 16 bytes and round `Value` up to 80
/// bytes; the limbs keep the alignment at 8, which keeps `Value` at 72 bytes.
#[derive(Clone, Copy)]
enum Repr {
    PosInt(u64),
    NegInt(i64),
    /// An integer above `u64::MAX`, as high and low limbs.
    PosWide(u64, u64),
    /// An integer below `i64::MIN`, as the high and low limbs of its `u128`
    /// two's-complement bits.
    NegWide(u64, u64),
    Float(f64),
}

/// Split a `u128` into high and low limbs.
const fn wide_limbs(value: u128) -> (u64, u64) {
    ((value >> 64) as u64, value as u64)
}

/// Join high and low limbs into a `u128`.
const fn wide_u128(high: u64, low: u64) -> u128 {
    ((high as u128) << 64) | low as u128
}

/// Join high and low limbs into the `i128` whose bits they hold.
const fn wide_i128(high: u64, low: u64) -> i128 {
    wide_u128(high, low) as i128
}

/// An integer's place on the number line, for ordering.
enum Integer {
    Negative(i128),
    NonNegative(u128),
}

impl Number {
    /// Create a non-negative integer.
    #[must_use]
    pub const fn from_u64(value: u64) -> Self {
        Self(Repr::PosInt(value))
    }

    /// Create a negative integer.
    #[must_use]
    pub const fn from_i64(value: i64) -> Self {
        if value >= 0 {
            Self(Repr::PosInt(value as u64))
        } else {
            Self(Repr::NegInt(value))
        }
    }

    /// Create an integer from a `u128`.
    #[must_use]
    pub const fn from_u128(value: u128) -> Self {
        if value <= u64::MAX as u128 {
            Self(Repr::PosInt(value as u64))
        } else {
            let (high, low) = wide_limbs(value);
            Self(Repr::PosWide(high, low))
        }
    }

    /// Create an integer from an `i128`.
    #[must_use]
    pub const fn from_i128(value: i128) -> Self {
        if value >= 0 {
            Self::from_u128(value as u128)
        } else if value >= i64::MIN as i128 {
            Self(Repr::NegInt(value as i64))
        } else {
            let (high, low) = wide_limbs(value as u128);
            Self(Repr::NegWide(high, low))
        }
    }

    /// Create a float.
    #[must_use]
    pub const fn from_f64(value: f64) -> Self {
        Self(Repr::Float(value))
    }

    /// Whether this is a non-negative integer.
    #[must_use]
    pub const fn is_u64(&self) -> bool {
        matches!(self.0, Repr::PosInt(_))
    }

    /// Whether this is an integer in the `i64` range.
    #[must_use]
    pub const fn is_i64(&self) -> bool {
        match self.0 {
            Repr::PosInt(value) => value <= i64::MAX as u64,
            Repr::NegInt(_) => true,
            Repr::PosWide(..) | Repr::NegWide(..) | Repr::Float(_) => false,
        }
    }

    /// Whether this is an integer, of any width.
    #[must_use]
    pub const fn is_integer(&self) -> bool {
        !self.is_f64()
    }

    /// Whether this is a float.
    #[must_use]
    pub const fn is_f64(&self) -> bool {
        matches!(self.0, Repr::Float(_))
    }

    /// Return the value as `u64` when it is a non-negative integer.
    #[must_use]
    pub const fn as_u64(&self) -> Option<u64> {
        match self.0 {
            Repr::PosInt(value) => Some(value),
            Repr::NegInt(_) | Repr::PosWide(..) | Repr::NegWide(..) | Repr::Float(_) => None,
        }
    }

    /// Return the value as `i64` when it fits.
    #[must_use]
    pub const fn as_i64(&self) -> Option<i64> {
        match self.0 {
            Repr::PosInt(value) if value <= i64::MAX as u64 => Some(value as i64),
            Repr::NegInt(value) => Some(value),
            Repr::PosInt(_) | Repr::PosWide(..) | Repr::NegWide(..) | Repr::Float(_) => None,
        }
    }

    /// Return the value as `u128` when it is a non-negative integer.
    #[must_use]
    pub const fn as_u128(&self) -> Option<u128> {
        match self.0 {
            Repr::PosInt(value) => Some(value as u128),
            Repr::PosWide(high, low) => Some(wide_u128(high, low)),
            Repr::NegInt(_) | Repr::NegWide(..) | Repr::Float(_) => None,
        }
    }

    /// Return the value as `i128` when it fits.
    #[must_use]
    pub const fn as_i128(&self) -> Option<i128> {
        match self.0 {
            Repr::PosInt(value) => Some(value as i128),
            Repr::NegInt(value) => Some(value as i128),
            Repr::PosWide(high, low) if wide_u128(high, low) <= i128::MAX as u128 => {
                Some(wide_u128(high, low) as i128)
            }
            Repr::NegWide(high, low) => Some(wide_i128(high, low)),
            Repr::PosWide(..) | Repr::Float(_) => None,
        }
    }

    /// Return the value as `f64`. Integers convert, rounding to the nearest
    /// float.
    #[must_use]
    pub const fn as_f64(&self) -> Option<f64> {
        match self.0 {
            Repr::Float(value) => Some(value),
            Repr::PosInt(value) => Some(value as f64),
            Repr::NegInt(value) => Some(value as f64),
            Repr::PosWide(high, low) => Some(wide_u128(high, low) as f64),
            Repr::NegWide(high, low) => Some(wide_i128(high, low) as f64),
        }
    }

    /// Whether this is an integer outside the 64-bit range.
    pub(crate) const fn is_wide(&self) -> bool {
        matches!(self.0, Repr::PosWide(..) | Repr::NegWide(..))
    }

    /// Hand the number to a Serde visitor, in its own width.
    pub(crate) fn visit<'de, V, E>(self, visitor: V) -> Result<V::Value, E>
    where
        V: serde::de::Visitor<'de>,
        E: serde::de::Error,
    {
        match self.0 {
            Repr::PosInt(value) => visitor.visit_u64(value),
            Repr::NegInt(value) => visitor.visit_i64(value),
            Repr::PosWide(high, low) => visitor.visit_u128(wide_u128(high, low)),
            Repr::NegWide(high, low) => visitor.visit_i128(wide_i128(high, low)),
            Repr::Float(value) => visitor.visit_f64(value),
        }
    }

    /// Write the number to a Serde serializer, in its own width.
    pub(crate) fn serialize_to<S: serde::Serializer>(
        self,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match self.0 {
            Repr::PosInt(value) => serializer.serialize_u64(value),
            Repr::NegInt(value) => serializer.serialize_i64(value),
            Repr::PosWide(high, low) => serializer.serialize_u128(wide_u128(high, low)),
            Repr::NegWide(high, low) => serializer.serialize_i128(wide_i128(high, low)),
            Repr::Float(value) => serializer.serialize_f64(value),
        }
    }

    fn integer(self) -> Option<Integer> {
        match self.0 {
            Repr::PosInt(value) => Some(Integer::NonNegative(u128::from(value))),
            Repr::PosWide(high, low) => Some(Integer::NonNegative(wide_u128(high, low))),
            Repr::NegInt(value) => Some(Integer::Negative(i128::from(value))),
            Repr::NegWide(high, low) => Some(Integer::Negative(wide_i128(high, low))),
            Repr::Float(_) => None,
        }
    }

    /// Whether the number is NaN.
    #[must_use]
    pub fn is_nan(&self) -> bool {
        matches!(self.0, Repr::Float(value) if value.is_nan())
    }

    /// Whether the number is infinite.
    #[must_use]
    pub fn is_infinite(&self) -> bool {
        matches!(self.0, Repr::Float(value) if value.is_infinite())
    }

    /// Whether the number is finite.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        !self.is_nan() && !self.is_infinite()
    }
}

/// Keep `Number`'s `Debug` output independent of the two-limb storage: a wide
/// integer still prints its value, as it did before step 2b.
impl fmt::Debug for Repr {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::PosInt(value) => formatter.debug_tuple("PosInt").field(&value).finish(),
            Self::NegInt(value) => formatter.debug_tuple("NegInt").field(&value).finish(),
            Self::PosWide(high, low) => formatter
                .debug_tuple("PosWide")
                .field(&wide_u128(high, low))
                .finish(),
            Self::NegWide(high, low) => formatter
                .debug_tuple("NegWide")
                .field(&wide_i128(high, low))
                .finish(),
            Self::Float(value) => formatter.debug_tuple("Float").field(&value).finish(),
        }
    }
}

impl fmt::Display for Number {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Repr::PosInt(value) => write!(formatter, "{value}"),
            Repr::NegInt(value) => write!(formatter, "{value}"),
            Repr::PosWide(high, low) => write!(formatter, "{}", wide_u128(high, low)),
            Repr::NegWide(high, low) => write!(formatter, "{}", wide_i128(high, low)),
            Repr::Float(value) if value.is_nan() => formatter.write_str(".nan"),
            Repr::Float(value) if value.is_infinite() && value.is_sign_positive() => {
                formatter.write_str(".inf")
            }
            Repr::Float(value) if value.is_infinite() => formatter.write_str("-.inf"),
            Repr::Float(value) => write!(formatter, "{value:?}"),
        }
    }
}

impl PartialEq for Number {
    fn eq(&self, other: &Self) -> bool {
        match (self.0, other.0) {
            (Repr::PosInt(left), Repr::PosInt(right)) => left == right,
            (Repr::NegInt(left), Repr::NegInt(right)) => left == right,
            (Repr::PosWide(left_high, left_low), Repr::PosWide(right_high, right_low)) => {
                (left_high, left_low) == (right_high, right_low)
            }
            (Repr::NegWide(left_high, left_low), Repr::NegWide(right_high, right_low)) => {
                (left_high, left_low) == (right_high, right_low)
            }
            (Repr::Float(left), Repr::Float(right)) => {
                if left.is_nan() && right.is_nan() {
                    true
                } else {
                    left == right
                }
            }
            _ => false,
        }
    }
}

impl Eq for Number {}

impl Hash for Number {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self.0 {
            // Every NaN is one value to Eq, and so are both zeros, so each has to hash as one.
            Repr::Float(value) => {
                3_u8.hash(state);
                let bits = if value.is_nan() {
                    f64::NAN.to_bits()
                } else if value == 0.0 {
                    0
                } else {
                    value.to_bits()
                };
                bits.hash(state);
            }
            Repr::PosInt(value) => value.hash(state),
            Repr::NegInt(value) => value.hash(state),
            Repr::PosWide(high, low) => wide_u128(high, low).hash(state),
            Repr::NegWide(high, low) => wide_i128(high, low).hash(state),
        }
    }
}

impl PartialOrd for Number {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A total order, consistent with `Eq`: integers by value, then floats,
/// with NaN after every other float.
impl Ord for Number {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.integer(), other.integer()) {
            (Some(Integer::Negative(left)), Some(Integer::Negative(right))) => left.cmp(&right),
            (Some(Integer::NonNegative(left)), Some(Integer::NonNegative(right))) => {
                left.cmp(&right)
            }
            (Some(Integer::Negative(_)), Some(Integer::NonNegative(_))) => Ordering::Less,
            (Some(Integer::NonNegative(_)), Some(Integer::Negative(_))) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => {
                let (Repr::Float(left), Repr::Float(right)) = (self.0, other.0) else {
                    unreachable!("only floats have no integer value");
                };
                left.partial_cmp(&right)
                    .unwrap_or_else(|| left.is_nan().cmp(&right.is_nan()))
            }
        }
    }
}

impl From<u64> for Number {
    fn from(value: u64) -> Self {
        Self::from_u64(value)
    }
}

impl From<i64> for Number {
    fn from(value: i64) -> Self {
        Self::from_i64(value)
    }
}

impl From<i32> for Number {
    fn from(value: i32) -> Self {
        Self::from_i64(i64::from(value))
    }
}

impl From<u32> for Number {
    fn from(value: u32) -> Self {
        Self::from_u64(u64::from(value))
    }
}

impl From<u128> for Number {
    fn from(value: u128) -> Self {
        Self::from_u128(value)
    }
}

impl From<i128> for Number {
    fn from(value: i128) -> Self {
        Self::from_i128(value)
    }
}

impl From<f64> for Number {
    fn from(value: f64) -> Self {
        Self::from_f64(value)
    }
}

#[cfg(test)]
mod tests {
    use super::Number;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn hash(number: Number) -> u64 {
        let mut hasher = DefaultHasher::new();
        number.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn integers_and_floats_are_distinct() {
        assert_ne!(Number::from(1_u64), Number::from(1.0_f64));
    }

    #[test]
    fn nan_equals_nan_and_signed_zero_equals_zero() {
        assert_eq!(Number::from(f64::NAN), Number::from(f64::NAN));
        assert_eq!(Number::from(-0.0_f64), Number::from(0.0_f64));
    }

    #[test]
    fn equal_numbers_hash_alike() {
        let quiet = f64::NAN;
        let other_nan = f64::from_bits(quiet.to_bits() | 1);
        let negative_nan = -quiet;
        assert!(other_nan.is_nan() && negative_nan.is_nan());
        for (left, right) in [
            (quiet, other_nan),
            (quiet, negative_nan),
            (0.0, -0.0),
            (1.5, 1.5),
            (f64::INFINITY, f64::INFINITY),
        ] {
            let (left, right) = (Number::from(left), Number::from(right));
            assert_eq!(left, right);
            assert_eq!(hash(left), hash(right));
        }
    }

    #[test]
    fn different_floats_hash_differently() {
        assert_ne!(hash(Number::from(1.0)), hash(Number::from(2.0)));
        assert_ne!(hash(Number::from(f64::NAN)), hash(Number::from(0.0)));
        assert_ne!(
            hash(Number::from(f64::INFINITY)),
            hash(Number::from(f64::NEG_INFINITY))
        );
    }

    #[test]
    fn integers_and_floats_stay_distinct() {
        assert_ne!(Number::from(1_u64), Number::from(1.0));
        assert_ne!(Number::from(0_u64), Number::from(0.0));
    }

    #[test]
    fn display_uses_yaml_non_finite_spellings() {
        assert_eq!(Number::from(1_u64).to_string(), "1");
        assert_eq!(Number::from(1.0_f64).to_string(), "1.0");
        assert_eq!(Number::from(f64::NAN).to_string(), ".nan");
        assert_eq!(Number::from(f64::INFINITY).to_string(), ".inf");
        assert_eq!(Number::from(f64::NEG_INFINITY).to_string(), "-.inf");
    }

    #[test]
    fn wide_integers_are_exact_and_canonical() {
        let above = Number::from(u128::from(u64::MAX) + 1);
        let below = Number::from(i128::from(i64::MIN) - 1);
        assert_eq!(above.as_u64(), None);
        assert_eq!(above.as_u128(), Some(u128::from(u64::MAX) + 1));
        assert_eq!(above.to_string(), "18446744073709551616");
        assert_eq!(below.as_i64(), None);
        assert_eq!(below.as_i128(), Some(i128::from(i64::MIN) - 1));
        assert_eq!(Number::from(u128::MAX).as_i128(), None);
        assert_eq!(Number::from(5_u128), Number::from(5_u64));
        assert_eq!(Number::from(-5_i128), Number::from(-5_i64));
        assert_eq!(hash(Number::from(5_u128)), hash(Number::from(5_u64)));
        let mut sorted = [
            Number::from(1.0),
            above,
            Number::from(0_u64),
            below,
            Number::from(-1_i64),
            Number::from(u64::MAX),
        ];
        sorted.sort();
        assert_eq!(
            sorted,
            [
                below,
                Number::from(-1_i64),
                Number::from(0_u64),
                Number::from(u64::MAX),
                above,
                Number::from(1.0),
            ]
        );
    }

    #[test]
    fn wide_debug_prints_the_value_not_the_limbs() {
        assert_eq!(
            format!("{:?}", Number::from(u128::MAX)),
            "Number(PosWide(340282366920938463463374607431768211455))"
        );
        assert_eq!(
            format!("{:?}", Number::from(i128::MIN)),
            "Number(NegWide(-170141183460469231731687303715884105728))"
        );
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn wide_integer_storage_keeps_value_compact() {
        // The two-limb representation keeps Number at 24 bytes and
        // Value at 72.
        assert_eq!(std::mem::size_of::<Number>(), 24);
        assert_eq!(std::mem::size_of::<crate::Value>(), 72);
    }

    #[test]
    fn conversions_are_checked() {
        assert_eq!(Number::from(3_i64).as_i64(), Some(3));
        assert_eq!(Number::from(3_u64).as_u64(), Some(3));
        assert_eq!(Number::from(3.5_f64).as_f64(), Some(3.5));
        assert_eq!(Number::from(3.5_f64).as_i64(), None);
    }
}
