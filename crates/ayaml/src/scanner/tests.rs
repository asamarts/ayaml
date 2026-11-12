#![allow(
    clippy::format_collect,
    reason = "test fixtures favor compact setup, generated inputs, and literal YAML braces"
)]

use std::collections::HashMap;

use super::line::*;
use super::scalars::*;
use super::{Scanner, Token, TokenKind};
use crate::event::ScalarStyle;
use crate::limits::Limits;
use crate::tag::Tag;
use proptest::prelude::*;

fn tokens(input: &str) -> Vec<TokenKind<'_>> {
    let mut scanner = Scanner::new(input, Limits::default()).expect("scanner");
    let mut tokens = Vec::new();
    while let Some(token) = scanner.next_token().expect("token") {
        tokens.push(token.kind);
    }
    tokens
}

fn scalar(input: &str) -> (String, ScalarStyle, String) {
    let tokens = tokens(input);
    for token in tokens {
        if let TokenKind::Scalar { value, raw, style } = token {
            return (value.into_owned(), style, raw.into_owned());
        }
    }
    panic!("no scalar in {input:?}");
}

/// The anchor names `input` leaves for its aliases to match, sorted.
fn colon_anchors(input: &str, compat: bool) -> Option<Vec<String>> {
    let mut scanner = Scanner::with_compat(input, Limits::default(), compat).expect("scanner");
    while scanner.next_token().expect("token").is_some() {}
    let mut names: Vec<String> = scanner
        .colon_anchors?
        .into_keys()
        .map(str::to_owned)
        .collect();
    names.sort();
    Some(names)
}

fn rules(compat: bool) -> NameRules<'static, 'static> {
    NameRules {
        compat,
        colon_anchors: None,
        input: "",
    }
}

#[test]
fn a_line_ends_an_alias_name_as_the_scanner_does() {
    // The anchor `a:` is defined on the second line, after the first.
    let input = "*a:\n&a: 1\n*a:\n*b:";
    let known = HashMap::from([("a:", 4)]);
    let none = NameRules {
        compat: true,
        colon_anchors: None,
        input,
    };
    let anchored = NameRules {
        compat: true,
        colon_anchors: Some(&known),
        input,
    };
    let default = NameRules {
        compat: false,
        colon_anchors: None,
        input,
    };
    let (before, after, other) = (&input[..3], &input[10..13], &input[14..]);
    let owner = LineShape::Owner(Owner::Block(0));
    // The colon is this line's indicator, unless an anchor before it has the whole name.
    assert_eq!(line_shape(after, none), owner);
    assert_eq!(line_shape(after, anchored), LineShape::Content);
    assert_eq!(line_shape(before, anchored), owner);
    assert_eq!(line_shape(other, anchored), owner);
    assert_eq!(line_shape(after, default), LineShape::Content);
    // No colon to give up, or none but the name.
    assert_eq!(line_shape("*ab", none), LineShape::Content);
    assert_eq!(line_shape("*:", none), LineShape::Content);
    // Only an alias gives one up, and a flow key's alias is inside its key.
    assert_eq!(line_shape("&a:", none), LineShape::Open);
    assert_eq!(line_shape("!a:", none), LineShape::Open);
    assert_eq!(line_shape("{*a: 1}:", none), owner);
}

#[test]
fn only_compat_keeps_anchor_names_and_only_those_ending_in_a_colon() {
    assert_eq!(
        colon_anchors("x: &a: 1\ny: &b 2\nz: &c:d: 3\n", true),
        Some(vec!["a:".to_owned()])
    );
    assert_eq!(colon_anchors("x: &a 1\ny: &b 2\n", true), None);
    assert_eq!(colon_anchors("x: &a: 1\ny: &b 2\n", false), None);
    assert_eq!(colon_anchors("x: &a: 1\n---\ny: 2\n", true), None);
    assert_eq!(colon_anchors("x: &a: 1\n...\n", true), None);
}

#[test]
fn a_line_walk_stops_where_the_scanner_would() {
    // A key that ends at the last `:` on the line, in both modes.
    for text in [
        "a: c",
        "a:",
        "a:\tc",
        "a#b: c",
        "a \"b: c",
        "a [b: c",
        "&a !t k: c",
        "&a\tk: c",
        "\"a: b\": c",
        "\"a\\\": b\": c",
        "\"a\\\\\": c",
        "'a: b': c",
        "'\\': c",
        "'a'': b': c",
        "[a]: c",
        "{a: b}: c",
        "[a#b]: c",
        "[\"]: \"]: c",
        "[']: ']: c",
        "['a'']: ']: c",
        "[\"\\\"]: \"]: c",
        "['\\']: c",
        "{\"a\":\"]: \"}: c",
        "{\"a\" :\"]: \"}: c",
        "{\"a\"\t:\"]: \"}: c",
        "{[a] :\"]: \"}: c",
        "['a' :\"]: \"]: c",
        "{\"a\"::\"b}: c",
        "[x &a: \"]: \"]: c",
        "[x !a: \"]: \"]: c",
        "{x *a: \"]: \"}: c",
        "{a:\t\"]: \"}: c",
        "{a:\"b: c}: d",
        "[&a,\"]: \"]: c",
        "[&a\t\"]: \"]: c",
        "{&a}: c",
        "[!t,\"]: \"]: c",
        "[&a:b \"]: \"]: c",
        "[!<t:]> \"]: \"]: c",
        "{!<t:]> : }: c",
        "[a[b]]: c",
    ] {
        for compat in [false, true] {
            assert_eq!(
                walk_line(text, rules(compat)),
                LineStop::Value(text.rfind(':').unwrap()),
                "{text:?} compat={compat}"
            );
        }
    }
    // A comment, which no later `:` or bracket changes.
    for text in [
        "# a: b",
        "a # b: c",
        "a\t# b: c",
        "'a' # b: c",
        "[a] # b: c",
        "&a # b: c",
    ] {
        for compat in [false, true] {
            assert_eq!(
                walk_line(text, rules(compat)),
                LineStop::Comment(text.find('#').unwrap()),
                "{text:?} compat={compat}"
            );
        }
    }
    // Nothing that ends a key on this line.
    for text in [
        "",
        "a",
        "a:b",
        "a#b",
        "!t",
        "&a b",
        "[",
        "[a",
        "\"a: b",
        "'a",
        "[\"a: b]: c",
        "[a, # b]: c",
        "[a # b]: c",
        "[a\t# b]: c",
        "&:b: c",
        "[a[b]: c",
        "[a{b}: c",
        "{a: [b}: c",
        "[!<t:]: c",
        "!a:b: c",
    ] {
        for compat in [false, true] {
            assert_eq!(
                walk_line(text, rules(compat)),
                LineStop::End,
                "{text:?} compat={compat}"
            );
        }
    }
}

#[test]
fn a_line_walk_reads_names_and_a_flow_question_mark_by_mode() {
    let walk = |text: &str| (walk_line(text, rules(false)), walk_line(text, rules(true)));
    let last = |text: &str| LineStop::Value(text.rfind(':').unwrap());
    for text in ["&a:b: c", "*a:b: c", "&a?b: c", "&a?b \"c: d"] {
        assert_eq!(
            walk(text),
            (LineStop::End, LineStop::Value(text.find(": ").unwrap())),
            "{text:?}"
        );
    }
    let text = "[?\"]: \"]: c";
    assert_eq!(
        walk(text),
        (LineStop::Value(text.find(':').unwrap()), last(text))
    );
    let text = "{?\"a: b}: c";
    assert_eq!(walk(text), (last(text), LineStop::End));
    let text = "[?\t\"]: \"]: c";
    assert_eq!(walk(text), (last(text), last(text)));
}

#[test]
fn a_closing_quote_is_the_one_the_scanner_stops_at() {
    for (text, close) in [
        ("\"a\"", 2),
        ("'a'", 2),
        ("\"a\"\"b\"", 2),
        ("'a''b'", 5),
        ("'a\\'", 3),
        ("\"a\\\"b\"", 5),
        ("\"a\\\\\"", 4),
        ("\"a", 2),
        ("'a''", 4),
    ] {
        assert_eq!(closing_quote(text.as_bytes(), 0), close, "{text:?}");
    }
}

#[test]
fn a_comment_comes_off_the_line_it_ends() {
    for compat in [false, true] {
        for (line, code) in [
            ("a: b", "a: b"),
            ("a: b # c: d", "a: b "),
            ("a: # c", "a: "),
            ("a: b: c # d", "a: b: c "),
            ("# c", ""),
            ("- # c", "- "),
            ("- [' # ']: # c", "- [' # ']: "),
            ("- [\"]: \"]: # c \"", "- [\"]: \"]: "),
            ("? {\", \"}: # c \"", "? {\", \"}: "),
            ("- a#b: # c", "- a#b: "),
            ("\u{feff}a: # c", "\u{feff}a: "),
            ("[a, # b", "[a, # b"),
        ] {
            assert_eq!(
                strip_comment(line, rules(compat)),
                code,
                "{line:?} compat={compat}"
            );
        }
    }
}

#[test]
fn block_tokens_and_indicators() {
    let kinds = tokens("a: 1\n- b\n");
    assert!(matches!(kinds[0], TokenKind::StreamStart));
    assert!(kinds.iter().any(|kind| matches!(kind, TokenKind::Value)));
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, TokenKind::BlockEntry))
    );
    assert!(matches!(kinds.last(), Some(TokenKind::StreamEnd)));
}

#[test]
fn flow_tokens_are_nested_and_comma_separated() {
    let kinds = tokens("[1, {a: 2}]");
    assert!(matches!(kinds[1], TokenKind::FlowSequenceStart));
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, TokenKind::FlowEntry))
    );
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, TokenKind::FlowMappingStart))
    );
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, TokenKind::FlowMappingEnd))
    );
}

#[test]
fn block_scalars_support_literal_folding_and_chomping() {
    let (value, style, raw) = scalar("|\n  a\n  b\n");
    assert_eq!(value, "a\nb\n");
    assert_eq!(style, ScalarStyle::Literal);
    assert_eq!(raw, "|\n  a\n  b\n");

    let (value, style, _) = scalar(">\n  a\n  b\n");
    assert_eq!(value, "a b\n");
    assert_eq!(style, ScalarStyle::Folded);

    let (value, _, _) = scalar("|-\n  a\n  b\n");
    assert_eq!(value, "a\nb");

    let (value, _, _) = scalar("|+\n  a\n\n");
    assert_eq!(value, "a\n\n");
}

/// How a block scalar's value was built from a list of its lines, before the scanner built
/// one string as it read them.
fn block_scalar_from_lines(lines: &[(String, bool)], literal: bool, chomping: Chomping) -> String {
    let mut raw = String::new();
    for (text, had_break) in lines {
        raw.push_str(text);
        if *had_break {
            raw.push('\n');
        }
    }
    let mut value = if literal {
        raw
    } else {
        let lines: Vec<&str> = raw.split('\n').collect();
        let mut value = String::new();
        let mut index = 0;
        while index < lines.len() {
            let line = lines[index];
            value.push_str(line);
            if index + 1 == lines.len() {
                break;
            }
            let mut next = index + 1;
            while next < lines.len() && lines[next].is_empty() {
                next += 1;
            }
            if next == lines.len() {
                for _ in index + 1..lines.len() {
                    value.push('\n');
                }
                break;
            }
            let breaks = next - index;
            let more_indented =
                line.starts_with([' ', '\t']) || lines[next].starts_with([' ', '\t']);
            if line.is_empty() || more_indented {
                for _ in 0..breaks {
                    value.push('\n');
                }
            } else if breaks == 1 {
                value.push(' ');
            } else {
                for _ in 0..breaks - 1 {
                    value.push('\n');
                }
            }
            index = next;
        }
        value
    };
    let final_break = lines
        .iter()
        .rev()
        .find(|(text, _)| !text.is_empty())
        .is_some_and(|(_, had_break)| *had_break);
    match chomping {
        Chomping::Strip => {
            while value.ends_with('\n') {
                value.pop();
            }
        }
        Chomping::Clip => {
            while value.ends_with('\n') {
                value.pop();
            }
            if final_break && !value.is_empty() {
                value.push('\n');
            }
        }
        Chomping::Keep => {}
    }
    value
}

/// Building one string as the lines are read gives what the list of lines gave, for every
/// sequence of up to five lines of these kinds, with or without a last line break.
#[test]
fn block_scalars_built_as_read_match_the_list_of_lines() {
    let kinds = ["", "a", "b c", " a", "\ta", "  "];
    let mut sequences: Vec<Vec<&str>> = vec![Vec::new()];
    let mut checked = 0;
    for _ in 0..5 {
        sequences = sequences
            .iter()
            .flat_map(|sequence| {
                kinds.iter().map(move |kind| {
                    let mut longer = sequence.clone();
                    longer.push(kind);
                    longer
                })
            })
            .collect();
        for sequence in &sequences {
            for last_break in [true, false] {
                let lines: Vec<(String, bool)> = sequence
                    .iter()
                    .enumerate()
                    .map(|(at, text)| ((*text).to_owned(), last_break || at + 1 < sequence.len()))
                    .collect();
                let text: String = lines
                    .iter()
                    .map(|(text, had_break)| {
                        format!("{text}{}", if *had_break { "\n" } else { "" })
                    })
                    .collect();
                let final_break = lines
                    .iter()
                    .rev()
                    .find(|(text, _)| !text.is_empty())
                    .is_some_and(|(_, had_break)| *had_break);
                for literal in [true, false] {
                    for chomping in [Chomping::Strip, Chomping::Clip, Chomping::Keep] {
                        assert_eq!(
                            build_block_scalar(text.clone(), final_break, literal, chomping),
                            block_scalar_from_lines(&lines, literal, chomping),
                            "{lines:?}, literal {literal}"
                        );
                        checked += 1;
                    }
                }
            }
        }
    }
    assert_eq!(
        checked,
        (1..=5).map(|n| 6_usize.pow(n) * 2 * 6).sum::<usize>()
    );
}

#[test]
fn block_scalar_indent_indicator_is_relative_to_the_header() {
    // A root node's indentation is -1, so |2 takes one space off a four-space line.
    let (value, _, _) = scalar("|2\n    a\n");
    assert_eq!(value, "   a\n");
}

#[test]
fn single_quotes_decode_doubled_quote() {
    let (value, style, raw) = scalar("'it''s'");
    assert_eq!(value, "it's");
    assert_eq!(style, ScalarStyle::SingleQuoted);
    assert_eq!(raw, "'it''s'");
}

#[test]
fn double_quotes_decode_escapes() {
    let (value, style, _) = scalar("\"a\\n\\u00e9\"");
    assert_eq!(value, "a\né");
    assert_eq!(style, ScalarStyle::DoubleQuoted);
}

#[test]
fn multiline_quoted_scalars_fold_line_breaks() {
    let (value, style, _) = scalar("'a\n  b'");
    assert_eq!(value, "a b");
    assert_eq!(style, ScalarStyle::SingleQuoted);

    let (value, style, _) = scalar("\"a\n  b\"");
    assert_eq!(value, "a b");
    assert_eq!(style, ScalarStyle::DoubleQuoted);

    let (value, _, _) = scalar("'a\n\n  b'");
    assert_eq!(value, "a\nb");
}

#[test]
fn escaped_line_break_in_double_quotes_is_a_continuation() {
    let (value, _, _) = scalar("\"a\\\n  b\"");
    assert_eq!(value, "ab");
}

#[test]
fn anchors_aliases_tags_and_directives() {
    let kinds = tokens("%YAML 1.2\n---\na: &b !Custom {x: 1}\nc: *b\n");
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, TokenKind::Directive { name, .. } if name == "YAML"))
    );
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, TokenKind::DocumentStart { explicit: true }))
    );
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, TokenKind::Anchor(name) if name == "b"))
    );
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, TokenKind::Alias(name) if name == "b"))
    );
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, TokenKind::Tag(tag) if tag == &Tag::local("Custom")))
    );
}

#[test]
fn comments_are_skipped_and_line_breaks_are_tokens() {
    let kinds = tokens("a: 1 # comment\nb: 2\n");
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, TokenKind::LineBreak))
    );
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, TokenKind::Scalar { value, .. } if value == "1"))
    );
    assert!(
        !kinds.iter().any(
            |kind| matches!(kind, TokenKind::Scalar { value, .. } if value.contains("comment"))
        )
    );
}

#[test]
fn crlf_and_bom_are_handled() {
    let kinds = tokens("\u{feff}a: 1\r\nb: 2\r\n");
    assert!(matches!(kinds[0], TokenKind::StreamStart));
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, TokenKind::LineBreak))
    );
}

#[test]
fn unicode_span_is_byte_based() {
    let mut scanner = Scanner::new("é: 1", Limits::default()).expect("scanner");
    let mut spans = Vec::new();
    while let Some(token) = scanner.next_token().expect("token") {
        if let TokenKind::Scalar { value, .. } = &token.kind
            && value == "é"
        {
            spans.push(token.span);
        }
    }
    assert_eq!(spans.len(), 1);
    assert_eq!((spans[0].start().index(), spans[0].end().index()), (0, 2));
}

#[test]
fn unterminated_quotes_and_unknown_escapes_error() {
    let mut scanner = Scanner::new("\"unterminated", Limits::default()).expect("scanner");
    let error = loop {
        match scanner.next_token() {
            Ok(Some(_)) => {}
            Ok(None) => panic!("expected an error"),
            Err(error) => break error,
        }
    };
    assert_eq!(error.kind(), crate::error::ErrorKind::Scanner);

    let mut scanner = Scanner::new("\"\\q\"", Limits::default()).expect("scanner");
    let error = loop {
        match scanner.next_token() {
            Ok(Some(_)) => {}
            Ok(None) => panic!("expected an error"),
            Err(error) => break error,
        }
    };
    assert_eq!(error.kind(), crate::error::ErrorKind::Scanner);
}

#[test]
fn input_limit_is_enforced_before_scanning() {
    let error = Scanner::new(
        "x",
        Limits {
            max_input_bytes: 0,
            ..Limits::default()
        },
    )
    .expect_err("input limit");
    assert_eq!(error.kind(), crate::error::ErrorKind::Limit);
}

fn scan_all(input: &str, fast: bool) -> Result<Vec<Token<'_>>, String> {
    let mut scanner = Scanner::new(input, Limits::default()).map_err(|error| error.to_string())?;
    scanner.fast_continuation = fast;
    let mut tokens = Vec::new();
    while let Some(token) = scanner.next_token().map_err(|error| error.to_string())? {
        tokens.push(token);
    }
    Ok(tokens)
}

const PLAIN_PIECES: &[&str] = &[
    "a", "b", "c", "first", "second", " ", "  ", "\n", "\n\n", "\t", "k: ", "- ", "? ", ": ",
    "!t ", "&a ", " # ", "#",
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn the_plain_scalar_fast_path_agrees_with_the_full_path(
        input in prop::collection::vec(prop::sample::select(PLAIN_PIECES), 0..40)
            .prop_map(|pieces| pieces.concat())
    ) {
        let fast = scan_all(&input, true);
        let full = scan_all(&input, false);
        prop_assert_eq!(fast, full, "input={:?}", input);
    }
}
