//! Integration tests pinning every published [`ayaml::ErrorCode`] variant.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert explicit error outcomes directly"
)]

use std::collections::HashSet;
use std::fmt;
use std::io;

use serde::de::{self, DeserializeSeed, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

use ayaml::{
    EmitOptions, Error, ErrorCode, ErrorKind, Event, LimitKind, Limits, ParseOptions, Span, Tag,
    Value, from_slice, from_str, from_str_with, from_value, parse_documents_with, parse_events,
    parse_events_with, parse_value, parse_value_with, to_string_events, to_string_value,
    to_string_value_with, to_value, to_writer,
};

fn assert_code(error: &Error, expected_kind: ErrorKind, expected_code: ErrorCode, expected: &str) {
    assert_eq!(error.kind(), expected_kind, "unexpected kind for {error}");
    assert_eq!(error.code(), expected_code, "unexpected code for {error}");
    assert_eq!(error.code().as_str(), expected);
    assert_eq!(error.code().to_string(), expected);
    assert_eq!(error.code().as_ref(), expected);
}

#[test]
fn encoding_error_code_is_pinned() {
    let error = from_slice::<Value>(&[0xff, 0xfe]).unwrap_err();
    assert_code(
        &error,
        ErrorKind::InvalidEncoding,
        ErrorCode::InvalidUtf8,
        "yaml.invalid-utf8",
    );
}

#[test]
fn scanner_error_codes_are_pinned() {
    let cases: &[(&str, ErrorCode, &str)] = &[
        (
            "\u{0007}",
            ErrorCode::NonPrintableChar,
            "yaml.non-printable-char",
        ),
        (
            "a: 1\n\u{feff}b: 2\n",
            ErrorCode::InvalidBom,
            "yaml.invalid-bom",
        ),
        ("a\u{feff}b", ErrorCode::InvalidBom, "yaml.invalid-bom"),
        (
            "@foo",
            ErrorCode::ReservedIndicator,
            "yaml.reserved-indicator",
        ),
        (
            "`foo",
            ErrorCode::ReservedIndicator,
            "yaml.reserved-indicator",
        ),
        (
            "-\t- a\n",
            ErrorCode::InvalidTabIndentation,
            "yaml.invalid-tab-indentation",
        ),
        (
            "a: \"x\n\ty\"\n",
            ErrorCode::InvalidTabIndentation,
            "yaml.invalid-tab-indentation",
        ),
        (
            "a: [\n\t1]\n",
            ErrorCode::InvalidFlowIndentation,
            "yaml.invalid-flow-indentation",
        ),
        (
            "'unterminated",
            ErrorCode::UnterminatedQuotedScalar,
            "yaml.unterminated-quoted-scalar",
        ),
        (
            "\"unterminated",
            ErrorCode::UnterminatedQuotedScalar,
            "yaml.unterminated-quoted-scalar",
        ),
        (
            "a: \"hello\nworld\"\n",
            ErrorCode::InvalidQuotedContinuation,
            "yaml.invalid-quoted-continuation",
        ),
        (
            "\"hello\n---\nworld\"",
            ErrorCode::InvalidQuotedContinuation,
            "yaml.invalid-quoted-continuation",
        ),
        ("\"\\q\"", ErrorCode::InvalidEscape, "yaml.invalid-escape"),
        ("\"\\x1\"", ErrorCode::InvalidEscape, "yaml.invalid-escape"),
        (
            "\"\\uD800\"",
            ErrorCode::InvalidEscape,
            "yaml.invalid-escape",
        ),
        (
            "|x\n  a\n",
            ErrorCode::InvalidBlockScalarHeader,
            "yaml.invalid-block-scalar-header",
        ),
        (
            "a:\n|\n  x\n",
            ErrorCode::InvalidBlockScalarIndentation,
            "yaml.invalid-block-scalar-indentation",
        ),
        (
            "|\n   \n  x\n",
            ErrorCode::InvalidBlockScalarIndentation,
            "yaml.invalid-block-scalar-indentation",
        ),
        (
            "a: [1,\n2]\n",
            ErrorCode::InvalidFlowIndentation,
            "yaml.invalid-flow-indentation",
        ),
        (
            "[a,\n---\nb]\n",
            ErrorCode::DocumentMarkerInFlow,
            "yaml.document-marker-in-flow",
        ),
        ("[?]", ErrorCode::InvalidFlowKey, "yaml.invalid-flow-key"),
        (
            "a: |#comment\n  x\n",
            ErrorCode::MissingCommentWhitespace,
            "yaml.missing-comment-whitespace",
        ),
        (
            "'a'#comment",
            ErrorCode::MissingCommentWhitespace,
            "yaml.missing-comment-whitespace",
        ),
        (
            "%YAML 2.0\n---\n1\n",
            ErrorCode::InvalidDirective,
            "yaml.invalid-directive",
        ),
        (
            "%TAG !e!\n---\n1\n",
            ErrorCode::InvalidDirective,
            "yaml.invalid-directive",
        ),
        (
            "%YAML 1.2\n%YAML 1.2\n---\n1\n",
            ErrorCode::DuplicateDirective,
            "yaml.duplicate-directive",
        ),
        (
            "%TAG !e! !a\n%TAG !e! !b\n---\n1\n",
            ErrorCode::DuplicateDirective,
            "yaml.duplicate-directive",
        ),
        ("& a", ErrorCode::InvalidAnchor, "yaml.invalid-anchor"),
        ("&a[1]", ErrorCode::InvalidAnchor, "yaml.invalid-anchor"),
        ("!<unterminated", ErrorCode::InvalidTag, "yaml.invalid-tag"),
        ("!! x", ErrorCode::InvalidTag, "yaml.invalid-tag"),
        (
            "!e!foo bar",
            ErrorCode::UndefinedTagHandle,
            "yaml.undefined-tag-handle",
        ),
    ];

    for &(input, code, name) in cases {
        let error = parse_value(input).unwrap_err();
        assert_code(&error, ErrorKind::Scanner, code, name);
        let event_error = parse_events(input)
            .and_then(Iterator::collect::<Result<Vec<_>, _>>)
            .unwrap_err();
        assert_eq!(
            event_error.code(),
            code,
            "event stream mismatch for {input:?}"
        );
    }
}

#[test]
fn parser_error_codes_are_pinned() {
    let long_key = format!("{}: 1\n", "a".repeat(1025));
    let cases: &[(&str, ErrorCode, &str)] = &[
        (
            "%YAML 1.2\n",
            ErrorCode::DirectiveWithoutDocument,
            "yaml.directive-without-document",
        ),
        (
            "%YAML 1.2\nfoo\n",
            ErrorCode::DirectiveWithoutDocument,
            "yaml.directive-without-document",
        ),
        (
            "--- a: b\n",
            ErrorCode::InvalidDocumentStart,
            "yaml.invalid-document-start",
        ),
        (
            "--- - a\n",
            ErrorCode::InvalidDocumentStart,
            "yaml.invalid-document-start",
        ),
        (
            "a\n... b\n",
            ErrorCode::ContentAfterDocumentEnd,
            "yaml.content-after-document-end",
        ),
        (
            "a: 1\nb\n",
            ErrorCode::TrailingContent,
            "yaml.trailing-content",
        ),
        (
            "  - a\n- b\n",
            ErrorCode::InvalidBlockIndentation,
            "yaml.invalid-block-indentation",
        ),
        (
            "a: \"1\"\n\n  b\n",
            ErrorCode::MissingMappingColon,
            "yaml.missing-mapping-colon",
        ),
        (
            "1\n---\n2\n",
            ErrorCode::TrailingDocuments,
            "yaml.trailing-documents",
        ),
        ("]", ErrorCode::UnexpectedToken, "yaml.unexpected-token"),
        (
            "a: b: c\n",
            ErrorCode::UnexpectedMappingValue,
            "yaml.unexpected-mapping-value",
        ),
        (
            "[1, 2",
            ErrorCode::UnterminatedFlowCollection,
            "yaml.unterminated-flow-collection",
        ),
        (
            "{a: 1",
            ErrorCode::UnterminatedFlowCollection,
            "yaml.unterminated-flow-collection",
        ),
        (
            "[, 1]",
            ErrorCode::InvalidFlowCollection,
            "yaml.invalid-flow-collection",
        ),
        (
            "[\"1\" \"2\"]",
            ErrorCode::InvalidFlowCollection,
            "yaml.invalid-flow-collection",
        ),
        (
            "{]}",
            ErrorCode::InvalidFlowCollection,
            "yaml.invalid-flow-collection",
        ),
        (
            "&a &b 1",
            ErrorCode::DuplicateNodeProperties,
            "yaml.duplicate-node-properties",
        ),
        (
            "!a !b 1",
            ErrorCode::DuplicateNodeProperties,
            "yaml.duplicate-node-properties",
        ),
        (
            "&a - 1\n",
            ErrorCode::MisplacedNodeProperties,
            "yaml.misplaced-node-properties",
        ),
        (
            "- &a ? k\n  : v\n",
            ErrorCode::MisplacedNodeProperties,
            "yaml.misplaced-node-properties",
        ),
        (
            "\"a\nb\": 1\n",
            ErrorCode::InvalidImplicitKey,
            "yaml.invalid-implicit-key",
        ),
        (
            long_key.as_str(),
            ErrorCode::InvalidImplicitKey,
            "yaml.invalid-implicit-key",
        ),
        (
            "- &a 1\n- !t *a\n",
            ErrorCode::AliasWithProperties,
            "yaml.alias-with-properties",
        ),
        // Parser-level tab indentation after closing a block mapping.
        (
            "a:\n\tb\n",
            ErrorCode::InvalidTabIndentation,
            "yaml.invalid-tab-indentation",
        ),
        // Parser-level block scalar indentation after closing a block mapping.
        (
            "a: 1\n|\n  x\n",
            ErrorCode::InvalidBlockScalarIndentation,
            "yaml.invalid-block-scalar-indentation",
        ),
    ];

    for &(input, code, name) in cases {
        let error = parse_value(input).unwrap_err();
        assert_code(&error, ErrorKind::Parser, code, name);
    }

    // Trailing documents across single-document readers (including streaming typed > 4 KiB).
    let padded_trailing = format!("a: \"{}\"\n---\nb: 2\n", "x".repeat(5000));
    let typed_err = from_str::<Value>(&padded_trailing).unwrap_err();
    assert_code(
        &typed_err,
        ErrorKind::Parser,
        ErrorCode::TrailingDocuments,
        "yaml.trailing-documents",
    );
    let slice_err = from_slice::<Value>(b"1\n---\n2\n").unwrap_err();
    assert_code(
        &slice_err,
        ErrorKind::Parser,
        ErrorCode::TrailingDocuments,
        "yaml.trailing-documents",
    );
}

#[test]
fn composition_alias_and_duplicate_key_codes_are_pinned() {
    let unknown = parse_value("*missing").unwrap_err();
    assert_code(
        &unknown,
        ErrorKind::Composition,
        ErrorCode::UnknownAnchor,
        "yaml.unknown-anchor",
    );
    assert_eq!(
        from_str::<Value>("*missing").unwrap_err().code(),
        ErrorCode::UnknownAnchor
    );

    let cyclic = parse_value("&a [*a]").unwrap_err();
    assert_code(
        &cyclic,
        ErrorKind::Composition,
        ErrorCode::CyclicAlias,
        "yaml.cyclic-alias",
    );
    assert_eq!(
        from_str::<Value>("&a [*a]").unwrap_err().code(),
        ErrorCode::CyclicAlias
    );

    let dup = parse_value("a: 1\na: 2\n").unwrap_err();
    assert_code(
        &dup,
        ErrorKind::DuplicateKey,
        ErrorCode::DuplicateKey,
        "yaml.duplicate-key",
    );
    let large_dup = format!("pad: \"{}\"\na: 1\na: 2\n", "x".repeat(5000));
    let typed_dup = from_str::<Value>(&large_dup).unwrap_err();
    assert_code(
        &typed_dup,
        ErrorKind::DuplicateKey,
        ErrorCode::DuplicateKey,
        "yaml.duplicate-key",
    );

    // Merge operand failure via Value::apply_merge_keys (available in default features).
    let mut bad_merge = parse_value("a: 1\n<<: 123\n").unwrap();
    let merge_err = bad_merge.apply_merge_keys().unwrap_err();
    assert_code(
        &merge_err,
        ErrorKind::Composition,
        ErrorCode::InvalidMergeOperand,
        "yaml.invalid-merge-operand",
    );
    let mut bad_merge_seq = parse_value("a: 1\n<<: [123]\n").unwrap();
    let merge_seq_err = bad_merge_seq.apply_merge_keys().unwrap_err();
    assert_code(
        &merge_seq_err,
        ErrorKind::Composition,
        ErrorCode::InvalidMergeOperand,
        "yaml.invalid-merge-operand",
    );

    // Default code for ErrorKind::Composition (malformed event streams).
    let malformed = Error::new(ErrorKind::Composition, "unbalanced event stream");
    assert_code(
        &malformed,
        ErrorKind::Composition,
        ErrorCode::MalformedEventStream,
        "yaml.malformed-event-stream",
    );
}

#[test]
fn resolver_error_codes_are_pinned() {
    let cases: &[(&str, ErrorCode, &str)] = &[
        (
            "!!int abc",
            ErrorCode::InvalidIntScalar,
            "yaml.invalid-int-scalar",
        ),
        (
            "!!float abc",
            ErrorCode::InvalidFloatScalar,
            "yaml.invalid-float-scalar",
        ),
        (
            "!!bool maybe",
            ErrorCode::InvalidBoolScalar,
            "yaml.invalid-bool-scalar",
        ),
        (
            "!!null nope",
            ErrorCode::InvalidNullScalar,
            "yaml.invalid-null-scalar",
        ),
    ];
    for &(input, code, name) in cases {
        let value_err = parse_value(input).unwrap_err();
        assert_code(&value_err, ErrorKind::Resolution, code, name);
        let typed_err = from_str::<Value>(input).unwrap_err();
        assert_code(&typed_err, ErrorKind::Resolution, code, name);
    }

    let generic = Error::new(ErrorKind::Resolution, "custom scalar failure");
    assert_code(
        &generic,
        ErrorKind::Resolution,
        ErrorCode::InvalidScalar,
        "yaml.invalid-scalar",
    );
}

#[test]
fn limit_error_codes_are_pinned() {
    let generic = Error::new(ErrorKind::Limit, "generic limit");
    assert_code(
        &generic,
        ErrorKind::Limit,
        ErrorCode::LimitExceeded,
        "yaml.limit-exceeded",
    );

    let invalid = parse_value_with(
        "a",
        &ParseOptions::new().with_limits(Limits::new().with_max_depth(usize::MAX)),
    )
    .unwrap_err();
    assert_code(
        &invalid,
        ErrorKind::Limit,
        ErrorCode::InvalidLimits,
        "yaml.invalid-limits",
    );
    let zero_cost = parse_value_with(
        "a",
        &ParseOptions::new().with_limits(Limits::new().with_node_cost_bytes(0)),
    )
    .unwrap_err();
    assert_code(
        &zero_cost,
        ErrorKind::Limit,
        ErrorCode::InvalidLimits,
        "yaml.invalid-limits",
    );

    let input_size = parse_value_with(
        "abcdef",
        &ParseOptions::new().with_limits(Limits::new().with_max_input_bytes(3)),
    )
    .unwrap_err();
    assert_code(
        &input_size,
        ErrorKind::Limit,
        ErrorCode::InputSizeLimit,
        "yaml.input-size-limit",
    );
    assert_eq!(input_size.limit().unwrap().kind(), LimitKind::InputBytes);

    let depth = parse_value_with(
        "[[1]]",
        &ParseOptions::new().with_limits(Limits::new().with_max_depth(1)),
    )
    .unwrap_err();
    assert_code(
        &depth,
        ErrorKind::Limit,
        ErrorCode::DepthLimit,
        "yaml.depth-limit",
    );
    assert_eq!(depth.limit().unwrap().kind(), LimitKind::Depth);

    let docs = parse_documents_with(
        "1\n---\n2\n",
        &ParseOptions::new().with_limits(Limits::new().with_max_documents(1)),
    )
    .unwrap_err();
    assert_code(
        &docs,
        ErrorKind::Limit,
        ErrorCode::DocumentCountLimit,
        "yaml.document-count-limit",
    );
    assert_eq!(docs.limit().unwrap().kind(), LimitKind::Documents);

    let scalar = parse_value_with(
        "abcdef",
        &ParseOptions::new().with_limits(Limits::new().with_max_scalar_bytes(3)),
    )
    .unwrap_err();
    assert_code(
        &scalar,
        ErrorKind::Limit,
        ErrorCode::ScalarSizeLimit,
        "yaml.scalar-size-limit",
    );
    assert_eq!(scalar.limit().unwrap().kind(), LimitKind::ScalarBytes);

    let entries = parse_value_with(
        "[1, 2, 3]",
        &ParseOptions::new().with_limits(Limits::new().with_max_collection_entries(2)),
    )
    .unwrap_err();
    assert_code(
        &entries,
        ErrorKind::Limit,
        ErrorCode::CollectionEntriesLimit,
        "yaml.collection-entries-limit",
    );
    assert_eq!(
        entries.limit().unwrap().kind(),
        LimitKind::CollectionEntries
    );

    let events = parse_events_with(
        "[1, 2, 3]",
        &ParseOptions::new().with_limits(Limits::new().with_max_events(3)),
    )
    .and_then(Iterator::collect::<Result<Vec<_>, _>>)
    .unwrap_err();
    assert_code(
        &events,
        ErrorKind::Limit,
        ErrorCode::EventCountLimit,
        "yaml.event-count-limit",
    );
    assert_eq!(events.limit().unwrap().kind(), LimitKind::Events);

    let anchors = parse_value_with(
        "- &a 1\n- &b 2\n",
        &ParseOptions::new().with_limits(Limits::new().with_max_anchors(1)),
    )
    .unwrap_err();
    assert_code(
        &anchors,
        ErrorKind::Limit,
        ErrorCode::AnchorCountLimit,
        "yaml.anchor-count-limit",
    );
    assert_eq!(anchors.limit().unwrap().kind(), LimitKind::Anchors);

    let derefs = parse_value_with(
        "- &a 1\n- *a\n- *a\n",
        &ParseOptions::new().with_limits(Limits::new().with_max_alias_dereferences(1)),
    )
    .unwrap_err();
    assert_code(
        &derefs,
        ErrorKind::Limit,
        ErrorCode::AliasDereferenceLimit,
        "yaml.alias-dereference-limit",
    );
    assert_eq!(derefs.limit().unwrap().kind(), LimitKind::AliasDereferences);

    let expansion = parse_value_with(
        "- &a [1, 2, 3, 4]\n- *a\n",
        &ParseOptions::new().with_limits(Limits::new().with_max_alias_expansion_bytes(1)),
    )
    .unwrap_err();
    assert_code(
        &expansion,
        ErrorKind::Limit,
        ErrorCode::AliasExpansionLimit,
        "yaml.alias-expansion-limit",
    );
    assert_eq!(expansion.limit().unwrap().kind(), LimitKind::AliasExpansion);

    let output = to_string_value_with(
        &Value::String("abcdefghij".to_owned()),
        &EmitOptions::new().with_limits(Limits::new().with_max_output_bytes(3)),
    )
    .unwrap_err();
    assert_code(
        &output,
        ErrorKind::Limit,
        ErrorCode::OutputSizeLimit,
        "yaml.output-size-limit",
    );
    assert_eq!(output.limit().unwrap().kind(), LimitKind::OutputBytes);

    let tag_exp = parse_value_with(
        "%TAG !e! tag:example.com,2026:very/long/prefix/\n---\n!e!a 1\n",
        &ParseOptions::new().with_limits(Limits::new().with_max_tag_expansion_bytes(5)),
    )
    .unwrap_err();
    assert_code(
        &tag_exp,
        ErrorKind::Limit,
        ErrorCode::TagExpansionLimit,
        "yaml.tag-expansion-limit",
    );
    assert_eq!(tag_exp.limit().unwrap().kind(), LimitKind::TagExpansion);
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    dead_code,
    reason = "fields are deserialized solely to exercise Serde field error codes"
)]
struct StrictRecord {
    #[serde(alias = "a_alias")]
    a: u32,
    b: String,
}

#[derive(Debug, Deserialize)]
enum SampleEnum {
    Unit,
    #[allow(
        dead_code,
        reason = "variant exists solely to exercise enum representation error codes"
    )]
    Tuple(u32),
}

struct FailingWriter;

impl io::Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("disk full"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[allow(
    dead_code,
    reason = "fields are deserialized solely to exercise streaming typed error codes"
)]
struct LargeTarget {
    pad: String,
    num: u32,
}

struct U32Seed;

impl<'de> DeserializeSeed<'de> for U32Seed {
    type Value = u32;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<u32, D::Error> {
        u32::deserialize(deserializer)
    }
}

#[derive(Debug)]
struct ValueBeforeKey;

impl<'de> Deserialize<'de> for ValueBeforeKey {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = ValueBeforeKey;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                drop(map.next_value::<Value>()?);
                Ok(ValueBeforeKey)
            }
        }
        deserializer.deserialize_map(V)
    }
}

#[derive(Debug)]
struct CustomFail;

impl<'de> Deserialize<'de> for CustomFail {
    fn deserialize<D: de::Deserializer<'de>>(_deserializer: D) -> Result<Self, D::Error> {
        Err(de::Error::custom("custom failure"))
    }
}

struct DupSerializeMap;

impl Serialize for DupSerializeMap {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("k", &1)?;
        map.serialize_entry("k", &2)?;
        map.end()
    }
}

#[test]
fn serde_error_codes_are_pinned() {
    // InvalidType (including radix integer read as f64)
    let invalid_type = from_str::<u32>("\"not-a-number\"").unwrap_err();
    assert_code(
        &invalid_type,
        ErrorKind::Serde,
        ErrorCode::InvalidType,
        "yaml.invalid-type",
    );
    let radix_f64 = from_str::<f64>("0x10").unwrap_err();
    assert_code(
        &radix_f64,
        ErrorKind::Serde,
        ErrorCode::InvalidType,
        "yaml.invalid-type",
    );
    let from_val_type = from_value::<u32>(Value::String("bad".to_owned())).unwrap_err();
    assert_code(
        &from_val_type,
        ErrorKind::Serde,
        ErrorCode::InvalidType,
        "yaml.invalid-type",
    );

    // InvalidValue
    let invalid_value = from_str::<u8>("999").unwrap_err();
    assert_code(
        &invalid_value,
        ErrorKind::Serde,
        ErrorCode::InvalidValue,
        "yaml.invalid-value",
    );
    let invalid_char = from_str::<char>("\"ab\"").unwrap_err();
    assert_code(
        &invalid_char,
        ErrorKind::Serde,
        ErrorCode::InvalidValue,
        "yaml.invalid-value",
    );

    // InvalidLength
    let invalid_length = from_str::<(u32, u32)>("[1]").unwrap_err();
    assert_code(
        &invalid_length,
        ErrorKind::Serde,
        ErrorCode::InvalidLength,
        "yaml.invalid-length",
    );
    let from_val_len =
        from_value::<(u32, u32)>(Value::Sequence(vec![Value::from(1_u64)])).unwrap_err();
    assert_code(
        &from_val_len,
        ErrorKind::Serde,
        ErrorCode::InvalidLength,
        "yaml.invalid-length",
    );

    // UnknownVariant
    let unknown_variant = from_str::<SampleEnum>("Nope").unwrap_err();
    assert_code(
        &unknown_variant,
        ErrorKind::Serde,
        ErrorCode::UnknownVariant,
        "yaml.unknown-variant",
    );

    // UnknownField
    let unknown_field = from_str::<StrictRecord>("a: 1\nb: ok\nextra: 2\n").unwrap_err();
    assert_code(
        &unknown_field,
        ErrorKind::Serde,
        ErrorCode::UnknownField,
        "yaml.unknown-field",
    );

    // MissingField
    let missing_field = from_str::<StrictRecord>("a: 1\n").unwrap_err();
    assert_code(
        &missing_field,
        ErrorKind::Serde,
        ErrorCode::MissingField,
        "yaml.missing-field",
    );

    // DuplicateField (via Serde field alias)
    let dup_field = from_str::<StrictRecord>("a: 1\na_alias: 2\nb: ok\n").unwrap_err();
    assert_code(
        &dup_field,
        ErrorKind::Serde,
        ErrorCode::DuplicateField,
        "yaml.duplicate-field",
    );

    // InvalidEnumRepresentation
    let multi_enum = from_str::<SampleEnum>("{Unit: ~, Tuple: 1}").unwrap_err();
    assert_code(
        &multi_enum,
        ErrorKind::Serde,
        ErrorCode::InvalidEnumRepresentation,
        "yaml.invalid-enum-representation",
    );
    let empty_enum = from_str::<SampleEnum>("{}").unwrap_err();
    assert_code(
        &empty_enum,
        ErrorKind::Serde,
        ErrorCode::InvalidEnumRepresentation,
        "yaml.invalid-enum-representation",
    );
    let non_string_key_enum = from_str::<SampleEnum>("{1: 2}").unwrap_err();
    assert_code(
        &non_string_key_enum,
        ErrorKind::Serde,
        ErrorCode::InvalidEnumRepresentation,
        "yaml.invalid-enum-representation",
    );
    let unit_payload = from_str::<SampleEnum>("Unit: 1").unwrap_err();
    assert_code(
        &unit_payload,
        ErrorKind::Serde,
        ErrorCode::InvalidEnumRepresentation,
        "yaml.invalid-enum-representation",
    );
    let seq_enum = from_str::<SampleEnum>("[1, 2]").unwrap_err();
    assert_code(
        &seq_enum,
        ErrorKind::Serde,
        ErrorCode::InvalidType,
        "yaml.invalid-type",
    );

    // MapValueBeforeKey (tree path, streaming > 4 KiB path, and from_value path)
    let map_before = from_str::<ValueBeforeKey>("a: 1\n").unwrap_err();
    assert_code(
        &map_before,
        ErrorKind::Serde,
        ErrorCode::MapValueBeforeKey,
        "yaml.map-value-before-key",
    );
    let large_map = format!("a: \"{}\"\n", "x".repeat(5000));
    let stream_before = from_str::<ValueBeforeKey>(&large_map).unwrap_err();
    assert_code(
        &stream_before,
        ErrorKind::Serde,
        ErrorCode::MapValueBeforeKey,
        "yaml.map-value-before-key",
    );
    let from_val_before = from_value::<ValueBeforeKey>(parse_value("a: 1\n").unwrap()).unwrap_err();
    assert_code(
        &from_val_before,
        ErrorKind::Serde,
        ErrorCode::MapValueBeforeKey,
        "yaml.map-value-before-key",
    );

    // SerdeCustom
    let custom = from_str::<CustomFail>("1").unwrap_err();
    assert_code(
        &custom,
        ErrorKind::Serde,
        ErrorCode::SerdeCustom,
        "yaml.serde-custom",
    );

    // DuplicateKey from Serialize
    let dup_ser = to_value(&DupSerializeMap).unwrap_err();
    assert_code(
        &dup_ser,
        ErrorKind::Serde,
        ErrorCode::DuplicateKey,
        "yaml.duplicate-key",
    );
}

#[test]
fn emitter_and_io_error_codes_are_pinned() {
    let bad_opts =
        to_string_value_with(&Value::Null, &EmitOptions::new().with_indent(0)).unwrap_err();
    assert_code(
        &bad_opts,
        ErrorKind::Emitter,
        ErrorCode::InvalidEmitterOptions,
        "yaml.invalid-emitter-options",
    );

    let bad_tag = to_string_value(&Value::Tagged(Box::new(ayaml::TaggedValue::new(
        Tag::new("tag:yaml.org,2002:", "int"),
        Value::String("not-an-int".to_owned()),
    ))))
    .unwrap_err();
    assert_code(
        &bad_tag,
        ErrorKind::Emitter,
        ErrorCode::UnsupportedTag,
        "yaml.unsupported-tag",
    );

    let unwriteable_tag = to_string_value(&Value::Tagged(Box::new(ayaml::TaggedValue::new(
        Tag::new("", "x"),
        Value::Null,
    ))))
    .unwrap_err();
    assert_code(
        &unwriteable_tag,
        ErrorKind::Emitter,
        ErrorCode::UnsupportedTag,
        "yaml.unsupported-tag",
    );

    let bad_event = to_string_events(&[Event::DocumentEnd {
        explicit: false,
        span: Span::empty(0),
    }])
    .unwrap_err();
    assert_code(
        &bad_event,
        ErrorKind::Emitter,
        ErrorCode::InvalidEmitEvent,
        "yaml.invalid-emit-event",
    );

    let io_err = to_writer(&mut FailingWriter, &Value::String("hello".to_owned())).unwrap_err();
    assert_code(&io_err, ErrorKind::Io, ErrorCode::Io, "yaml.io");
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_error_codes_are_preserved_across_kind_remappings() {
    // Depth limit remaps ErrorKind to Serde in compat while keeping DepthLimit code.
    let depth_opts =
        ayaml::compat::ParseOptions::new().with_limits(Limits::new().with_max_depth(1));
    let depth = ayaml::compat::from_str_with::<Value>("[[1]]", &depth_opts).unwrap_err();
    assert_code(
        &depth,
        ErrorKind::Serde,
        ErrorCode::DepthLimit,
        "yaml.depth-limit",
    );

    // Trailing documents keep TrailingDocuments code.
    let trailing = ayaml::compat::from_str::<Value>("1\n---\n2\n").unwrap_err();
    assert_code(
        &trailing,
        ErrorKind::Parser,
        ErrorCode::TrailingDocuments,
        "yaml.trailing-documents",
    );

    // Document count limit remaps ErrorKind to Parser while keeping DocumentCountLimit code.
    let doc_opts =
        ayaml::compat::ParseOptions::new().with_limits(Limits::new().with_max_documents(1));
    let doc_limit = ayaml::compat::from_str_with::<Value>("1\n---\n2\n", &doc_opts).unwrap_err();
    assert_code(
        &doc_limit,
        ErrorKind::Parser,
        ErrorCode::DocumentCountLimit,
        "yaml.document-count-limit",
    );

    // Reserved indicator after BOM remaps wording while keeping ReservedIndicator code.
    let bom_reserved = ayaml::compat::from_str::<Value>("\u{feff}@x").unwrap_err();
    assert_code(
        &bom_reserved,
        ErrorKind::Scanner,
        ErrorCode::ReservedIndicator,
        "yaml.reserved-indicator",
    );

    // Unexpected mapping value remaps wording while keeping UnexpectedMappingValue code.
    let bad_val = ayaml::compat::from_str::<Value>("a: b: c\n").unwrap_err();
    assert_code(
        &bad_val,
        ErrorKind::Parser,
        ErrorCode::UnexpectedMappingValue,
        "yaml.unexpected-mapping-value",
    );

    // Unterminated flow collection in compat keeps UnterminatedFlowCollection even when libyaml wording switches.
    let unterminated = ayaml::compat::from_str::<Value>("[a,").unwrap_err();
    assert_code(
        &unterminated,
        ErrorKind::Parser,
        ErrorCode::UnterminatedFlowCollection,
        "yaml.unterminated-flow-collection",
    );

    // Compat duplicate keys reach Serde as DuplicateField.
    let dup_field = ayaml::compat::from_str::<StrictRecord>("a: 1\na: 2\nb: ok\n").unwrap_err();
    assert_code(
        &dup_field,
        ErrorKind::Serde,
        ErrorCode::DuplicateField,
        "yaml.duplicate-field",
    );
}

#[test]
fn error_clone_and_partial_eq_work_across_real_failures() {
    let first = parse_value("a: 1\na: 2\n").unwrap_err();
    let cloned = first.clone();
    let second = parse_value("a: 1\na: 2\n").unwrap_err();
    let different = parse_value("a: 1\nb: 2\nb: 3\n").unwrap_err();

    assert_eq!(first, cloned);
    assert_eq!(first, second);
    assert_ne!(first, different);

    // Custom with_code changes equality even with identical kind and message.
    let a = Error::new(ErrorKind::Parser, "same");
    let b = Error::new(ErrorKind::Parser, "same").with_code(ErrorCode::TrailingContent);
    assert_ne!(a, b);
}

#[test]
fn streaming_typed_reader_preserves_error_codes() {
    let large_bad_type = format!("pad: \"{}\"\nnum: \"oops\"\n", "x".repeat(5000));
    let err = from_str::<LargeTarget>(&large_bad_type).unwrap_err();
    assert_code(
        &err,
        ErrorKind::Serde,
        ErrorCode::InvalidType,
        "yaml.invalid-type",
    );

    let large_missing = format!("pad: \"{}\"\n", "x".repeat(5000));
    let err = from_str::<LargeTarget>(&large_missing).unwrap_err();
    assert_code(
        &err,
        ErrorKind::Serde,
        ErrorCode::MissingField,
        "yaml.missing-field",
    );

    let large_depth = format!("{}{}", "[".repeat(4096), "]".repeat(4096));
    let opts = ParseOptions::new().with_limits(Limits::new().with_max_depth(8));
    let err = from_str_with::<Value>(&large_depth, &opts).unwrap_err();
    assert_code(
        &err,
        ErrorKind::Limit,
        ErrorCode::DepthLimit,
        "yaml.depth-limit",
    );

    // Seed reader preserves Serde error codes too.
    let seed_err = ayaml::from_str_seed("\"nope\"", U32Seed).unwrap_err();
    assert_code(
        &seed_err,
        ErrorKind::Serde,
        ErrorCode::InvalidType,
        "yaml.invalid-type",
    );
}

#[test]
fn every_documented_error_code_is_pinned_by_this_suite() {
    let doc = include_str!("../../../docs/error-codes.md");
    let mut documented = HashSet::new();
    for line in doc.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = trimmed.split('|').map(str::trim).collect();
        if cells.len() >= 4
            && let Some(code_str) = cells[2].strip_prefix('`').and_then(|s| s.strip_suffix('`'))
            && code_str.starts_with("yaml.")
        {
            documented.insert(code_str);
        }
    }

    // Collect every `yaml.<kebab-name>` string asserted in this test file.
    let source = include_str!("error_codes.rs");
    let mut pinned = HashSet::new();
    for part in source.split("\"yaml.").skip(1) {
        if let Some(end) = part.find('"') {
            let suffix = &part[..end];
            if !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                pinned.insert(format!("yaml.{suffix}"));
            }
        }
    }

    let missing: Vec<_> = documented
        .iter()
        .filter(|code| !pinned.contains(**code))
        .copied()
        .collect();
    assert!(
        missing.is_empty(),
        "documented error codes not pinned in error_codes.rs: {missing:?}"
    );
}
