use super::{
    TOKEN_ARENA_CHUNK, TOKEN_ARENA_MID_CHUNK, TOKEN_ARENA_SMALL_CHUNK, arena_growth, parse_events,
    push_token,
};
use crate::error::ErrorKind;
use crate::event::Event;
use crate::limits::Limits;
use crate::scanner::{Token, TokenKind};
use crate::span::{Marker, Span};

fn events(input: &str) -> Vec<Event<'_>> {
    parse_events(input, &Limits::default()).expect("parse")
}

/// The tokens a `realloc` that copies would move while the arena grows to `tokens`, and the
/// arena's capacity then.
fn arena_copies(tokens: usize, chunk: usize) -> (usize, usize) {
    let (mut capacity, mut copied) = (0_usize, 0_usize);
    while capacity < tokens {
        copied += capacity;
        capacity += arena_growth(capacity, chunk);
    }
    (copied, capacity)
}

/// A growing arena costs copies linear in its length, at most 17 times it, with less than a
/// sixteenth of it, or one chunk, unused. Growing by a fixed chunk copied quadratically: about
/// 37 times as much at ten million tokens, and the gap grows with the length. Up to 16 chunks,
/// the arena grows by chunks alone.
#[test]
fn the_token_arena_grows_with_linear_copying_and_bounded_waste() {
    for chunk in [TOKEN_ARENA_SMALL_CHUNK, TOKEN_ARENA_CHUNK] {
        for tokens in [
            1,
            chunk,
            chunk + 1,
            16 * chunk + 1,
            100_000,
            1_000_000,
            10_000_000,
        ] {
            let (copied, capacity) = arena_copies(tokens, chunk);
            assert!(copied <= 17 * tokens, "{tokens} tokens: {copied} copied");
            assert!(
                capacity - tokens <= chunk.max(tokens / 16),
                "{tokens} tokens: capacity {capacity}"
            );
        }
        for len in [0, chunk, 16 * chunk] {
            assert_eq!(arena_growth(len, chunk), chunk);
        }
        assert!(arena_growth(17 * chunk, chunk) > chunk);
    }
    let fixed: usize = (0..10_000_000 / TOKEN_ARENA_CHUNK)
        .map(|n| n * TOKEN_ARENA_CHUNK)
        .sum();
    assert!(fixed > 35 * arena_copies(10_000_000, TOKEN_ARENA_CHUNK).0);
}

/// The arena grows only when it's full, and then by what `arena_growth` gives and no more, in
/// each chunk the parser uses. A `Vec` takes the capacity `reserve_exact` asks for.
#[test]
fn push_token_grows_a_full_arena_as_arena_growth_says() {
    let token = Token {
        kind: TokenKind::FlowEntry,
        span: Span::empty(Marker::new(0, 1, 1)),
    };
    for chunk in [
        TOKEN_ARENA_SMALL_CHUNK,
        TOKEN_ARENA_MID_CHUNK,
        TOKEN_ARENA_CHUNK,
    ] {
        let mut tokens = Vec::new();
        let (mut capacity, mut growths) = (0, 0);
        for _ in 0..40 * chunk {
            let before = tokens.capacity();
            push_token(&mut tokens, token.clone(), chunk);
            if tokens.capacity() != before {
                assert_eq!(before + 1, tokens.len(), "{chunk}: grew before it was full");
                capacity += arena_growth(before, chunk);
                growths += 1;
            }
            assert_eq!(
                tokens.capacity(),
                capacity,
                "{chunk}: {} tokens",
                tokens.len()
            );
        }
        // A chunk at a time to 17 chunks, then 15 sixteenths to 42 chunks.
        assert_eq!(growths, 32, "{chunk}");
    }
}

fn event_kinds(input: &str) -> Vec<&'static str> {
    events(input)
        .iter()
        .map(|event| match event {
            Event::StreamStart { .. } => "stream-start",
            Event::StreamEnd { .. } => "stream-end",
            Event::DocumentStart { .. } => "doc-start",
            Event::DocumentEnd { .. } => "doc-end",
            Event::Scalar { .. } => "scalar",
            Event::SequenceStart { .. } => "seq-start",
            Event::SequenceEnd { .. } => "seq-end",
            Event::MappingStart { .. } => "map-start",
            Event::MappingEnd { .. } => "map-end",
            Event::Directive { .. } => "directive",
            Event::Alias { .. } => "alias",
        })
        .collect()
}

#[test]
fn simple_mapping_events() {
    let kinds = event_kinds("a: 1\nb: 2\n");
    assert_eq!(
        kinds,
        vec![
            "stream-start",
            "doc-start",
            "map-start",
            "scalar",
            "scalar",
            "scalar",
            "scalar",
            "map-end",
            "doc-end",
            "stream-end",
        ]
    );
}

#[test]
fn block_sequence_events() {
    let kinds = event_kinds("- a\n- b\n");
    assert_eq!(
        kinds,
        vec![
            "stream-start",
            "doc-start",
            "seq-start",
            "scalar",
            "scalar",
            "seq-end",
            "doc-end",
            "stream-end",
        ]
    );
}

#[test]
fn sequence_of_mappings_events() {
    let input = "- id: a\n  x: 1\n- id: b\n  x: 2\n";
    let kinds = event_kinds(input);
    assert!(kinds.contains(&"seq-start"));
    assert_eq!(kinds.iter().filter(|kind| **kind == "map-start").count(), 2);
    assert_eq!(kinds.iter().filter(|kind| **kind == "map-end").count(), 2);
}

#[test]
fn nested_sequence_events() {
    let kinds = event_kinds("- - a\n  - b\n");
    assert_eq!(kinds.iter().filter(|kind| **kind == "seq-start").count(), 2);
    assert_eq!(kinds.iter().filter(|kind| **kind == "seq-end").count(), 2);
}

#[test]
fn flow_sequence_and_mapping_events() {
    let kinds = event_kinds("a: [1, {b: 2}]\n");
    assert!(kinds.contains(&"seq-start"));
    assert!(kinds.contains(&"map-start"));
    assert!(kinds.contains(&"seq-end"));
    assert!(kinds.contains(&"map-end"));
}

#[test]
fn anchors_tags_and_aliases_are_events() {
    let input = "base: &b !Custom {x: 1}\nuse: *b\n";
    let events = events(input);
    assert!(events.iter().any(|event| matches!(event, Event::MappingStart { anchor: Some(anchor), tag: Some(tag), .. } if anchor.as_str() == "b" && tag.to_string() == "!Custom")));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Alias { anchor, .. } if anchor.as_str() == "b"))
    );
}

#[test]
fn multi_document_limit_is_enforced() {
    let limits = Limits {
        max_documents: 2,
        ..Limits::default()
    };
    let events = parse_events("a: 1\n---\nb: 2\n", &limits).expect("parse");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::DocumentStart { .. }))
            .count(),
        2
    );
    let error = parse_events("a: 1\n---\nb: 2\n", &Limits::compat()).expect_err("limit");
    assert_eq!(error.kind(), ErrorKind::Limit);
}

#[test]
fn toolchain_fixture_parses_to_events() {
    let input = include_str!("../../../../tests/fixtures/compat/toolchain-pins.yml");
    let events = parse_events(input, &Limits::default()).expect("toolchain fixture");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::MappingStart { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::SequenceStart { .. }))
    );
}

#[test]
fn scalar_tags_fixture_parses_to_events() {
    let input = include_str!("../../../../tests/fixtures/compat/scalar-tags-merges-aliases.yml");
    let events = parse_events(input, &Limits::default()).expect("scalar fixture");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Alias { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::MappingStart { tag: Some(_), .. }))
    );
}

#[test]
fn empty_document_is_a_null_scalar() {
    let events = events("---\n");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Scalar { value, .. } if value.is_empty()))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::DocumentStart { explicit: true, .. }))
    );
}

#[test]
fn nested_block_mapping_and_sequence() {
    let input = "root:\n  items:\n    - one\n    - two\n  next: 3\n";
    let kinds = event_kinds(input);
    assert_eq!(kinds.iter().filter(|kind| **kind == "map-start").count(), 2);
    assert_eq!(kinds.iter().filter(|kind| **kind == "seq-start").count(), 1);
}

#[test]
fn scalar_spans_slice_the_original_input() {
    let input = "name: \"ayaml\"\n";
    let events = events(input);
    let scalar = events
        .iter()
        .find_map(|event| match event {
            Event::Scalar { value, span, .. } if value == "ayaml" => Some(*span),
            _ => None,
        })
        .expect("scalar");
    assert_eq!(scalar.slice(input), Some("\"ayaml\""));
}

#[test]
fn comments_only_is_an_empty_stream() {
    let events = events("# comment only\n");
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0], Event::StreamStart { .. }));
    assert!(matches!(events[1], Event::StreamEnd { .. }));
}

#[test]
fn bom_crlf_and_comments_parse() {
    let events = events("\u{feff}a: 1 # comment\r\nb: 2\r\n");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::MappingStart { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Scalar { value, .. } if value == "1"))
    );
}

#[test]
fn malformed_flow_is_a_parser_or_scanner_error() {
    let error = parse_events("a: [1,\n", &Limits::default()).expect_err("malformed flow");
    assert!(matches!(
        error.kind(),
        ErrorKind::Parser | ErrorKind::Scanner
    ));
}

#[test]
fn block_scalar_events_are_emitted() {
    let input = "a: |\n  line1\n  line2\n";
    let events = events(input);
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar {
            value,
            style: crate::event::ScalarStyle::Literal,
            span,
            ..
        } if value == "line1\nline2\n" && span.slice(input) == Some("|\n  line1\n  line2\n")
    )));
}

#[test]
fn explicit_and_complex_keys_are_supported() {
    let kinds = event_kinds("? key\n: value\n");
    assert!(kinds.contains(&"map-start"));
    assert!(kinds.contains(&"map-end"));

    let kinds = event_kinds("? [a, b]\n: value\n");
    assert!(kinds.contains(&"seq-start"));
    assert!(kinds.contains(&"map-start"));

    let kinds = event_kinds(": empty-key-value\n");
    assert!(kinds.contains(&"map-start"));
}

#[test]
fn explicit_key_can_share_a_line_with_its_value() {
    let kinds = event_kinds("? key : value\n");
    assert!(kinds.contains(&"map-start"));
    assert!(kinds.contains(&"scalar"));
}

#[test]
fn flow_explicit_and_empty_keys_are_supported() {
    let kinds = event_kinds("a: {? key: value}\n");
    assert!(kinds.contains(&"map-start"));
    assert!(kinds.contains(&"scalar"));

    let kinds = event_kinds("a: {? : value}\n");
    assert!(kinds.contains(&"map-start"));
    assert!(kinds.contains(&"scalar"));
}

#[test]
fn flow_explicit_key_with_complex_key() {
    let kinds = event_kinds("a: {? [x, y]: value}\n");
    assert!(kinds.contains(&"seq-start"));
    assert!(kinds.contains(&"map-start"));
}

#[test]
fn multiline_plain_scalars_fold() {
    let first_events = events("key: first\n  second\nother: x\n");
    assert!(first_events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, style: crate::event::ScalarStyle::Plain, .. }
            if value == "first second"
    )));
    assert!(first_events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, .. } if value == "x"
    )));

    let second_events = events("key:\n  first\n  second\n");
    assert!(second_events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, .. } if value == "first second"
    )));
}

#[test]
fn plain_scalar_folding_stops_at_a_mapping_key() {
    let input = "a: first\n  second\n  nested: value\n";
    let error = parse_events(input, &Limits::default()).expect_err("invalid nested mapping");
    assert_eq!(error.kind(), ErrorKind::Parser);
}

#[test]
fn block_scalar_does_not_swallow_the_next_line() {
    let events = events("literal: |\n  some\n  text\nfolded: >\n  some\n  text\n");
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, style: crate::event::ScalarStyle::Literal, .. } if value == "some\ntext\n"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, .. } if value == "folded"
    )));
}

#[test]
fn indentless_sequences_are_mapping_values() {
    let events = events("key:\n- a\n- b\nother: c\n");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::SequenceStart { .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, .. } if value == "b"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, .. } if value == "other"
    )));
}

#[test]
fn standalone_properties_attach_to_the_next_node() {
    let tagged = events("--- !Custom\n? a\n: b\n");
    assert!(tagged.iter().any(|event| matches!(
        event,
        Event::MappingStart { tag: Some(tag), .. } if tag.to_string() == "!Custom"
    )));

    let anchored = events("&a\n- x\n");
    assert!(anchored.iter().any(|event| matches!(
        event,
        Event::SequenceStart { anchor: Some(anchor), .. } if anchor.as_str() == "a"
    )));
}

#[test]
fn property_only_mapping_values_become_null_or_nested_nodes() {
    let null_anchored = events("a: &x\nb: *x\n");
    assert!(null_anchored.iter().any(|event| matches!(
        event,
        Event::Scalar { value, anchor: Some(anchor), .. }
            if value.is_empty() && anchor.as_str() == "x"
    )));
    assert!(null_anchored.iter().any(|event| matches!(
        event,
        Event::Alias { anchor, .. } if anchor.as_str() == "x"
    )));

    let nested = events("a: &x\n  b: c\n");
    assert!(nested.iter().any(|event| matches!(
        event,
        Event::MappingStart { anchor: Some(anchor), .. } if anchor.as_str() == "x"
    )));
}

#[test]
fn flow_collections_span_lines_and_allow_omitted_values() {
    let sequence = events("[\n  a,\n  b\n]\n");
    assert!(sequence.iter().any(|event| matches!(
        event,
        Event::Scalar { value, .. } if value == "b"
    )));
    let mapping = events("{a, b: c}\n");
    assert!(mapping.iter().any(|event| matches!(
        event,
        Event::Scalar { value, .. } if value.is_empty()
    )));
}

#[test]
fn implicit_documents_may_follow_explicit_end_markers() {
    let limits = Limits {
        max_documents: 2,
        ..Limits::default()
    };
    let events = parse_events("--- a\n...\nb: c\n", &limits).expect("parse");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::DocumentStart { .. }))
            .count(),
        2
    );
}

#[test]
fn block_scalar_header_at_eof_is_an_empty_scalar() {
    let events = events("--- |1-");
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, style: crate::event::ScalarStyle::Literal, .. }
            if value.is_empty()
    )));
}

#[test]
fn standalone_properties_do_not_cross_a_dedent() {
    let events = events("key:\n  &a\nother: x\n");
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, anchor: Some(anchor), .. }
            if value.is_empty() && anchor.as_str() == "a"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, .. } if value == "other"
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        Event::MappingStart {
            anchor: Some(_),
            ..
        }
    )));
}

#[test]
fn tag_directives_expand_custom_handles() {
    let events = events("%TAG !e! tag:example.com,2020:\n--- !e!foo bar\n");
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { tag: Some(tag), .. }
            if tag.handle() == "tag:example.com,2020:" && tag.suffix() == "foo"
    )));
}

#[test]
fn undefined_tag_handles_are_errors() {
    let error =
        parse_events("--- !missing!foo bar\n", &Limits::default()).expect_err("undefined handle");
    assert_eq!(error.kind(), ErrorKind::Scanner);
}

#[test]
fn block_mapping_key_properties_do_not_anchor_the_mapping() {
    let events = events("&a a: &b b\n*b : *a\n");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::MappingStart { anchor: None, .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, anchor: Some(anchor), .. }
            if value == "a" && anchor.as_str() == "a"
    )));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Alias { .. }))
            .count(),
        2
    );
}

#[test]
fn sequence_item_mapping_key_properties_belong_to_the_key() {
    let events = events("- &a key: value\n");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::MappingStart { anchor: None, .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, anchor: Some(anchor), .. }
            if value == "key" && anchor.as_str() == "a"
    )));
}

#[test]
fn block_scalars_allow_tabs_in_content_and_root_indentation() {
    let tabbed = events("foo: |-\n \tbar\n");
    assert!(tabbed.iter().any(|event| matches!(
        event,
        Event::Scalar { value, style: crate::event::ScalarStyle::Literal, .. }
            if value == "\tbar"
    )));
    let root = events("--- >\nline1\nline2\n");
    assert!(root.iter().any(|event| matches!(
        event,
        Event::Scalar { value, style: crate::event::ScalarStyle::Folded, .. }
            if value == "line1 line2\n"
    )));
}

#[test]
fn flow_sequences_accept_implicit_mappings_and_adjacent_colons() {
    let events = events("[foo: bar, \"key\":value]\n");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::MappingStart { .. }))
            .count(),
        2
    );
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, .. } if value == "value"
    )));
}

#[test]
fn explicit_keys_support_null_and_block_sequence_values() {
    let null_keys = events("? a\n? b\nc:\n");
    assert!(
        null_keys
            .iter()
            .filter(|event| matches!(event, Event::Scalar { value, .. } if value.is_empty()))
            .count()
            >= 2
    );

    let sequence_key = events("? - a\n  - b\n: c\n");
    assert!(
        sequence_key
            .iter()
            .any(|event| matches!(event, Event::SequenceStart { .. }))
    );
    assert!(sequence_key.iter().any(|event| matches!(
        event,
        Event::Scalar { value, .. } if value == "c"
    )));
}

#[test]
fn property_only_mapping_keys_are_null_scalars() {
    let events = events("- !!str : !!null\n");
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, tag: Some(tag), .. }
            if value.is_empty() && tag.to_string() == "!!str"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, tag: Some(tag), .. }
            if value.is_empty() && tag.to_string() == "!!null"
    )));
}

#[test]
fn multiline_plain_sequence_items_may_start_with_a_dash() {
    let events = events("- single multiline\n - sequence entry\n");
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, .. } if value == "single multiline - sequence entry"
    )));
}

#[test]
fn invalid_document_and_directive_forms_are_rejected() {
    for input in [
        "... invalid\n",
        "%YAML 1.2\n",
        "%YAML 1.2 foo\n---\n",
        "%YAML 1.2\n%YAML 1.2\n---\n",
    ] {
        assert!(
            parse_events(input, &Limits::default()).is_err(),
            "accepted invalid input {input:?}"
        );
    }
}

#[test]
fn implicit_keys_must_stay_on_one_line() {
    let error = parse_events("\"a\n b\": 1\n", &Limits::default()).expect_err("multiline key");
    assert_eq!(error.kind(), ErrorKind::Parser);
}

#[test]
fn aliases_cannot_carry_properties() {
    let error = parse_events("a: &x *y\n", &Limits::default()).expect_err("tagged alias");
    assert_eq!(error.kind(), ErrorKind::Parser);
}

#[test]
fn empty_flow_entries_are_rejected() {
    assert!(parse_events("[ , a ]\n", &Limits::default()).is_err());
    assert!(parse_events("{ , a: b }\n", &Limits::default()).is_err());
}

#[test]
fn compact_block_sequences_are_only_valid_after_explicit_value_lines() {
    let error = parse_events("key: - a\n", &Limits::default()).expect_err("inline sequence");
    assert_eq!(error.kind(), ErrorKind::Parser);

    let events = events("? - a\n: - b\n");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::SequenceStart { .. }))
    );
}

#[test]
fn compact_mapping_explicit_key_and_value_are_supported() {
    let events = events("- ? earth: blue\n  : moon: white\n");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::MappingStart { .. }))
            .count(),
        3
    );
}

#[test]
fn block_scalar_header_comments_require_whitespace() {
    let error =
        parse_events("key: ># comment\n  x\n", &Limits::default()).expect_err("header comment");
    assert_eq!(error.kind(), ErrorKind::Scanner);
}

#[test]
fn quoted_continuations_require_parent_indentation() {
    assert!(parse_events("key: \"a\nb\"\n", &Limits::default()).is_err());
    let events = events("--- \"a\nb\"\n");
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, .. } if value == "a b"
    )));
}

#[test]
fn tags_and_flow_markers_require_separation() {
    assert!(parse_events("!invalid{}tag scalar\n", &Limits::default()).is_err());
    assert!(parse_events("[a,\n--- ,\n]\n", &Limits::default()).is_err());
    assert!(parse_events("[a,#comment\n]\n", &Limits::default()).is_err());
}

#[test]
fn property_only_flow_tags_before_delimiters_are_valid() {
    let events = events("{ !!str, foo: bar }\n");
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, tag: Some(tag), .. }
            if value.is_empty() && tag.to_string() == "!!str"
    )));
}

#[test]
fn properties_cannot_precede_compact_block_sequences() {
    assert!(parse_events("&a - entry\n", &Limits::default()).is_err());
    assert!(parse_events("- &a - nested\n", &Limits::default()).is_err());
}

#[test]
fn tabs_cannot_separate_block_indicators() {
    assert!(parse_events("-\t- a\n", &Limits::default()).is_err());
    assert!(parse_events("key:\n\tb: 1\n", &Limits::default()).is_err());
}

#[test]
fn leading_block_scalar_lines_may_not_be_over_indented() {
    let error = parse_events("key: >\n \n  \n invalid\n", &Limits::default())
        .expect_err("over-indented leading line");
    assert_eq!(error.kind(), ErrorKind::Scanner);
}

#[test]
fn tag_directives_are_scoped_to_one_document() {
    let limits = Limits {
        max_documents: 2,
        ..Limits::default()
    };
    let error = parse_events(
        "%TAG !e! tag:example.com,2020:\n--- !e!first x\n--- !e!second y\n",
        &limits,
    )
    .expect_err("undefined tag handle in second document");
    assert_eq!(error.kind(), ErrorKind::Scanner);
}

#[test]
fn duplicate_tag_or_anchor_properties_are_rejected() {
    let error = parse_events("a: &x\n  &y value\n", &Limits::default())
        .expect_err("duplicate anchor properties");
    assert_eq!(error.kind(), ErrorKind::Parser);
}

#[test]
fn implicit_flow_mapping_colons_must_share_the_key_line() {
    assert!(parse_events("[ key\n : value ]\n", &Limits::default()).is_err());
    let events = events("[? key\n value : item]\n");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::MappingStart { .. }))
    );
}

#[test]
fn long_single_line_flow_sequences_do_not_scan_the_prefix_per_item() {
    let mut input = String::from("items: [");
    for index in 0..20_000 {
        if index > 0 {
            input.push(',');
        }
        input.push('x');
    }
    input.push_str("]\n");
    let events = events(&input);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::SequenceStart { .. }))
    );
}

#[test]
fn flow_sequence_items_in_mapping_values_must_be_indented() {
    assert!(parse_events("key: [a,\nb]\n", &Limits::default()).is_err());
    let events = events("[\na,\nb\n]\n");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::SequenceStart { .. }))
    );
}

#[test]
fn bom_at_a_line_start_is_ignored() {
    let events = events("\n\u{feff}");
    assert!(!events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, .. } if value == "\u{feff}"
    )));
}

#[test]
fn block_scalar_without_final_line_break_has_no_trailing_newline() {
    let events = events("|\n  : ");
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, style: crate::event::ScalarStyle::Literal, .. }
            if value == ": "
    )));
}

#[test]
fn trailing_blank_lines_do_not_extend_a_plain_scalar() {
    let events = events("x\n\n");
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Scalar { value, .. } if value == "x"
    )));
}

#[test]
fn empty_flow_mapping_value_is_null() {
    let events = events("a: {b: }\n");
    assert!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Scalar { value, .. } if value.is_empty()))
            .count()
            >= 1
    );
}
