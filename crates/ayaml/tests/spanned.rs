//! Path lookup and spans over the read-only spanned tree.

use ayaml::{Event, PathSegment, SpannedTree, Value};

#[test]
fn scalar_positions_and_encodings_follow_the_source() {
    let input = "a:\n  b: plain\n  c: \"quoted\"\nd: |\n  block\n  text\ne: [1, {f: flow}]\n";
    let tree = SpannedTree::parse(input).expect("spanned tree");
    let path = [PathSegment::key("a"), PathSegment::key("b")];
    let span = tree.span_at(&path).expect("span");
    assert_eq!(span.slice(input), Some("plain"));
    assert_eq!(tree.scalar_text_at(&path), Some("plain"));
    assert_eq!(tree.scalar_encoding_at(&path), Some("plain"));

    let path = [PathSegment::key("a"), PathSegment::key("c")];
    assert_eq!(tree.scalar_text_at(&path), Some("\"quoted\""));
    let path = [PathSegment::key("d")];
    assert_eq!(tree.scalar_text_at(&path), Some("|\n  block\n  text\n"));
    let path = [
        PathSegment::key("e"),
        PathSegment::index(1),
        PathSegment::key("f"),
    ];
    assert_eq!(tree.scalar_text_at(&path), Some("flow"));
    assert!(tree.span_at(&[PathSegment::key("e")]).is_some());

    assert_eq!(entry(input, &[key("a"), key("b")]), Some("  b: plain\n"));
}

#[test]
fn aliases_merge_keys_duplicates_and_multiple_documents_are_ambiguous() {
    let alias = SpannedTree::parse("a: &x 1\nb: *x\n").expect("tree");
    assert!(alias.lookup(&[PathSegment::key("b")]).is_none());
    let merge = SpannedTree::parse("base: {a: 1}\nuse:\n  <<: {b: 2}\n").expect("tree");
    assert!(
        merge
            .lookup(&[PathSegment::key("use"), PathSegment::key("<<")])
            .is_none()
    );
    let duplicate = SpannedTree::parse("a: 1\na: 2\n").expect("tree");
    assert!(duplicate.lookup(&[PathSegment::key("a")]).is_none());
    let multiple = SpannedTree::parse("a\n---\nb\n").expect("tree");
    assert!(multiple.lookup(&[PathSegment::key("a")]).is_none());
    assert_eq!(multiple.documents().len(), 2);
}

#[test]
fn spans_keep_crlf_bom_and_multibyte_offsets() {
    let input = "\u{feff}a:\r\n  é: \"x\"\r\n  ü: y\r\n";
    let tree = SpannedTree::parse(input).expect("tree");
    let path = [PathSegment::key("a"), PathSegment::key("é")];
    let span = tree.span_at(&path).expect("span");
    assert_eq!(span.slice(input), Some("\"x\""));
    assert_eq!(entry(input, &path), Some("  é: \"x\"\r\n"));
}

#[test]
fn spanned_tree_uses_normal_limits() {
    let limits = ayaml::Limits::default().with_max_events(1);
    assert!(SpannedTree::parse_with_limits("a: 1\n", &limits).is_err());
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_parse_looks_up_the_same_paths() {
    let input = "a:\n  b: plain\n";
    let tree = SpannedTree::parse_compat(input).expect("compat tree");
    assert_eq!(
        tree.scalar_text_at(&[PathSegment::key("a"), PathSegment::key("b")]),
        Some("plain")
    );
}

fn key(name: &str) -> PathSegment {
    PathSegment::key(name)
}

fn index(at: usize) -> PathSegment {
    PathSegment::index(at)
}

/// The mode a document is read in: the default one, or the one that reads as libyaml does.
#[derive(Clone, Copy, Debug)]
enum Mode {
    Default,
    #[cfg(feature = "libyaml-compat")]
    Compat,
}

impl Mode {
    fn all() -> Vec<Self> {
        vec![
            Self::Default,
            #[cfg(feature = "libyaml-compat")]
            Self::Compat,
        ]
    }

    fn tree(self, input: &str) -> Result<SpannedTree<'_>, ayaml::Error> {
        match self {
            Self::Default => SpannedTree::parse(input),
            #[cfg(feature = "libyaml-compat")]
            Self::Compat => SpannedTree::parse_compat(input),
        }
    }

    fn value(self, input: &str) -> Result<Value, ayaml::Error> {
        match self {
            Self::Default => ayaml::parse_value(input),
            #[cfg(feature = "libyaml-compat")]
            Self::Compat => ayaml::compat::from_str(input),
        }
    }

    /// The document's events without their places: what each is, with its tag, its anchor,
    /// its style, and its text. `None` if the events don't read.
    fn events(self, input: &str) -> Option<Vec<String>> {
        let stream = match self {
            Self::Default => ayaml::parse_events(input),
            #[cfg(feature = "libyaml-compat")]
            Self::Compat => ayaml::compat::parse_events(input),
        };
        stream
            .ok()?
            .map(|event| {
                Some(match event.ok()? {
                    Event::Scalar {
                        value,
                        style,
                        tag,
                        anchor,
                        ..
                    } => format!("scalar {value:?} {style:?} {tag:?} {anchor:?}"),
                    Event::Alias { anchor, .. } => format!("alias {anchor:?}"),
                    Event::SequenceStart {
                        tag, anchor, style, ..
                    } => format!("sequence {style:?} {tag:?} {anchor:?}"),
                    Event::MappingStart {
                        tag, anchor, style, ..
                    } => format!("mapping {style:?} {tag:?} {anchor:?}"),
                    Event::DocumentStart { explicit, .. } => format!("document {explicit}"),
                    Event::DocumentEnd { explicit, .. } => format!("document end {explicit}"),
                    Event::Directive { name, value, .. } => format!("directive {name} {value}"),
                    other => format!("{:?}", std::mem::discriminant(&other)),
                })
            })
            .collect()
    }
}

/// Whether `after` is `before` with one run of events taken out, and nothing else changed. An
/// entry's events stand together, so that is what removing an entry does to them. It tells
/// what a value can't: a tag that is dropped, an anchor, and a scalar's style.
fn one_run_removed(before: &[String], after: &[String]) -> bool {
    let same = |pair: &(&String, &String)| pair.0 == pair.1;
    let prefix = before.iter().zip(after).take_while(same).count();
    let suffix = before[prefix..]
        .iter()
        .rev()
        .zip(after[prefix..].iter().rev())
        .take_while(same)
        .count();
    after.len() < before.len() && prefix + suffix == after.len()
}

/// `value` without the entry at `path`, or `None` if the path leads to no entry, or to the only
/// entry of its collection, which has no entry span.
fn without(value: &Value, path: &[PathSegment]) -> Option<Value> {
    let (first, rest) = path.split_first()?;
    let mut value = value.clone();
    match (&mut value, first) {
        // A tagged collection's entries are found at the collection's own path.
        (Value::Tagged(tagged), _) => tagged.value = without(&tagged.value, path)?,
        (Value::Mapping(mapping), PathSegment::Key(name)) => {
            if rest.is_empty() {
                mapping.shift_remove(name)?;
                if mapping.is_empty() {
                    return None;
                }
            } else {
                let inner = mapping.get_mut(name)?;
                *inner = without(inner, rest)?;
            }
        }
        (Value::Sequence(items), PathSegment::Index(at)) => {
            if rest.is_empty() {
                if *at >= items.len() || items.len() == 1 {
                    return None;
                }
                items.remove(*at);
            } else {
                let inner = items.get_mut(*at)?;
                *inner = without(inner, rest)?;
            }
        }
        _ => return None,
    }
    Some(value)
}

/// What is wrong with the entry span at `path`, if the tree gives one and anything is: deleting
/// those bytes must leave a document that reads as the original without that entry, and with
/// nothing else changed.
fn inexact(
    mode: Mode,
    input: &str,
    tree: &SpannedTree<'_>,
    path: &[PathSegment],
) -> Option<String> {
    let span = tree.entry_span_at(path)?;
    if span.slice(input).is_none() {
        return Some(format!("{span:?} is not a slice of the input"));
    }
    let mut rest = input.to_owned();
    rest.replace_range(span.range(), "");
    let before = mode.events(input)?;
    match mode.events(&rest) {
        None => return Some(format!("the rest, {rest:?}, has no events")),
        Some(after) if !one_run_removed(&before, &after) => {
            return Some(format!(
                "the rest, {rest:?}, differs by more than one run of events: {after:?}"
            ));
        }
        Some(_) => {}
    }
    let original = mode.value(input).ok()?;
    let Some(expected) = without(&original, path) else {
        return Some("it removes the only entry of a collection".to_owned());
    };
    match mode.value(&rest) {
        Err(error) => Some(format!("the rest, {rest:?}, does not read: {error}")),
        // A mapping is equal to one with its entries in another order, so what is printed is
        // compared too.
        Ok(read) if read != expected || format!("{read:?}") != format!("{expected:?}") => {
            Some(format!("the rest, {rest:?}, reads as {read:?}"))
        }
        Ok(_) => None,
    }
}

/// The entry span at `path`, as the text it covers, in every mode. Each span is checked by
/// deleting it and reading the rest (see `inexact`), and the modes have to agree.
fn entry<'a>(input: &'a str, path: &[PathSegment]) -> Option<&'a str> {
    let mut spans = Mode::all().into_iter().map(|mode| {
        let tree = mode.tree(input).expect("a tree");
        if let Some(problem) = inexact(mode, input, &tree, path) {
            panic!("{mode:?} {input:?} at {path:?}: {problem}");
        }
        tree.entry_span_at(path)
            .map(|span| span.slice(input).expect("a span inside the input"))
    });
    let first = spans.next().expect("a mode");
    for other in spans {
        assert_eq!(other, first, "{input:?} at {path:?}: the modes differ");
    }
    first
}

/// A block entry's span takes the whole entry: the lines of a nested value, a block scalar
/// through its last line break, and a comment after the value. Each span here is checked by
/// deleting it and reading the rest (see `entry`).
#[test]
fn a_block_entry_span_removes_the_whole_entry() {
    let nested = "a:\n  b: 1\n  c: 2\nd: 3\n";
    assert_eq!(entry(nested, &[key("a")]), Some("a:\n  b: 1\n  c: 2\n"));
    assert_eq!(entry(nested, &[key("a"), key("c")]), Some("  c: 2\n"));
    assert_eq!(entry(nested, &[key("d")]), Some("d: 3\n"));

    let block = "a: |\n  line1\n  line2\nd: 3\n";
    assert_eq!(entry(block, &[key("a")]), Some("a: |\n  line1\n  line2\n"));
    // A block scalar's trailing blank lines are its own, whatever it does with them.
    let keep = "a: |+\n  x\n\nb: 1\n";
    assert_eq!(entry(keep, &[key("a")]), Some("a: |+\n  x\n\n"));

    let items = "a:\n  - 1\n  - 2\nd: 3\n";
    assert_eq!(entry(items, &[key("a")]), Some("a:\n  - 1\n  - 2\n"));
    assert_eq!(entry(items, &[key("a"), index(0)]), Some("  - 1\n"));
    // A sequence at its key's indentation.
    let indentless = "a:\n- 1\n- 2\nb: 3\n";
    assert_eq!(entry(indentless, &[key("a")]), Some("a:\n- 1\n- 2\n"));
    assert_eq!(entry(indentless, &[key("a"), index(1)]), Some("- 2\n"));

    // An item that ends in a block scalar stops at the scalar's end, not at the next item's.
    assert_eq!(entry("- |\n  a\n- b\n", &[index(0)]), Some("- |\n  a\n"));

    let comment = "a: 1 # note\n\n# about b\nb: 2\n";
    assert_eq!(entry(comment, &[key("a")]), Some("a: 1 # note\n"));
    assert_eq!(entry(comment, &[key("b")]), Some("b: 2\n"));
    // A comment between a key and its nested value goes with the entry.
    let inner = "a: # about a\n  b: 1\n  c: 2\nd: 3\n";
    assert_eq!(
        entry(inner, &[key("a")]),
        Some("a: # about a\n  b: 1\n  c: 2\n")
    );

    // A flow collection is one value, on however many lines, and so is a scalar on several.
    let flow = "a: [1,\n  2]\nx: {k: v}\ny: one\n  two\nz: \"one\n  two\"\n";
    assert_eq!(entry(flow, &[key("a")]), Some("a: [1,\n  2]\n"));
    assert_eq!(entry(flow, &[key("x")]), Some("x: {k: v}\n"));
    assert_eq!(entry(flow, &[key("y")]), Some("y: one\n  two\n"));
    assert_eq!(entry(flow, &[key("z")]), Some("z: \"one\n  two\"\n"));

    let nested_items = "- - a\n  - b\n- c\n";
    assert_eq!(entry(nested_items, &[index(0)]), Some("- - a\n  - b\n"));
    assert_eq!(entry(nested_items, &[index(0), index(1)]), Some("  - b\n"));
    let compact = "- a: 1\n  b: 2\n- c\n";
    assert_eq!(entry(compact, &[index(0)]), Some("- a: 1\n  b: 2\n"));
    assert_eq!(entry(compact, &[index(0), key("b")]), Some("  b: 2\n"));

    // A tagged value, a quoted key, and a key that is a flow collection.
    let tagged = "a: !t x\n\"b\": 1\n[1, 2]: 3\nc: !t\n  d: 1\n  e: 2\n";
    assert_eq!(entry(tagged, &[key("a")]), Some("a: !t x\n"));
    assert_eq!(entry(tagged, &[key("b")]), Some("\"b\": 1\n"));
    assert_eq!(entry(tagged, &[key("c"), key("d")]), Some("  d: 1\n"));

    // The last entry of an input with no final line break.
    assert_eq!(entry("a: 1\nb: 2", &[key("b")]), Some("b: 2"));
    // A byte-order mark stays with the document, and a CRLF or a lone CR goes with its entry.
    assert_eq!(
        entry("\u{feff}a: 1\r\nb: 2\r\n", &[key("a")]),
        Some("a: 1\r\n")
    );
    assert_eq!(entry("a: 1\rb: 2\r", &[key("a")]), Some("a: 1\r"));
    // An entry before a document's end marker.
    assert_eq!(entry("a: 1\nb: 2\n...\n", &[key("b")]), Some("b: 2\n"));
}

/// An entry with no span that removes it and nothing else has no entry span.
#[test]
fn an_entry_with_no_exact_span_has_none() {
    // A flow entry's separator belongs to its neighbor as much as to it.
    assert_eq!(
        entry("x: {a: 1, b: 2}\ny: 0\n", &[key("x"), key("a")]),
        None
    );
    assert_eq!(entry("x: [10, 20]\ny: 0\n", &[key("x"), index(0)]), None);
    // The first key of a mapping that begins on its item's line shares that line with the `-`,
    // and so does the first item of a sequence that begins there.
    assert_eq!(entry("- a: 1\n  b: 2\n- c\n", &[index(0), key("a")]), None);
    assert_eq!(entry("- - a\n  - b\n- c\n", &[index(0), index(0)]), None);
    // An item with no value, and one whose value begins on the next line.
    assert_eq!(entry("-\n- b\n", &[index(0)]), None);
    assert_eq!(entry("- a\n- ", &[index(1)]), None);
    assert_eq!(entry("-\n  a: 1\n- b\n", &[index(0)]), None);
    // An entry that defines an anchor, which an alias may name, and an entry inside a
    // collection that has one, which an alias to the collection reads.
    assert_eq!(entry("a: &x 1\nb: 2\n", &[key("a")]), None);
    assert_eq!(entry("a:\n  c: &x 1\n  d: 2\nb: 2\n", &[key("a")]), None);
    assert_eq!(
        entry(
            "base: &b\n  x: 1\n  y: 2\nuse: *b\n",
            &[key("base"), key("x")]
        ),
        None
    );
    // The only entry of a collection: without it there is no collection.
    assert_eq!(entry("a:\n  b: 1\nc: 2\n", &[key("a"), key("b")]), None);
    assert_eq!(entry("a:\n  - 1\nc: 2\n", &[key("a"), index(0)]), None);
    assert_eq!(entry("a: 1\n", &[key("a")]), None);
    assert_eq!(
        entry("k: !!int\n  a: 1\nz: 0\n", &[key("k"), key("a")]),
        None
    );
    // A key that is a block scalar ends with its last line, so the `:` on the next line is
    // another entry's, and the entry around the key ends where no one can say.
    assert_eq!(entry("a:\n  ? |\n:\nb: 1\n", &[key("a")]), None);
    assert_eq!(entry("a:\n  ? |\n    k\n:\nb: 1\n", &[key("a")]), None);
    // The same where lines end in a carriage return alone, or in both.
    assert_eq!(entry("a:\r  ? |\r:\rb: 1\r", &[key("a")]), None);
    assert_eq!(entry("a:\r\n  ? |\r\n:\r\nb: 1\r\n", &[key("a")]), None);
}

/// A key that is an alias has no text of its own to compare, and nor has a key that holds one.
/// It may be any other key of its mapping again, which a reader that keeps the last of two
/// equal keys then reads from it. So no key of that mapping is looked up, and none of its
/// entries has a span. The mappings around it and beside it are as they would be without it.
#[test]
fn a_key_that_is_an_alias_makes_its_mapping_s_lookups_a_guess() {
    for mode in Mode::all() {
        for (input, path) in [
            ("x: &k a\na: 1\n*k : 2\n", vec![key("a")]),
            ("x: &k a\n*k : 1\na: 2\n", vec![key("a")]),
            ("x: &k a\n*k : 1\nb: 2\nc: 3\n", vec![key("b")]),
            ("x: &k a\n*k : 1\nb: 2\nc: 3\n", vec![key("x")]),
            (
                "p:\n  x: &k a\n  *k : 1\n  b: 2\nq: 1\n",
                vec![key("p"), key("b")],
            ),
            ("x: &k [1]\n? [*k]\n: 1\nb: 2\nc: 3\n", vec![key("b")]),
        ] {
            let tree = mode.tree(input).expect("a tree");
            assert_eq!(tree.span_at(&path), None, "{mode:?} {input:?}");
            assert_eq!(tree.entry_span_at(&path), None, "{mode:?} {input:?}");
            assert!(tree.lookup(&path).is_none(), "{mode:?} {input:?}");
        }
        let beside = "p:\n  x: &k a\n  *k : 1\n  b: 2\nq: 1\nr: 2\n";
        let tree = mode.tree(beside).expect("a tree");
        let span = tree.span_at(&[key("q")]).expect("a span");
        assert_eq!(span.slice(beside), Some("1"), "{mode:?}");
        let span = tree.entry_span_at(&[key("q")]).expect("an entry span");
        assert_eq!(span.slice(beside), Some("q: 1\n"), "{mode:?}");
        assert!(tree.lookup(&[key("p")]).is_some(), "{mode:?}");
        // An alias that is a value is no key, and its mapping's keys are looked up.
        let value = "x: &k a\ny: *k\nz: 1\n";
        let tree = mode.tree(value).expect("a tree");
        let span = tree.entry_span_at(&[key("z")]).expect("an entry span");
        assert_eq!(span.slice(value), Some("z: 1\n"), "{mode:?}");
    }
}

/// A byte-order mark at the start of a later line doesn't open the stream. The compatibility
/// reader counts it toward that line's indentation, and reads past one that begins a comment
/// or a blank line. With the lines before it gone it would open the input and count for
/// nothing. So what such a mark means depends on the lines before it, and an input that holds
/// one past its first character gives no entry span.
#[cfg(feature = "libyaml-compat")]
#[test]
fn an_input_with_a_byte_order_mark_past_its_start_has_no_entry_span() {
    for (input, path) in [
        (" - 1\n\u{feff}- 2\n - 3\n", vec![index(0)]),
        (" - 1\n\u{feff}- 2\n - 3\n", vec![index(2)]),
        ("? a\nb: 2\n\u{feff}# c\n: 3\n", vec![key("b")]),
        ("? a\nb: 2\n\n\u{feff}# c\n: 3\n", vec![key("b")]),
    ] {
        let tree = SpannedTree::parse_compat(input).expect("a tree");
        assert_eq!(tree.entry_span_at(&path), None, "{input:?} at {path:?}");
    }
    // A mark that opens the input is no part of the first entry, and takes no span away.
    assert_eq!(entry("\u{feff}a: 1\nb: 2\n", &[key("a")]), Some("a: 1\n"));
}

/// In such an input a line's indentation is the reader's to say too: the compatibility reader
/// counts the mark as a column, so a line that a mark begins may hold the value of the key
/// before it where its spaces alone would put it at the margin. A value that isn't written
/// then stays where the parser places it, at the next token, and is not moved to its key's
/// `:`.
#[cfg(feature = "libyaml-compat")]
#[test]
fn a_value_keeps_the_parser_s_place_after_a_byte_order_mark_past_the_start() {
    for (input, path, place) in [
        ("a:\n\u{feff}  !!map\nb: 1\n", vec![key("a")], 8),
        ("a:\n\u{feff}!!map\n", vec![key("a")], 6),
        (
            "top:\n  a:\n\u{feff}   !!map\n  b: 1\n",
            vec![key("top"), key("a")],
            16,
        ),
        (" a:\n\u{feff}\n  !!set\n", vec![key("a")], 10),
        // A value that is plainly missing stays at the next token there too.
        ("a:\nb: 1\n\u{feff}# c\n", vec![key("a")], 3),
    ] {
        let tree = SpannedTree::parse_compat(input).expect("a tree");
        let span = tree.span_at(&path).expect("a span");
        assert_eq!((span.start(), span.end()), (place, place), "{input:?}");
    }
    // Without the mark, the value on the line after its key is that line's tag, and a value
    // that is plainly missing stands after its key's `:`.
    let tree = SpannedTree::parse_compat("a:\n  !!map\nb: 1\n").expect("a tree");
    assert_eq!(tree.span_at(&[key("a")]).map(ayaml::Span::start), Some(5));
    let tree = SpannedTree::parse_compat("a:\nb: 1\n# c\n").expect("a tree");
    assert_eq!(tree.span_at(&[key("a")]).map(ayaml::Span::start), Some(2));
}

/// A tag or an anchor before a key puts the key further right than its mapping. Whether the
/// next line could hold the key's value is judged from the mapping's column: a line indented
/// past the mapping that holds only a tag for a collection's kind is that value, with no event
/// to show for it, so the entries around it end where no one can say.
#[test]
fn a_value_s_place_is_judged_from_the_mapping_s_column() {
    let tagged = "k: 0\ntop:\n  x: 1\n  !t a:\n   !!seq\nnext: 1\n";
    assert_eq!(entry(tagged, &[key("top")]), None);
    assert_eq!(entry("- x\n- !t a:\n   !!map\n- y\n", &[index(1)]), None);
    // A byte-order mark that opens the input is no column of the mapping it stands before.
    assert_eq!(entry("\u{feff}a:\n !!map\nb: 1\n", &[key("a")]), None);
    assert_eq!(entry("\u{feff}a:\nb: 1\nc: 2\n", &[key("a")]), Some("a:\n"));
    // With the next line at the mapping's column, the value is plainly missing.
    let missing = "top:\n  x: 1\n  &n a:\n  b: 2\nnext: 1\n";
    for mode in Mode::all() {
        let tree = mode.tree(missing).expect("a tree");
        let at = tree.span_at(&[key("top"), key("a")]).expect("a span");
        assert_eq!((at.start(), at.end()), (19, 19), "{mode:?}");
        // The first key of a mapping on its item's line: the mapping begins at the key.
        let item = "- a:\n  b: 1\n- c\n";
        let tree = mode.tree(item).expect("a tree");
        let at = tree.span_at(&[index(0), key("a")]).expect("a span");
        assert_eq!((at.start(), at.end()), (4, 4), "{mode:?}");
    }
    let tagged = "top:\n  x: 1\n  !t a:\n  b: 2\nnext: 1\n";
    assert_eq!(
        entry(tagged, &[key("top")]),
        Some("top:\n  x: 1\n  !t a:\n  b: 2\n")
    );
}

/// Each of these has no span for a reason of its own, which no other test reaches.
#[test]
fn an_entry_has_no_span_for_each_of_these_reasons() {
    // A flow entry that begins its own line is still a flow entry.
    let flow = "x: {\n  a: 1\n  ,\n  b: 2\n}\ny: 0\n";
    assert_eq!(entry(flow, &[key("x"), key("a")]), None);
    // An item of a sequence that has an anchor, and an entry of an item that has one.
    let sequence = "base: &b\n  - x\n  - y\nuse: *b\n";
    assert_eq!(entry(sequence, &[key("base"), index(0)]), None);
    assert_eq!(entry(sequence, &[key("base")]), None);
    let item = "- &m\n  k: 1\n  j: 2\n- *m\n- z\n";
    assert_eq!(entry(item, &[index(0), key("k")]), None);
    assert_eq!(entry(item, &[index(0)]), None);
    assert_eq!(entry(item, &[index(2)]), Some("- z\n"));
    // A `:` line is looked for past blank and comment lines, and a tab may follow the `:`.
    assert_eq!(entry("? a\nb: 1\n\n# c\n: c\n", &[key("b")]), None);
    assert_eq!(entry("? a\nb: 1\n:\tc\n", &[key("b")]), None);
    // An item that defines an anchor, an entry around one that does, an entry around a key
    // that does, and an entry whose key holds one.
    assert_eq!(entry("- &n 1\n- *n\n- 2\n", &[index(0)]), None);
    assert_eq!(entry("- k: &n 1\n  j: 2\n- *n\n- z\n", &[index(0)]), None);
    assert_eq!(entry("- - &n 1\n  - 2\n- *n\n- z\n", &[index(0)]), None);
    assert_eq!(entry("a:\n  - &n 1\n  - 2\nb: *n\n", &[key("a")]), None);
    assert_eq!(entry("a:\n  &k x: 1\n  y: 2\nb: *k\n", &[key("a")]), None);
    let holds = PathSegment::Key(ayaml::parse_value("[a]").expect("a key"));
    assert_eq!(entry("[&k a]: 1\nb: *k\nc: 2\n", &[holds]), None);
    let own = "base: &b\n  x: 1\n  y: 2\nuse: *b\n";
    assert_eq!(entry(own, &[key("base")]), None);
    // A key with no text.
    assert_eq!(entry(": 1\nb: 2\n", &[PathSegment::Key(Value::Null)]), None);
    // A mapping item and a sequence item after a block scalar, with a blank line after each.
    let after = "- |+\n  x\n- a: 1\n  b: 2\n\n- c\n";
    assert_eq!(entry(after, &[index(1)]), None);
    let after = "- |+\n  x\n- - 1\n  - 2\n\n- c\n";
    assert_eq!(entry(after, &[index(1)]), None);
    // In a stream of several documents no path leads to one of them.
    for mode in Mode::all() {
        let tree = mode.tree("a: 1\nb: 2\n---\na: 3\nb: 4\n").expect("a tree");
        assert_eq!(tree.entry_span_at(&[key("a")]), None, "{mode:?}");
        assert!(tree.lookup(&[key("a")]).is_none(), "{mode:?}");
    }
}

/// Each of these has a span, where a rule that is too wide would give none.
#[test]
fn an_entry_keeps_its_span_in_each_of_these() {
    // A tab before a comment, and a blank before the key's `:`.
    assert_eq!(entry("a: 1\t# c\nb: 2\n", &[key("a")]), Some("a: 1\t# c\n"));
    assert_eq!(entry("a : 1\nb: 2\n", &[key("a")]), Some("a : 1\n"));
    // Lines that end in a carriage return alone.
    assert_eq!(entry("a: 1\rb: 2\rc: 3\r", &[key("b")]), Some("b: 2\r"));
    assert_eq!(entry("a: 1\rb: 2\rc: 3\r", &[key("c")]), Some("c: 3\r"));
    // An entry that ends in an alias.
    let alias = "- &n 1\n- k:\n    - 2\n    - *n\n- 3\n";
    assert_eq!(entry(alias, &[index(1)]), Some("- k:\n    - 2\n    - *n\n"));
    // A comment line between two entries stays with the entry after it.
    assert_eq!(
        entry("a: 1\n# about b\nb: 2\n", &[key("a")]),
        Some("a: 1\n")
    );
    // An alias and an empty flow collection are written, so each ends the block scalar before
    // it: the entry after one is not after a block scalar, and a blank line may follow it.
    for between in ["- *n", "- []", "- {}"] {
        let input = format!("- &n q\n- |\n  x\n{between}\n- y\n\n- z\n");
        assert_eq!(entry(&input, &[index(3)]), Some("- y\n"), "{input:?}");
    }
}

/// A value that isn't written stands after its key's `:` where the source shows that it wrote
/// none: past a comment after tabs, and in a flow mapping before a `,` or the closing brace. An
/// empty value with an anchor or a tag is written, as that, and stays where the parser puts it.
#[test]
fn a_missing_value_is_placed_by_what_follows_its_key() {
    for mode in Mode::all() {
        for (input, path, place) in [
            ("a:\n\t# c\nb: 1\n", vec![key("a")], Some(2)),
            ("a:\n  \t# c\nb: 1\n", vec![key("a")], Some(2)),
            ("x: {a: , b: 1}\n", vec![key("x"), key("a")], Some(6)),
            ("x: {a: 1, b: }\n", vec![key("x"), key("b")], Some(12)),
            ("a: &n\nb: *n\n", vec![key("a")], None),
            ("a: !t\nb: 1\n", vec![key("a")], None),
            // A tag for a collection's kind leaves no event, and stands where a value would.
            ("x: {a: !!map, b: 1}\n", vec![key("x"), key("a")], None),
            ("x:\n  a: !!map\n  b: 1\n", vec![key("x"), key("a")], None),
        ] {
            let tree = mode.tree(input).expect("a tree");
            let span = tree.span_at(&path).expect("a span");
            match place {
                Some(place) => {
                    assert_eq!(
                        (span.start(), span.end()),
                        (place, place),
                        "{mode:?} {input:?}"
                    );
                }
                None => assert_ne!(
                    span.start(),
                    after_colon(input, &path),
                    "{mode:?} {input:?}"
                ),
            }
        }
    }
}

/// The offset after the `:` of the last key of `path`, found by the key's name.
fn after_colon(input: &str, path: &[PathSegment]) -> usize {
    let Some(PathSegment::Key(Value::String(name))) = path.last() else {
        panic!("a path that ends in a string key");
    };
    input.find(&format!("{name}:")).expect("the key") + name.len() + 1
}

/// A pair in a flow sequence is a mapping with no braces. Its span ends where its value does,
/// not at the token after it, whatever its key is. Where the source writes no value, the pair
/// ends with the last thing it does write: its key, a `?` or `:`, a tag, or an anchor.
#[test]
fn a_pair_in_a_flow_sequence_ends_with_its_value() {
    for mode in Mode::all() {
        for (input, text) in [
            ("x: [a: b ]\n", "a: b"),
            ("x: [a: b\n  ]\n", "a: b"),
            ("x: [ a: b # c\n  , d]\n", "a: b"),
            ("x: [a: [1, 2] , d]\n", "a: [1, 2]"),
            ("x: [{a: b} , d]\n", "{a: b}"),
            // A key in braces doesn't make the pair a mapping in braces.
            ("x: [{a: 1}: v ]\n", "{a: 1}: v"),
            ("x: [{a: 1}: v # c\n  , d]\n", "{a: 1}: v"),
            ("x: [{}: {b: 2}\n]\n", "{}: {b: 2}"),
            ("x: [{a: 1}: ]\n", "{a: 1}:"),
            // A value that isn't written.
            ("x: [a: ]\n", "a:"),
            ("x: [a:\n  # c\n  ]\n", "a:"),
            ("x: [a: !t ]\n", "a: !t"),
            ("x: [a: &n # c\n ]\n", "a: &n"),
            ("x: [a: !!map , d]\n", "a: !!map"),
            ("x: [!t a: !u ]\n", "!t a: !u"),
            ("x: [? a # c\n ]\n", "? a"),
            ("x: [? a : ]\n", "? a :"),
            ("x: [ : ]\n", ":"),
            ("x: [? ]\n", "?"),
        ] {
            let tree = mode.tree(input).expect("a tree");
            let span = tree.span_at(&[key("x"), index(0)]).expect("a span");
            assert_eq!(span.slice(input), Some(text), "{mode:?} {input:?}");
        }
    }
}

/// A key after a `?`, on that line or a later one, has no entry span, and nor does a key with
/// a property before it or a key with no text. An entry before a line that begins with `:` has
/// none either: with the entry gone, that line would give its value to a key before it.
#[test]
fn an_explicit_key_has_no_entry_span() {
    assert_eq!(entry("? a\n: 1\nb: 2\n", &[key("a")]), None);
    assert_eq!(entry("?\n  a\n: 1\nb: 2\n", &[key("a")]), None);
    assert_eq!(entry("?\n  a\nb: 1\n", &[key("a")]), None);
    assert_eq!(
        entry(
            "? - a\n  - b\n: v\nc: 1\n",
            &[PathSegment::Key(list(&["a", "b"]))]
        ),
        None
    );
    assert_eq!(
        entry(
            "?\n- a\n- b\n: v\nc: 1\n",
            &[PathSegment::Key(list(&["a", "b"]))]
        ),
        None
    );
    // A key that is a block mapping, which ends in a block scalar, with its `:` below it.
    let block_key = PathSegment::Key(Value::Mapping(
        [(Value::from("h"), Value::from(""))].into_iter().collect(),
    ));
    assert_eq!(entry("?\n h: >-\n: 1\nb: 2\n", &[block_key]), None);
    // A tag or an anchor before the key.
    assert_eq!(entry("!!str a: 1\nb: 2\n", &[key("a")]), None);
    assert_eq!(entry("&k a: 1\nb: 2\n", &[key("a")]), None);
    // A key with no text stands at its `:`, with or without a `?` and a tag before it.
    assert_eq!(entry("a: 1\n: 2\n", &[PathSegment::Key(Value::Null)]), None);
    // An entry between a `?` key with no value and a line that begins with `:`.
    assert_eq!(entry("? a\nb: 1\n: c\n", &[key("b")]), None);
    // An entry that ends in a `?` key whose `:` is on its own line ends on that line, which
    // nothing here finds, so it has no span.
    assert_eq!(entry("top:\n  ? a\n  :\nnext: 1\n", &[key("top")]), None);
    assert_eq!(entry("- ? a\n  :\n- b\n", &[index(0)]), None);
}

fn list(items: &[&str]) -> Value {
    Value::Sequence(items.iter().map(|item| Value::from(*item)).collect())
}

/// The blank and comment lines after an entry stay when it goes. A block scalar before the
/// entry would take them as its own, so the entry has a span only when content follows it.
#[test]
fn an_entry_after_a_block_scalar_needs_content_after_it() {
    // The next entry follows directly, or the input ends.
    assert_eq!(
        entry("a: |\n  x\nb: 1\nc: 2\n", &[key("b")]),
        Some("b: 1\n")
    );
    assert_eq!(entry("a: |\n  x\nb: 1\n", &[key("b")]), Some("b: 1\n"));
    assert_eq!(entry("- |\n  x\n- b\n- c\n", &[index(1)]), Some("- b\n"));
    // A blank line that a keeping scalar would keep, a line of blanks that any would, a comment
    // that would be its text, and a tab that it refuses.
    assert_eq!(entry("a: |+\n  x\nb: 1\n\nc: 2\n", &[key("b")]), None);
    assert_eq!(entry("a: |\n  x\nb: 1\n    \nc: 2\n", &[key("b")]), None);
    assert_eq!(entry("a: |\n  x\nb: 1\n\t# c\nd: 2\n", &[key("b")]), None);
    assert_eq!(entry("- |+\n  x\n- b\n\n- c\n", &[index(1)]), None);
    // The scalar is the last thing written before the entry, however deep, and in a key too.
    let nested = "a:\n  script: |\n    echo hi\n  env:\n    FOO: 1\n    # BAR: 2\nb: 1\n";
    assert_eq!(entry(nested, &[key("a"), key("env")]), None);
    assert_eq!(
        entry(nested, &[key("a"), key("script")]),
        Some("  script: |\n    echo hi\n")
    );
    let in_key = PathSegment::Key(list(&[""]));
    assert_eq!(entry("?\n - |\no: 'q'\n  \t# c\nz: 1\n", &[key("o")]), None);
    assert!(entry("?\n - |\no: 'q'\nz: 1\n", &[in_key]).is_none());
    // Anything else before the entry takes no line after it.
    assert_eq!(
        entry("a: x\nb: 1\n\n  # c\nc: 2\n", &[key("b")]),
        Some("b: 1\n")
    );
}

/// A value that the source doesn't write after its key's `:`, on the key's line, has an empty
/// span right after that `:`, and its entry is the key's line.
#[test]
fn a_missing_value_stands_after_its_key() {
    let input = "a:\nb: 1\nc:   # none\nd:";
    for mode in Mode::all() {
        let tree = mode.tree(input).expect("a tree");
        for (name, at) in [("a", 2), ("c", 10), ("d", 22)] {
            let span = tree.span_at(&[key(name)]).expect("a span");
            assert_eq!((span.start(), span.end()), (at, at), "{mode:?} {name}");
            assert_eq!(&input[at - 2..at], format!("{name}:"), "{name}");
        }
    }
    assert_eq!(entry(input, &[key("a")]), Some("a:\n"));
    assert_eq!(entry(input, &[key("c")]), Some("c:   # none\n"));
    assert_eq!(entry(input, &[key("d")]), Some("d:"));
    // A flow mapping's key with a `:` and no value.
    let flow = "{a: , b}\n";
    let tree = SpannedTree::parse(flow).expect("a tree");
    let span = tree.span_at(&[key("a")]).expect("a span");
    assert_eq!((span.start(), span.end()), (3, 3));
}

/// A value stays where the parser places it, at the token after it, when nothing shows where it
/// would be written: after a key with no `:` on its line, and where something that the events
/// don't carry stands between the `:` and that token. Its entry then has no span.
#[test]
fn a_value_that_is_not_plainly_missing_keeps_its_place() {
    // An explicit key with its `:` on a line of its own.
    let explicit = "? a\n:\nb: 1\n";
    let tree = SpannedTree::parse(explicit).expect("a tree");
    let span = tree.span_at(&[key("a")]).expect("a span");
    assert_eq!((span.start(), span.end()), (6, 6));
    // A tag that names the kind of a collection, which the events drop from a scalar: on the
    // line after the key, and on the key's line.
    for dropped in ["z: 0\na:\n  !!map\nb: 1\n", "z: 0\na: !!map\nb: 1\n"] {
        let tree = SpannedTree::parse(dropped).expect("a tree");
        let span = tree.span_at(&[key("a")]).expect("a span");
        assert!(span.start() > 7, "{dropped:?}: {span:?}");
        assert_eq!(entry(dropped, &[key("a")]), None, "{dropped:?}");
    }
    // A list that ends in an item with no value ends at a place that isn't known, so its entry
    // doesn't take the comment about the entry after it.
    assert_eq!(
        entry(
            "list:\n  - a\n  -\n# settings for b\nb: 1\n",
            &[key("list")]
        ),
        None
    );
    assert_eq!(entry("a: !t\n\n# about b\nb: 1\n", &[key("a")]), None);
}

/// Lines that a document is made of, each at a random indentation. Most documents made this way
/// don't parse. The ones that do put every kind of entry next to every other, which is where an
/// entry span goes wrong: what is exact for an entry depends on the lines around it.
const LINES: &[&str] = &[
    "a: 1",
    "b: two",
    "c:",
    "d: [1, 2]",
    "e: {x: 1}",
    "f: |",
    "g: |+",
    "h: >-",
    "i: &n 3",
    "j: *n",
    "k: !t 4",
    "l: !!map",
    "m: !!str",
    "n: \"q\"",
    "o: 'q'",
    "p: # c",
    "- 1",
    "- two",
    "-",
    "- a: 1",
    "- - 1",
    "- |",
    "- &n 5",
    "- *n",
    "- [1, 2]",
    "? a",
    "?",
    ": 1",
    ":",
    "text",
    "more text",
    "# comment",
    "",
    "  ",
    "\t",
    "\t# c",
    "!t",
    "&n",
    "!!int",
    "a: 1 # c",
    "- 1 # c",
    "a:   ",
    "\"k\": 1",
    "[1, 2]: 3",
    "<<: *n",
    "a: b: c",
    "? - 1",
    "? a: 1",
    "x: 1\r",
    "...",
    "? |",
    "? >-",
    "\u{feff}- 1",
    "\u{feff}a: 1",
    "\u{feff}",
    "\u{feff}# c",
    "!t a:",
    "&n b:",
    "!!map",
    "!!seq",
    "c: &n",
    "- &n",
    "a: !!set",
];

/// The next number of a fixed sequence (xorshift64*), so the documents are the same in every
/// run.
fn next(state: &mut u64) -> usize {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    usize::try_from(state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33).expect("31 bits")
}

fn generated(state: &mut u64) -> String {
    let lines = 2 + next(state) % 7;
    let mut text = String::new();
    for _ in 0..lines {
        let indent = [0, 0, 0, 2, 2, 4, 1, 3][next(state) % 8];
        text.push_str(&" ".repeat(indent));
        text.push_str(LINES[next(state) % LINES.len()]);
        text.push_str(if next(state).is_multiple_of(16) {
            "\r\n"
        } else {
            "\n"
        });
    }
    if next(state).is_multiple_of(8) {
        text.pop();
    }
    text
}

/// Every path to an entry of `value`, a tagged collection's at the collection's own path.
fn paths(value: &Value, here: &mut Vec<PathSegment>, all: &mut Vec<Vec<PathSegment>>) {
    match value {
        Value::Tagged(tagged) => paths(&tagged.value, here, all),
        Value::Mapping(mapping) => {
            for (name, inner) in mapping {
                here.push(PathSegment::Key(name.clone()));
                all.push(here.clone());
                paths(inner, here, all);
                here.pop();
            }
        }
        Value::Sequence(items) => {
            for (at, inner) in items.iter().enumerate() {
                here.push(PathSegment::index(at));
                all.push(here.clone());
                paths(inner, here, all);
                here.pop();
            }
        }
        _ => {}
    }
}

/// On generated documents, every entry span that a tree gives is exact: deleting it leaves a
/// document that reads as the original without that entry (see `inexact`). The count of spans
/// keeps the test from passing because the generator stopped making documents that parse.
#[test]
fn every_entry_span_of_a_generated_document_is_exact() {
    for mode in Mode::all() {
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let (mut documents, mut spans) = (0, 0);
        for _ in 0..60_000 {
            let input = generated(&mut state);
            let (Ok(tree), Ok(value)) = (mode.tree(&input), mode.value(&input)) else {
                continue;
            };
            if tree.documents().len() != 1 {
                continue;
            }
            documents += 1;
            let mut all = Vec::new();
            paths(&value, &mut Vec::new(), &mut all);
            for path in all {
                spans += usize::from(tree.entry_span_at(&path).is_some());
                if let Some(problem) = inexact(mode, &input, &tree, &path) {
                    panic!("{mode:?} {input:?} at {path:?}: {problem}");
                }
            }
        }
        assert!(
            documents >= 5_000 && spans >= 2_000,
            "{mode:?}: {spans} entry spans in {documents} documents"
        );
    }
}
