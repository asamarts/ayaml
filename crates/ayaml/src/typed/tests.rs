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
fn a_medium_document_is_built_as_its_events_arrive() {
    use std::fmt::Write as _;

    let mut input = String::from("values:\n");
    for index in 0..500 {
        writeln!(input, "- value-{index}").expect("write");
    }
    assert!(input.len() > SMALL_INPUT_BYTES);
    assert!(input.len() <= 2 * SMALL_INPUT_BYTES);

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
    assert!(input.len() > 2 * SMALL_INPUT_BYTES);

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
    assert!(input.len() > 2 * SMALL_INPUT_BYTES);

    let limits = Limits::for_max_input();
    let error = parse_document(&input, &limits, NodeOptions::default()).expect_err("error");
    assert!(error.to_string().contains("trailing documents"), "{error}");
}

/// A collection with more entries than `max_collection_entries` is refused, so the room that a
/// parser asks the builder to reserve for one stops at that limit.
#[test]
fn a_reserved_collection_stops_at_the_entry_limit() {
    let limits = Limits::default().with_max_collection_entries(3);
    let mut builder = Builder::streaming(
        0,
        &limits,
        NodeOptions::default(),
        Some(Rc::new(RefCell::new(None))),
    );
    EventConsumer::reserve_collection(&mut builder, 2);
    assert_eq!(builder.pending_capacity, Some(2));
    EventConsumer::reserve_collection(&mut builder, 1_000_000);
    assert_eq!(builder.pending_capacity, Some(3));
}

/// A root flow collection past a document marker is built with exactly the room its entries
/// take: the parser counts them and the builder reserves that many before the first arrives.
#[test]
fn a_root_flow_collection_is_built_with_room_for_its_entries() {
    for input in ["[1, 2, 3, 4, 5]\n", "---\n[1, 2, 3, 4, 5]\n"] {
        let node =
            parse_streaming(input, &Limits::default(), NodeOptions::default()).expect("a node");
        let NodeKind::Sequence(items) = &node.kind else {
            panic!("{input:?}: a sequence");
        };
        assert_eq!((items.len(), items.capacity()), (5, 5), "{input:?}");
    }
    let input = "{a: 1, b: 2, c: 3}\n";
    let node = parse_streaming(input, &Limits::default(), NodeOptions::default()).expect("a node");
    let NodeKind::Mapping(entries) = &node.kind else {
        panic!("{input:?}: a mapping");
    };
    assert_eq!((entries.len(), entries.capacity()), (3, 3), "{input:?}");
}

/// The room is for the root collection alone: a collection inside it starts with none, so a
/// count of a hundred thousand entries is not reserved again for each of them.
#[test]
fn the_room_reserved_for_a_root_collection_is_not_reserved_for_its_items() {
    let input = "[[1], {a: 1}, [], 4]\n";
    let node = parse_streaming(input, &Limits::default(), NodeOptions::default()).expect("a node");
    let NodeKind::Sequence(items) = &node.kind else {
        panic!("a sequence");
    };
    assert_eq!((items.len(), items.capacity()), (4, 4));
    let NodeKind::Sequence(inner) = &items[0].kind else {
        panic!("a sequence in the sequence");
    };
    assert!(inner.capacity() < 16, "{}", inner.capacity());
    let NodeKind::Mapping(inner) = &items[1].kind else {
        panic!("a mapping in the sequence");
    };
    assert!(inner.capacity() < 16, "{}", inner.capacity());
    let NodeKind::Sequence(inner) = &items[2].kind else {
        panic!("an empty sequence in the sequence");
    };
    assert_eq!(inner.capacity(), 0);
}

/// With duplicate keys refused, every key of an open mapping has a slot in a set. Nearly every
/// key is a scalar, so a slot is as large as a scalar's text and no larger: a node in it, for
/// the rare key that is a collection, would more than triple the set.
#[test]
fn a_seen_key_is_no_larger_than_a_scalar_s_text() {
    assert_eq!(
        std::mem::size_of::<SeenKey<'_>>(),
        std::mem::size_of::<std::borrow::Cow<'_, str>>()
    );
    assert!(std::mem::size_of::<Node<'_>>() > 3 * std::mem::size_of::<SeenKey<'_>>());
}
