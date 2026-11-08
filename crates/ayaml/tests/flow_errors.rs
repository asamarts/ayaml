//! A malformed flow collection's error says where it failed and where the collection opened, in
//! both modes. The opening comes from the parser's tokens, never from a search through the text.

use ayaml::{Deserializer, Error, Value};

type At = (usize, usize);

/// Every default entry point's error for `input`.
fn default_errors(input: &str) -> Vec<(&'static str, Error)> {
    let events = ayaml::parse_events(input).and_then(|mut events| {
        events.try_for_each(|event| event.map(drop))?;
        Ok(())
    });
    let results: Vec<(&'static str, Result<(), Error>)> = vec![
        ("parse_value", ayaml::parse_value(input).map(drop)),
        ("parse_documents", ayaml::parse_documents(input).map(drop)),
        ("from_str", ayaml::from_str::<Value>(input).map(drop)),
        (
            "from_slice",
            ayaml::from_slice::<Value>(input.as_bytes()).map(drop),
        ),
        (
            "from_str_borrowed",
            ayaml::from_str_borrowed::<Value>(input).map(drop),
        ),
        (
            "Deserializer",
            Deserializer::from_str(input).and_then(|mut documents| {
                while documents.next::<Value>()?.is_some() {}
                Ok(())
            }),
        ),
        ("parse_events", events),
    ];
    results
        .into_iter()
        .map(|(name, result)| match result {
            Ok(()) => panic!("{name} read {input:?}"),
            Err(error) => (name, error),
        })
        .collect()
}

fn location(error: &Error) -> At {
    let location = error.location().expect("a location");
    (location.line(), location.column())
}

/// What `message()` gives for an error that displays as `shown` at `failure`: the same text
/// without the failure's location, which is the first one in it.
fn without_location(shown: &str, failure: At) -> String {
    let (line, column) = failure;
    let location = format!(" at line {line} column {column}");
    assert!(shown.contains(&location), "{shown:?}");
    shown.replacen(&location, "", 1)
}

/// Where a collection fails.
#[derive(Clone, Copy, PartialEq)]
enum Failure {
    /// At a token where `,` or the closing bracket belongs.
    Token,
    /// At the end of the input, after a node.
    End,
    /// At the end of the input, where an entry, a key or a value starts: after `[`, `{`, `,`, `?`
    /// or `:`. libyaml finds no node there, and compat says so.
    EndWhereANodeBelongs,
}

/// In both modes, `input` fails at `failure`, in the collection opened at `opening`.
#[track_caller]
fn failure_is(input: &str, failure: At, opening: At, sequence: bool, kind: Failure) {
    let (name, closing) = if sequence {
        ("sequence", ']')
    } else {
        ("mapping", '}')
    };
    let (line, column) = failure;
    let (opened_line, opened_column) = opening;
    let default = if kind != Failure::Token {
        format!(
            "unterminated flow {name} at line {line} column {column}, \
             opened at line {opened_line} column {opened_column}"
        )
    } else {
        format!(
            "expected ',' or '{closing}' at line {line} column {column}, \
             in the flow {name} opened at line {opened_line} column {opened_column}"
        )
    };
    for (reader, error) in default_errors(input) {
        assert_eq!(error.to_string(), default, "{reader}, {input:?}");
        assert_eq!(
            error.message(),
            without_location(&default, failure),
            "{reader}, {input:?}"
        );
        assert_eq!(location(&error), failure, "{reader}, {input:?}");
    }
    #[cfg(feature = "libyaml-compat")]
    {
        let problem = if kind == Failure::EndWhereANodeBelongs {
            "did not find expected node content".to_owned()
        } else {
            format!("did not find expected ',' or '{closing}'")
        };
        compat_failure_is(
            input,
            &format!(
                "{problem} at line {line} column {column}, \
                 while parsing a flow {name} at line {opened_line} column {opened_column}"
            ),
            failure,
        );
    }
}

/// In compat, every entry point gives `input` this message and failure location.
#[cfg(feature = "libyaml-compat")]
#[track_caller]
fn compat_failure_is(input: &str, message: &str, failure: At) {
    use ayaml::Limits;
    use serde::de::IgnoredAny;

    let errors = [
        (
            "from_str",
            ayaml::compat::from_str::<Value>(input).map(drop),
        ),
        (
            "from_str_seed",
            ayaml::compat::from_str_seed(input, std::marker::PhantomData::<Value>).map(drop),
        ),
        (
            "deserialize_any",
            ayaml::compat::deserialize_any(input, IgnoredAny).map(drop),
        ),
        (
            "parse_events_with_limits",
            ayaml::compat::parse_events_with_limits(input, &Limits::default()).map(drop),
        ),
    ];
    for (reader, result) in errors {
        let error = result.expect_err(reader);
        assert_eq!(error.to_string(), message, "{reader}, {input:?}");
        assert_eq!(
            error.message(),
            without_location(message, failure),
            "{reader}, {input:?}"
        );
        assert_eq!(location(&error), failure, "{reader}, {input:?}");
    }
}

#[track_caller]
fn sequence_ends(input: &str, failure: At, opening: At) {
    failure_is(input, failure, opening, true, Failure::End);
}

#[track_caller]
fn mapping_ends(input: &str, failure: At, opening: At) {
    failure_is(input, failure, opening, false, Failure::End);
}

#[track_caller]
fn sequence_ends_lacking_a_node(input: &str, failure: At, opening: At) {
    failure_is(input, failure, opening, true, Failure::EndWhereANodeBelongs);
}

#[track_caller]
fn mapping_ends_lacking_a_node(input: &str, failure: At, opening: At) {
    failure_is(
        input,
        failure,
        opening,
        false,
        Failure::EndWhereANodeBelongs,
    );
}

#[track_caller]
fn sequence_fails(input: &str, failure: At, opening: At) {
    failure_is(input, failure, opening, true, Failure::Token);
}

#[track_caller]
fn mapping_fails(input: &str, failure: At, opening: At) {
    failure_is(input, failure, opening, false, Failure::Token);
}

#[test]
fn input_that_ends_inside_a_sequence_names_the_end_and_the_opening() {
    sequence_ends_lacking_a_node("[", (1, 2), (1, 1));
    sequence_ends("[a", (1, 3), (1, 1));
    sequence_ends("[a\n", (2, 1), (1, 1));
    sequence_ends_lacking_a_node("[a,", (1, 4), (1, 1));
    sequence_ends("[a, b\n", (2, 1), (1, 1));
    sequence_ends_lacking_a_node("[a: ", (1, 5), (1, 1));
    sequence_ends("[a: b", (1, 6), (1, 1));
    sequence_ends_lacking_a_node("[a: b, c: ", (1, 11), (1, 1));
    sequence_ends_lacking_a_node("[?", (1, 3), (1, 1));
    sequence_ends("[? a", (1, 5), (1, 1));
    sequence_ends("[? a : b", (1, 9), (1, 1));
    sequence_ends("[&a", (1, 4), (1, 1));
    sequence_ends("[!!str", (1, 7), (1, 1));
    sequence_ends("[a #c\n", (2, 1), (1, 1));
    sequence_ends_lacking_a_node("[a,\n\n\n", (4, 1), (1, 1));
    // The next line is indented past the block sequence, as a flow collection's lines must be,
    // so the scalar runs on to the end.
    sequence_ends("- [a\n  - b\n", (3, 1), (1, 3));
}

#[test]
fn input_that_ends_inside_a_mapping_names_the_end_and_the_opening() {
    mapping_ends_lacking_a_node("{", (1, 2), (1, 1));
    mapping_ends("{a", (1, 3), (1, 1));
    mapping_ends_lacking_a_node("{a: ", (1, 5), (1, 1));
    mapping_ends("{a: 1", (1, 6), (1, 1));
    mapping_ends_lacking_a_node("{a: 1,", (1, 7), (1, 1));
    mapping_ends("{\"a\" ", (1, 6), (1, 1));
    mapping_ends_lacking_a_node("{\"a\": ", (1, 7), (1, 1));
    mapping_ends("{? a", (1, 5), (1, 1));
    mapping_ends_lacking_a_node("{? a : ", (1, 8), (1, 1));
    mapping_ends("{&a", (1, 4), (1, 1));
    mapping_ends_lacking_a_node("{a: 1, # c\n\n", (3, 1), (1, 1));
    mapping_ends("x: {a\n b\n", (3, 1), (1, 4));
}

#[test]
fn a_token_where_a_separator_belongs_names_itself_and_the_opening() {
    sequence_fails("[a}", (1, 3), (1, 1));
    sequence_fails("[a: b c: d]", (1, 8), (1, 1));
    sequence_fails("x: [a]\ny: [b\n  z: 1\n", (3, 4), (2, 4));
    sequence_fails("[{a: 1} b]", (1, 9), (1, 1));
    mapping_fails("{a]", (1, 3), (1, 1));
    mapping_fails("{a: 1]", (1, 6), (1, 1));
    mapping_fails("{a: 1 b: 2}", (1, 8), (1, 1));
    mapping_fails("{a: 1, b: 2 c: 3}", (1, 14), (1, 1));
    mapping_fails("{\"a\" b: 1}", (1, 6), (1, 1));
}

#[test]
fn the_innermost_open_collection_is_the_one_named() {
    // A closing bracket of the wrong kind fails where it stands.
    sequence_fails("{a: [b}", (1, 7), (1, 5));
    sequence_fails("{a: [b, c}\n", (1, 10), (1, 5));
    sequence_fails("{a: [b}, c}", (1, 7), (1, 5));
    sequence_fails("[a: {b: [c}], d]", (1, 11), (1, 9));
    mapping_fails("[{a: 1]", (1, 7), (1, 2));
    mapping_fails("[a: {b], c]", (1, 7), (1, 5));
    // A nested collection that never closes.
    sequence_ends_lacking_a_node("[[", (1, 3), (1, 2));
    sequence_ends("[? [a", (1, 6), (1, 4));
    sequence_ends("[a: [b", (1, 7), (1, 5));
    sequence_ends_lacking_a_node("{[", (1, 3), (1, 2));
    sequence_ends("{[a", (1, 4), (1, 2));
    sequence_ends("{? [a", (1, 6), (1, 4));
    mapping_ends_lacking_a_node("[{", (1, 3), (1, 2));
    mapping_ends_lacking_a_node("{{", (1, 3), (1, 2));
}

#[test]
fn a_collection_that_has_closed_is_never_named() {
    sequence_ends("[[a]", (1, 5), (1, 1));
    sequence_ends("[[a], {b: 1}", (1, 13), (1, 1));
    sequence_ends("[a]: [b", (1, 8), (1, 6));
    sequence_fails("[[a] b]", (1, 6), (1, 1));
    sequence_fails("{a: [b], c: [d}", (1, 15), (1, 13));
    mapping_ends("{a: {b: 1}", (1, 11), (1, 1));
    mapping_ends_lacking_a_node("{[a]: ", (1, 7), (1, 1));
    mapping_fails("{a: [b] c: d}", (1, 9), (1, 1));
}

#[test]
fn a_bracket_in_a_quoted_scalar_is_never_named() {
    sequence_fails("[\"[\" x]", (1, 6), (1, 1));
    sequence_fails("['[' x]", (1, 6), (1, 1));
    sequence_fails("[\"{\", \"]\", '}' x]", (1, 16), (1, 1));
    sequence_fails("x: \"[\"\ny: [a}\n", (2, 6), (2, 4));
    sequence_fails("\"[\": [a}", (1, 8), (1, 6));
    sequence_ends("x: ['{', [a", (1, 12), (1, 10));
    mapping_fails("{\"[\": 1 y: 2}", (1, 10), (1, 1));
    mapping_ends("{\"]\": '}'", (1, 10), (1, 1));
    // A bracket in a plain scalar or a comment isn't one either.
    sequence_fails("x: a[b\ny: [c}\n", (2, 6), (2, 4));
    sequence_ends("# [\n[a # {\n", (3, 1), (2, 1));
}

#[test]
fn line_endings_and_marks_and_wide_characters_count_as_they_do_elsewhere() {
    // CRLF.
    sequence_ends("a: [b,\r\n  c\r\n", (3, 1), (1, 4));
    sequence_fails("a: [b,\r\n  \"c\" d\r\n]\r\n", (2, 7), (1, 4));
    mapping_ends("a: {b: 1,\r\n  c: 2\r\n", (3, 1), (1, 4));
    // A lone CR breaks a line too.
    sequence_ends("a: [b,\r  c\r", (3, 1), (1, 4));
    // A byte order mark at the start of the input takes up no column.
    sequence_ends("\u{feff}[a", (1, 3), (1, 1));
    mapping_fails("\u{feff}{a: 1]", (1, 6), (1, 1));
    sequence_ends_lacking_a_node("\u{feff}x: [\n", (2, 1), (1, 4));
    // Columns count characters, not bytes.
    sequence_ends("é: [ñ, é", (1, 9), (1, 4));
    sequence_fails("é: [ñ, \"é\" é: x", (1, 12), (1, 4));
    mapping_fails("日本語: {鍵: 値 x: y}", (1, 13), (1, 6));
    mapping_ends("日本語: {鍵: 値\n", (2, 1), (1, 6));
    sequence_ends("😀: [😀", (1, 6), (1, 4));
}

/// A node is what's missing: default mode says what stands there instead, with both locations.
/// Compat gives libyaml's problem for a missing node, and the collection's opening as its context
/// where libyaml names the node.
#[track_caller]
fn node_fails(input: &str, problem: &str, name: &str, failure: At, opening: At) {
    let (line, column) = failure;
    let (opened_line, opened_column) = opening;
    let default = format!(
        "{problem} at line {line} column {column}, \
         in the flow {name} opened at line {opened_line} column {opened_column}"
    );
    for (reader, error) in default_errors(input) {
        assert_eq!(error.to_string(), default, "{reader}, {input:?}");
        assert_eq!(
            error.message(),
            without_location(&default, failure),
            "{reader}, {input:?}"
        );
        assert_eq!(location(&error), failure, "{reader}, {input:?}");
    }
    #[cfg(feature = "libyaml-compat")]
    compat_failure_is(
        input,
        &format!(
            "did not find expected node content at line {line} column {column}, \
             while parsing a flow {name} at line {opened_line} column {opened_column}"
        ),
        failure,
    );
}

#[test]
fn a_token_where_a_node_belongs_names_itself_and_the_opening() {
    let empty = "unexpected empty flow sequence entry";
    node_fails("[,]", empty, "sequence", (1, 2), (1, 1));
    node_fails("[a,,b]", empty, "sequence", (1, 4), (1, 1));
    node_fails("{a: [b,,]}", empty, "sequence", (1, 8), (1, 5));
    let empty = "unexpected empty flow mapping entry";
    node_fails("{,}", empty, "mapping", (1, 2), (1, 1));
    node_fails("{a,,b}", empty, "mapping", (1, 4), (1, 1));
    node_fails("[{a,,b}]", empty, "mapping", (1, 5), (1, 2));
    let closing = "unexpected } in node position";
    node_fails("[}", closing, "sequence", (1, 2), (1, 1));
    node_fails("[a, }", closing, "sequence", (1, 5), (1, 1));
}

/// A `]` ends a mapping's value before it starts, which isn't where the error may stop looking.
#[test]
fn a_stray_bracket_where_a_mapping_value_belongs_names_itself_and_the_opening() {
    let problem = "unexpected ] in node position";
    node_fails("{a: ]", problem, "mapping", (1, 5), (1, 1));
    node_fails("{a: ]}", problem, "mapping", (1, 5), (1, 1));
    node_fails("[{a: ]", problem, "mapping", (1, 6), (1, 2));
    node_fails("a: 1\nb: 2\nc: {d: ]\n", problem, "mapping", (3, 8), (3, 4));
    // Where a key belongs, too: nothing stands for it.
    node_fails("{]", problem, "mapping", (1, 2), (1, 1));
    node_fails("{a: 1, ]", problem, "mapping", (1, 8), (1, 1));
    node_fails("{? ]", problem, "mapping", (1, 4), (1, 1));
}

/// A key on two lines is refused where it starts, unless its brackets don't match: that's the
/// failure, and it says more.
#[test]
fn a_malformed_collection_in_a_key_reports_the_collection() {
    sequence_fails("[a\n}: 1", (2, 1), (1, 1));
    mapping_fails("{a\n]: 1", (2, 1), (1, 1));
    node_fails(
        "[\n}: 1",
        "unexpected } in node position",
        "sequence",
        (2, 1),
        (1, 1),
    );
    for input in ["[a\n]: 1", "{a\n}: 1", "[\n]: 1", "[[a]\n]: 1"] {
        for (name, error) in default_errors(input) {
            assert_eq!(
                error.to_string(),
                "implicit mapping keys must be on one line at line 1 column 1",
                "{name} on {input:?}"
            );
        }
    }
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn a_stray_bracket_before_an_alias_key_is_the_failure_in_compat() {
    compat_failure_is(
        "[\n}*a:",
        "did not find expected node content at line 2 column 1, while parsing a flow sequence at line 1 column 1",
        (2, 1),
    );
}

#[test]
fn outside_a_flow_collection_an_error_stays_as_it_was() {
    for (input, message, at) in [
        ("]", "unexpected ] in node position", (1, 1)),
        ("}", "unexpected } in node position", (1, 1)),
        (",", "unexpected , in node position", (1, 1)),
        ("- ]", "unexpected ] in node position", (1, 3)),
        ("[a] b", "unexpected token after node", (1, 5)),
        ("[a]]", "unexpected token after node", (1, 4)),
        ("{a: 1}}", "unexpected token after node", (1, 7)),
    ] {
        let (line, column) = at;
        let want = format!("{message} at line {line} column {column}");
        for (reader, error) in default_errors(input) {
            assert_eq!(error.to_string(), want, "{reader}, {input:?}");
            assert_eq!(location(&error), at, "{reader}, {input:?}");
        }
        #[cfg(feature = "libyaml-compat")]
        {
            let error = ayaml::compat::from_str::<Value>(input).expect_err("compat");
            assert_eq!(error.to_string(), want, "{input:?}");
        }
    }
}

/// An error about a node itself keeps its wording inside a collection too, and names no opening.
#[test]
fn an_error_about_a_node_names_no_opening() {
    for (input, at) in [
        ("{!t *a", (1, 5)),
        ("[!t *a", (1, 5)),
        ("{a: &x *a}", (1, 8)),
    ] {
        let (line, column) = at;
        let want = format!("aliases cannot carry tags or anchors at line {line} column {column}");
        for (reader, error) in default_errors(input) {
            assert_eq!(error.to_string(), want, "{reader}, {input:?}");
        }
        #[cfg(feature = "libyaml-compat")]
        {
            let error = ayaml::compat::from_str::<Value>(input).expect_err("compat");
            assert_eq!(error.to_string(), want, "{input:?}");
        }
    }
}

#[test]
fn a_scalar_that_never_ends_reports_itself_and_not_the_collection() {
    for input in ["[\"a", "['a", "{a: \"b"] {
        for (reader, error) in default_errors(input) {
            let text = error.to_string();
            assert!(text.contains("unterminated"), "{reader}: {text}");
            assert!(text.contains("quoted scalar"), "{reader}: {text}");
            assert!(!text.contains("flow"), "{reader}: {text}");
        }
    }
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_gives_the_wording_libyaml_gives_for_an_unclosed_sequence() {
    let input = "name: x\nitems: [a, b\n";
    compat_failure_is(
        input,
        "did not find expected ',' or ']' at line 3 column 1, \
         while parsing a flow sequence at line 2 column 8",
        (3, 1),
    );
}

/// Compat words a flow sequence's failure as libyaml's parser does from where it stands, which an
/// empty `?` key moves: libyaml takes the `:` or `,` after it into the pair, and then asks for a
/// `,` or `]`. The problems are libyaml 0.2.5's; the locations are compat's own, the end of the
/// last line where the input ends without a line break.
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_words_a_flow_sequence_s_failure_from_libyaml_s_state() {
    let separator = "did not find expected ',' or ']'";
    let node = "did not find expected node content";
    for (input, problem, failure, opening) in [
        // After an empty `?` key, at the end of the input or at a `}`.
        ("[? :", separator, (1, 5), (1, 1)),
        ("[? ,", separator, (1, 5), (1, 1)),
        ("[? :\n", separator, (2, 1), (1, 1)),
        ("[a, ? :", separator, (1, 8), (1, 1)),
        ("--- [? ,", separator, (1, 9), (1, 5)),
        ("[? : }", separator, (1, 6), (1, 1)),
        ("[?, }", separator, (1, 5), (1, 1)),
        // A `}` where a key or a value starts is where libyaml looks for a node.
        ("[a: }", node, (1, 5), (1, 1)),
        ("[? }", node, (1, 4), (1, 1)),
        ("[a, ? }", node, (1, 7), (1, 1)),
        // Elsewhere the failure is the token that stands where a `,` belongs, even when the
        // input then ends where a node would start.
        ("[\"a\" \"b\",", separator, (1, 6), (1, 1)),
        ("[\"a\" \"b\" [", separator, (1, 6), (1, 1)),
    ] {
        let (line, column) = failure;
        let (opened_line, opened_column) = opening;
        compat_failure_is(
            input,
            &format!(
                "{problem} at line {line} column {column}, \
                 while parsing a flow sequence at line {opened_line} column {opened_column}"
            ),
            failure,
        );
    }
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn the_modes_keep_their_own_readings() {
    // Compat takes a zero-indented flow continuation, as libyaml does, and default mode doesn't.
    let input = "a: [\nb]\n";
    assert!(ayaml::parse_value(input).is_err());
    ayaml::compat::from_str::<Value>(input).expect("compat reads it");
    // A later line's mark takes up a column in compat, as in libyaml, and is an error by default.
    let input = "---\n\u{feff}[a";
    compat_failure_is(
        input,
        "did not find expected ',' or ']' at line 2 column 4, \
         while parsing a flow sequence at line 2 column 2",
        (2, 4),
    );
    let error = ayaml::parse_value(input).expect_err("default mode");
    assert_eq!(
        error.to_string(),
        "a byte order mark can only start a document at line 2 column 1"
    );
    // At the very end of the input the mark is whitespace by default.
    let input = "[a,\n\u{feff}";
    sequence_ends_by_default(input, (2, 1), (1, 1));
    compat_failure_is(
        input,
        "did not find expected node content at line 2 column 2, \
         while parsing a flow sequence at line 1 column 1",
        (2, 2),
    );
}

#[cfg(feature = "libyaml-compat")]
#[track_caller]
fn sequence_ends_by_default(input: &str, failure: At, opening: At) {
    let (line, column) = failure;
    let (opened_line, opened_column) = opening;
    let want = format!(
        "unterminated flow sequence at line {line} column {column}, \
         opened at line {opened_line} column {opened_column}"
    );
    for (reader, error) in default_errors(input) {
        assert_eq!(error.to_string(), want, "{reader}, {input:?}");
        assert_eq!(location(&error), failure, "{reader}, {input:?}");
    }
}
