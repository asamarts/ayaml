#[test]
fn tagged_multiline_strings_emit_as_tagged_block_scalars() {
    let value = crate::parse_value("!Custom |\n  a\n  b\n").expect("parse");
    let output = crate::to_string_value(&value).expect("emit");
    assert!(output.starts_with("!Custom |"), "{output:?}");
    let decoded = crate::parse_value(&output).expect("reparse");
    assert_eq!(decoded, value);

    let value = crate::parse_value("key: !Custom |\n  a\n  b\n").expect("parse");
    let output = crate::to_string_value(&value).expect("emit");
    assert!(output.contains("!Custom |"), "{output:?}");
    assert_eq!(crate::parse_value(&output).expect("reparse"), value);
}
