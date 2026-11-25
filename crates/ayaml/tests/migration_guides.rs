//! End-to-end verification tests for all three library migration guides
//! (`docs/migrating-from/serde-yaml.md`, `serde-yml-and-forks.md`, and
//! `yaml-rust2-and-saphyr.md`) and `docs/migration.md`.

use std::collections::HashMap;
use std::io::Cursor;

use ayaml::{
    ColumnEncoding, Deserializer, DocumentDeserializer, EmitOptions, ErrorCode, ErrorKind, Event,
    LimitExceeded, LimitKind, Limits, LineIndex, Location, ParseOptions, PathSegment, Spanned,
    SpannedTree, Value, from_reader_with, from_str, from_str_borrowed, from_str_with, from_value,
    parse_documents, parse_events, parse_value, parse_value_with, to_string, to_string_value_with,
    to_value,
};
use serde::de::{self, Deserialize, MapAccess, Visitor};
use serde::{Deserialize as DeriveDeserialize, Serialize};

#[derive(Debug, PartialEq, DeriveDeserialize, Serialize)]
enum SecretRef {
    Env(String),
    Literal(String),
}

#[derive(Debug, PartialEq, DeriveDeserialize, Serialize)]
struct ServiceConfig {
    name: String,
    country: String,
    port: u16,
    token: SecretRef,
}

#[derive(Debug, DeriveDeserialize)]
struct RootConfig {
    service: ServiceConfig,
}

#[derive(Debug, DeriveDeserialize)]
struct Route<'a> {
    #[serde(borrow)]
    prefix: Spanned<&'a str>,
    #[serde(borrow)]
    upstream: &'a str,
    timeout_ms: u32,
}

struct UnknownCollector {
    known_name: String,
    unknown_keys: Vec<String>,
}

impl<'de> Deserialize<'de> for UnknownCollector {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = UnknownCollector;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("mapping")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut known_name = String::new();
                let mut unknown_keys = Vec::new();
                while let Some(k) = map.next_key::<String>()? {
                    if k == "name" {
                        known_name = map.next_value()?;
                    } else {
                        let _: de::IgnoredAny = map.next_value()?;
                        unknown_keys.push(k);
                    }
                }
                Ok(UnknownCollector {
                    known_name,
                    unknown_keys,
                })
            }
        }
        deserializer.deserialize_map(V)
    }
}

#[test]
fn serde_yaml_migration_semantics_and_roundtrip() {
    // YAML 1.2.2 keeps `yes`/`no`/`on`/`off`/`0644`/dates as strings, unlike YAML 1.1.
    let val = parse_value(
        "\
yes_word: yes
no_word: no
on_word: on
off_word: off
country: NO
mode: 0644
octal: 0o644
underscored: 1_000
sexagesimal: 1:20
date: 2026-05-01
",
    )
    .expect("valid YAML 1.2.2");
    assert_eq!(val["yes_word"].as_str(), Some("yes"));
    assert_eq!(val["no_word"].as_str(), Some("no"));
    assert_eq!(val["on_word"].as_str(), Some("on"));
    assert_eq!(val["off_word"].as_str(), Some("off"));
    assert_eq!(val["country"].as_str(), Some("NO"));
    assert_eq!(val["mode"].as_str(), Some("0644"));
    assert_eq!(val["octal"].as_u64(), Some(0o644));
    assert_eq!(val["underscored"].as_str(), Some("1_000"));
    assert_eq!(val["sexagesimal"].as_str(), Some("1:20"));
    assert_eq!(val["date"].as_str(), Some("2026-05-01"));

    // Duplicate mapping keys are rejected deterministically in default mode.
    let dup = parse_value("port: 80\nport: 443\n").unwrap_err();
    assert_eq!(dup.kind(), ErrorKind::DuplicateKey);
    assert_eq!(dup.code(), ErrorCode::DuplicateKey);

    // Opt-in merge keys expand `<<:` while keeping quoted `"<<"` literal.
    let merged_doc = "\
defaults: &def
  country: NO
  port: 8080
service:
  <<: *def
  \"<<\": kept
  name: billing
  token: !Env BILLING_KEY
";
    let merge_opts = ParseOptions::new().with_merge_keys(true);
    let root = parse_value_with(merged_doc, &merge_opts).expect("merge succeeds");
    assert_eq!(root["service"]["<<"].as_str(), Some("kept"));

    let decoded: RootConfig = from_str_with(merged_doc, &merge_opts).expect("typed merge succeeds");
    assert_eq!(
        decoded.service,
        ServiceConfig {
            name: "billing".into(),
            country: "NO".into(),
            port: 8080,
            token: SecretRef::Env("BILLING_KEY".into()),
        }
    );

    // Value <-> Serde conversion and deterministic emit round-trip.
    let as_val = to_value(&decoded.service).expect("to_value");
    let back: ServiceConfig = from_value(as_val).expect("from_value");
    assert_eq!(back, decoded.service);

    let emitted = to_string(&decoded.service).expect("to_string");
    assert_eq!(
        from_str::<ServiceConfig>(&emitted).expect("round-trip"),
        decoded.service
    );

    // Multi-document Deserializer pull loop + single-doc TrailingDocuments check.
    let multi = "1\n---\n2\n---\n3\n";
    assert_eq!(
        from_str::<u32>(multi).unwrap_err().code(),
        ErrorCode::TrailingDocuments
    );
    let mut de = Deserializer::from_str(multi).expect("stream");
    let mut nums = Vec::new();
    while let Some(n) = de.next::<u32>().expect("doc") {
        nums.push(n);
    }
    assert_eq!(nums, vec![1, 2, 3]);
}

#[test]
fn serde_yml_and_forks_zero_copy_spanned_middleware_and_bounded_reader() {
    let input = "prefix: /v1/orders\nupstream: orders.internal\ntimeout_ms: 2500\n";
    let route: Route<'_> = from_str_borrowed(input).expect("borrowed decode");
    assert_eq!(*route.prefix, "/v1/orders");
    assert_eq!(route.prefix.span().slice(input), Some("/v1/orders"));
    assert_eq!(route.upstream, "orders.internal");
    assert_eq!(route.timeout_ms, 2500);

    let doc_de =
        DocumentDeserializer::from_str("name: gateway\nlegacy: 1\nunused: 2\n").expect("doc_de");
    let collected = UnknownCollector::deserialize(doc_de).expect("middleware decode");
    assert_eq!(collected.known_name, "gateway");
    assert_eq!(collected.unknown_keys, vec!["legacy", "unused"]);

    // Bounded reader refuses oversized stream before UTF-8 decode.
    let opts = ParseOptions::new().with_limits(Limits::new().with_max_input_bytes(64));
    let oversized = vec![0xFF; 128];
    let err = from_reader_with::<_, Value>(Cursor::new(oversized), &opts).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Limit);
    assert_eq!(
        err.limit().map(LimitExceeded::kind),
        Some(LimitKind::InputBytes)
    );

    // Masked errors never leak raw secret scalar values.
    let secret = "canary-secret-value-998877";
    let err = from_str::<HashMap<String, u32>>(&format!("token: {secret}\n")).unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidType);
    let masked = err.masked().to_string();
    assert!(!masked.contains(secret));
    assert!(masked.contains("yaml.invalid-type"));
}

#[test]
fn yaml_rust2_and_saphyr_documents_pull_events_spans_and_emit() {
    // Multi-document parse_documents replaces YamlLoader::load_from_str.
    let docs = parse_documents("kind: ConfigMap\n---\nkind: Secret\n").expect("docs");
    assert_eq!(docs.len(), 2);
    assert_eq!(docs[0]["kind"].as_str(), Some("ConfigMap"));
    assert_eq!(docs[1]["kind"].as_str(), Some("Secret"));

    // Pull parse_events exposes duplicate keys and spans without callback traits.
    let raw = "dup: 1\ndup: 2\n";
    let scalars: Vec<(String, &str)> = parse_events(raw)
        .expect("events")
        .filter_map(|res| match res.expect("event") {
            Event::Scalar { value, span, .. } => {
                Some((value.into_owned(), span.slice(raw).unwrap()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        scalars,
        vec![
            ("dup".into(), "dup"),
            ("1".into(), "1"),
            ("dup".into(), "dup"),
            ("2".into(), "2"),
        ]
    );

    // SpannedTree + LineIndex replace MarkedYaml across UTF-8, scalar, and UTF-16 columns.
    let src = "db:\n  pool: 16\n";
    let tree = SpannedTree::parse(src).expect("tree");
    let span = tree
        .span_at(&[PathSegment::key("db"), PathSegment::key("pool")])
        .expect("span");
    assert_eq!(span.slice(src), Some("16"));
    let idx = LineIndex::new(src);
    let loc = Location::from(
        idx.to_position(span.start(), ColumnEncoding::Utf16)
            .expect("pos"),
    );
    assert_eq!((loc.line(), loc.column()), (2, 9));

    // Flow emission + YAML 1.1-safe leading-zero quoting.
    let v = parse_value("mode: 0644\ncount: 2\n").expect("value");
    let block = to_string_value_with(&v, &EmitOptions::new()).expect("emit block");
    assert!(
        block.contains("\"0644\"") || block.contains("'0644'"),
        "leading-zero decimal strings are quoted for YAML 1.1 interoperability: {block:?}"
    );
    let flow = to_string_value_with(&v, &EmitOptions::flow()).expect("emit flow");
    assert!(flow.starts_with('{') && flow.ends_with("}\n"));
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_boundary_handles_legacy_duplicates_and_global_tags() {
    use ayaml::compat;

    let legacy = "port: 80\nport: 443\ncreated: !!timestamp 2026-05-01\n";
    let opts = compat::ParseOptions::new().with_global_tags(compat::GlobalTags::Drop);
    let map: HashMap<String, Value> = compat::from_str_with(legacy, &opts).expect("compat decode");
    assert_eq!(map["port"].as_u64(), Some(443));
    assert_eq!(map["created"].as_str(), Some("2026-05-01"));
}
