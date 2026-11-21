use crate::event::ScalarStyle;
use crate::value::Value;

#[test]
fn multiline_value_strings_emit_as_block_scalars() {
    for text in ["a\nb\n", "a\nb", "a\n\n", "line\n\nnext\n"] {
        let value = Value::from(text);
        let output = crate::to_string_value(&value).expect("emit");
        assert!(output.starts_with('|'), "output={output:?}");
        assert_eq!(
            crate::parse_value(&output).expect("reparse"),
            value,
            "output={output:?}"
        );
    }
}

#[test]
fn forced_block_scalar_style_round_trips() {
    let value = Value::from("plain text");
    let output = crate::to_string_value_with(
        &value,
        &crate::EmitOptions::new().with_scalar_style(Some(ScalarStyle::Literal)),
    )
    .expect("literal");
    assert!(output.starts_with("|-"));
    assert_eq!(crate::parse_value(&output).unwrap(), value);

    let output = crate::to_string_value_with(
        &value,
        &crate::EmitOptions::new().with_scalar_style(Some(ScalarStyle::Folded)),
    )
    .expect("folded");
    assert!(output.starts_with(">-"));
    assert_eq!(crate::parse_value(&output).unwrap(), value);
}
