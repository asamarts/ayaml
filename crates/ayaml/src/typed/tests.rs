use super::*;

fn build(input: &str) -> Node<'_> {
    let limits = Limits::default();
    let events = parser::parse_events(input, &limits).expect("events");
    super::build(
        events,
        input.len(),
        &limits,
        NodeOptions {
            allow_duplicates: true,
            ..NodeOptions::default()
        },
        &mut AliasBudget::default(),
    )
    .expect("node")
}

#[test]
fn a_node_stays_small() {
    assert!(size_of::<Node<'_>>() <= 80);
}

#[test]
fn aliases_are_reference_nodes_not_expanded_copies() {
    let node = build("a: &a {x: 1}\nb: *a\n");
    let NodeKind::Mapping(entries) = &node.kind else {
        panic!("root mapping");
    };
    let value = entries
        .iter()
        .find(|(key, _)| matches!(&key.kind, NodeKind::String(text) if text == "b"))
        .map(|(_, value)| value)
        .expect("b value");
    let NodeKind::Alias(target) = &value.kind else {
        panic!("alias node");
    };
    let NodeKind::Mapping(target_entries) = &target.kind else {
        panic!("alias target mapping");
    };
    assert_eq!(target_entries.len(), 1);
}

#[test]
fn a_medium_document_uses_the_chunked_event_feed() {
    use std::fmt::Write as _;

    let mut input = String::from("values:\n");
    for index in 0..500 {
        writeln!(input, "- value-{index}").expect("write");
    }
    assert!(input.len() > CHUNKED_INPUT_THRESHOLD);
    assert!(input.len() <= STREAMING_INPUT_THRESHOLD);

    let limits = Limits::for_max_input();
    let node = parse_document(&input, &limits, NodeOptions::default()).expect("node");
    let NodeKind::Mapping(entries) = &node.kind else {
        panic!("root mapping");
    };
    let (_, values) = entries.first().expect("values entry");
    let NodeKind::Sequence(values) = &values.kind else {
        panic!("values sequence");
    };
    assert_eq!(values.len(), 500);
    assert!(matches!(
        &values[499].kind,
        NodeKind::String(value) if value == "value-499"
    ));
}

#[test]
fn a_larger_document_streams_events_into_the_builder() {
    let mut input =
        String::from("base: &base {x: 1}\ntagged: !custom value\nblock: |\n  text\nvalues:\n");
    for _ in 0..6_000 {
        input.push_str("- *base\n");
    }
    assert!(input.len() > STREAMING_INPUT_THRESHOLD);

    let limits = Limits::for_max_input();
    let node = parse_document(&input, &limits, NodeOptions::default()).expect("node");
    let NodeKind::Mapping(entries) = &node.kind else {
        panic!("root mapping");
    };
    assert!(matches!(entries[1].1.kind, NodeKind::Tagged(_, _)));
    assert!(matches!(
        &entries[2].1.kind,
        NodeKind::String(text) if text == "text\n"
    ));
    let (_, values) = entries.get(3).expect("values entry");
    let NodeKind::Sequence(values) = &values.kind else {
        panic!("values sequence");
    };
    assert_eq!(values.len(), 6_000);
    assert!(
        values
            .iter()
            .all(|value| matches!(value.kind, NodeKind::Alias(_)))
    );
}

#[test]
fn a_large_stream_reports_trailing_documents() {
    use std::fmt::Write as _;

    let mut input = String::from("values:\n");
    for index in 0..1_500 {
        writeln!(input, "- {index}").expect("write");
    }
    input.push_str("---\nother: 1\n");
    assert!(input.len() > STREAMING_INPUT_THRESHOLD);

    let limits = Limits::for_max_input();
    let error = parse_document(&input, &limits, NodeOptions::default()).expect_err("error");
    assert!(error.to_string().contains("trailing documents"), "{error}");
}
