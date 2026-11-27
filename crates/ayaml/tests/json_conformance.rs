//! RFC 8259 JSON-superset conformance test oracle (AY-131).
//!
//! YAML 1.2.2 is specified as a strict superset of JSON (RFC 8259). Comparing
//! `ayaml` directly against `serde_json` on a deterministic corpus, targeted
//! edge cases, and generated JSON values provides an independent oracle in CI
//! for flow collections, Unicode and surrogate-pair escapes, numeric syntax,
//! whitespace, and quoting.
//!
//! Intentional YAML 1.2.2 versus RFC 8259 boundary cases (duplicate keys,
//! 128-bit integer width, UTF-16 surrogate-pair escapes, overflowing floats,
//! leading-zero strings, trailing commas, leading BOM, unescaped tab in
//! double-quoted strings, and `-0`) are explicitly documented and asserted in
//! `documented_yaml_1_2_vs_rfc8259_boundary_cases`.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ayaml::{ErrorCode, ErrorKind, Number, Value};
use proptest::prelude::*;

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusCase {
    name: String,
    json: String,
}

fn corpus_path() -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    if root.join(".alint.yml").is_file() {
        let path = root.join("tests/data/json-conformance.json");
        assert!(
            path.is_file(),
            "JSON conformance corpus missing at {}",
            path.display()
        );
        return Some(path);
    }
    assert!(
        manifest.join("Cargo.toml.orig").is_file(),
        "{} is neither in the repository nor an unpacked package",
        manifest.display()
    );
    None
}

/// Compare two `serde_json::Value` trees. When `max_f64_ulps == 0`, floats must match bit for
/// bit; a small ULP bound accounts for `serde_json`'s default float parser (without its
/// `float_roundtrip` feature), which can drift by a few ULPs at large exponents compared to
/// Rust's exact `str::parse::<f64>()` used by `ayaml`.
fn assert_json_values_match(
    actual: &serde_json::Value,
    expected: &serde_json::Value,
    label: &str,
    input: &str,
    max_f64_ulps: u64,
) {
    match (actual, expected) {
        (serde_json::Value::Null, serde_json::Value::Null) => {}
        (serde_json::Value::Bool(a), serde_json::Value::Bool(b)) => {
            assert_eq!(a, b, "{label}: bool mismatch\ninput: {input:?}");
        }
        (serde_json::Value::String(a), serde_json::Value::String(b)) => {
            assert_eq!(a, b, "{label}: string mismatch\ninput: {input:?}");
        }
        (serde_json::Value::Number(a), serde_json::Value::Number(b)) => {
            if a == b {
                return;
            }
            // Allow exact numeric equivalence across integer vs. exponent-written float
            // spellings (e.g. `1e10` or `0e0`) when `serde_json` stores f64 and both equal.
            let exact = match (a.as_i64(), b.as_i64(), a.as_u64(), b.as_u64()) {
                (Some(x), Some(y), _, _) => x == y,
                (_, _, Some(x), Some(y)) => x == y,
                _ => match (a.as_f64(), b.as_f64()) {
                    (Some(x), Some(y)) => {
                        x.is_sign_positive() == y.is_sign_positive()
                            && x.to_bits().abs_diff(y.to_bits()) <= max_f64_ulps
                    }
                    _ => false,
                },
            };
            assert!(
                exact,
                "{label}: number mismatch: actual={a:?} expected={b:?}\ninput: {input:?}"
            );
        }
        (serde_json::Value::Array(a), serde_json::Value::Array(b)) => {
            assert_eq!(
                a.len(),
                b.len(),
                "{label}: array len mismatch\nactual: {a:?}\nexpected: {b:?}\ninput: {input:?}"
            );
            for (idx, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                assert_json_values_match(x, y, &format!("{label}[{idx}]"), input, max_f64_ulps);
            }
        }
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
            assert_eq!(
                a.len(),
                b.len(),
                "{label}: object len mismatch\nactual: {a:?}\nexpected: {b:?}\ninput: {input:?}"
            );
            for (key, expected_val) in b {
                let actual_val = a.get(key).unwrap_or_else(|| {
                    panic!("{label}: missing key {key:?}\nactual: {a:?}\ninput: {input:?}")
                });
                assert_json_values_match(
                    actual_val,
                    expected_val,
                    &format!("{label}.{key:?}"),
                    input,
                    max_f64_ulps,
                );
            }
        }
        _ => panic!(
            "{label}: variant mismatch: actual={actual:?} expected={expected:?}\ninput: {input:?}"
        ),
    }
}

/// Assert that a valid RFC 8259 JSON input parses identically through `serde_json::from_str`,
/// `ayaml::from_str`, `ayaml::from_str_borrowed`, `ayaml::parse_value`, `ayaml::Deserializer`,
/// emitter round-trip, and (when `libyaml-compat` is enabled) `ayaml::compat::from_str`.
fn assert_valid_json_matches_oracle(label: &str, input: &str) {
    let oracle: serde_json::Value = serde_json::from_str(input)
        .unwrap_or_else(|error| panic!("{label}: serde_json oracle failed on {input:?}: {error}"));

    let from_str: serde_json::Value = ayaml::from_str(input)
        .unwrap_or_else(|error| panic!("{label}: ayaml::from_str failed on {input:?}: {error}"));
    assert_json_values_match(&from_str, &oracle, &format!("{label}::from_str"), input, 0);

    let borrowed: serde_json::Value = ayaml::from_str_borrowed(input).unwrap_or_else(|error| {
        panic!("{label}: ayaml::from_str_borrowed failed on {input:?}: {error}")
    });
    assert_json_values_match(
        &borrowed,
        &oracle,
        &format!("{label}::from_str_borrowed"),
        input,
        0,
    );

    let from_slice: serde_json::Value = ayaml::from_slice(input.as_bytes())
        .unwrap_or_else(|error| panic!("{label}: ayaml::from_slice failed on {input:?}: {error}"));
    assert_json_values_match(
        &from_slice,
        &oracle,
        &format!("{label}::from_slice"),
        input,
        0,
    );

    let parsed: Value = ayaml::parse_value(input)
        .unwrap_or_else(|error| panic!("{label}: ayaml::parse_value failed on {input:?}: {error}"));
    let parsed_docs = ayaml::parse_documents(input).unwrap_or_else(|error| {
        panic!("{label}: ayaml::parse_documents failed on {input:?}: {error}")
    });
    assert_eq!(
        parsed_docs.len(),
        1,
        "{label}: single JSON value must produce 1 document"
    );
    assert_eq!(parsed_docs[0], parsed, "{label}: parse_documents vs value");
    let parsed_as_json = serde_json::to_value(&parsed)
        .unwrap_or_else(|error| panic!("{label}: Value -> serde_json::Value failed: {error}"));
    assert_json_values_match(
        &parsed_as_json,
        &oracle,
        &format!("{label}::parse_value"),
        input,
        0,
    );

    let tape = ayaml::parse_tape(input)
        .unwrap_or_else(|error| panic!("{label}: ayaml::parse_tape failed on {input:?}: {error}"));
    assert_eq!(tape.to_value(), parsed, "{label}: tape.to_value vs parsed");
    let tape_json = <serde_json::Value as serde::Deserialize>::deserialize(&tape)
        .unwrap_or_else(|error| panic!("{label}: serde_json::Value::deserialize(&tape): {error}"));
    assert_json_values_match(&tape_json, &oracle, &format!("{label}::tape_de"), input, 0);
    let mut buf = ayaml::ParserBuf::new();
    tape.recycle_into(&mut buf);
    let buf_tape = buf
        .parse_tape(input)
        .unwrap_or_else(|error| panic!("{label}: ParserBuf::parse_tape failed: {error}"));
    assert_eq!(buf_tape.to_value(), parsed, "{label}: buf_tape.to_value");
    buf_tape.recycle_into(&mut buf);
    let buf_from_str: serde_json::Value = buf
        .from_str(input)
        .unwrap_or_else(|error| panic!("{label}: ParserBuf::from_str failed: {error}"));
    assert_json_values_match(
        &buf_from_str,
        &oracle,
        &format!("{label}::buf_from_str"),
        input,
        0,
    );

    let mut cursor = ayaml::Deserializer::from_str(input)
        .unwrap_or_else(|error| panic!("{label}: Deserializer failed on {input:?}: {error}"));
    let first = cursor
        .next_borrowed::<serde_json::Value>()
        .unwrap_or_else(|error| panic!("{label}: cursor.next_borrowed failed: {error}"))
        .unwrap_or_else(|| panic!("{label}: cursor yielded no document for {input:?}"));
    assert_json_values_match(&first, &oracle, &format!("{label}::cursor"), input, 0);
    assert!(
        cursor
            .next_borrowed::<serde_json::Value>()
            .expect("second cursor poll")
            .is_none(),
        "{label}: single JSON value yielded more than one document"
    );

    let emitted = ayaml::to_string_value(&parsed)
        .unwrap_or_else(|error| panic!("{label}: to_string_value failed: {error}"));
    let round_tripped: serde_json::Value = ayaml::from_str(&emitted)
        .unwrap_or_else(|error| panic!("{label}: reparse emitted {emitted:?} failed: {error}"));
    assert_json_values_match(
        &round_tripped,
        &oracle,
        &format!("{label}::round_trip"),
        input,
        0,
    );

    #[cfg(feature = "libyaml-compat")]
    {
        let compat: serde_json::Value = ayaml::compat::from_str(input).unwrap_or_else(|error| {
            panic!("{label}: compat::from_str failed on {input:?}: {error}")
        });
        assert_json_values_match(&compat, &oracle, &format!("{label}::compat"), input, 0);
    }
}

#[test]
fn rfc8259_corpus_matches_serde_json() {
    if let Some(path) = corpus_path() {
        let text = std::fs::read_to_string(&path).expect("read json-conformance.json");
        let cases: Vec<CorpusCase> =
            serde_json::from_str(&text).expect("parse json-conformance.json");
        assert!(
            cases.len() >= 35,
            "expected comprehensive JSON corpus, found {}",
            cases.len()
        );
        let mut seen = HashSet::with_capacity(cases.len());
        for case in &cases {
            assert!(
                seen.insert(case.name.as_str()),
                "duplicate case name in json-conformance.json: {:?}",
                case.name
            );
            assert_valid_json_matches_oracle(&case.name, &case.json);
        }
    }

    // Inline RFC 8259 whitespace, CRLF, tab indentation, and raw U+007F DEL vectors
    // (kept in Rust literals so repository text files remain LF-only with no trailing spaces).
    let whitespace_cases: &[(&str, &str)] = &[
        ("leading_trailing_spaces", "   {\"a\": 1}   "),
        (
            "crlf_pretty_object",
            "{\r\n  \"a\": [\r\n    1,\r\n    2\r\n  ]\r\n}\r\n",
        ),
        ("cr_only_whitespace", "{\r\"a\":\r[\r1,\r2\r]\r}"),
        (
            "tab_indented_flow_object",
            "{\n\t\"a\":\t[\n\t\t1,\n\t\t\"two\"\n\t],\n\t\"b\":\t{\n\t\t\"c\":\ttrue\n\t}\n}",
        ),
        ("tabs_around_root_scalar", "\t\r\n 12345 \t\r\n"),
        ("tabs_inside_empty_collections", "[\t\r\n ]"),
        ("tabs_inside_empty_object", "{\t\r\n }"),
        // Raw U+007F (DEL) is unescaped-valid in RFC 8259 §7 (%x23-5B / %x5D-10FFFF) and
        // YAML 1.2.2 §5.1 ([2] nb-json).
        ("raw_del_in_string", "{\"del\":\"a\u{7f}b\"}"),
    ];

    for &(name, input) in whitespace_cases {
        assert_valid_json_matches_oracle(name, input);
    }
}

#[test]
fn rfc8259_unicode_escapes_and_surrogate_boundaries() {
    let valid_unicode_cases = [
        ("lower_hex", r#""\u0061\u00e9\u20ac""#),
        ("upper_hex", r#""\u0041\u00C9\u20AC""#),
        ("nul_inside_string", r#""before\u0000after""#),
        (
            "all_c0_controls_escaped",
            r#""\u0000\u0001\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u001b\u001f""#,
        ),
        ("bmp_boundaries", r#""\u0000\ud7ff\ue000\ufffd\uffff""#),
        ("escaped_solidus_and_quotes", r#""\/\"\\""#),
        ("raw_ls_ps_bom_in_string", "\"\u{2028}\u{2029}\u{feff}\""),
        (
            "raw_non_bmp_utf8_and_escapes",
            "{\"k𝄞\":\"a\\n🚀\\t\\\"\\\\\\/\\u0000🇺🇸\"}",
        ),
    ];

    for (name, input) in valid_unicode_cases {
        assert_valid_json_matches_oracle(name, input);
    }

    // Documented YAML 1.2.2 §5.7 vs. RFC 8259 §7 boundary:
    // RFC 8259 encodes non-BMP escapes using UTF-16 surrogate pairs (`\uD834\uDD1E`).
    // Strict YAML 1.2.2 §5.7 (and `libyaml`) defines `\uXXXX` strictly as a Unicode scalar
    // value and uses 32-bit `\UXXXXXXXX` (or raw UTF-8) for supplementary code points, so
    // `ayaml` rejects surrogate code units with `ErrorCode::InvalidEscape` and decodes the
    // corresponding `\UXXXXXXXX` escape to the exact scalar that `serde_json` produces for the
    // surrogate pair.
    for (name, json_surrogate_pair, yaml_32bit_escape, expected_scalar) in [
        ("g_clef", r#""\uD834\uDD1E""#, r#""\U0001D11E""#, "𝄞"),
        (
            "min_non_bmp",
            r#""\uD800\uDC00""#,
            r#""\U00010000""#,
            "\u{10000}",
        ),
        (
            "max_unicode",
            r#""\uDBFF\uDFFF""#,
            r#""\U0010FFFF""#,
            "\u{10FFFF}",
        ),
        ("rocket_lower", r#""\ud83d\ude80""#, r#""\U0001F680""#, "🚀"),
        (
            "flag_pair",
            r#""\uD83C\uDDFA\uD83C\uDDF8""#,
            r#""\U0001F1FA\U0001F1F8""#,
            "🇺🇸",
        ),
    ] {
        let json_decoded: String = serde_json::from_str(json_surrogate_pair)
            .unwrap_or_else(|error| panic!("{name}: serde_json failed: {error}"));
        assert_eq!(json_decoded, expected_scalar, "{name}: serde_json scalar");

        let err = ayaml::parse_value(json_surrogate_pair)
            .expect_err("YAML 1.2.2 rejects UTF-16 surrogate escapes");
        assert_eq!(err.kind(), ErrorKind::Scanner, "{name}");
        assert_eq!(err.code(), ErrorCode::InvalidEscape, "{name}");

        #[cfg(feature = "libyaml-compat")]
        {
            let compat_err = ayaml::compat::from_str::<String>(json_surrogate_pair)
                .expect_err("compat matches libyaml on UTF-16 surrogate escapes");
            assert_eq!(compat_err.code(), ErrorCode::InvalidEscape, "{name}");
        }

        let yaml_decoded: String = ayaml::from_str(yaml_32bit_escape)
            .unwrap_or_else(|error| panic!("{name}: YAML \\U escape failed: {error}"));
        assert_eq!(
            yaml_decoded, json_decoded,
            "{name}: YAML \\U escape must match JSON surrogate pair"
        );
    }

    // Invalid / unpaired UTF-16 surrogates and malformed \u escapes must be rejected by BOTH
    // `serde_json` and `ayaml`.
    let invalid_escape_cases = [
        ("lone_high_surrogate", r#""\uD800""#),
        ("lone_high_surrogate_max", r#""\uDBFF""#),
        ("lone_low_surrogate", r#""\uDC00""#),
        ("lone_low_surrogate_max", r#""\uDFFF""#),
        ("reversed_surrogate_pair", r#""\uDC00\uD800""#),
        ("high_surrogate_followed_by_ascii", r#""\uD800a""#),
        ("high_surrogate_followed_by_bmp", r#""\uD800\u0041""#),
        ("high_surrogate_followed_by_short_escape", r#""\uD800\n""#),
        ("double_high_surrogate", r#""\uD800\uD800""#),
        ("truncated_u_escape_0", r#""\u""#),
        ("truncated_u_escape_1", r#""\u1""#),
        ("truncated_u_escape_3", r#""\u123""#),
        ("non_hex_u_escape", r#""\u123g""#),
        ("negative_u_escape", r#""\u-001""#),
        ("high_surrogate_truncated_low", r#""\uD83D\uDE8""#),
    ];

    for (name, input) in invalid_escape_cases {
        assert!(
            serde_json::from_str::<serde_json::Value>(input).is_err(),
            "{name}: expected serde_json to reject {input:?}"
        );
        assert!(
            ayaml::parse_value(input).is_err(),
            "{name}: expected ayaml::parse_value to reject {input:?}"
        );
        assert!(
            ayaml::from_str::<serde_json::Value>(input).is_err(),
            "{name}: expected ayaml::from_str to reject {input:?}"
        );
    }
}

#[test]
fn rfc8259_extreme_numbers_and_deep_flow_match_serde_json() {
    let number_cases = [
        ("zero", "0"),
        ("neg_zero_float", "-0.0"),
        ("i64_max", "9223372036854775807"),
        ("i64_min", "-9223372036854775808"),
        ("u64_max", "18446744073709551615"),
        ("f64_max", "1.7976931348623157e+308"),
        ("f64_neg_max", "-1.7976931348623157e+308"),
        ("f64_min_positive", "2.2250738585072014e-308"),
        ("f64_min_subnormal", "5e-324"),
        ("underflow_pos", "1e-400"),
        ("underflow_neg", "-1e-400"),
        ("tiny_mantissa_large_exp", "0.00000000000000000001e+25"),
        (
            "large_mantissa_neg_exp",
            "123456789012345678901234567890e-20",
        ),
        ("zero_exp_pos", "0e+999"),
        ("zero_exp_neg", "0e-999"),
    ];
    for (name, input) in number_cases {
        assert_valid_json_matches_oracle(name, input);
    }

    // Deep nested flow arrays and objects (depth 64, safely inside MAX_DEPTH = 128).
    let depth = 64;
    let deep_arrays = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
    assert_valid_json_matches_oracle("deep_arrays_64", &deep_arrays);

    let mut deep_objects = String::new();
    for i in 0..depth {
        let _ = write!(deep_objects, "{{\"k{i}\":");
    }
    deep_objects.push_str("true");
    deep_objects.push_str(&"}".repeat(depth));
    assert_valid_json_matches_oracle("deep_objects_64", &deep_objects);

    let mut deep_mixed = String::new();
    for i in 0..(depth / 2) {
        let _ = write!(deep_mixed, "[{{\"m{i}\":");
    }
    deep_mixed.push_str("null");
    deep_mixed.push_str(&"}]".repeat(depth / 2));
    assert_valid_json_matches_oracle("deep_mixed_64", &deep_mixed);

    // Wide flat flow array (256 elements) and wide flat flow object (128 keys) with zero spaces.
    let wide_array = format!(
        "[{}]",
        (0..256)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    assert_valid_json_matches_oracle("wide_array_256", &wide_array);

    let wide_object = format!(
        "{{{}}}",
        (0..128)
            .map(|i| format!("\"k{i}\":{i}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    assert_valid_json_matches_oracle("wide_object_128", &wide_object);
}

/// Explicitly document and assert every intentional boundary where YAML 1.2.2 (or `ayaml`'s
/// default profile) diverges from RFC 8259 / `serde_json`.
#[test]
fn documented_yaml_1_2_vs_rfc8259_boundary_cases() {
    // 1. Duplicate object keys (top-level and nested): RFC 8259 says keys "SHOULD be unique" and
    //    `serde_json` accepts duplicate keys (last wins). YAML 1.2.2 §3.2.1.1 requires unique
    //    mapping keys, so `ayaml` rejects duplicate keys with `ErrorKind::DuplicateKey`.
    for dup in [r#"{"a": 1, "a": 2}"#, r#"{"outer": {"k": 1, "k": 2}}"#] {
        assert!(serde_json::from_str::<serde_json::Value>(dup).is_ok());
        let err = ayaml::parse_value(dup).expect_err("ayaml rejects duplicate keys");
        assert_eq!(err.kind(), ErrorKind::DuplicateKey);
        assert_eq!(err.code(), ErrorCode::DuplicateKey);
        let err_typed =
            ayaml::from_str::<serde_json::Value>(dup).expect_err("typed rejects duplicate keys");
        assert_eq!(err_typed.code(), ErrorCode::DuplicateKey);
    }

    // 2. Wide integers (> 64 bits, <= 128 bits): `serde_json::Value` converts integers outside
    //    i64/u64 to lossy f64, whereas `ayaml` preserves exact 128-bit integers in `Value::Number`
    //    and typed `u128`/`i128` reads.
    let u64_plus_one = "18446744073709551616";
    assert!(
        serde_json::from_str::<serde_json::Value>(u64_plus_one)
            .expect("serde_json")
            .is_f64()
    );
    assert_eq!(
        ayaml::parse_value(u64_plus_one).expect("ayaml u128"),
        Value::Number(Number::from_u128(u128::from(u64::MAX) + 1))
    );
    assert_eq!(
        ayaml::from_str::<u128>(u64_plus_one).expect("typed u128"),
        u128::from(u64::MAX) + 1
    );

    let i64_minus_one = "-9223372036854775809";
    assert!(
        serde_json::from_str::<serde_json::Value>(i64_minus_one)
            .expect("serde_json")
            .is_f64()
    );
    assert_eq!(
        ayaml::parse_value(i64_minus_one).expect("ayaml i128"),
        Value::Number(Number::from_i128(i128::from(i64::MIN) - 1))
    );
    assert_eq!(
        ayaml::from_str::<i128>(i64_minus_one).expect("typed i128"),
        i128::from(i64::MIN) - 1
    );

    let u128_max = "340282366920938463463374607431768211455";
    assert_eq!(
        ayaml::parse_value(u128_max)
            .expect("u128::MAX")
            .as_number()
            .and_then(Number::as_u128),
        Some(u128::MAX)
    );
    let i128_min = "-170141183460469231731687303715884105728";
    assert_eq!(
        ayaml::parse_value(i128_min)
            .expect("i128::MIN")
            .as_number()
            .and_then(Number::as_i128),
        Some(i128::MIN)
    );

    // Past 128 bits, both `serde_json::Value` and `ayaml` resolve decimal integers to finite f64.
    let past_u128 = "340282366920938463463374607431768211456";
    assert_eq!(
        ayaml::parse_value(past_u128).expect("past u128").as_f64(),
        serde_json::from_str::<serde_json::Value>(past_u128)
            .expect("serde_json past u128")
            .as_f64()
    );

    // 3. Negative zero integer `-0`: `serde_json` parses `-0` as `-0.0` (f64) to preserve sign,
    //    whereas YAML 1.2.2 Core Schema matches `[-+]?[0-9]+` as integer `0` (signed integer
    //    zero has no negative sign bit); `-0.0` stays negative float `-0.0` in both.
    let json_neg_zero: serde_json::Value = serde_json::from_str("-0").expect("serde_json -0");
    assert_eq!(json_neg_zero.as_f64(), Some(-0.0));
    assert!(json_neg_zero.as_f64().unwrap().is_sign_negative());
    assert_eq!(
        ayaml::parse_value("-0").expect("ayaml -0"),
        Value::Number(Number::from_i64(0))
    );
    let neg_zero_float = ayaml::parse_value("-0.0")
        .expect("ayaml -0.0")
        .as_f64()
        .expect("f64");
    assert!(neg_zero_float == 0.0 && neg_zero_float.is_sign_negative());

    // 4. Overflowing floats (`1e400`, `-1e400`): `serde_json` rejects non-finite float overflow;
    //    `ayaml`'s Core Schema resolver keeps non-finite decimal spellings as plain strings.
    for overflow in ["1e400", "-1e400"] {
        assert!(serde_json::from_str::<serde_json::Value>(overflow).is_err());
        assert_eq!(
            ayaml::parse_value(overflow).expect("ayaml keeps overflow as string"),
            Value::from(overflow)
        );
    }

    // 5. Leading-zero numbers (`01`, `-01`, `00`): invalid in RFC 8259; `ayaml`'s default profile
    //    keeps multi-digit leading-zero decimals as strings (never legacy octal).
    for leading_zero in ["01", "-01", "00", "0123"] {
        assert!(serde_json::from_str::<serde_json::Value>(leading_zero).is_err());
        assert_eq!(
            ayaml::parse_value(leading_zero).expect("ayaml keeps leading zero as string"),
            Value::from(leading_zero)
        );
    }

    // 6. Trailing commas in flow collections: invalid in RFC 8259, valid in YAML 1.2.2 §7.4.
    assert!(serde_json::from_str::<serde_json::Value>("[1, 2,]").is_err());
    assert_eq!(
        ayaml::from_str::<serde_json::Value>("[1, 2,]").expect("YAML trailing comma seq"),
        serde_json::json!([1, 2])
    );
    assert!(serde_json::from_str::<serde_json::Value>(r#"{"a": 1,}"#).is_err());
    assert_eq!(
        ayaml::from_str::<serde_json::Value>(r#"{"a": 1,}"#).expect("YAML trailing comma map"),
        serde_json::json!({"a": 1})
    );

    // 7. Leading UTF-8 BOM: `serde_json` rejects it (RFC 8259 §8.1), YAML 1.2.2 §5.2 allows it.
    let with_bom = "\u{feff}{\"a\":1}";
    assert!(serde_json::from_str::<serde_json::Value>(with_bom).is_err());
    assert_eq!(
        ayaml::from_str::<serde_json::Value>(with_bom).expect("YAML BOM"),
        serde_json::json!({"a": 1})
    );

    // 8. Unescaped horizontal tab (U+0009) inside a double-quoted string: forbidden by RFC 8259
    //    §7, permitted by YAML 1.2.2 §7.3.1 (`nb-double-char`).
    let raw_tab = "\"a\tb\"";
    assert!(serde_json::from_str::<serde_json::Value>(raw_tab).is_err());
    assert_eq!(
        ayaml::parse_value(raw_tab).expect("YAML double-quoted raw tab"),
        Value::from("a\tb")
    );

    // 9. Unescaped line break inside double-quoted string: forbidden by RFC 8259 §7; YAML 1.2.2
    //    §7.3.1 folds a single line break into a space.
    let raw_nl = "\"a\nb\"";
    assert!(serde_json::from_str::<serde_json::Value>(raw_nl).is_err());
    assert_eq!(
        ayaml::parse_value(raw_nl).expect("YAML double-quoted line fold"),
        Value::from("a b")
    );

    // 10. Implicit empty flow-mapping value (`{"a":}`) and empty/whitespace-only inputs:
    //     invalid in RFC 8259; valid in YAML 1.2.2 (resolving to `null` / zero documents).
    assert!(serde_json::from_str::<serde_json::Value>(r#"{"a":}"#).is_err());
    assert_eq!(
        ayaml::from_str::<serde_json::Value>(r#"{"a":}"#).expect("YAML implicit null value"),
        serde_json::json!({"a": null})
    );
    for empty in ["", "   \n\t "] {
        assert!(serde_json::from_str::<serde_json::Value>(empty).is_err());
        assert_eq!(ayaml::parse_value(empty).expect("empty YAML"), Value::Null);
        assert_eq!(
            ayaml::parse_documents(empty).expect("empty stream"),
            Vec::<Value>::new()
        );
    }

    // 11. YAML 1.2.2 extensions rejected by RFC 8259 JSON: leading `+`, `.5`, `1.`, `0x10`,
    //     `.inf`/`-.inf`/`.nan`, single quotes, unquoted keys, and `#` comments.
    for (input, expected) in [
        ("+1", Value::from(1_u64)),
        (".5", Value::Number(Number::from_f64(0.5))),
        ("1.", Value::Number(Number::from_f64(1.0))),
        ("0x10", Value::from(16_u64)),
        (".inf", Value::Number(Number::from_f64(f64::INFINITY))),
        ("-.inf", Value::Number(Number::from_f64(f64::NEG_INFINITY))),
        ("'single'", Value::from("single")),
        ("{a: 1} # comment\n", {
            let mut m = ayaml::Mapping::new();
            m.insert(Value::from("a"), Value::from(1_u64));
            Value::Mapping(m)
        }),
    ] {
        assert!(
            serde_json::from_str::<serde_json::Value>(input).is_err(),
            "serde_json unexpectedly accepted {input:?}"
        );
        assert_eq!(
            ayaml::parse_value(input).expect("YAML extension"),
            expected,
            "YAML extension mismatch for {input:?}"
        );
    }
    assert!(serde_json::from_str::<serde_json::Value>(".nan").is_err());
    assert!(
        ayaml::parse_value(".nan")
            .expect("YAML .nan")
            .as_f64()
            .is_some_and(f64::is_nan)
    );
}

#[test]
fn invalid_in_both_rfc8259_and_yaml_1_2() {
    let invalid_both = [
        ("unterminated_string", r#""unterminated"#),
        ("unterminated_array", "[1, 2"),
        ("unterminated_object", r#"{"a": 1"#),
        ("mismatched_array_brace", "[1, 2}"),
        ("mismatched_object_bracket", r#"{"a": 1]"#),
        ("extra_closing_bracket", "[1]]"),
        ("extra_closing_brace", r#"{"a":1}}"#),
        ("trailing_token_after_array", "[1] [2]"),
        ("trailing_token_after_object", r#"{"a":1} 2"#),
        ("unknown_escape", r#""\q""#),
        ("trailing_backslash", r#""abc\"#),
        ("leading_comma_array", "[,1]"),
        ("double_comma_array", "[1,,2]"),
        ("missing_colon_in_object_value_syntax", r#"{"a" 1}"#),
        ("unescaped_nul_in_string", "\"a\u{0000}b\""),
        ("unescaped_bs_in_string", "\"a\u{0008}b\""),
        ("unescaped_vt_in_string", "\"a\u{000b}b\""),
        ("unescaped_ff_in_string", "\"a\u{000c}b\""),
        ("unescaped_esc_in_string", "\"a\u{001b}b\""),
        ("unescaped_us_in_string", "\"a\u{001f}b\""),
    ];

    for (name, input) in invalid_both {
        assert!(
            serde_json::from_str::<serde_json::Value>(input).is_err(),
            "{name}: expected serde_json to reject {input:?}"
        );
        assert!(
            ayaml::parse_value(input).is_err(),
            "{name}: expected ayaml::parse_value to reject {input:?}"
        );
        assert!(
            ayaml::from_str::<serde_json::Value>(input).is_err(),
            "{name}: expected ayaml::from_str to reject {input:?}"
        );
    }

    // Invalid UTF-8 bytes must be rejected by both `serde_json::from_slice` and `ayaml::from_slice`.
    let invalid_utf8 = b"\"\xff\"";
    assert!(serde_json::from_slice::<serde_json::Value>(invalid_utf8).is_err());
    let utf8_err = ayaml::from_slice::<serde_json::Value>(invalid_utf8)
        .expect_err("ayaml::from_slice rejects invalid UTF-8");
    assert_eq!(utf8_err.kind(), ErrorKind::InvalidEncoding);
    assert_eq!(utf8_err.code(), ErrorCode::InvalidUtf8);
}

fn json_string_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[a-zA-Z0-9 _:/.,-]{0,16}",
        2 => prop::sample::select(vec![
            "",
            "null",
            "true",
            "false",
            "~",
            "0x10",
            "0123",
            ".inf",
            ".nan",
            "---",
            "...",
            "<<",
            "a: b",
            "\"quoted\"\\slash/",
            "line1\nline2\r\ntab\tend",
            "nul\u{0000}ctrl\u{001f}del\u{007f}",
            "café 日本語 🚀 𝄞 🇺🇸",
            "\u{2028}\u{2029}\u{feff}",
        ])
        .prop_map(str::to_owned),
        2 => any::<String>(),
    ]
}

fn json_value_strategy() -> impl Strategy<Value = serde_json::Value> {
    let leaf = prop_oneof![
        2 => Just(serde_json::Value::Null),
        2 => any::<bool>().prop_map(serde_json::Value::Bool),
        3 => any::<i64>().prop_map(|n| serde_json::Value::Number(n.into())),
        2 => any::<u64>().prop_map(|n| serde_json::Value::Number(n.into())),
        2 => any::<f64>()
            .prop_filter("finite non-negative-zero float", |f| {
                f.is_finite() && f.to_bits() != (-0.0_f64).to_bits()
            })
            .prop_map(|f| {
                serde_json::Value::Number(
                    serde_json::Number::from_f64(f).expect("finite f64"),
                )
            }),
        4 => json_string_strategy().prop_map(serde_json::Value::String),
    ];

    leaf.prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(serde_json::Value::Array),
            prop::collection::btree_map(json_string_strategy(), inner, 0..6).prop_map(|entries| {
                let mut map = serde_json::Map::with_capacity(entries.len());
                for (k, v) in entries {
                    map.insert(k, v);
                }
                serde_json::Value::Object(map)
            }),
        ]
    })
}

proptest! {
    /// Any `serde_json`-serialized JSON value (compact or pretty-printed) round-trips bit-for-bit
    /// identically to the original `serde_json::Value` through `ayaml::from_str`,
    /// `ayaml::from_str_borrowed`, `ayaml::parse_value`, and `ayaml::compat::from_str`, and agrees
    /// with `serde_json::from_str` (allowing a few ULPs for `serde_json`'s default float parser
    /// without `float_roundtrip`).
    #[test]
    fn random_valid_json_matches_serde_json(value in json_value_strategy()) {
        let compact = serde_json::to_string(&value).expect("compact JSON");
        let pretty = serde_json::to_string_pretty(&value).expect("pretty JSON");

        for (mode, text) in [("compact", &compact), ("pretty", &pretty)] {
            let oracle: serde_json::Value =
                serde_json::from_str(text).expect("serde_json reparses its own output");

            let typed: serde_json::Value = ayaml::from_str(text).unwrap_or_else(|error| {
                panic!("{mode}: ayaml::from_str failed ({error}) on {text}")
            });
            // Exact bit-for-bit round trip against the original generated value (0 ULP error).
            assert_json_values_match(&typed, &value, &format!("{mode}::from_str::exact"), text, 0);
            assert_json_values_match(&typed, &oracle, &format!("{mode}::from_str::oracle"), text, 8);

            let borrowed: serde_json::Value =
                ayaml::from_str_borrowed(text).unwrap_or_else(|error| {
                    panic!("{mode}: ayaml::from_str_borrowed failed ({error}) on {text}")
                });
            assert_json_values_match(&borrowed, &value, &format!("{mode}::borrowed::exact"), text, 0);
            assert_json_values_match(&borrowed, &oracle, &format!("{mode}::borrowed::oracle"), text, 8);

            let parsed = ayaml::parse_value(text).unwrap_or_else(|error| {
                panic!("{mode}: ayaml::parse_value failed ({error}) on {text}")
            });
            let parsed_json = serde_json::to_value(&parsed).expect("Value -> JSON");
            assert_json_values_match(&parsed_json, &value, &format!("{mode}::parse_value::exact"), text, 0);
            assert_json_values_match(&parsed_json, &oracle, &format!("{mode}::parse_value::oracle"), text, 8);

            #[cfg(feature = "libyaml-compat")]
            {
                let compat: serde_json::Value = ayaml::compat::from_str(text).unwrap_or_else(
                    |error| panic!("{mode}: compat::from_str failed ({error}) on {text}"),
                );
                assert_json_values_match(&compat, &value, &format!("{mode}::compat::exact"), text, 0);
                assert_json_values_match(&compat, &oracle, &format!("{mode}::compat::oracle"), text, 8);
            }
        }
    }
}
