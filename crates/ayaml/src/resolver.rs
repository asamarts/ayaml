//! Scalar resolution for the compatibility profile.
//!
//! Parser events retain raw scalar text, style, tag, anchor, and span; this
//! module turns one scalar into a `Value` without losing that metadata.

use crate::error::{Error, ErrorKind};
use crate::event::ScalarStyle;
use crate::number::Number;
use crate::tag::{Tag, TaggedValue};
use crate::value::Value;

/// Resolve a scalar under the compatibility profile.
pub(crate) fn resolve_scalar(
    text: &str,
    style: ScalarStyle,
    tag: Option<&Tag>,
) -> Result<Value, Error> {
    if let Some(tag) = tag {
        if tag.is_core() {
            return resolve_explicit_core(text, tag.suffix());
        }
        // A custom tag keeps its payload resolved as if untagged:
        // `!Flag true` holds a bool. The non-specific
        // tag `!` makes a scalar a string (YAML 1.2.2 section 6.9.1).
        let payload = if tag.handle() == "!" && tag.suffix().is_empty() {
            Value::String(text.to_owned())
        } else {
            resolve_scalar(text, style, None)?
        };
        return Ok(Value::from(TaggedValue::new(tag.clone(), payload)));
    }

    if style != ScalarStyle::Plain {
        return Ok(Value::String(text.to_owned()));
    }
    Ok(resolve_plain(text).unwrap_or_else(|| Value::String(text.to_owned())))
}

/// Whether an untagged plain scalar with this text resolves to a string.
/// Unlike [`resolve_scalar`], this does not allocate.
pub(crate) fn plain_resolves_to_string(text: &str) -> bool {
    resolve_plain(text).is_none()
}

/// Resolve untagged plain text to a null, bool or number, or `None` when it
/// is a string. Integers are exact up to 128 bits. A wider decimal
/// integer is a float, and a wider `0x`/`0o`/`0b` integer is a string.
fn resolve_plain(text: &str) -> Option<Value> {
    if is_null(text) {
        return Some(Value::Null);
    }
    if let Some(value) = parse_bool(text) {
        return Some(Value::Bool(value));
    }
    if let Some(number) = parse_int(text) {
        return Some(Value::Number(number));
    }
    parse_float(text).map(Value::Number)
}

fn resolve_explicit_core(text: &str, suffix: &str) -> Result<Value, Error> {
    match suffix {
        "str" => Ok(Value::String(text.to_owned())),
        "int" => parse_int_with(text, true)
            .map(Value::Number)
            .ok_or_else(|| resolution_error("invalid !!int scalar")),
        // The core schema reads `!!float 01` as 1.0. Untagged, `01` stays a
        // string.
        "float" => parse_float_with(text, true)
            .map(Value::Number)
            .ok_or_else(|| resolution_error("invalid !!float scalar")),
        "bool" => parse_bool(text)
            .map(Value::Bool)
            .ok_or_else(|| resolution_error("invalid !!bool scalar")),
        "null" if is_null(text) => Ok(Value::Null),
        "null" => Err(resolution_error("invalid !!null scalar")),
        _ => Ok(Value::String(text.to_owned())),
    }
}

fn resolution_error(message: &str) -> Error {
    Error::new(ErrorKind::Resolution, message)
}

fn is_null(text: &str) -> bool {
    matches!(text, "" | "~" | "null" | "Null" | "NULL")
}

fn parse_bool(text: &str) -> Option<bool> {
    match text {
        "true" | "True" | "TRUE" => Some(true),
        "false" | "False" | "FALSE" => Some(false),
        _ => None,
    }
}

fn parse_int(text: &str) -> Option<Number> {
    parse_int_with(text, false)
}

fn parse_int_with(text: &str, allow_leading_zero: bool) -> Option<Number> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits.is_empty() || digits.starts_with(['+', '-']) {
        return None;
    }
    let (radix, digits) = if let Some(rest) = digits.strip_prefix("0b") {
        (2, rest)
    } else if let Some(rest) = digits.strip_prefix("0o") {
        (8, rest)
    } else if let Some(rest) = digits.strip_prefix("0x") {
        (16, rest)
    } else {
        (10, digits)
    };
    if digits.is_empty() || digits.starts_with(['+', '-']) {
        return None;
    }
    if !allow_leading_zero && radix == 10 && digits.len() > 1 && digits.starts_with('0') {
        // Preserve the compatibility profile: 0123 is a string, not legacy octal.
        return None;
    }
    let magnitude = u128::from_str_radix(digits, radix).ok()?;
    if negative {
        return 0_i128
            .checked_sub_unsigned(magnitude)
            .map(Number::from_i128);
    }
    Some(Number::from_u128(magnitude))
}

fn decimal_integer_like(text: &str) -> bool {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn parse_float(text: &str) -> Option<Number> {
    parse_float_with(text, false)
}

/// The finite float that a core-schema number spelling stands for, a leading zero allowed, as in
/// `01` or `01.5`, which resolve to strings. Other text, such as `inf`, gives `None`, and so does a
/// spelling too large for a float, such as `1e999`.
pub(crate) fn float_from_text(text: &str) -> Option<f64> {
    parse_float_with(text, true).and_then(|number| number.as_f64())
}

fn parse_float_with(text: &str, allow_leading_zero: bool) -> Option<Number> {
    match text {
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => {
            return Some(Number::from_f64(f64::INFINITY));
        }
        "-.inf" | "-.Inf" | "-.INF" => {
            return Some(Number::from_f64(f64::NEG_INFINITY));
        }
        ".nan" | ".NaN" | ".NAN" => return Some(Number::from_f64(f64::NAN)),
        _ => {}
    }
    if decimal_integer_like(text) {
        let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
        if !allow_leading_zero && digits.len() > 1 && digits.starts_with('0') {
            return None;
        }
    } else if !text.contains(['.', 'e', 'E']) {
        return None;
    }
    let value = text.parse::<f64>().ok()?;
    value.is_finite().then(|| Number::from_f64(value))
}

#[cfg(test)]
mod tests {
    use super::resolve_scalar;
    use crate::event::ScalarStyle;
    use crate::tag::Tag;
    use crate::value::Value;

    fn plain(text: &str) -> Value {
        resolve_scalar(text, ScalarStyle::Plain, None).expect("resolve")
    }

    #[test]
    fn compatibility_scalars() {
        assert_eq!(plain("yes"), Value::from("yes"));
        assert_eq!(plain("0123"), Value::from("0123"));
        assert_eq!(plain("2026-10-19"), Value::from("2026-10-19"));
        assert_eq!(plain("0b11").as_u64(), Some(3));
        assert_eq!(plain("0x1f").as_u64(), Some(31));
        assert_eq!(plain("0o17").as_u64(), Some(15));
        assert_eq!(plain("-0b11").as_i64(), Some(-3));
        assert_eq!(plain("-0x1f").as_i64(), Some(-31));
        assert_eq!(plain("--9"), Value::from("--9"));
        assert_eq!(plain("++9"), Value::from("++9"));
        assert_eq!(plain("0x+3"), Value::from("0x+3"));
        assert_eq!(plain("0x-3"), Value::from("0x-3"));
        assert!(
            plain("99999999999999999999999999999999999999999999")
                .as_f64()
                .is_some_and(|value| value > 1e43)
        );
        assert_eq!(plain("1e400"), Value::from("1e400"));
        let wide = |text| plain(text).as_number().copied().expect("number");
        assert_eq!(
            wide("18446744073709551616").as_u128(),
            Some(u128::from(u64::MAX) + 1)
        );
        assert_eq!(
            wide("-9223372036854775809").as_i128(),
            Some(i128::from(i64::MIN) - 1)
        );
        assert_eq!(
            wide("340282366920938463463374607431768211455").as_u128(),
            Some(u128::MAX)
        );
        assert_eq!(
            wide("-170141183460469231731687303715884105728").as_i128(),
            Some(i128::MIN)
        );
        assert!(wide("-170141183460469231731687303715884105729").is_f64());
        assert!(
            plain("340282366920938463463374607431768211456")
                .as_f64()
                .is_some_and(f64::is_finite)
        );
        assert_eq!(wide("0x10000000000000000").as_u128(), Some(1 << 64));
        assert_eq!(wide("-0x10000000000000000").as_i128(), Some(-(1 << 64)));
        // Past 128 bits a radix integer is a string.
        assert_eq!(
            plain("0x100000000000000000000000000000000"),
            Value::from("0x100000000000000000000000000000000")
        );
        assert_eq!(
            plain("-0x80000000000000000000000000000001"),
            Value::from("-0x80000000000000000000000000000001")
        );
        assert_eq!(plain("-1e400"), Value::from("-1e400"));
        assert_eq!(
            plain("012345678901234567890123456789012345678901"),
            Value::from("012345678901234567890123456789012345678901")
        );
        assert_eq!(plain("1e3").as_f64(), Some(1000.0));
        assert!(plain("~").is_null());
        assert_eq!(plain(".inf").as_f64(), Some(f64::INFINITY));
        assert!(plain(".nan").as_f64().is_some_and(f64::is_nan));
        assert!(plain(".NAN").as_f64().is_some_and(f64::is_nan));
        assert_eq!(plain(".INF").as_f64(), Some(f64::INFINITY));
        assert_eq!(plain("+.nan"), Value::from("+.nan"));
        assert_eq!(plain("-.nan"), Value::from("-.nan"));
        assert_eq!(plain(".nAn"), Value::from(".nAn"));
        assert_eq!(plain(".iNf"), Value::from(".iNf"));
    }

    #[test]
    fn quoted_scalars_stay_strings() {
        assert_eq!(
            resolve_scalar("123", ScalarStyle::DoubleQuoted, None).unwrap(),
            Value::from("123")
        );
        assert_eq!(
            resolve_scalar("true", ScalarStyle::SingleQuoted, None).unwrap(),
            Value::from("true")
        );
    }

    #[test]
    fn core_tags_override_style() {
        assert_eq!(
            resolve_scalar("123", ScalarStyle::DoubleQuoted, Some(&Tag::core("str"))).unwrap(),
            Value::from("123")
        );
        assert_eq!(
            resolve_scalar("0123", ScalarStyle::Plain, Some(&Tag::core("int")))
                .unwrap()
                .as_u64(),
            Some(123)
        );
        assert_eq!(
            resolve_scalar("x", ScalarStyle::Plain, Some(&Tag::core("int")))
                .unwrap_err()
                .kind(),
            crate::error::ErrorKind::Resolution
        );
    }

    #[test]
    fn custom_tags_remain_tagged() {
        let value =
            resolve_scalar("hello", ScalarStyle::Plain, Some(&Tag::local("Custom"))).unwrap();
        assert!(value.is_tagged());
        assert_eq!(value.as_tagged().unwrap().tag.to_string(), "!Custom");
        assert_eq!(value.as_tagged().unwrap().value.as_str(), Some("hello"));
    }

    #[test]
    fn booleans_and_numbers() {
        assert_eq!(plain("true"), Value::from(true));
        assert_eq!(plain("False"), Value::from(false));
        assert_eq!(plain("-42").as_i64(), Some(-42));
        assert_eq!(plain("+42").as_i64(), Some(42));
        assert_eq!(plain("1.5").as_f64(), Some(1.5));
    }
}
