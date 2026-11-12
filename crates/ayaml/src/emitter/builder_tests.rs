use super::{Emitter, EmitterOptions, EmitterStyles, NodeStyle};
use crate::value::Value;

#[test]
fn builder_emits_values_writers_and_documents() {
    let limits = crate::Limits::default();
    let options = EmitterOptions::default().with_flow(true);
    let emitter = Emitter::new(&limits, &options);
    let value = crate::parse_value("a: 1\n").expect("parse");

    let text = emitter.value_to_string(&value).expect("string");
    assert_eq!(text, "{a: 1}\n");
    assert_eq!(crate::parse_value(&text).expect("reparse"), value);

    let mut output = Vec::new();
    emitter.value_to_writer(&mut output, &value).expect("write");
    assert_eq!(String::from_utf8(output).expect("utf8"), text);

    let documents = [value, Value::from("next")];
    let stream = emitter.documents_to_string(&documents).expect("documents");
    assert_eq!(stream, "{a: 1}\n---\nnext\n");
}

#[test]
fn builder_applies_per_node_styles() {
    let limits = crate::Limits::default();
    let options = EmitterOptions::default();
    let mut styles = EmitterStyles::new();
    styles.set(
        vec![crate::PathSegment::key("a")],
        NodeStyle::default().with_scalar_style(Some(crate::ScalarStyle::SingleQuoted)),
    );
    let emitter = Emitter::new(&limits, &options).with_styles(&styles);
    let value = crate::parse_value("a: plain\n").expect("parse");
    assert_eq!(
        emitter.value_to_string(&value).expect("emit"),
        "a: 'plain'\n"
    );
}
