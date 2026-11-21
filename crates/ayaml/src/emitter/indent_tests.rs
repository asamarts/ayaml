#[test]
fn custom_indent_is_used_for_nested_blocks() {
    let value = crate::parse_value("a:\n  b: 1\n").expect("parse");
    let output = crate::to_string_value_with(&value, &crate::EmitOptions::new().with_indent(4))
        .expect("emit");
    assert!(output.contains("a:\n    b: 1"), "{output:?}");
    assert_eq!(crate::parse_value(&output).expect("reparse"), value);
}
