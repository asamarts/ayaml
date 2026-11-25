//! Unit and property tests for `Spanned<T>` typed deserialization (AY-145).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use ayaml::{Deserializer, PathSegment, Span, Spanned, SpannedTree};
use proptest::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
struct Nested<'a> {
    #[serde(borrow)]
    host: Spanned<&'a str>,
    ports: Spanned<Vec<Spanned<u16>>>,
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
struct Config<'a> {
    #[serde(borrow)]
    name: Spanned<&'a str>,
    #[serde(borrow)]
    nested: Spanned<Nested<'a>>,
    labels: Spanned<BTreeMap<String, Spanned<String>>>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
struct OwnedSummary {
    name: Spanned<String>,
}

/// Prefix `input` with > 4 KiB of comment lines so `from_str_borrowed` takes the streaming
/// event-driven reader path (`> SMALL_INPUT_BYTES`) rather than the small-tree path.
fn pad_for_stream_path(input: &str) -> (String, usize) {
    const COMMENT: &str = "# padding comment line to exceed the 4 KiB streaming threshold\n";
    let mut padded = COMMENT.repeat(80);
    let offset = padded.len();
    assert!(offset > 4096);
    padded.push_str(input);
    (padded, offset)
}

#[test]
fn spanned_matches_spanned_tree_on_small_tree_and_stream_paths() {
    let input = "\
name: demo
nested:
  host: localhost
  ports: [80, 443]
labels:
  env: prod
  tier: web
";
    let tree = SpannedTree::parse(input).expect("spanned tree");
    let small: Config<'_> = ayaml::from_str_borrowed(input).expect("small path");

    assert_eq!(*small.name, "demo");
    assert_eq!(small.name.span().slice(input), Some("demo"));
    assert_eq!(
        small.name.span(),
        tree.span_at(&[PathSegment::key("name")])
            .expect("name span")
    );
    assert_eq!(
        small.nested.span(),
        tree.span_at(&[PathSegment::key("nested")])
            .expect("nested span")
    );
    assert_eq!(
        small.nested.host.span(),
        tree.span_at(&[PathSegment::key("nested"), PathSegment::key("host")])
            .expect("host span")
    );
    assert_eq!(
        small.nested.ports.span(),
        tree.span_at(&[PathSegment::key("nested"), PathSegment::key("ports")])
            .expect("ports span")
    );
    assert_eq!(
        small.nested.ports[1].span(),
        tree.span_at(&[
            PathSegment::key("nested"),
            PathSegment::key("ports"),
            PathSegment::index(1),
        ])
        .expect("port 1 span")
    );
    assert_eq!(
        small.labels.span(),
        tree.span_at(&[PathSegment::key("labels")])
            .expect("labels span")
    );
    assert_eq!(
        small.labels["tier"].span(),
        tree.span_at(&[PathSegment::key("labels"), PathSegment::key("tier")])
            .expect("tier span")
    );

    // Event-stream path (> 4 KiB) produces identical values and shifted spans matching SpannedTree.
    let (padded, offset) = pad_for_stream_path(input);
    let padded_tree = SpannedTree::parse(&padded).expect("padded tree");
    let streamed: Config<'_> = ayaml::from_str_borrowed(&padded).expect("stream path");

    assert_eq!(*streamed.name, *small.name);
    assert_eq!(
        streamed.name.span(),
        Span::new(
            small.name.span().start() + offset,
            small.name.span().end() + offset
        )
    );
    assert_eq!(
        streamed.nested.span(),
        padded_tree
            .span_at(&[PathSegment::key("nested")])
            .expect("padded nested")
    );
    assert_eq!(
        streamed.nested.ports.span(),
        padded_tree
            .span_at(&[PathSegment::key("nested"), PathSegment::key("ports")])
            .expect("padded ports")
    );
    assert_eq!(
        streamed.labels["env"].span(),
        padded_tree
            .span_at(&[PathSegment::key("labels"), PathSegment::key("env")])
            .expect("padded env")
    );

    // Works through Deserializer cursor, DocumentDeserializer, and Serialize round-trip.
    let mut cursor = Deserializer::from_str(input).expect("cursor");
    let from_cursor: Config<'_> = cursor.next_borrowed().expect("next").expect("doc");
    assert_eq!(from_cursor, small);

    let doc_de = ayaml::DocumentDeserializer::from_str(input).expect("doc de");
    let from_doc_de: Config<'_> = Config::deserialize(doc_de).expect("doc de decode");
    assert_eq!(from_doc_de, small);

    // Owned cursor (`Deserializer::next`), frontmatter, empty document, and compat also preserve spans.
    let mut owned_cursor = Deserializer::from_str(input).expect("owned cursor");
    let owned_summary: OwnedSummary = owned_cursor.next().expect("next owned").expect("doc");
    assert_eq!(*owned_summary.name, "demo");
    assert_eq!(owned_summary.name.span(), small.name.span());

    let fm_doc = "---\nname: guide\n---\n# Body\n";
    let fm: ayaml::ParsedFrontmatter<'_, OwnedSummary> =
        ayaml::parse_frontmatter(fm_doc).expect("fm").expect("some");
    assert_eq!(fm.frontmatter.name.span().slice(fm_doc), Some("guide"));

    let empty_spanned: Spanned<Option<u32>> = ayaml::from_str("").expect("empty");
    assert_eq!(*empty_spanned, None);
    assert_eq!(empty_spanned.span(), Span::empty(0));

    #[cfg(feature = "libyaml-compat")]
    {
        let compat_summary: OwnedSummary = ayaml::compat::from_str(input).expect("compat");
        assert_eq!(compat_summary, owned_summary);
    }

    let emitted = ayaml::to_string(&small).expect("serialize Spanned");
    assert_eq!(
        ayaml::parse_value(&emitted).expect("reparse"),
        ayaml::parse_value(input).expect("orig")
    );
}

#[test]
fn spanned_handles_aliases_tags_options_and_rejects_non_source_deserializers() {
    #[derive(Debug, Deserialize)]
    struct OptionalAliasDoc {
        base: Option<Spanned<u32>>,
        copy: Option<Spanned<u32>>,
    }
    #[derive(Debug, Deserialize)]
    struct FlattenedInner {
        #[allow(dead_code, reason = "tests flatten forgery rejection")]
        val: Spanned<u32>,
    }
    #[derive(Debug, Deserialize)]
    struct FlattenedOuter {
        #[serde(flatten)]
        #[allow(dead_code, reason = "tests flatten forgery rejection")]
        inner: FlattenedInner,
    }
    #[derive(Debug, Deserialize)]
    #[serde(untagged)]
    enum UntaggedSpoof {
        #[allow(dead_code, reason = "tests untagged forgery rejection")]
        Spanned(Spanned<u32>),
    }

    #[derive(Debug, Deserialize)]
    struct Item {
        x: Spanned<u32>,
    }

    #[derive(Debug, Deserialize)]
    struct AliasedDoc {
        base: Spanned<Item>,
        copy: Spanned<Item>,
        tagged_str: Spanned<String>,
        custom_tag: Spanned<u16>,
        missing: Option<Spanned<u32>>,
        explicit_null: Spanned<Option<u32>>,
    }

    let input = "\
base: &b
  x: 10
copy: *b
tagged_str: !!str 123
custom_tag: !port 8080
explicit_null: null
";

    for (label, text) in [
        ("small", input.to_owned()),
        ("stream", pad_for_stream_path(input).0),
    ] {
        let local_tree = SpannedTree::parse(&text).expect("local tree");
        let doc: AliasedDoc = ayaml::from_str(&text).expect(label);

        // Aliased outer node reports the alias use-site span (`*b`), matching SpannedTree.
        assert_eq!(doc.copy.span().slice(&text), Some("*b"), "{label}");
        assert_eq!(
            doc.copy.span(),
            local_tree
                .span_at(&[PathSegment::key("copy")])
                .expect("copy span"),
            "{label}"
        );
        // Inner Spanned field inside the aliased struct reports the target field's span (`10`).
        assert_eq!(*doc.copy.x, 10);
        assert_eq!(doc.copy.x.span(), doc.base.x.span(), "{label}");
        assert_eq!(doc.copy.x.span().slice(&text), Some("10"), "{label}");

        // Tagged nodes report their node span matching SpannedTree.
        assert_eq!(*doc.tagged_str, "123");
        assert_eq!(
            doc.tagged_str.span(),
            local_tree
                .span_at(&[PathSegment::key("tagged_str")])
                .expect("tagged_str span"),
            "{label}"
        );
        assert_eq!(*doc.custom_tag, 8080);
        assert_eq!(
            doc.custom_tag.span(),
            local_tree
                .span_at(&[PathSegment::key("custom_tag")])
                .expect("custom_tag span"),
            "{label}"
        );

        assert!(doc.missing.is_none());
        assert_eq!(*doc.explicit_null, None);
        assert_eq!(doc.explicit_null.span().slice(&text), Some("null"));
    }

    // Non-source deserializers (serde_json and ayaml::from_value) refuse Spanned<T> cleanly
    // instead of fabricating a placeholder span.
    let json_err = serde_json::from_str::<Spanned<u32>>("42").expect_err("serde_json");
    assert!(
        json_err.to_string().contains("spanned YAML value"),
        "{json_err}"
    );
    let json_map_err = serde_json::from_str::<Spanned<u32>>(r#"{"x": 42}"#).expect_err("json map");
    assert!(
        json_map_err
            .to_string()
            .contains("ayaml source-backed deserializer"),
        "{json_map_err}"
    );
    let value = ayaml::parse_value("a: 42\n").expect("value");
    let value_err = ayaml::from_value::<BTreeMap<String, Spanned<u32>>>(value)
        .expect_err("from_value rejects Spanned");
    assert!(
        value_err.to_string().contains("Spanned") || value_err.to_string().contains("spanned"),
        "{value_err}"
    );

    // Option<Spanned<T>> preserves the alias use-site span (`*b`), matching Spanned<T>.
    let opt_input = "base: &b 42\ncopy: *b\n";
    let opt_doc: OptionalAliasDoc = ayaml::from_str(opt_input).expect("opt alias");
    assert_eq!(opt_doc.base.unwrap().span().slice(opt_input), Some("42"));
    assert_eq!(opt_doc.copy.unwrap().span().slice(opt_input), Some("*b"));

    // Spoofed sentinel maps cannot forge arbitrary out-of-bounds Spans through from_value,
    // serde_json, #[serde(flatten)], or #[serde(untagged)].
    let spoof_yaml = "$__ayaml_private_start: 1000000\n$__ayaml_private_end: 2000000\n$__ayaml_private_value: 42\n";
    let spoof_val = ayaml::parse_value(spoof_yaml).expect("parse spoof");
    assert!(ayaml::from_value::<Spanned<u32>>(spoof_val).is_err());

    let spoof_json = r#"{"$__ayaml_private_start": 1000000, "$__ayaml_private_end": 2000000, "$__ayaml_private_value": 42}"#;
    assert!(serde_json::from_str::<Spanned<u32>>(spoof_json).is_err());

    let flatten_spoof = "val:\n  $__ayaml_private_start: 1000000\n  $__ayaml_private_end: 2000000\n  $__ayaml_private_value: 42\n";
    assert!(ayaml::from_str::<FlattenedOuter>(flatten_spoof).is_err());
    assert!(ayaml::from_str::<UntaggedSpoof>(spoof_yaml).is_err());
}

fn proptest_config() -> ProptestConfig {
    let default_cases = if cfg!(miri) { 2 } else { 128 };
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default_cases);
    ProptestConfig {
        cases,
        ..ProptestConfig::default()
    }
}

proptest! {
    #![proptest_config(proptest_config())]

    #[test]
    fn spanned_spans_are_valid_utf8_and_match_spanned_tree(
        stream_path in any::<bool>(),
        title in "[a-zA-Z][a-zA-Z0-9_.-]{0,15}",
        items in proptest::collection::vec(0u32..1000, 1..6),
        flow_items in any::<bool>(),
        flag in any::<bool>(),
    ) {
        #[derive(Debug, Deserialize)]
        struct Doc<'a> {
            #[serde(borrow)]
            title: Spanned<&'a str>,
            items: Spanned<Vec<Spanned<u32>>>,
            flag: Spanned<bool>,
        }

        let mut yaml = String::new();
        if stream_path {
            yaml.push_str(&"# padding line to cross the 4 KiB event-stream threshold\n".repeat(80));
        }
        yaml.push_str("title: ");
        yaml.push_str(&title);
        yaml.push('\n');
        if flow_items {
            yaml.push_str("items: [");
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    yaml.push_str(", ");
                }
                yaml.push_str(&item.to_string());
            }
            yaml.push_str("]\n");
        } else {
            yaml.push_str("items:\n");
            for item in &items {
                let _ = writeln!(yaml, "  - {item}");
            }
        }
        let _ = writeln!(yaml, "flag: {flag}");

        let tree = SpannedTree::parse(&yaml).expect("tree");
        let root: Spanned<Doc<'_>> = ayaml::from_str_borrowed(&yaml).expect("spanned root");

        // Every captured span is a valid UTF-8 slice boundary in `yaml`.
        prop_assert!(root.span().slice(&yaml).is_some());
        prop_assert!(root.title.span().slice(&yaml).is_some());
        prop_assert!(root.items.span().slice(&yaml).is_some());
        prop_assert!(root.flag.span().slice(&yaml).is_some());

        // Every captured span matches SpannedTree::span_at exactly.
        prop_assert_eq!(root.span(), tree.span_at(&[]).expect("root span"));
        prop_assert_eq!(
            root.title.span(),
            tree.span_at(&[PathSegment::key("title")]).expect("title span")
        );
        prop_assert_eq!(
            root.items.span(),
            tree.span_at(&[PathSegment::key("items")]).expect("items span")
        );
        prop_assert_eq!(
            root.flag.span(),
            tree.span_at(&[PathSegment::key("flag")]).expect("flag span")
        );
        for (idx, elem) in root.items.iter().enumerate() {
            prop_assert!(elem.span().slice(&yaml).is_some());
            prop_assert_eq!(
                elem.span(),
                tree.span_at(&[PathSegment::key("items"), PathSegment::index(idx)])
                    .expect("elem span")
            );
            prop_assert_eq!(**elem, items[idx]);
        }
        prop_assert_eq!(*root.title, title.as_str());
        prop_assert_eq!(*root.flag, flag);
    }
}
