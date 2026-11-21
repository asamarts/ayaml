use crate::event::ScalarStyle;
use crate::tag::{Tag, TaggedValue};
use crate::value::Value;
use crate::{EmitOptions, Limits, ParseOptions};

#[test]
fn flow_and_document_options_round_trip() {
    let value = crate::parse_value("a:\n  - 1\n  - 2\n").expect("parse");
    let flow = crate::to_string_value_with(
        &value,
        &EmitOptions::flow()
            .with_explicit_start(true)
            .with_explicit_end(true),
    )
    .expect("flow emit");
    assert!(flow.starts_with("---\n{"));
    assert!(flow.ends_with("...\n"));
    let decoded = crate::parse_value(&flow).expect("flow reparse");
    assert_eq!(decoded, value);

    let first = crate::parse_value("a: 1\n").expect("first");
    let second = crate::parse_value("b: 2\n").expect("second");
    let documents = crate::to_string_documents(&[first, second]).expect("documents");
    let options = ParseOptions::new().with_limits(Limits::new().with_max_documents(2));
    let decoded = crate::parse_documents_with(&documents, &options).expect("reparse documents");
    assert_eq!(decoded.len(), 2);
    assert_eq!(decoded[1]["b"], Value::from(2_u64));
}

#[test]
fn ambiguous_strings_round_trip() {
    let strings = [
        "",
        "null",
        "true",
        "0123",
        "0b11",
        "1e3",
        "a: b",
        "a #b",
        " leading",
        "trailing ",
        "-",
        "?",
        ":",
        "---",
        "...",
        "yes",
        "line\nbreak",
        "quote\"slash\\",
        "é",
    ];
    let value = Value::Sequence(strings.iter().map(|text| Value::from(*text)).collect());
    let text = crate::to_string_value(&value).expect("emit");
    let decoded = crate::parse_value(&text).expect("reparse");
    assert_eq!(decoded, value, "output={text:?}");
}

#[test]
fn scalar_style_options_are_applied() {
    let value = Value::from("plain text");
    let text = crate::to_string_value_with(
        &value,
        &EmitOptions::new().with_scalar_style(Some(ScalarStyle::SingleQuoted)),
    )
    .expect("single quote");
    assert_eq!(text, "'plain text'\n");

    let text = crate::to_string_value_with(
        &value,
        &EmitOptions::new().with_scalar_style(Some(ScalarStyle::DoubleQuoted)),
    )
    .expect("double quote");
    assert_eq!(text, "\"plain text\"\n");
    assert_eq!(crate::parse_value(&text).unwrap(), value);
}

#[test]
fn output_limits_are_enforced_for_values_events_and_documents() {
    let value = Value::from("long enough to exceed the limit");
    let emit_options = EmitOptions::new().with_limits(Limits::new().with_max_output_bytes(3));
    let error = crate::to_string_value_with(&value, &emit_options).expect_err("value limit");
    assert_eq!(error.kind(), crate::ErrorKind::Limit);

    let parse_options = ParseOptions::new().with_limits(Limits::new().with_max_documents(2));
    let documents =
        crate::parse_documents_with("a: 1\n---\nb: 2\n", &parse_options).expect("documents");
    let error =
        crate::to_string_documents_with(&documents, &emit_options).expect_err("document limit");
    assert_eq!(error.kind(), crate::ErrorKind::Limit);

    let events: Vec<crate::Event<'_>> = crate::parse_events("a: 1\n")
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let error = crate::to_string_events_with(&events, &emit_options).expect_err("event limit");
    assert_eq!(error.kind(), crate::ErrorKind::Limit);
}

#[test]
fn expanded_tags_use_verbatim_syntax() {
    let value = Value::from(TaggedValue::new(
        Tag::new("tag:example.com,2026:", "foo"),
        Value::from("x"),
    ));
    let text = crate::to_string_value(&value).expect("emit");
    assert!(text.contains("!<tag:example.com,2026:foo>"), "{text:?}");
    let decoded = crate::parse_value(&text).expect("reparse");
    assert!(decoded.is_tagged());
}

#[test]
fn writer_options_apply_and_round_trip() {
    let value = crate::parse_value("a: 1\n").expect("parse");
    let mut output = Vec::new();
    crate::to_writer_value_with(
        &mut output,
        &value,
        &EmitOptions::flow().with_explicit_start(true),
    )
    .expect("write");
    let text = std::str::from_utf8(&output).expect("utf8");
    assert!(text.starts_with("---\n{"));
    assert_eq!(crate::parse_value(text).expect("reparse"), value);
}

#[test]
fn value_writer_is_incremental_and_limits_output() {
    let value = crate::parse_value("a: 1\n").expect("parse");
    let mut output = Vec::new();
    crate::to_writer_value(&mut output, &value).expect("write");
    assert_eq!(
        std::str::from_utf8(&output).expect("utf8"),
        crate::to_string_value(&value).expect("string")
    );

    let options = EmitOptions::new().with_limits(Limits::new().with_max_output_bytes(2));
    let mut limited = Vec::new();
    let error = crate::to_writer_value_with(&mut limited, &value, &options).expect_err("limit");
    assert_eq!(error.kind(), crate::ErrorKind::Limit);
    assert!(limited.len() <= 2);
}
