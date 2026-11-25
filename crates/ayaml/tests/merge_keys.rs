//! Unit and property tests for opt-in YAML 1.1 merge-key expansion across typed and event
//! readers (`ParseOptions::with_merge_keys`, AY-147).

use std::collections::BTreeMap;

use ayaml::{
    Deserializer, DocumentDeserializer, ErrorCode, ErrorKind, Event, LimitKind, Limits,
    ParseOptions, PathSegment, Spanned, SpannedTree, Value,
};
use proptest::prelude::*;
use serde::Deserialize;

const MERGE: ParseOptions = ParseOptions::new().with_merge_keys(true);

#[derive(Debug, PartialEq, Eq, Deserialize)]
struct Service<'a> {
    host: &'a str,
    port: u16,
    #[serde(rename = "<<")]
    literal_merge: &'a str,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
struct Doc<'a> {
    #[serde(borrow)]
    service: Service<'a>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
struct SpannedService<'a> {
    #[serde(borrow)]
    host: Spanned<&'a str>,
    port: Spanned<u16>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
struct SpannedDoc<'a> {
    #[serde(borrow)]
    service: SpannedService<'a>,
}

#[derive(Debug, Deserialize)]
struct ExpectInt {
    #[serde(rename = "final")]
    _final_map: BTreeMap<String, u32>,
}

#[derive(Debug, Deserialize)]
struct PropOut {
    k1: u32,
    k2: u32,
    k3: u32,
    k4: u32,
}

#[derive(Debug, Deserialize)]
struct PropRoot {
    out: PropOut,
}

#[test]
fn merge_keys_option_expands_plain_merges_and_keeps_quoted_keys_literal() {
    let input = "\
defaults: &def
  host: localhost
  port: 8080
service:
  <<: *def
  \"<<\": literal_double
  port: 9090
single:
  '<<': literal_single
explicit_str:
  !!str <<: literal_str
explicit_merge:
  !!merge <<: *def
";
    // Default YAML 1.2.2 parsing treats both plain `<<` and quoted `"<<"` as literal string keys,
    // so having both in `service:` is a DuplicateKey under default mode, whereas a mapping with
    // only plain `<<` keeps `<<` as a literal key.
    assert_eq!(
        ayaml::parse_value(input)
            .expect_err("default dup <<")
            .kind(),
        ErrorKind::DuplicateKey
    );
    let plain_only = "defaults: &def\n  host: localhost\nservice:\n  <<: *def\n";
    let default_val = ayaml::parse_value(plain_only).expect("default parse");
    assert!(
        default_val["service"]
            .as_mapping()
            .unwrap()
            .contains_key("<<")
    );
    assert_eq!(default_val["service"]["host"], Value::Null);

    // With merge_keys enabled, plain `<<` and `!!merge <<` expand, while quoted and `!!str <<`
    // remain literal keys.
    let merged_val = ayaml::parse_value_with(input, &MERGE).expect("merged parse");
    assert_eq!(merged_val["service"]["host"].as_str(), Some("localhost"));
    assert_eq!(merged_val["service"]["port"].as_u64(), Some(9090));
    assert_eq!(merged_val["service"]["<<"].as_str(), Some("literal_double"));
    assert_eq!(merged_val["single"]["<<"].as_str(), Some("literal_single"));
    assert_eq!(
        merged_val["explicit_str"]["<<"].as_str(),
        Some("literal_str")
    );
    assert_eq!(
        merged_val["explicit_merge"]["host"].as_str(),
        Some("localhost")
    );

    // Typed borrowed deserialization borrows merged strings directly from the original input.
    let typed: Doc<'_> = ayaml::from_str_with(input, &MERGE).expect("typed merged");
    assert_eq!(
        typed.service,
        Service {
            host: "localhost",
            port: 9090,
            literal_merge: "literal_double",
        }
    );

    // Works identically via from_slice_with, from_reader_with, DocumentDeserializer, Deserializer,
    // parse_documents_with, and parse_frontmatter_with.
    let from_slice: Doc<'_> =
        ayaml::from_slice_with(input.as_bytes(), &MERGE).expect("slice merged");
    assert_eq!(from_slice, typed);

    let from_reader: Value =
        ayaml::from_reader_with(input.as_bytes(), &MERGE).expect("reader merged");
    assert_eq!(from_reader, merged_val);

    let doc_de = DocumentDeserializer::from_str_with(input, &MERGE).expect("doc de merged");
    let from_doc_de = Doc::deserialize(doc_de).expect("doc de decode");
    assert_eq!(from_doc_de, typed);

    let mut cursor = Deserializer::from_str_with(input, &MERGE).expect("cursor merged");
    let from_cursor: Doc<'_> = cursor.next_borrowed().expect("next").expect("doc");
    assert_eq!(from_cursor, typed);

    let docs = ayaml::parse_documents_with(input, &MERGE).expect("docs merged");
    assert_eq!(docs, vec![merged_val]);

    let fm_doc = format!("---\n{input}---\n# Body\n");
    let fm: ayaml::ParsedFrontmatter<'_, Doc<'_>> = ayaml::parse_frontmatter_with(&fm_doc, &MERGE)
        .expect("frontmatter merged")
        .expect("some");
    assert_eq!(fm.frontmatter, typed);
    assert_eq!(fm.body, "# Body\n");

    // Spanned<T> on merged fields preserves the original source span from the anchored mapping.
    let spanned: SpannedDoc<'_> = ayaml::from_str_with(input, &MERGE).expect("spanned merged");
    assert_eq!(*spanned.service.host, "localhost");
    assert_eq!(spanned.service.host.span().slice(input), Some("localhost"));
    assert_eq!(*spanned.service.port, 9090);
    assert_eq!(spanned.service.port.span().slice(input), Some("9090"));
}

#[test]
fn merge_keys_precedence_chaining_spans_and_error_cases() {
    // Sequence operands honor first-wins precedence; explicit own keys win over all merged keys;
    // chained merges flatten completely; source spans point at the original source tokens.
    let input = "\
a: &a
  k1: from_a
  k2: from_a
  k3: from_a
b: &b
  <<: *a
  k2: from_b
  k4: from_b
c: &c
  k1: from_c
  k4: from_c
  k5: from_c
final:
  <<: [*b, *c]
  k3: explicit_final
";
    let merged = ayaml::parse_value_with(input, &MERGE).expect("chained merge");
    assert_eq!(merged["final"]["k1"].as_str(), Some("from_a"));
    assert_eq!(merged["final"]["k2"].as_str(), Some("from_b"));
    assert_eq!(merged["final"]["k3"].as_str(), Some("explicit_final"));
    assert_eq!(merged["final"]["k4"].as_str(), Some("from_b"));
    assert_eq!(merged["final"]["k5"].as_str(), Some("from_c"));

    // Event stream and SpannedTree preserve original source spans on merged entries.
    let events: Vec<Event<'_>> = ayaml::parse_events_with(input, &MERGE)
        .expect("merged events")
        .collect::<Result<_, _>>()
        .expect("collect events");
    assert!(events.iter().all(|event| !matches!(
        event,
        Event::Scalar { value, style: ayaml::ScalarStyle::Plain, .. } if value == "<<"
    )));

    let tree = SpannedTree::parse_with(input, &MERGE).expect("merged spanned tree");
    let k5_span = tree
        .span_at(&[PathSegment::key("final"), PathSegment::key("k5")])
        .expect("k5 span");
    assert_eq!(k5_span.slice(input), Some("from_c"));
    // Merged trees suppress entry_span conservatively so deleting a merged entry cannot mutate
    // its anchored source mapping.
    assert_eq!(
        tree.entry_span_at(&[PathSegment::key("final"), PathSegment::key("k5")]),
        None
    );

    // Typed Serde error on a merged value points back to the original source span!
    let err = ayaml::from_str_with::<ExpectInt>(input, &MERGE).expect_err("type error");
    assert_eq!(err.kind(), ErrorKind::Serde);
    let err_span = err.span().expect("error span");
    assert!(
        err_span.slice(input).is_some(),
        "merged error span must slice original input: {err}"
    );

    // Invalid merge operands return ErrorCode::InvalidMergeOperand.
    for bad in [
        "a:\n  <<: 123\n",
        "a:\n  <<: [123]\n",
        "base: &b\n  x: 1\na:\n  <<: [*b, not_a_map]\n",
    ] {
        let err = ayaml::parse_value_with(bad, &MERGE).expect_err(bad);
        assert_eq!(err.kind(), ErrorKind::Composition, "{bad}: {err}");
        assert_eq!(err.code(), ErrorCode::InvalidMergeOperand, "{bad}: {err}");
    }

    // Duplicate plain `<<:` in the same mapping is rejected.
    let dup_merge = "a: &a\n  x: 1\nb: &b\n  y: 2\nc:\n  <<: *a\n  <<: *b\n";
    let err = ayaml::parse_value_with(dup_merge, &MERGE).expect_err("duplicate merge");
    assert_eq!(err.kind(), ErrorKind::DuplicateKey);

    // Collection-entry limit is enforced when merging adds entries.
    let tight = ParseOptions::new()
        .with_limits(Limits::new().with_max_collection_entries(2))
        .with_merge_keys(true);
    let over_entries = "a: &a\n  k1: 1\n  k2: 2\nb:\n  <<: *a\n  k3: 3\n";
    let err = ayaml::from_str_with::<Value>(over_entries, &tight).expect_err("entry limit");
    assert_eq!(err.kind(), ErrorKind::Limit);
    assert_eq!(
        err.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::CollectionEntries)
    );

    // Explicit alias key overrides a matching merged key instead of causing a duplicate key error.
    let alias_key =
        "k: &k host\nbase: &b\n  host: default\n  port: 80\nout:\n  <<: *b\n  *k : override\n";
    let alias_val = ayaml::parse_value_with(alias_key, &MERGE).expect("alias key override");
    assert_eq!(alias_val["out"]["host"].as_str(), Some("override"));
    assert_eq!(alias_val["out"]["port"].as_u64(), Some(80));

    // Sequence-alias merge operand (`<<: *seq`) and inline merge operand preserving inner anchors.
    let seq_and_inline = "\
a: &a
  x: 1
  y: 2
b: &b
  y: 20
  z: 30
list: &list [*a, *b]
from_seq:
  <<: *list
inline_anchor:
  <<: { inner: &kept 42 }
ref_kept: *kept
";
    let si_val = ayaml::parse_value_with(seq_and_inline, &MERGE).expect("seq alias + inline");
    assert_eq!(si_val["from_seq"]["x"].as_u64(), Some(1));
    assert_eq!(si_val["from_seq"]["y"].as_u64(), Some(2));
    assert_eq!(si_val["from_seq"]["z"].as_u64(), Some(30));
    assert_eq!(si_val["inline_anchor"]["inner"].as_u64(), Some(42));
    assert_eq!(si_val["ref_kept"].as_u64(), Some(42));

    // Single-document readers reject trailing documents when merge_keys is enabled.
    let multi = "---\na: &a\n  x: 1\nb:\n  <<: *a\n---\nc: 2\n";
    assert_eq!(
        ayaml::parse_value_with(multi, &MERGE)
            .expect_err("trailing value")
            .code(),
        ErrorCode::TrailingDocuments
    );
    assert_eq!(
        ayaml::from_str_with::<Value>(multi, &MERGE)
            .expect_err("trailing typed")
            .code(),
        ErrorCode::TrailingDocuments
    );
    assert_eq!(
        DocumentDeserializer::from_str_with(multi, &MERGE)
            .expect_err("trailing doc_de")
            .code(),
        ErrorCode::TrailingDocuments
    );

    // Multi-document `parse_documents_with` and `Deserializer::from_str_with` share one alias
    // dereference budget between merge expansion and regular alias composition without
    // double-charging merged entries.
    let two_aliases = ParseOptions::new()
        .with_limits(Limits::new().with_max_alias_dereferences(2))
        .with_merge_keys(true);
    let one_alias = ParseOptions::new()
        .with_limits(Limits::new().with_max_alias_dereferences(1))
        .with_merge_keys(true);
    let two_alias_doc = "\
a: &a
  x: 1
  y: 2
  z: 3
b:
  <<: *a
c: *a
";
    // Exactly two alias dereferences (`<<: *a` and `c: *a`) fit within limit 2 regardless of
    // how many keys `&a` holds.
    assert!(ayaml::parse_documents_with(two_alias_doc, &two_aliases).is_ok());
    let mut cursor_ok = Deserializer::from_str_with(two_alias_doc, &two_aliases).expect("cursor");
    assert!(cursor_ok.next_borrowed::<Value>().expect("doc").is_some());

    // Limit 1 rejects the second alias dereference on both `parse_documents_with` and
    // `Deserializer::from_str_with`.
    let err =
        ayaml::parse_documents_with(two_alias_doc, &one_alias).expect_err("shared alias budget");
    assert_eq!(err.kind(), ErrorKind::Limit);
    assert_eq!(
        err.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::AliasDereferences)
    );
    let mut cursor_tight = Deserializer::from_str_with(two_alias_doc, &one_alias).expect("cursor");
    let cursor_err = cursor_tight
        .next_borrowed::<Value>()
        .expect_err("cursor shares alias budget with merge expansion");
    assert_eq!(cursor_err.kind(), ErrorKind::Limit);
    assert_eq!(
        cursor_err.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::AliasDereferences)
    );

    // Alias-free inline merges charge zero alias dereferences.
    let zero_aliases = ParseOptions::new()
        .with_limits(Limits::new().with_max_alias_dereferences(0))
        .with_merge_keys(true);
    let inline_only = "out:\n  <<:\n    k1: 1\n    k2: 2\n";
    let inline_val = ayaml::parse_value_with(inline_only, &zero_aliases).expect("inline merge");
    assert_eq!(inline_val["out"]["k1"].as_u64(), Some(1));
    assert_eq!(inline_val["out"]["k2"].as_u64(), Some(2));
}

#[test]
fn merge_keys_preserves_lexical_scoping_tags_inline_validation_and_stream_parity() {
    // 1. Anchor redefinition between source mapping and merge site keeps lexical value.
    let redef = "v1: &v 1\nbase: &base\n  a: *v\nv2: &v 2\nmerged:\n  <<: *base\n";
    let val = ayaml::parse_value_with(redef, &MERGE).expect("lexical redef");
    assert_eq!(val["merged"]["a"].as_u64(), Some(1));

    // 2. Redefining an outer anchor on the merge target does not create a spurious cycle.
    let non_cyclic = "first: &x 1\nbase: &base\n  val: *x\nsecond: &x\n  <<: *base\n";
    let val = ayaml::parse_value_with(non_cyclic, &MERGE).expect("non-cyclic anchor reuse");
    assert_eq!(val["second"]["val"].as_u64(), Some(1));

    // 3. Own key referencing an anchor defined inside a preceding inline merge operand works.
    let inline_before = "merged:\n  <<:\n    a: &inner 1\n  b: *inner\n";
    let val = ayaml::parse_value_with(inline_before, &MERGE).expect("inline anchor before own");
    assert_eq!(val["merged"]["a"].as_u64(), Some(1));
    assert_eq!(val["merged"]["b"].as_u64(), Some(1));

    // 4. Overridden inline merge entry defining an anchor used by a sibling keeps lexical value.
    let overridden_anchor = "merged:\n  <<:\n    a: &inner 1\n    b: *inner\n  a: 2\n";
    let val = ayaml::parse_value_with(overridden_anchor, &MERGE).expect("overridden inline anchor");
    assert_eq!(val["merged"]["a"].as_u64(), Some(2));
    assert_eq!(val["merged"]["b"].as_u64(), Some(1));

    // 5. Anchor on the inline merge operand itself stays usable by later aliases.
    let anchored_operand = "a:\n  <<: &m\n    x: 1\nb: *m\n";
    let val = ayaml::parse_value_with(anchored_operand, &MERGE).expect("anchored inline operand");
    assert_eq!(val["a"]["x"].as_u64(), Some(1));
    assert_eq!(val["b"]["x"].as_u64(), Some(1));

    // 6. Tagged complex keys stay distinct during merge deduplication.
    let tagged_keys =
        "base: &b\n  ? !tagA [1]\n  : from_base\nout:\n  <<: *b\n  ? !tagB [1]\n  : from_out\n";
    let val = ayaml::parse_value_with(tagged_keys, &MERGE).expect("tagged complex keys");
    assert_eq!(val["out"].as_mapping().expect("map").len(), 2);

    // 7. Duplicate keys and invalid overridden scalars/aliases inside inline operands fail cleanly.
    assert_eq!(
        ayaml::parse_value_with("out:\n  <<:\n    x: 1\n    x: 2\n", &MERGE)
            .expect_err("dup in inline operand")
            .code(),
        ErrorCode::DuplicateKey
    );
    assert_eq!(
        ayaml::parse_value_with("out:\n  x: 1\n  <<:\n    x: !!int bad\n", &MERGE)
            .expect_err("invalid int in overridden inline operand")
            .code(),
        ErrorCode::InvalidIntScalar
    );
    assert_eq!(
        ayaml::parse_value_with("out:\n  x: 1\n  <<:\n    x: *missing\n", &MERGE)
            .expect_err("dangling alias in overridden inline operand")
            .code(),
        ErrorCode::UnknownAnchor
    );

    // 8. Custom-tagged merge operands are rejected with InvalidMergeOperand.
    for bad in [
        "out:\n  <<: !custom { x: 1 }\n",
        "out:\n  <<: !custom [{ x: 1 }]\n",
    ] {
        assert_eq!(
            ayaml::parse_value_with(bad, &MERGE).expect_err(bad).code(),
            ErrorCode::InvalidMergeOperand
        );
    }

    // 9. MergedEventStream is fused after returning an error and preserves BOM StreamStart span.
    let mut stream = ayaml::parse_events_with("a:\n  <<: 1\n---\nb: 2\n", &MERGE).expect("stream");
    assert!(matches!(stream.next(), Some(Ok(Event::StreamStart { .. }))));
    assert!(matches!(stream.next(), Some(Err(_))));
    assert!(stream.next().is_none(), "stream must fuse after error");

    let mut bom_stream = ayaml::parse_events_with("\u{feff}a: 1\n", &MERGE).expect("bom stream");
    assert!(matches!(
        bom_stream.next(),
        Some(Ok(Event::StreamStart { span })) if span == ayaml::Span::empty(3)
    ));

    // 10. Single-document merge readers reject trailing documents before expanding doc 2.
    let trailing_bad = "a: 1\n---\nb:\n  <<: 123\n";
    assert_eq!(
        ayaml::parse_value_with(trailing_bad, &MERGE)
            .expect_err("trailing before doc2 merge")
            .code(),
        ErrorCode::TrailingDocuments
    );

    // 11. Alias-key depth does not falsely trigger DuplicateKey at tight max_depth.
    let alias_key_depth = "s: &s x\nk: &k [*s]\n? *k\n: from_out\n<<:\n  ? *k\n  : from_base\n";
    let depth2 = ParseOptions::new()
        .with_limits(Limits::new().with_max_depth(2))
        .with_merge_keys(true);
    let val = ayaml::parse_value_with(alias_key_depth, &depth2).expect("alias key at depth 2");
    assert_eq!(val.as_mapping().expect("map").len(), 3);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn merge_keys_property_matches_expected_precedence_and_preserves_spans(
        a1 in 0u32..100,
        a2 in 0u32..100,
        b2 in 0u32..100,
        b3 in 0u32..100,
        own3 in 0u32..100,
        own4 in 0u32..100,
    ) {
        let yaml = format!(
            "a: &a\n  k1: {a1}\n  k2: {a2}\nb: &b\n  k2: {b2}\n  k3: {b3}\nout:\n  <<: [*a, *b]\n  k3: {own3}\n  k4: {own4}\n"
        );
        let parsed: PropRoot = ayaml::from_str_with(&yaml, &MERGE).expect("merged typed");
        prop_assert_eq!(parsed.out.k1, a1);
        prop_assert_eq!(parsed.out.k2, a2); // *a beats *b (first-wins)
        prop_assert_eq!(parsed.out.k3, own3); // explicit key beats *b
        prop_assert_eq!(parsed.out.k4, own4);

        // SpannedTree with merge_keys enabled preserves exact source slices for every merged key.
        let tree = SpannedTree::parse_with(&yaml, &MERGE).expect("merged tree");
        let k1_span = tree
            .span_at(&[PathSegment::key("out"), PathSegment::key("k1")])
            .expect("k1 span");
        let a1_text = a1.to_string();
        prop_assert_eq!(k1_span.slice(&yaml), Some(a1_text.as_str()));
        let k3_span = tree
            .span_at(&[PathSegment::key("out"), PathSegment::key("k3")])
            .expect("k3 span");
        let own3_text = own3.to_string();
        prop_assert_eq!(k3_span.slice(&yaml), Some(own3_text.as_str()));
    }
}
