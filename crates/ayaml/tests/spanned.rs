//! Path lookup and spans over the read-only spanned tree.

use ayaml::{PathSegment, SpannedTree, Value};

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

    let entry = tree
        .entry_span_at(&[PathSegment::key("a"), PathSegment::key("b")])
        .expect("entry span");
    assert_eq!(entry.slice(input), Some("  b: plain\n"));
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
    let input = "\u{feff}a:\r\n  é: \"x\"\r\n";
    let tree = SpannedTree::parse(input).expect("tree");
    let path = [PathSegment::key("a"), PathSegment::key("é")];
    let span = tree.span_at(&path).expect("span");
    assert_eq!(span.slice(input), Some("\"x\""));
    let entry = tree.entry_span_at(&path).expect("entry");
    assert_eq!(entry.slice(input), Some("  é: \"x\"\r\n"));
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

/// `value` without the entry at `path`. A block collection left with no entry reads as an empty
/// value, which is a null.
fn without(value: &Value, path: &[PathSegment]) -> Value {
    let (first, rest) = path.split_first().expect("a path to an entry");
    let mut value = value.clone();
    match (&mut value, first) {
        (Value::Mapping(mapping), PathSegment::Key(name)) => {
            if rest.is_empty() {
                mapping.shift_remove(name).expect("the entry");
            } else {
                let inner = mapping.get_mut(name).expect("the entry");
                *inner = without(inner, rest);
            }
            if mapping.is_empty() {
                return Value::Null;
            }
        }
        (Value::Sequence(items), PathSegment::Index(at)) => {
            if rest.is_empty() {
                items.remove(*at);
            } else {
                items[*at] = without(&items[*at], rest);
            }
            if items.is_empty() {
                return Value::Null;
            }
        }
        (value, segment) => panic!("{segment:?} does not lead into {value:?}"),
    }
    value
}

/// The entry span at `path`, as the text it covers. For a span that exists, deleting those bytes
/// must leave a document that reads as the original without that entry and nothing else changed.
fn entry<'a>(input: &'a str, path: &[PathSegment]) -> Option<&'a str> {
    let tree = SpannedTree::parse(input).expect("a tree");
    let span = tree.entry_span_at(path)?;
    let mut rest = input.to_owned();
    rest.replace_range(span.range(), "");
    let original = ayaml::parse_value(input).expect("the input reads");
    let read = ayaml::parse_value(&rest)
        .unwrap_or_else(|error| panic!("{input:?} without {path:?} is {rest:?}: {error}"));
    assert_eq!(
        read,
        without(&original, path),
        "{input:?} without {path:?} is {rest:?}"
    );
    Some(span.slice(input).expect("a span inside the input"))
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

    let items = "a:\n  - 1\n  - 2\nd: 3\n";
    assert_eq!(entry(items, &[key("a")]), Some("a:\n  - 1\n  - 2\n"));
    assert_eq!(entry(items, &[key("a"), index(0)]), Some("  - 1\n"));

    // An item that ends in a block scalar stops at the scalar's end, not at the next item's.
    assert_eq!(entry("- |\n  a\n- b\n", &[index(0)]), Some("- |\n  a\n"));

    let comment = "a: 1 # note\n\n# about b\nb: 2\n";
    assert_eq!(entry(comment, &[key("a")]), Some("a: 1 # note\n"));
    assert_eq!(entry(comment, &[key("b")]), Some("b: 2\n"));

    // A flow collection is one value, on however many lines.
    let flow = "a: [1,\n  2]\nx: {k: v}\n";
    assert_eq!(entry(flow, &[key("a")]), Some("a: [1,\n  2]\n"));
    assert_eq!(entry(flow, &[key("x")]), Some("x: {k: v}\n"));

    let nested_items = "- - a\n  - b\n- c\n";
    assert_eq!(entry(nested_items, &[index(0)]), Some("- - a\n  - b\n"));
    assert_eq!(entry(nested_items, &[index(0), index(1)]), Some("  - b\n"));

    // The last entry of an input with no final line break.
    assert_eq!(entry("a: 1\nb: 2", &[key("b")]), Some("b: 2"));
    // A byte-order mark stays with the document, and a CRLF goes with its entry.
    assert_eq!(
        entry("\u{feff}a: 1\r\nb: 2\r\n", &[key("a")]),
        Some("a: 1\r\n")
    );
}

/// An entry with no span that removes it and nothing else has no entry span.
#[test]
fn an_entry_with_no_exact_span_has_none() {
    // A flow entry's separator belongs to its neighbor as much as to it.
    assert_eq!(entry("x: {a: 1, b: 2}\n", &[key("x"), key("a")]), None);
    assert_eq!(entry("x: [10, 20]\n", &[key("x"), index(0)]), None);
    // The first key of a mapping that begins on its item's line shares that line with the `-`.
    let compact = "- a: 1\n  b: 2\n- c\n";
    assert_eq!(entry(compact, &[index(0), key("a")]), None);
    assert_eq!(entry(compact, &[index(0), key("b")]), Some("  b: 2\n"));
    assert_eq!(entry(compact, &[index(0)]), Some("- a: 1\n  b: 2\n"));
    // So does the first item of a sequence that begins on its item's line.
    assert_eq!(entry("- - a\n  - b\n", &[index(0), index(0)]), None);
    // An explicit key, and a key with a property before it.
    assert_eq!(entry("? a\n: 1\nb: 2\n", &[key("a")]), None);
    // An entry that defines an anchor, which an alias may name.
    assert_eq!(entry("a: &x 1\nb: 2\n", &[key("a")]), None);
    assert_eq!(entry("a:\n  c: &x 1\nb: 2\n", &[key("a")]), None);
    // An item with no value.
    assert_eq!(entry("-\n- b\n", &[index(0)]), None);
}

/// A value that the source doesn't write stands right after its key's `:`, and its entry is the
/// key's line.
#[test]
fn a_missing_value_stands_after_its_key() {
    let input = "a:\nb: 1\nc:   # none\nd:";
    let tree = SpannedTree::parse(input).expect("a tree");
    for (name, at) in [("a", 2), ("c", 10), ("d", 22)] {
        let span = tree.span_at(&[key(name)]).expect("a span");
        assert_eq!((span.start(), span.end()), (at, at), "{name}");
        assert_eq!(&input[at - 2..at], format!("{name}:"), "{name}");
    }
    assert_eq!(entry(input, &[key("a")]), Some("a:\n"));
    assert_eq!(entry(input, &[key("c")]), Some("c:   # none\n"));
    assert_eq!(entry(input, &[key("d")]), Some("d:"));
    // A flow mapping's key with no value, and one with no `:` either.
    let flow = "{a: , b}\n";
    let tree = SpannedTree::parse(flow).expect("a tree");
    let span = tree.span_at(&[key("a")]).expect("a span");
    assert_eq!((span.start(), span.end()), (3, 3));
    let span = tree.span_at(&[key("b")]).expect("a span");
    assert_eq!((span.start(), span.end()), (7, 7));
}
