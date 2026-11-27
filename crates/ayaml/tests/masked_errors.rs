//! Integration tests verifying that [`ayaml::Error::masked`] omits document text across every
//! error path that can interpolate values, keys, tags, directives, anchors, aliases, or sources.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert explicit error outcomes directly"
)]

use std::collections::HashMap;
use std::error::Error as _;
use std::io;

use serde::de::{self, DeserializeSeed};
use serde::{Deserialize, Serialize};

use ayaml::{
    Anchor, Error, ErrorCode, ErrorKind, Event, LimitKind, Limits, ParseOptions, ScalarStyle, Span,
    SpannedTree, Tag, TaggedValue, Value, from_slice, from_slice_borrowed, from_str,
    from_str_borrowed, from_str_seed, from_value, from_value_seed, parse_documents, parse_events,
    parse_value, parse_value_with, to_string, to_string_events, to_string_value, to_value,
    to_writer, to_writer_events,
};

const SECRET: &str = "AYAML-SECRET-MARKER-7391";
const SECRET_VAL: &str = "AYAML-SECRET-VALUE-8426";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    dead_code,
    reason = "fields are deserialized solely to trigger unknown-field errors carrying input keys"
)]
struct StrictConfig {
    host: String,
}

#[derive(Debug, Deserialize)]
enum Mode {
    Fast,
    #[allow(
        dead_code,
        reason = "variant exists solely to exercise enum error paths"
    )]
    Slow(u32),
}

#[derive(Debug)]
struct CustomDeSecret;

impl<'de> Deserialize<'de> for CustomDeSecret {
    fn deserialize<D: de::Deserializer<'de>>(_deserializer: D) -> Result<Self, D::Error> {
        Err(de::Error::custom(format!("rejected {SECRET}")))
    }
}

struct CustomSerSecret;

impl Serialize for CustomSerSecret {
    fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom(format!("cannot encode {SECRET}")))
    }
}

struct DupKeySer;

impl Serialize for DupKeySer {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry(SECRET, &1)?;
        map.serialize_entry(SECRET, &2)?;
        map.end()
    }
}

struct SecretFailingWriter;

impl io::Write for SecretFailingWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other(format!("sink failed for {SECRET}")))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct CustomTagReject;

impl<'de> Deserialize<'de> for CustomTagReject {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RejectVisitor;
        impl<'de> de::Visitor<'de> for RejectVisitor {
            type Value = CustomTagReject;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a plain value")
            }
            fn visit_enum<A: de::EnumAccess<'de>>(self, _data: A) -> Result<Self::Value, A::Error> {
                Err(de::Error::custom(format!("rejected {SECRET_VAL}")))
            }
        }
        deserializer.deserialize_any(RejectVisitor)
    }
}

struct U32Seed;

impl<'de> DeserializeSeed<'de> for U32Seed {
    type Value = u32;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<u32, D::Error> {
        u32::deserialize(deserializer)
    }
}

fn assert_masked_omits(error: &Error, expected_code: ErrorCode, markers: &[&str]) {
    assert_eq!(error.code(), expected_code, "unexpected code for {error}");

    let masked = error.masked();
    let masked_display = masked.to_string();
    let masked_debug = format!("{masked:?}");
    let masked_pretty = format!("{masked:#?}");

    for &marker in markers {
        assert!(
            !masked_display.contains(marker),
            "masked Display leaked {marker:?}: {masked_display:?}"
        );
        assert!(
            !masked_debug.contains(marker),
            "masked Debug leaked {marker:?}: {masked_debug:?}"
        );
        assert!(
            !masked_pretty.contains(marker),
            "masked pretty Debug leaked {marker:?}: {masked_pretty:?}"
        );
    }

    let expected_display = match error.location() {
        Some(location) => format!(
            "{} ({}) at line {} column {}",
            error.kind(),
            error.code(),
            location.line(),
            location.column()
        ),
        None => format!("{} ({})", error.kind(), error.code()),
    };
    assert_eq!(masked_display, expected_display);
    assert!(masked_debug.contains(&format!("{:?}", error.kind())));
    assert!(masked_debug.contains(&format!("{:?}", error.code())));
    assert!(masked.source().is_none());
    assert_eq!(masked.kind(), error.kind());
    assert_eq!(masked.code(), error.code());
    assert_eq!(masked.span(), error.span());
    assert_eq!(masked.location(), error.location());
    assert_eq!(masked.limit(), error.limit());
}

/// Assert that:
/// 1. The unmasked error actually carries each marker in `markers` (in Display, Debug, or source),
///    proving the path really interpolates document/source text.
/// 2. Neither `error.masked().to_string()` nor `format!("{:?}", error.masked())` contains any
///    marker.
/// 3. `error.masked().to_string()` still exposes the kind, stable code, and location (if any).
/// 4. `error.masked().source()` is always `None`.
fn assert_masked_redacts(error: &Error, expected_code: ErrorCode, markers: &[&str]) {
    let raw_display = error.to_string();
    let raw_debug = format!("{error:?}");
    let raw_source = error.source().map(ToString::to_string).unwrap_or_default();
    for &marker in markers {
        assert!(
            raw_display.contains(marker)
                || raw_debug.contains(marker)
                || raw_source.contains(marker),
            "expected unmasked error to carry marker {marker:?} (code={expected_code}): \
             display={raw_display:?}, source={raw_source:?}"
        );
    }
    assert_masked_omits(error, expected_code, markers);
}

#[test]
fn scanner_errors_carrying_document_text_are_masked() {
    let invalid_tag_handle = format!("%TAG {SECRET} !a\n---\n1\n");
    assert_masked_redacts(
        &parse_value(&invalid_tag_handle).unwrap_err(),
        ErrorCode::InvalidDirective,
        &[SECRET],
    );

    let dup_tag_handle = format!("%TAG !{SECRET}! !a\n%TAG !{SECRET}! !b\n---\n1\n");
    assert_masked_redacts(
        &parse_value(&dup_tag_handle).unwrap_err(),
        ErrorCode::DuplicateDirective,
        &[SECRET],
    );

    let invalid_tag_prefix = format!("%TAG !e! <{SECRET}>\n---\n1\n");
    assert_masked_redacts(
        &parse_value(&invalid_tag_prefix).unwrap_err(),
        ErrorCode::InvalidDirective,
        &[SECRET],
    );

    let undef_handle = format!("!{SECRET}!suffix value\n");
    assert_masked_redacts(
        &parse_value(&undef_handle).unwrap_err(),
        ErrorCode::UndefinedTagHandle,
        &[SECRET],
    );
    let undef_events = parse_events(&undef_handle)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .unwrap_err();
    assert_masked_redacts(&undef_events, ErrorCode::UndefinedTagHandle, &[SECRET]);

    // Character-interpolating scanner errors.
    assert_masked_redacts(
        &parse_value("\"\\Q\"").unwrap_err(),
        ErrorCode::InvalidEscape,
        &["\\Q"],
    );
    assert_masked_redacts(
        &parse_value("|Q\n  body\n").unwrap_err(),
        ErrorCode::InvalidBlockScalarHeader,
        &["'Q'"],
    );
    assert_masked_redacts(
        &parse_value("@secret").unwrap_err(),
        ErrorCode::ReservedIndicator,
        &["'@'"],
    );
    assert_masked_redacts(
        &parse_value("\x07").unwrap_err(),
        ErrorCode::NonPrintableChar,
        &["U+0007"],
    );

    // Multi-document parse_documents and SpannedTree::parse carrying secret directives/handles.
    let multi_doc = format!("1\n---\n!{SECRET}!suffix value\n");
    assert_masked_redacts(
        &parse_documents(&multi_doc).unwrap_err(),
        ErrorCode::UndefinedTagHandle,
        &[SECRET],
    );
    assert_masked_redacts(
        &SpannedTree::parse(&invalid_tag_handle).unwrap_err(),
        ErrorCode::InvalidDirective,
        &[SECRET],
    );

    // Invalid UTF-8 carries a std::str::Utf8Error source; masked hides the source too.
    let invalid_utf8 = from_slice::<Value>(&[0xff, 0xfe]).unwrap_err();
    assert!(invalid_utf8.source().is_some());
    assert_masked_redacts(
        &invalid_utf8,
        ErrorCode::InvalidUtf8,
        &["UTF-8", "sequence"],
    );

    // Resource limit error exposes structured LimitExceeded on MaskedError without message text.
    let depth_opts = ParseOptions::new().with_limits(Limits::default().with_max_depth(1));
    let depth_err = parse_value_with("[[1]]\n", &depth_opts).unwrap_err();
    assert_eq!(
        depth_err.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::Depth)
    );
    assert_masked_redacts(&depth_err, ErrorCode::DepthLimit, &["exceeded"]);
}

#[test]
fn duplicate_key_and_resolver_errors_carrying_document_text_are_masked() {
    let dup_scalar = format!("{SECRET}: 1\n{SECRET}: 2\n");
    assert_masked_redacts(
        &parse_value(&dup_scalar).unwrap_err(),
        ErrorCode::DuplicateKey,
        &[SECRET],
    );
    assert_masked_redacts(
        &from_str::<Value>(&dup_scalar).unwrap_err(),
        ErrorCode::DuplicateKey,
        &[SECRET],
    );

    let dup_collection = format!("[{SECRET}]: 1\n[{SECRET}]: 2\n");
    assert_masked_redacts(
        &parse_value(&dup_collection).unwrap_err(),
        ErrorCode::DuplicateKey,
        &[SECRET],
    );

    // > 4 KiB streaming typed path duplicate key.
    let large_dup = format!("pad: \"{}\"\n{SECRET}: 1\n{SECRET}: 2\n", "x".repeat(5000));
    assert_masked_redacts(
        &from_str::<Value>(&large_dup).unwrap_err(),
        ErrorCode::DuplicateKey,
        &[SECRET],
    );

    // Core schema scalar resolution errors across both parse_value and from_str.
    for (tag, code) in [
        ("!!int", ErrorCode::InvalidIntScalar),
        ("!!float", ErrorCode::InvalidFloatScalar),
        ("!!bool", ErrorCode::InvalidBoolScalar),
        ("!!null", ErrorCode::InvalidNullScalar),
    ] {
        let input = format!("{tag} {SECRET}\n");
        assert_masked_omits(&parse_value(&input).unwrap_err(), code, &[SECRET]);
        assert_masked_omits(&from_str::<Value>(&input).unwrap_err(), code, &[SECRET]);
    }
}

#[test]
fn serde_errors_carrying_keys_values_tags_variants_and_fields_are_masked() {
    // Scalar string value in InvalidType across from_str, from_slice, from_str_seed, from_value.
    let bad_u32 = format!("\"{SECRET}\"\n");
    assert_masked_redacts(
        &from_str::<u32>(&bad_u32).unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET],
    );
    assert_masked_redacts(
        &from_slice::<u32>(bad_u32.as_bytes()).unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET],
    );
    assert_masked_redacts(
        &from_str_seed(&bad_u32, U32Seed).unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET],
    );
    assert_masked_redacts(
        &from_value::<u32>(Value::String(SECRET.to_owned())).unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET],
    );
    assert_masked_redacts(
        &from_value_seed(Value::String(SECRET.to_owned()), U32Seed).unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET],
    );
    assert_masked_redacts(
        &from_str_borrowed::<u32>(&bad_u32).unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET],
    );
    assert_masked_redacts(
        &from_slice_borrowed::<u32>(bad_u32.as_bytes()).unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET],
    );

    // Nested mapping path carrying BOTH a secret key (bare, quoted, long) and a secret value.
    let nested = format!("{SECRET}: \"{SECRET_VAL}\"\n");
    assert_masked_redacts(
        &from_str::<HashMap<String, u32>>(&nested).unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET, SECRET_VAL],
    );
    let quoted_key = format!("\"has space {SECRET}\": \"{SECRET_VAL}\"\n");
    assert_masked_redacts(
        &from_str::<HashMap<String, u32>>(&quoted_key).unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET, SECRET_VAL],
    );
    let long_key = format!("k_{SECRET}_{}: \"{SECRET_VAL}\"\n", "z".repeat(70));
    assert_masked_redacts(
        &from_str::<HashMap<String, u32>>(&long_key).unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET, SECRET_VAL],
    );
    let enum_payload = format!("Slow: \"{SECRET_VAL}\"\n");
    assert_masked_redacts(
        &from_str::<Mode>(&enum_payload).unwrap_err(),
        ErrorCode::InvalidType,
        &["Slow", SECRET_VAL],
    );
    let large_typed_err = from_str::<HashMap<String, u32>>(&format!(
        "pad: 1\n{SECRET}: \"{SECRET_VAL}\"\n# {}\n",
        "x".repeat(5000)
    ))
    .unwrap_err();
    assert_masked_redacts(
        &large_typed_err,
        ErrorCode::InvalidType,
        &[SECRET, SECRET_VAL],
    );

    // Tagged node refusal carrying a non-core global tag name.
    let tagged_input = format!("!!{SECRET} {{host: ok}}\n");
    assert_masked_redacts(
        &from_str::<StrictConfig>(&tagged_input).unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET],
    );
    let mut map = ayaml::Mapping::new();
    map.insert(Value::from("host"), Value::from("ok"));
    assert_masked_redacts(
        &from_value::<StrictConfig>(Value::Tagged(Box::new(TaggedValue::new(
            Tag::new("tag:yaml.org,2002:", SECRET),
            Value::Mapping(map),
        ))))
        .unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET],
    );
    // Custom tagged-node refusal (`tagged value \`!!SECRET\`: rejected ...`).
    assert_masked_redacts(
        &from_str::<CustomTagReject>(&tagged_input).unwrap_err(),
        ErrorCode::SerdeCustom,
        &[SECRET, SECRET_VAL],
    );
    assert_masked_redacts(
        &from_value::<CustomTagReject>(Value::Tagged(Box::new(TaggedValue::new(
            Tag::new("tag:yaml.org,2002:", SECRET),
            Value::Null,
        ))))
        .unwrap_err(),
        ErrorCode::SerdeCustom,
        &[SECRET, SECRET_VAL],
    );

    // Radix integer read as f64 carries raw integer text.
    assert_masked_redacts(
        &from_str::<f64>("0x7391\n").unwrap_err(),
        ErrorCode::InvalidType,
        &["0x7391"],
    );

    // InvalidValue carrying a string value.
    assert_masked_redacts(
        &from_str::<char>(&bad_u32).unwrap_err(),
        ErrorCode::InvalidValue,
        &[SECRET],
    );

    // UnknownVariant carrying a variant name from the document.
    assert_masked_redacts(
        &from_str::<Mode>(&format!("{SECRET}\n")).unwrap_err(),
        ErrorCode::UnknownVariant,
        &[SECRET],
    );

    // UnknownField carrying a key name from the document.
    let unknown_field = format!("host: ok\n{SECRET}: 1\n");
    assert_masked_redacts(
        &from_str::<StrictConfig>(&unknown_field).unwrap_err(),
        ErrorCode::UnknownField,
        &[SECRET],
    );

    // Multi-document Deserializer carrying a secret value in a later document.
    let doc_stream = format!("1\n---\n\"{SECRET}\"\n");
    let mut docs = ayaml::Deserializer::from_str(&doc_stream).unwrap();
    assert_eq!(docs.next::<u32>().unwrap(), Some(1));
    assert_masked_redacts(
        &docs.next::<u32>().unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET],
    );

    // Custom Deserialize, custom Serialize, and duplicate Serialize key.
    assert_masked_redacts(
        &from_str::<CustomDeSecret>("1\n").unwrap_err(),
        ErrorCode::SerdeCustom,
        &[SECRET],
    );
    assert_masked_redacts(
        &to_value(&CustomSerSecret).unwrap_err(),
        ErrorCode::SerdeCustom,
        &[SECRET],
    );
    assert_masked_redacts(
        &to_string(&CustomSerSecret).unwrap_err(),
        ErrorCode::SerdeCustom,
        &[SECRET],
    );
    assert_masked_redacts(
        &to_value(&DupKeySer).unwrap_err(),
        ErrorCode::DuplicateKey,
        &[SECRET],
    );
    assert_masked_redacts(
        &to_string(&DupKeySer).unwrap_err(),
        ErrorCode::DuplicateKey,
        &[SECRET],
    );
}

#[test]
fn emitter_event_emitter_and_io_errors_carrying_text_are_masked() {
    // Core tag scalar mismatch carrying the scalar value.
    let bad_core_tag = to_string_value(&Value::Tagged(Box::new(TaggedValue::new(
        Tag::new("tag:yaml.org,2002:", "int"),
        Value::String(SECRET.to_owned()),
    ))))
    .unwrap_err();
    assert_masked_redacts(&bad_core_tag, ErrorCode::UnsupportedTag, &[SECRET]);

    // Unwritable single-character non-local tag name.
    let unwriteable_tag = to_string_value(&Value::Tagged(Box::new(TaggedValue::new(
        Tag::new("", "Q"),
        Value::Null,
    ))))
    .unwrap_err();
    assert_masked_redacts(&unwriteable_tag, ErrorCode::UnsupportedTag, &["\"Q\""]);

    // Event emitter: unwritable directive, invalid %TAG directive, unwritable anchor,
    // unwritable alias, and dangling alias.
    let bad_dir = to_string_events(&[Event::Directive {
        name: SECRET.into(),
        value: format!("{SECRET_VAL}\nline2").into(),
        span: Span::empty(0),
    }])
    .unwrap_err();
    assert_masked_redacts(&bad_dir, ErrorCode::InvalidEmitEvent, &[SECRET, SECRET_VAL]);

    let bad_tag_dir = to_string_events(&[Event::Directive {
        name: "TAG".into(),
        value: format!("!{SECRET}!").into(),
        span: Span::empty(0),
    }])
    .unwrap_err();
    assert_masked_redacts(&bad_tag_dir, ErrorCode::InvalidEmitEvent, &[SECRET]);

    let bad_anchor = to_string_events(&[
        Event::DocumentStart {
            explicit: false,
            span: Span::empty(0),
        },
        Event::Scalar {
            value: "v".into(),
            raw: "v".into(),
            style: ScalarStyle::Plain,
            tag: None,
            anchor: Some(Anchor::new(format!("{SECRET} space"))),
            span: Span::empty(0),
        },
        Event::DocumentEnd {
            explicit: false,
            span: Span::empty(0),
        },
    ])
    .unwrap_err();
    assert_masked_redacts(&bad_anchor, ErrorCode::InvalidEmitEvent, &[SECRET]);

    let bad_alias = to_string_events(&[
        Event::DocumentStart {
            explicit: false,
            span: Span::empty(0),
        },
        Event::Alias {
            anchor: Anchor::new(format!("{SECRET} space")),
            span: Span::empty(0),
        },
        Event::DocumentEnd {
            explicit: false,
            span: Span::empty(0),
        },
    ])
    .unwrap_err();
    assert_masked_redacts(&bad_alias, ErrorCode::InvalidEmitEvent, &[SECRET]);

    let dangling_alias = to_string_events(&[
        Event::DocumentStart {
            explicit: false,
            span: Span::empty(0),
        },
        Event::Alias {
            anchor: Anchor::new(SECRET),
            span: Span::empty(0),
        },
        Event::DocumentEnd {
            explicit: false,
            span: Span::empty(0),
        },
    ])
    .unwrap_err();
    assert_masked_redacts(&dangling_alias, ErrorCode::InvalidEmitEvent, &[SECRET]);

    // I/O error with secret text in underlying std::io::Error source.
    let io_err = to_writer(&mut SecretFailingWriter, &Value::String("x".to_owned())).unwrap_err();
    assert_masked_redacts(&io_err, ErrorCode::Io, &[SECRET]);
    let event_io_err = to_writer_events(
        &mut SecretFailingWriter,
        &[
            Event::DocumentStart {
                explicit: false,
                span: Span::empty(0),
            },
            Event::Scalar {
                value: "x".into(),
                raw: "x".into(),
                style: ScalarStyle::Plain,
                tag: None,
                anchor: None,
                span: Span::empty(0),
            },
            Event::DocumentEnd {
                explicit: false,
                span: Span::empty(0),
            },
        ],
    )
    .unwrap_err();
    assert_masked_redacts(&event_io_err, ErrorCode::Io, &[SECRET]);

    // Manually constructed error with secret text in both message and source.
    let sourced = Error::new(ErrorKind::Parser, format!("message {SECRET}"))
        .with_source(io::Error::other(format!("source {SECRET_VAL}")));
    assert_masked_redacts(&sourced, ErrorCode::UnexpectedToken, &[SECRET, SECRET_VAL]);
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_errors_carrying_document_text_are_masked() {
    struct ExpectU32;
    impl de::Visitor<'_> for ExpectU32 {
        type Value = u32;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a u32")
        }
    }

    let nested = format!("{SECRET}: \"{SECRET_VAL}\"\n");
    assert_masked_redacts(
        &ayaml::compat::from_str::<HashMap<String, u32>>(&nested).unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET, SECRET_VAL],
    );

    let tagged = format!("!!{SECRET} {{host: ok}}\n");
    assert_masked_redacts(
        &ayaml::compat::from_str::<StrictConfig>(&tagged).unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET],
    );
    assert_masked_redacts(
        &ayaml::compat::from_str_seed(&format!("\"{SECRET}\"\n"), U32Seed).unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET],
    );

    // Compat duplicate key inside Value::deserialize and Mapping::deserialize carries the key text.
    let dup = format!("{SECRET}: 1\n{SECRET}: 2\n");
    assert_masked_redacts(
        &ayaml::compat::from_str::<Value>(&dup).unwrap_err(),
        ErrorCode::SerdeCustom,
        &[SECRET],
    );
    assert_masked_redacts(
        &ayaml::compat::from_str::<ayaml::Mapping>(&dup).unwrap_err(),
        ErrorCode::SerdeCustom,
        &[SECRET],
    );

    // Compat deserialize_any carrying a secret value in a visitor rejection.
    assert_masked_redacts(
        &ayaml::compat::deserialize_any(&format!("\"{SECRET}\"\n"), ExpectU32).unwrap_err(),
        ErrorCode::InvalidType,
        &[SECRET],
    );

    // Compat unknown field, unknown variant, invalid value, and SpannedTree::parse_compat.
    assert_masked_redacts(
        &ayaml::compat::from_str::<StrictConfig>(&format!("host: ok\n{SECRET}: 1\n")).unwrap_err(),
        ErrorCode::UnknownField,
        &[SECRET],
    );
    assert_masked_redacts(
        &ayaml::compat::from_str::<Mode>(&format!("{SECRET}\n")).unwrap_err(),
        ErrorCode::UnknownVariant,
        &[SECRET],
    );
    assert_masked_redacts(
        &ayaml::compat::from_str::<char>(&format!("\"{SECRET}\"\n")).unwrap_err(),
        ErrorCode::InvalidValue,
        &[SECRET],
    );
    assert_masked_redacts(
        &SpannedTree::parse_compat(&format!("%TAG {SECRET} !a\n---\n1\n")).unwrap_err(),
        ErrorCode::InvalidDirective,
        &[SECRET],
    );
}

#[cfg(feature = "bench-internals")]
#[test]
fn internals_compose_events_key_without_value_is_masked() {
    let events = vec![
        Event::DocumentStart {
            explicit: false,
            span: Span::empty(0),
        },
        Event::MappingStart {
            style: ayaml::CollectionStyle::Block,
            tag: None,
            anchor: None,
            span: Span::empty(0),
        },
        Event::Scalar {
            value: SECRET.into(),
            raw: SECRET.into(),
            style: ScalarStyle::Plain,
            tag: None,
            anchor: None,
            span: Span::empty(0),
        },
        Event::MappingEnd {
            span: Span::empty(0),
        },
        Event::DocumentEnd {
            explicit: false,
            span: Span::empty(0),
        },
    ];
    let err =
        ayaml::internals::compose_events(&events, SECRET.len(), &Limits::default()).unwrap_err();
    assert_masked_redacts(&err, ErrorCode::MalformedEventStream, &[SECRET]);
}

#[test]
fn tape_and_parser_buf_errors_are_masked() {
    let mut buf = ayaml::ParserBuf::new();

    // Duplicate key via parse_tape, parse_tape_in, parse_tape_documents.
    let dup = format!("{SECRET}: 1\n{SECRET}: 2\n");
    assert_masked_redacts(
        &ayaml::parse_tape(&dup).unwrap_err(),
        ErrorCode::DuplicateKey,
        &[SECRET],
    );
    assert_masked_redacts(
        &buf.parse_tape(&dup).unwrap_err(),
        ErrorCode::DuplicateKey,
        &[SECRET],
    );
    assert_masked_redacts(
        &buf.parse_tape_documents(&dup).unwrap_err(),
        ErrorCode::DuplicateKey,
        &[SECRET],
    );

    // Serde errors deserializing directly from `&Tape` and `TapeRef`.
    let unknown_field = format!("host: ok\n{SECRET}: 1\n");
    let tape = buf.parse_tape(&unknown_field).expect("valid yaml");
    assert_masked_redacts(
        &StrictConfig::deserialize(&tape).unwrap_err(),
        ErrorCode::UnknownField,
        &[SECRET],
    );
    tape.recycle_into(&mut buf);

    let unknown_var = format!("{SECRET}\n");
    let var_tape = buf.parse_tape(&unknown_var).expect("valid yaml");
    assert_masked_redacts(
        &Mode::deserialize(&var_tape).unwrap_err(),
        ErrorCode::UnknownVariant,
        &[SECRET],
    );
    var_tape.recycle_into(&mut buf);

    let tagged_secret = format!("!{SECRET} {SECRET_VAL}\n");
    let tag_tape = buf.parse_tape(&tagged_secret).expect("valid yaml");
    assert_masked_redacts(
        &CustomTagReject::deserialize(&tag_tape).unwrap_err(),
        ErrorCode::SerdeCustom,
        &[SECRET_VAL],
    );
    tag_tape.recycle_into(&mut buf);

    // ParserBuf::from_str and parse_value_in errors remain masked identically.
    assert_masked_redacts(
        &buf.from_str::<StrictConfig>(&unknown_field).unwrap_err(),
        ErrorCode::UnknownField,
        &[SECRET],
    );
    assert_masked_redacts(
        &buf.parse_value(&dup).unwrap_err(),
        ErrorCode::DuplicateKey,
        &[SECRET],
    );
}
