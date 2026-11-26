use crate::value::Value;

#[test]
fn existing_tagged_values_round_trip_through_the_value_emitter() {
    for input in ["!Custom text\n", "!Unit\n", "!Tag [a, b]\n"] {
        let value = crate::parse_value(input).expect("parse");
        let text = crate::to_string_value(&value).expect("emit");
        let decoded = crate::parse_value(&text).expect("reparse");
        assert_eq!(decoded, value, "input={input:?} output={text:?}");
        assert!(matches!(decoded, Value::Tagged(_)));
    }
}
