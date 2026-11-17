//! Path lookup and spans over the read-only spanned tree.

use ayaml::{PathSegment, SpannedTree};

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
