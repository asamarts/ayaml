#![allow(
    clippy::literal_string_with_formatting_args,
    reason = "test fixtures favor compact setup, generated inputs, and literal YAML braces"
)]

use super::{
    DocumentEvents, EventSink, TOKEN_ARENA_CHUNK, TOKEN_ARENA_MID_CHUNK, TOKEN_ARENA_SMALL_CHUNK,
    arena_growth, flow_ends, flow_entry_count, parse_document_events, parse_events, push_token,
    read_lines,
};
use crate::error::{Error, ErrorKind};
use crate::event::Event;
use crate::limits::Limits;
use crate::scanner::{Token, TokenKind};
use crate::span::Span;

fn fixture(name: &str) -> Option<String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat");
    std::fs::read_to_string(root.join(name)).ok()
}

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

/// A growing arena copies at most twice its final length and keeps less than its length, or one
/// chunk, unused. Growing by a fixed chunk copied quadratically and allocated many times the
/// arena in total.
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
            assert!(copied <= 2 * tokens, "{tokens} tokens: {copied} copied");
            assert!(
                capacity - tokens < tokens.max(chunk),
                "{tokens} tokens: capacity {capacity}"
            );
        }
        assert_eq!(arena_growth(0, chunk), chunk);
        assert_eq!(arena_growth(chunk, chunk), chunk);
        assert_eq!(arena_growth(16 * chunk, chunk), 16 * chunk);
        assert_eq!(arena_growth(17 * chunk, chunk), 17 * chunk);
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
        span: Span::empty(0),
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
            push_token(&mut tokens, token.clone(), chunk, usize::MAX);
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
        // The first chunk, then doubling to 64 chunks for 40 chunks of tokens.
        assert_eq!(growths, 7, "{chunk}");
    }
    // The arena stops at the tokens the limits let a reader keep: 64, then 100 and not 128.
    let mut tokens = Vec::new();
    for _ in 0..100 {
        push_token(&mut tokens, token.clone(), TOKEN_ARENA_SMALL_CHUNK, 100);
    }
    assert_eq!(tokens.capacity(), 100);
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
    let Some(input) = fixture("toolchain-pins.yml") else {
        return;
    };
    let events = parse_events(&input, &Limits::default()).expect("toolchain fixture");
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
    let Some(input) = fixture("scalar-tags-merges-aliases.yml") else {
        return;
    };
    let events = parse_events(&input, &Limits::default()).expect("scalar fixture");
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

#[test]
fn chunked_document_events_match_the_collected_stream() {
    use std::fmt::Write as _;

    let mut input = String::new();
    for index in 0..20_000 {
        writeln!(input, "- {index}").expect("write");
    }
    let limits = Limits::for_max_input();
    let collected = parse_document_events(&input, &limits, false).expect("collected events");
    let chunks =
        super::parse_document_event_chunks(&input, &limits, false).expect("chunked events");
    let chunk_count = chunks.chunks.len() + usize::from(!chunks.current.is_empty());
    assert!(
        chunk_count > 1,
        "the fixture should span more than one chunk: {chunk_count} chunks, {} events, {} in the current chunk, chunk size {}",
        collected.len(),
        chunks.current.len(),
        chunks.chunk_size
    );
    let flattened: Vec<Event<'_>> = chunks.into_chunks().into_iter().flatten().collect();
    assert_eq!(collected, flattened);
}

#[test]
fn mid_size_inputs_reserve_a_token_arena_from_their_length() {
    let most = usize::MAX;
    assert_eq!(
        super::mid_input_token_capacity(20_493, 435, most),
        Some(2_049)
    );
    assert_eq!(super::mid_input_token_capacity(4_097, 100, most), Some(409));
    assert_eq!(
        super::mid_input_token_capacity(32_768, 1_000, most),
        Some(3_276)
    );
    assert_eq!(super::mid_input_token_capacity(4_096, 100, most), None);
    assert_eq!(super::mid_input_token_capacity(32_769, 1_000, most), None);
    assert_eq!(super::mid_input_token_capacity(20_000, 2, most), None);
    assert_eq!(super::mid_input_token_capacity(20_000, 0, most), None);
    // No reader keeps more tokens than the limit, so none is reserved past it.
    assert_eq!(super::mid_input_token_capacity(20_493, 435, 100), Some(100));
}

#[test]
fn prefix_token_estimate_uses_the_scanned_density_with_a_cap() {
    let most = usize::MAX;
    // The density of the prefix gives 1,980,453 tokens for the whole input, to which the
    // estimate adds a sixty-fourth, and one reservation stays within eight times the tokens
    // already scanned.
    assert_eq!(
        super::prefix_token_capacity(1_024, 5_177, 10_012_509, most),
        Some(8_192)
    );
    assert_eq!(
        super::prefix_token_capacity(524_288, 2_650_624, 10_012_509, most),
        Some(1_980_453 + 1_980_453 / 64)
    );
    assert_eq!(
        super::prefix_token_capacity(1_023, 5_177, 10_012_509, most),
        None
    );
    assert_eq!(
        super::prefix_token_capacity(1_024, 0, 10_012_509, most),
        None
    );
    // A token takes a byte at least, so an estimate stays within the input's length, and it
    // never asks for less than the tokens already scanned.
    assert_eq!(
        super::prefix_token_capacity(1_024, 1_024, 3_000, most),
        Some(3_000)
    );
    assert_eq!(
        super::prefix_token_capacity(2_000, 1_000, 1_500, most),
        Some(2_000)
    );
    // The lines of the input don't enter into it: one long line is estimated like any other.
    assert_eq!(
        super::prefix_token_capacity(8_192, 8_192, 1_000_000, most),
        Some(65_536)
    );
    // Nor more than the limits let a reader keep.
    assert_eq!(
        super::prefix_token_capacity(524_288, 2_650_624, 10_012_509, 1_000_000),
        Some(1_000_000)
    );
}

/// The product of the tokens and the input's length passes 64 bits here. Worked in `usize` it
/// would saturate, and the estimate would fall to the tokens already scanned.
#[cfg(target_pointer_width = "64")]
#[test]
fn prefix_token_estimate_does_not_saturate() {
    assert_eq!(
        super::prefix_token_capacity(1 << 40, 1 << 41, 1 << 42, usize::MAX),
        Some((1 << 41) + (1 << 35))
    );
}

/// A full arena grows to its estimate, by a sixteenth of itself or a chunk at least, and never
/// past the tokens the limits let a reader keep.
#[test]
fn an_estimated_arena_grows_within_its_bounds() {
    let chunk = TOKEN_ARENA_CHUNK;
    let most = usize::MAX;
    // To the estimate, when that is more than the least step.
    assert_eq!(
        super::estimated_growth(65_536, 600_000, chunk, most),
        534_464
    );
    // By a sixteenth, when the estimate expects fewer tokens than that, or none.
    assert_eq!(
        super::estimated_growth(160_000, 161_000, chunk, most),
        10_000
    );
    assert_eq!(
        super::estimated_growth(160_000, 100_000, chunk, most),
        10_000
    );
    // By a chunk, when a sixteenth is less.
    assert_eq!(super::estimated_growth(16_000, 16_100, chunk, most), chunk);
    // Up to the token limit and no further, whichever of them asks for more.
    assert_eq!(
        super::estimated_growth(65_536, 600_000, chunk, 100_000),
        34_464
    );
    assert_eq!(
        super::estimated_growth(160_000, 161_000, chunk, 165_000),
        5_000
    );
    // A full arena at the limit still takes the token that the scanner let through.
    assert_eq!(super::estimated_growth(100_000, 100_000, chunk, 100_000), 1);
}

/// What [`read_lines`] ends with for `input`: the arena's capacity, its tokens, and how many
/// times it was reallocated.
fn arena_of(input: &str, limits: Limits) -> (usize, usize, usize) {
    let mut scanner = crate::scanner::Scanner::new(input, limits).expect("scanner");
    let lines = read_lines(&mut scanner).expect("lines");
    (lines.tokens.capacity(), lines.tokens.len(), lines.growths)
}

/// A document of even density takes a few reservations, each from a later estimate, and ends
/// close to its tokens, whether its lines are short, long, or one.
#[test]
fn a_document_of_even_density_takes_few_reservations() {
    let short_lines = "- ab\n".repeat(150_000);
    let long_lines = "- [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]\n".repeat(40_000);
    let one_line = format!("[{}a]\n", "a,".repeat(300_000));
    for (name, input, tokens) in [
        ("short lines", &short_lines, 300_000),
        ("long lines", &long_lines, 880_000),
        ("one line", &one_line, 600_003),
    ] {
        let (capacity, len, growths) = arena_of(input, Limits::for_max_input());
        assert_eq!(len, tokens, "{name}");
        assert!(
            capacity <= len + len / 32,
            "{name}: {capacity} tokens reserved for {len}"
        );
        assert!(growths <= 5, "{name}: {growths} reallocations");
    }
}

#[test]
fn a_dense_prefix_reserves_no_arena_for_a_sparse_rest() {
    // 20,000 dense entries, then comment lines that hold no token. The density of the prefix
    // says the whole input is as dense, which would be over a million tokens, and the rest of
    // them never arrive. The arena stays within eight times the tokens it holds.
    let mut input = "a: b\n".repeat(20_000);
    input.push_str(&"#\n".repeat(1_000_000));
    let (capacity, len, growths) = arena_of(&input, Limits::for_max_input());
    assert_eq!(len, 60_000);
    assert!(
        capacity <= len * super::TOKEN_ARENA_AHEAD,
        "{capacity} tokens reserved for {len}"
    );
    assert!(growths <= 3, "{growths} reallocations");
}

#[test]
fn a_sparse_prefix_grows_to_a_dense_rest_in_few_steps() {
    // The estimate from the sparse prefix is far too low. Each time the arena fills it is
    // estimated again, so the copies stay few and the arena ends close to its tokens.
    let mut input = "key: value # a long comment that holds no token at all\n".repeat(600);
    input.push_str(&"- a\n".repeat(300_000));
    let (capacity, len, growths) = arena_of(&input, Limits::for_max_input());
    assert_eq!(len, 601_800);
    assert!(
        capacity <= len + len / 8,
        "{capacity} tokens reserved for {len}"
    );
    assert!(growths <= 6, "{growths} reallocations");
}

#[test]
fn the_arena_never_reserves_past_the_token_limit() {
    // As many tokens as the limit allows: an estimate that runs ahead of the tokens scanned
    // would pass the limit, and the arena stops at it.
    let input = "- a\n".repeat(50_000);
    let limits = Limits::default().with_max_events(100_000);
    let (capacity, len, _) = arena_of(&input, limits);
    assert_eq!((capacity, len), (100_000, 100_000));
    // One token more than the limit allows is refused, by the scanner.
    let input = "- a\n".repeat(50_001);
    let limits = Limits::default().with_max_events(100_000);
    let mut scanner = crate::scanner::Scanner::new(&input, limits).expect("scanner");
    let Err(error) = read_lines(&mut scanner) else {
        panic!("the input is over the token limit");
    };
    assert_eq!(
        error.limit().map(crate::limits::LimitExceeded::kind),
        Some(crate::limits::LimitKind::Events)
    );
}

/// What a parse tells its consumer to reserve for root collections.
#[derive(Default)]
struct Reserved(std::rc::Rc<std::cell::RefCell<Vec<usize>>>);

impl<'a> super::EventConsumer<'a> for Reserved {
    fn event(&mut self, _event: Event<'a>) -> Result<(), Error> {
        Ok(())
    }

    fn reserve_collection(&mut self, entries: usize) {
        self.0.borrow_mut().push(entries);
    }

    fn finish(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

/// A root flow collection reserves its entry count on whichever line of the stream it opens,
/// and a collection inside another reserves nothing.
#[test]
fn a_root_flow_collection_reserves_its_entries_on_any_line() {
    for (input, entries) in [
        ("[1, 2, 3]\n", vec![3]),
        ("---\n[1, 2, 3]\n", vec![3]),
        ("%YAML 1.2\n---\n{a: 1, b: 2}\n", vec![2]),
        ("# a comment\n\n[1, [2, 3], {a: [4]}]\n", vec![3]),
        ("--- !t\n[1, 2]\n", vec![2]),
        ("[1,\n 2,\n 3, 4]\n", vec![4]),
        ("a: [1, 2, 3]\n", vec![]),
        ("- [1, 2, 3]\n", vec![]),
    ] {
        let reserved = Reserved::default();
        let seen = std::rc::Rc::clone(&reserved.0);
        super::parse_document_with_consumer(input, &Limits::default(), false, Box::new(reserved))
            .unwrap_or_else(|error| panic!("{input:?}: {error}"));
        assert_eq!(*seen.borrow(), entries, "{input:?}");
    }
}

#[test]
fn a_token_stays_small() {
    assert!(size_of::<Token<'_>>() <= 32, "{}", size_of::<Token<'_>>());
}

#[test]
fn root_flow_collections_count_their_entries() {
    let input = "[1, 2, {a: 3, b: [4, 5]}]\n";
    let mut scanner = crate::scanner::Scanner::new(input, Limits::default()).expect("scanner");
    let lines = read_lines(&mut scanner).expect("lines");
    let ends = flow_ends(&lines.tokens);
    let open = lines
        .tokens
        .iter()
        .position(|token| matches!(token.kind, TokenKind::FlowSequenceStart))
        .expect("sequence start");
    assert_eq!(flow_entry_count(&lines.tokens, open, ends[&open]), 3);
    let mapping = lines
        .tokens
        .iter()
        .position(|token| matches!(token.kind, TokenKind::FlowMappingStart))
        .expect("mapping start");
    assert_eq!(flow_entry_count(&lines.tokens, mapping, ends[&mapping]), 2);
}

#[test]
fn separators_alone_count_no_flow_entry() {
    for (input, entries) in [
        ("[]\n", 0),
        ("[1]\n", 1),
        ("[1, 2,]\n", 2),
        ("[[], {}]\n", 2),
        ("{a: 1, b: 2}\n", 2),
    ] {
        let mut scanner = crate::scanner::Scanner::new(input, Limits::default()).expect("scanner");
        let lines = read_lines(&mut scanner).expect("lines");
        let ends = flow_ends(&lines.tokens);
        assert_eq!(
            flow_entry_count(&lines.tokens, 0, ends[&0]),
            entries,
            "{input:?}"
        );
    }
    // A run of separators is not a run of entries, whatever the parser then says of it.
    let indicator = |kind| Token {
        kind,
        span: Span::empty(0),
    };
    let commas = [
        indicator(TokenKind::FlowSequenceStart),
        indicator(TokenKind::FlowEntry),
        indicator(TokenKind::FlowEntry),
        indicator(TokenKind::FlowEntry),
        indicator(TokenKind::FlowSequenceEnd),
    ];
    assert_eq!(flow_entry_count(&commas, 0, 4), 0);
}

#[test]
fn event_capacity_reserves_from_the_token_count() {
    let token = Token {
        kind: TokenKind::PlainScalar,
        span: Span::empty(0),
    };
    let tokens = vec![token; 1_000];
    let lines = vec![super::Line {
        start: 0,
        end: tokens.len(),
        indent: 0,
    }];
    assert_eq!(super::event_capacity(&lines, &tokens), 1_000 * 4 / 5 + 200);
}

/// One document's events, taken one at a time, and the most that waited for the reader at once.
fn events_on_demand<'a>(
    input: &'a str,
    limits: &Limits,
    compat: bool,
) -> Result<(Vec<Event<'a>>, usize), Error> {
    let mut source = DocumentEvents::new(input, limits, compat)?;
    let (mut events, mut most) = (Vec::new(), 0);
    while let Some(event) = source.next_event()? {
        events.push(event);
        most = most.max(source.waiting());
    }
    Ok((events, most))
}

/// A few lines from this pool, at any indentation, make documents of every form, most of
/// them with a fault somewhere.
const POOL: &[&str] = &[
    "a: 1",
    "b:",
    "c: [1, {x: 2}]",
    "d: |",
    "e: &n 3",
    "f: *n",
    "g: !t 4",
    "- 1",
    "-",
    "- - 2",
    "- a: 1",
    "- [1, 2",
    "? k",
    ": v",
    "text",
    "---",
    "...",
    "%YAML 1.2",
    "# c",
    "",
    "{a: 1, b}",
    "[x, y]: z",
    "\"q\\x41\"",
    "- !!int '5'",
];

fn generated(state: &mut u64) -> String {
    let mut next = || {
        *state ^= *state >> 12;
        *state ^= *state << 25;
        *state ^= *state >> 27;
        usize::try_from(state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33).expect("31 bits")
    };
    let mut text = String::new();
    for _ in 0..=next() % 8 {
        text.push_str(&" ".repeat([0, 0, 0, 2, 2, 4, 1][next() % 7]));
        text.push_str(POOL[next() % POOL.len()]);
        text.push('\n');
    }
    text
}

/// A reader that takes a document's events as it needs them gets the events that a reader of
/// the whole document collects, or the same error where the document has one.
#[test]
fn events_on_demand_are_the_events_that_are_collected() {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut documents = vec![String::new(), "a\n".to_owned(), "- 1\n".repeat(500)];
    documents.extend((0..3_000).map(|_| generated(&mut state)));
    let limits = [Limits::default(), Limits::default().with_max_events(12)];
    let (mut read, mut refused) = (0, 0);
    for document in &documents {
        for compat in [false, true] {
            for limits in &limits {
                let collected = parse_document_events(document, limits, compat);
                let on_demand = events_on_demand(document, limits, compat);
                match (collected, on_demand) {
                    (Ok(collected), Ok((on_demand, _))) => {
                        assert_eq!(collected, on_demand, "{document:?}");
                        read += 1;
                    }
                    (Err(collected), Err(on_demand)) => {
                        assert_eq!(
                            format!("{collected:?}"),
                            format!("{on_demand:?}"),
                            "{document:?}"
                        );
                        refused += 1;
                    }
                    (collected, on_demand) => {
                        panic!("{document:?}: collected {collected:?}, on demand {on_demand:?}")
                    }
                }
            }
        }
    }
    assert!(
        read > 1_000 && refused > 1_000,
        "{read} read, {refused} refused"
    );
}

/// The grammar reads a run of scalar entries in one step. It stops for a reader that takes its
/// events one at a time, so that a document of a hundred thousand entries has a few of its
/// events waiting at any moment and not all of them.
#[test]
fn few_events_wait_for_a_reader_that_takes_them_one_at_a_time() {
    let limits = Limits::for_max_input();
    for document in [
        "- a\n".repeat(100_000),
        "? a\n: b\n".repeat(50_000),
        "k: v\n".repeat(50_000),
        format!("[{}]\n", "1, ".repeat(100_000)),
    ] {
        let (events, most) = events_on_demand(&document, &limits, false).expect("events");
        assert!(events.len() > 100_000, "{} events", events.len());
        assert!(most <= 2 * EventSink::QUEUE_ROOM, "{most} events waited");
    }
}
