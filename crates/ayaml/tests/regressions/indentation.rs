use super::*;

/// A tab separates wherever it doesn't indent a block collection (YAML 1.2.2 section 6.1): after a
/// key or a node property, and before a flow collection, a quoted scalar, a block scalar's header,
/// or a comment, though a `: ` follows in one of them. Each reads as it does with a space, in
/// each mode.
#[test]
fn a_tab_separates_where_it_does_not_indent() {
    for (input, with_spaces) in [
        // After `key:`, before a flow collection or a quoted value holding a colon, and after a
        // quoted value before a comment holding one.
        ("key:\t\"a: b\"", "key: \"a: b\""),
        ("key:\t'a: b'", "key: 'a: b'"),
        ("key:\t{a: 1}", "key: {a: 1}"),
        ("key:\t[a: 1]", "key: [a: 1]"),
        ("a: \"1\"\t# note: x", "a: \"1\" # note: x"),
        // Before `:` after a quoted, empty, alias, or flow key, or a node property alone.
        ("\"a\"\t: 1", "\"a\" : 1"),
        ("''\t: 1", "'' : 1"),
        ("- &a x\n- *a\t: 2", "- &a x\n- *a : 2"),
        ("[1]\t: x", "[1] : x"),
        ("{a: 1}\t: x", "{a: 1} : x"),
        ("&x\t: 1", "&x : 1"),
        ("!t\t: 1", "!t : 1"),
        // Between a node property and a key or a flow collection, and after `---` before a flow
        // collection or a property.
        ("!t\ta: 1", "!t a: 1"),
        ("- &x\ta: 1", "- &x a: 1"),
        ("&x\t[1]", "&x [1]"),
        ("---\t[1]", "--- [1]"),
        ("---\t{a: 1}", "--- {a: 1}"),
        ("---\t!:", "--- !:"),
        // Where the only later `: ` is in a comment, whatever comes before the tab.
        ("- a\t# c: d", "- a # c: d"),
        ("k:\t# c: d\n  v", "k: # c: d\n  v"),
        ("-\t# c: d\n  x", "- # c: d\n  x"),
        ("\t# c: d\nx", " # c: d\nx"),
        ("a: 1\n\t# c: d\nb: 2", "a: 1\n # c: d\nb: 2"),
        ("a\n...\t# c: d", "a\n... # c: d"),
        // After `---`, `-`, `?`, or `:`, or at a line's start, before a quoted scalar holding `: `
        // or a flow collection.
        ("---\t'a: b'", "--- 'a: b'"),
        ("---\t\"a: b\"", "--- \"a: b\""),
        ("-\t[1]", "- [1]"),
        ("-\t'a: b'", "- 'a: b'"),
        ("-\t\"a: b\"", "- \"a: b\""),
        ("\t[1]", " [1]"),
        ("\t{a: b}", " {a: b}"),
        ("\t'a: b'", " 'a: b'"),
        ("? a\n:\t'b: c'", "? a\n: 'b: c'"),
        ("?\t[a]\n:\tb", "? [a]\n: b"),
        // Or one that goes on to the next line, so it can't be a key.
        ("-\t\"a: b\n  c\"", "- \"a: b\n  c\""),
        ("k:\t'a: b\n  c'", "k: 'a: b\n  c'"),
        ("-\t[a: b,\n  c]", "- [a: b,\n  c]"),
        // Before a block scalar's header, with a `: ` in its comment.
        ("---\t>\n a", "--- >\n a"),
        ("-\t| # c: d\n  a", "- | # c: d\n  a"),
        // A property's name may end in `:`.
        ("-\t&a: b", "- &a: b"),
        ("&x:\ta: b", "&x: a: b"),
    ] {
        for ((mode, read), (_, expected)) in readings(input).into_iter().zip(readings(with_spaces))
        {
            let expected = expected.unwrap_or_else(|error| panic!("{with_spaces:?}: {error}"));
            assert_eq!(read, Ok(expected), "{input:?} in {mode}");
        }
    }
    // A block scalar's header is judged on its own after a tab too, so a comment with no blank
    // before it is the error, though a `: ` follows.
    for (input, with_spaces) in [
        ("---\t|#: c\n x", "--- |#: c\n x"),
        ("-\t>#: c\n  x", "- >#: c\n  x"),
    ] {
        for ((mode, read), (_, expected)) in readings(input).into_iter().zip(readings(with_spaces))
        {
            assert!(
                expected
                    .as_ref()
                    .is_err_and(|error| error.contains("comment")),
                "{with_spaces:?} in {mode}: {expected:?}"
            );
            assert_eq!(read, expected, "{input:?} in {mode}");
        }
    }
}

/// A tab still can't indent a block collection: at a line's start, or after a `-`, `?`, or `:`
/// indicator or `---`, before a block indicator or an implicit key, whichever blank follows that
/// indicator.
#[test]
fn a_tab_cannot_indent_a_block_collection() {
    for (input, line, column) in [
        ("\ta: 1", 1, 1),
        ("\t- a", 1, 1),
        ("\t? a", 1, 1),
        ("\t: a", 1, 1),
        ("\t[1]: x", 1, 1),
        ("-\ta: 1", 1, 2),
        ("-\t- a", 1, 2),
        ("- \t- a", 1, 3),
        ("?\ta: 1", 1, 2),
        ("-\t[1]: x", 1, 2),
        ("-\t'a': b", 1, 2),
        ("-\t\"a\"\t: b", 1, 2),
        ("-\t&a b: c", 1, 2),
        ("-\t!t : c", 1, 2),
        ("- a\n-\t\"b\": c", 2, 2),
        ("---\ta: 1", 1, 4),
        ("key:\ta: b", 1, 5),
        ("key:\t- a", 1, 5),
        ("\u{feff}\t- a", 1, 1),
        // A tab after the indicator, too.
        ("\t-\tb", 1, 1),
        ("\t?\tb", 1, 1),
        ("-\t-\tb", 1, 2),
        ("k:\n\t-\tb", 2, 1),
        // After a document end marker, as after `---`.
        ("...\ta: 1", 1, 4),
        ("--- a\n...\ta: 1", 2, 4),
    ] {
        let expected = format!(
            "tab cannot separate a block structural indicator at line {line} column {column}"
        );
        for (mode, read) in readings(input) {
            assert_eq!(read, Err(expected.clone()), "{input:?} in {mode}");
        }
    }
    // A line's leading byte order marks are counted again on each line, after a tab on an earlier
    // one. The modes count the mark's column differently, so only the line is checked.
    for input in ["a:\tb\n...\n\u{feff}\t- c", "-\ta\n...\n\u{feff}\tc: d"] {
        for (mode, read) in readings(input) {
            let error = read.expect_err(input);
            assert!(
                error.starts_with("tab cannot separate a block structural indicator at line 3 "),
                "{input:?} in {mode}: {error}"
            );
        }
    }
    // In compat, an anchor's name ends at a `?`, or at a `:` with more text after it, and an alias
    // gives up a `:` that ends its name. Then what follows is a key, so the tab indents a mapping.
    // By default the name runs on, so the tab only separates it.
    #[cfg(feature = "libyaml-compat")]
    for (input, line, column) in [
        ("-\t&a:b: c", 1, 2),
        ("-\t&a?b: c", 1, 2),
        ("- &a x\n-\t*a: b", 2, 2),
    ] {
        let expected = format!(
            "tab cannot separate a block structural indicator at line {line} column {column}"
        );
        let read =
            ayaml::compat::from_str::<ayaml::Value>(input).map_err(|error| error.to_string());
        assert_eq!(read, Err(expected), "{input:?}");
    }
    assert_eq!(
        ayaml::parse_value("-\t&a:b: c").expect("a name"),
        ayaml::parse_value("- c").expect("a sequence")
    );
}

/// A closing quote may follow tabs alone on its line, as a closing `]` or `}` may. Only the
/// scalar's own quote closes it, and `''` is a quote in a single-quoted scalar's text.
#[test]
fn a_closing_quote_may_follow_tabs_alone() {
    for (input, with_spaces) in [
        ("k: 'a\n\t'", "k: 'a\n '"),
        ("k: \"a\n\t\"", "k: \"a\n \""),
        ("- 'a\n\t\t'", "- 'a\n '"),
        ("k: 'a\n\n\t'", "k: 'a\n\n '"),
        ("k: \"a\\\n\t\"", "k: \"a\\\n \""),
    ] {
        for ((mode, read), (_, expected)) in readings(input).into_iter().zip(readings(with_spaces))
        {
            let expected = expected.unwrap_or_else(|error| panic!("{with_spaces:?}: {error}"));
            assert_eq!(read, Ok(expected), "{input:?} in {mode}");
        }
    }
    for input in [
        "k: 'a\n\tb'",
        "k: 'a\n\t''b'",
        "k: 'a\n\t\"'",
        "k: \"a\n\t'\"",
    ] {
        for (mode, read) in readings(input) {
            let error = read.expect_err(input);
            assert!(
                error.starts_with("tabs cannot be the only quoted-scalar indentation at line 2"),
                "{input:?} in {mode}: {error}"
            );
        }
    }
}

/// A comment may follow a `%TAG` directive's prefix, as it may follow `%YAML`'s version, and
/// the directive then reads as it does without it. Only a comment may follow the prefix.
#[test]
fn a_comment_may_follow_a_tag_directive() {
    for (input, without) in [
        ("%TAG ! tag:x: # c\n--- !a 1", "%TAG ! tag:x:\n--- !a 1"),
        (
            "%TAG !e! tag:e: #c\n---\n!e!a b",
            "%TAG !e! tag:e:\n---\n!e!a b",
        ),
        (
            "%TAG !! tag:y:\t#: c\n--- !!a b",
            "%TAG !! tag:y:\n--- !!a b",
        ),
        ("%TAG ! tag:x: #\n--- !a 1", "%TAG ! tag:x:\n--- !a 1"),
    ] {
        for ((mode, read), (_, expected)) in readings(input).into_iter().zip(readings(without)) {
            let expected = expected.unwrap_or_else(|error| panic!("{without:?}: {error}"));
            assert_eq!(read, Ok(expected), "{input:?} in {mode}");
        }
    }
    // A comment may hold a byte order mark in compat, as any comment may there, and the directive
    // still may not. By default no comment may hold one.
    #[cfg(feature = "libyaml-compat")]
    for (input, without) in [
        (
            "%TAG ! tag:x: #\u{feff}\n--- !a 1",
            "%TAG ! tag:x:\n--- !a 1",
        ),
        ("%YAML 1.2 # a\u{feff}\n--- a", "%YAML 1.2\n--- a"),
        ("%FOO bar\t#\u{feff}\n--- a", "%FOO bar\n--- a"),
    ] {
        let read =
            ayaml::compat::from_str::<ayaml::Value>(input).map_err(|error| error.to_string());
        let expected = ayaml::compat::from_str::<ayaml::Value>(without).expect("no mark");
        assert_eq!(read, Ok(expected), "{input:?}");
        let error = ayaml::parse_value(input).expect_err(input);
        assert!(
            error
                .to_string()
                .starts_with("a directive cannot contain a byte order mark"),
            "{input:?}: {error}"
        );
    }
    for input in [
        "%TAG ! tag:x:\u{feff}\n--- a",
        "%TAG ! tag:x:\u{feff} # c\n--- a",
        "%FOO b#\u{feff}\n--- a",
    ] {
        for (mode, read) in readings(input) {
            let error = read.expect_err(input);
            assert!(
                error.starts_with("a directive cannot contain a byte order mark"),
                "{input:?} in {mode}: {error}"
            );
        }
    }
    for input in [
        "%TAG ! tag:x: x # c\n--- a",
        "%TAG ! tag:x: x\n--- a",
        "%TAG ! tag:x: x#c\n--- a",
        "%TAG ! # c\n--- a",
    ] {
        for (mode, read) in readings(input) {
            let error = read.expect_err(input);
            assert!(
                error.starts_with("%TAG requires a handle and a prefix at line 1"),
                "{input:?} in {mode}: {error}"
            );
        }
    }
}

/// A plain scalar's line may start with `---` or `...` unless that is a document marker: at
/// column 0, with whitespace or the line's end after it (YAML 1.2.2 production 206).
#[test]
fn a_plain_scalar_line_may_start_with_marker_text() {
    reads_as(&[
        ("a\n ---", "'a ---'"),
        ("a\n ---x", "'a ---x'"),
        ("a\n---b", "'a ---b'"),
        ("a\n  ...", "'a ...'"),
        ("a\n ...\nb", "'a ... b'"),
        ("1\n...)", "'1 ...)'"),
        ("a\n\t---", "'a ---'"),
        ("key: a\n  ---", "key: 'a ---'"),
        ("k: a\n  ...and so on", "k: 'a ...and so on'"),
        ("- a\n  --- b", "- 'a --- b'"),
        // A marker still ends it.
        ("a\n...", "a"),
        ("a\n... # c", "a"),
    ]);
    for input in ["a\n---", "a\n--- b", "a\n---\tb", "a\n---\n b"] {
        let documents = ayaml::parse_documents(input).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(documents.len(), 2, "{input:?}");
    }
    // `...` and then a mark isn't a marker either. Compat reads the mark as text, as libyaml does,
    // and default mode refuses a mark there.
    #[cfg(feature = "libyaml-compat")]
    assert_eq!(
        ayaml::compat::from_str::<ayaml::Value>("0\n...\u{feff}").unwrap(),
        ayaml::Value::from("0 ...\u{feff}")
    );
    assert_eq!(
        ayaml::parse_value("0\n...\u{feff}")
            .unwrap_err()
            .to_string(),
        "a byte order mark can only start a document at line 2 column 4"
    );
}

/// A `:` in a comment isn't a mapping indicator, so a plain scalar's line may hold one there.
#[test]
fn a_comment_colon_does_not_end_a_plain_scalar() {
    reads_as(&[
        ("a\nb #c: d", "'a b'"),
        ("a\n b\t# c: d", "'a b'"),
        ("k: a\n  b # note: d", "k: 'a b'"),
        ("- a\n  b # c: d", "- 'a b'"),
        ("c\n| #:", "'c |'"),
        ("$\n? #:", "'$ ?'"),
        ("a\n ... # c: d", "'a ...'"),
        // A `:` before the comment still does.
        ("k: a\nb: c # d: e", "{k: a, b: c}"),
    ]);
    // Without a blank before it, `#` is text, and the `: ` after it an indicator, so the line
    // isn't the scalar's.
    let after = "unexpected content after document";
    refused_with(&[
        ("a\nb#c: d", &format!("{after} at line 2 column 1")),
        ("k: a\n  b#c: d", &format!("{after} at line 2 column 3")),
    ]);
}

/// A node property may end in `:`, as in `&a:`, and that `:` isn't an indicator. So a plain
/// scalar after one, on a line that the node starts or after `---`, goes on at its line's
/// indentation in default mode, as it does in compat.
#[test]
fn a_plain_scalar_after_a_property_ending_in_a_colon_goes_on() {
    reads_as(&[
        ("&a: b\nc", "&a: 'b c'"),
        ("--- &a: b\nc", "--- &a: 'b c'"),
        ("!t: b\nc", "!t: 'b c'"),
        ("&a: b\n c", "&a: 'b c'"),
        ("- &a: b\n  c", "- &a: 'b c'"),
    ]);
    // A block indicator or a key before the scalar still holds its lines past them.
    refused_with(&[
        (
            "- &a: b\nc",
            "unexpected content after document at line 2 column 1",
        ),
        (
            "k: b\nc",
            "unexpected content after document at line 2 column 1",
        ),
        (
            "&a k: b\nc",
            "unexpected content after document at line 2 column 1",
        ),
    ]);
}

/// A quoted scalar on its own line, or in a flow collection that is, is measured against the
/// block collection that holds it, not the line it opens on (YAML 1.2.2 production 69). Its
/// lines need only pass that collection's column, as a flow collection's do.
#[test]
fn a_quoted_scalar_on_its_own_line_is_measured_against_its_parent() {
    reads_as(&[
        ("k:\n  'a\n b'", "k: 'a b'"),
        ("k:\n  \"a\n b\"", "k: 'a b'"),
        ("- \n    'a\n  b'", "- 'a b'"),
        ("  k:\n      'a\n     b'", "k: 'a b'"),
        ("'k':\n  'a\n b'", "k: 'a b'"),
        ("k:\n  !t 'a\n b'", "k: !t 'a b'"),
        ("k: !t\n  'a\n b'", "k: !t 'a b'"),
        ("k:\n  &x\n  'a\n b'", "k: 'a b'"),
        ("k:\n  'a\n \tb'", "k: 'a b'"),
        ("k: [\n    'a\n  b']", "k: ['a b']"),
        ("k: [\n    \"a\n b\"]", "k: ['a b']"),
        ("k: [[\n    'a\n b']]", "k: [['a b']]"),
        ("k: {\n    a: 'b\n  c'}", "k: {a: 'b c'}"),
        ("k: [{\n    a: 'b\n  c'}]", "k: [{a: 'b c'}]"),
        ("k:\n  [? \"x\n y\"]", "k: [? 'x y']"),
        ("k:\n  [&a \"x\n y\"]", "k: ['x y']"),
        ("k:\n  [a,\n b]", "k: [a, b]"),
        ("- k:\n    [a,\n   b]", "- k: [a, b]"),
    ]);
    // The lines still have to pass it, and only spaces indent them.
    let below = "quoted scalar continuation must be indented more than its parent";
    let tabs = "tabs cannot be the only quoted-scalar indentation";
    refused_with(&[
        ("k:\n  'a\nb'", &format!("{below} at line 3 column 1")),
        (
            "  k:\n      'a\n  b'",
            &format!("{below} at line 3 column 3"),
        ),
        ("k: [\n    'a\nb']", &format!("{below} at line 3 column 1")),
        ("k:\n  'a\n\tb'", &format!("{tabs} at line 3 column 1")),
        ("k:\n  'a\n\t\tb'", &format!("{tabs} at line 3 column 1")),
        // Each outermost flow collection has its own owner, so a later one isn't held to an
        // earlier one's.
        (
            "a: [\n    'x\n  y']\nb:\n  - [\n      'x\n  y']",
            &format!("{below} at line 7 column 3"),
        ),
    ]);
}
