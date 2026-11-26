#[test]
fn mapping_sequences_are_indentless_and_sequence_mappings_are_compact() {
    let value = crate::parse_value("traces:\n  - a\n  - b\nitems:\n  - k: v\n").unwrap();
    let text = crate::to_string_value(&value).expect("emit");
    assert_eq!(text, "traces:\n- a\n- b\nitems:\n- k: v\n");
    assert_eq!(crate::parse_value(&text).unwrap(), value);
}

#[test]
fn block_scalars_need_no_flow_quoting_and_control_escapes_are_named() {
    assert_eq!(crate::to_string("a, b").unwrap(), "a, b\n");
    assert_eq!(crate::to_string("a[b").unwrap(), "a[b\n");
    assert_eq!(crate::to_string("\u{1b}[41m").unwrap(), "\"\\e[41m\"\n");
}

#[test]
fn nested_and_tagged_collections_round_trip() {
    for input in [
        "key:\n  - a\n  - b\n",
        "key: !Tag\n  - a\n  - b\n",
        "key: !Tag\n  a: b\n",
        "1: one\n2:\n  nested: true\n",
        "empty: {}\nitems: []\n",
        "? [a, b]\n: value\n",
    ] {
        let value = crate::parse_value(input).expect("parse");
        let text = crate::to_string_value(&value).expect("emit");
        let decoded = crate::parse_value(&text).expect("reparse");
        assert_eq!(decoded, value, "input={input:?} output={text:?}");
    }
}
