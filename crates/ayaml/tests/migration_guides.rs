//! End-to-end verification tests for all three library migration guides
//! (`docs/migrating-from/serde-yaml.md`, `serde-yml-and-forks.md`, and
//! `yaml-rust2-and-saphyr.md`) and `docs/migration.md`.

use std::collections::HashMap;
use std::io::Cursor;

use ayaml::{
    ColumnEncoding, Deserializer, DocumentDeserializer, EmitOptions, ErrorCode, ErrorKind, Event,
    LimitExceeded, LimitKind, Limits, LineIndex, Location, Number, ParseOptions, PathSegment,
    Spanned, SpannedTree, Value, from_reader_with, from_slice, from_slice_borrowed,
    from_slice_with, from_str, from_str_borrowed, from_str_with, from_value, parse_documents,
    parse_events, parse_frontmatter, parse_frontmatter_with, parse_value, parse_value_with,
    split_frontmatter, to_string, to_string_value_with, to_value, to_writer, to_writer_with,
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

#[derive(Debug, Default, PartialEq, DeriveDeserialize)]
#[serde(default)]
struct DefaultedConfig {
    retries: u8,
}

#[derive(Debug, DeriveDeserialize)]
struct Route<'a> {
    #[serde(borrow)]
    prefix: Spanned<&'a str>,
    #[serde(borrow)]
    upstream: &'a str,
    timeout_ms: u32,
}

#[derive(Debug, DeriveDeserialize)]
struct BorrowedDoc<'a> {
    #[serde(borrow)]
    name: &'a str,
}

#[derive(Debug, DeriveDeserialize)]
struct FlattenedInner {
    #[allow(dead_code, reason = "only used to verify deserialization failure")]
    port: Spanned<u16>,
}

#[derive(Debug, DeriveDeserialize)]
struct FlattenedOuter {
    #[allow(dead_code, reason = "only used to verify deserialization failure")]
    #[serde(flatten)]
    inner: FlattenedInner,
}

#[derive(Debug, DeriveDeserialize)]
#[serde(untagged)]
enum UntaggedSpanned {
    #[allow(dead_code, reason = "only used to verify deserialization failure")]
    Port(Spanned<u16>),
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
    // YAML 1.2.2 keeps `yes`/`no`/`on`/`off`/`0644`/dates/bare `NaN`/`Infinity` as strings,
    // while accepting 0o/0x/0b integers, 128-bit integers, and `.nan`/`.inf`/`-.inf`.
    let val = parse_value(
        "\
yes_word: yes
no_word: no
on_word: on
off_word: off
country: NO
mode: 0644
octal: 0o644
hex: 0x1f
binary: 0b11
big_i128: 170141183460469231731687303715884105727
big_u128: 340282366920938463463374607431768211455
underscored: 1_000
sexagesimal: 1:20
date: 2026-05-01
yaml_nan: .nan
yaml_inf: .inf
yaml_ninf: -.inf
bare_nan: NaN
bare_inf: Infinity
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
    assert_eq!(val["hex"].as_u64(), Some(0x1f));
    assert_eq!(val["binary"].as_u64(), Some(0b11));
    assert_eq!(
        val["big_i128"].as_number().and_then(Number::as_i128),
        Some(i128::MAX)
    );
    assert_eq!(
        val["big_u128"].as_number().and_then(Number::as_u128),
        Some(u128::MAX)
    );
    assert_eq!(val["underscored"].as_str(), Some("1_000"));
    assert_eq!(val["sexagesimal"].as_str(), Some("1:20"));
    assert_eq!(val["date"].as_str(), Some("2026-05-01"));
    assert!(val["yaml_nan"].as_f64().is_some_and(f64::is_nan));
    assert_eq!(val["yaml_inf"].as_f64(), Some(f64::INFINITY));
    assert_eq!(val["yaml_ninf"].as_f64(), Some(f64::NEG_INFINITY));
    assert_eq!(val["bare_nan"].as_str(), Some("NaN"));
    assert_eq!(val["bare_inf"].as_str(), Some("Infinity"));

    // Emitter formats non-finite floats using YAML 1.2.2 `.nan`/`.inf`/`-.inf`.
    assert_eq!(to_string(&f64::NAN).expect("emit nan").trim(), ".nan");
    assert_eq!(to_string(&f64::INFINITY).expect("emit inf").trim(), ".inf");
    assert_eq!(
        to_string(&f64::NEG_INFINITY).expect("emit -inf").trim(),
        "-.inf"
    );

    // Duplicate mapping keys are rejected deterministically in default mode.
    let dup = parse_value("port: 80\nport: 443\n").unwrap_err();
    assert_eq!(dup.kind(), ErrorKind::DuplicateKey);
    assert_eq!(dup.code(), ErrorCode::DuplicateKey);
    assert!(dup.location().is_some());
    assert!(dup.span().is_some());

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

    // Post-parse Value::apply_merge_keys / apply_merge_keys_with_limits expand `<<` on composed
    // Values (where plain and quoted `"<<"` are indistinguishable).
    let mut post_merge = parse_value(
        "\
defaults: &def
  port: 8080
service:
  <<: *def
  name: billing
",
    )
    .expect("raw value");
    post_merge.apply_merge_keys().expect("apply_merge_keys");
    assert_eq!(post_merge["service"]["port"].as_u64(), Some(8080));
    assert!(post_merge["service"].get("<<").is_none());

    let mut quoted_post = parse_value(
        "\
defaults: &def
  port: 8080
service:
  \"<<\": *def
",
    )
    .expect("quoted merge value");
    quoted_post
        .apply_merge_keys_with_limits(&Limits::new())
        .expect("apply_merge_keys_with_limits");
    assert_eq!(quoted_post["service"]["port"].as_u64(), Some(8080));

    // Single-entry mapping enum syntax (`Env: API_TOKEN`) deserializes alongside `!Env`.
    let mapped_enum: SecretRef = from_str("Env: API_TOKEN\n").expect("single-entry map enum");
    assert_eq!(mapped_enum, SecretRef::Env("API_TOKEN".into()));

    // Empty documents and `from_value` rules.
    assert_eq!(parse_value("").expect("empty value"), Value::Null);
    assert_eq!(
        from_str::<DefaultedConfig>("").expect("empty fills #[serde(default)]"),
        DefaultedConfig { retries: 0 }
    );
    assert!(from_value::<DefaultedConfig>(Value::Null).is_err());
    assert!(from_value::<String>(Value::from(42u64)).is_err());

    // Value::Tagged look-through accessors, structured Tag, and JSON serialization.
    let tagged = parse_value("!Custom\nport: 8080\n").expect("tagged value");
    assert_eq!(tagged["port"].as_u64(), Some(8080));
    if let Value::Tagged(tv) = &tagged {
        assert_eq!(tv.tag.handle(), "!");
        assert_eq!(tv.tag.suffix(), "Custom");
    } else {
        panic!("expected Value::Tagged");
    }
    let json = serde_json::to_value(&tagged).expect("serialize tagged to JSON");
    assert_eq!(json, serde_json::json!({"!Custom": {"port": 8080}}));

    // Value <-> Serde conversion, to_string, to_writer, and to_writer_with round-trip.
    let as_val = to_value(&decoded.service).expect("to_value");
    let back: ServiceConfig = from_value(as_val).expect("from_value");
    assert_eq!(back, decoded.service);

    let emitted = to_string(&decoded.service).expect("to_string");
    assert_eq!(
        from_str::<ServiceConfig>(&emitted).expect("round-trip"),
        decoded.service
    );
    let mut writer_buf = Vec::new();
    to_writer(&mut writer_buf, &decoded.service).expect("to_writer");
    assert_eq!(
        from_slice::<ServiceConfig>(&writer_buf).expect("from_slice"),
        decoded.service
    );
    let mut flow_buf = Vec::new();
    to_writer_with(&mut flow_buf, &decoded.service, &EmitOptions::flow()).expect("to_writer_with");
    assert_eq!(
        from_slice::<ServiceConfig>(&flow_buf).expect("from_slice flow"),
        decoded.service
    );

    // Multi-document Deserializer pull loop (owned & borrowed) + TrailingDocuments check.
    let multi = "name: one\n---\nname: two\n";
    assert_eq!(
        from_str_borrowed::<BorrowedDoc<'_>>(multi)
            .unwrap_err()
            .code(),
        ErrorCode::TrailingDocuments
    );
    let mut de = Deserializer::from_str(multi).expect("stream");
    let mut names = Vec::new();
    while let Some(doc) = de.next_borrowed::<BorrowedDoc<'_>>().expect("borrowed doc") {
        names.push(doc.name);
    }
    assert_eq!(names, vec!["one", "two"]);
}

#[test]
fn serde_yml_and_forks_zero_copy_spanned_middleware_and_bounded_reader() {
    let input = "prefix: /v1/orders\nupstream: orders.internal\ntimeout_ms: 2500\n";
    let route: Route<'_> = from_str_borrowed(input).expect("borrowed decode");
    assert_eq!(*route.prefix, "/v1/orders");
    assert_eq!(route.prefix.span().slice(input), Some("/v1/orders"));
    assert_eq!(route.upstream, "orders.internal");
    assert_eq!(route.timeout_ms, 2500);

    // Zero-copy from_slice_borrowed and from_slice_with.
    let route_bytes: Route<'_> = from_slice_borrowed(input.as_bytes()).expect("slice borrowed");
    assert_eq!(route_bytes.upstream, "orders.internal");
    let slice_opts = ParseOptions::new().with_limits(Limits::new().with_max_input_bytes(16));
    let oversized_bad_utf8 = vec![0xFF; 64];
    let slice_err = from_slice_with::<Value>(&oversized_bad_utf8, &slice_opts).unwrap_err();
    assert_eq!(slice_err.kind(), ErrorKind::Limit);
    assert_eq!(
        slice_err.limit().map(LimitExceeded::kind),
        Some(LimitKind::InputBytes)
    );

    // Spanned<T> refuses from_value, #[serde(flatten)], and #[serde(untagged)] cleanly.
    assert!(from_value::<Spanned<u16>>(Value::from(80u64)).is_err());
    assert!(from_str::<FlattenedOuter>("port: 80\n").is_err());
    assert!(from_str::<UntaggedSpanned>("80\n").is_err());

    // DocumentDeserializer works by value and by `&mut` for Serde middleware.
    let doc_de =
        DocumentDeserializer::from_str("name: gateway\nlegacy: 1\nunused: 2\n").expect("doc_de");
    let collected = UnknownCollector::deserialize(doc_de).expect("middleware by value");
    assert_eq!(collected.known_name, "gateway");
    assert_eq!(collected.unknown_keys, vec!["legacy", "unused"]);

    let mut mut_de =
        DocumentDeserializer::from_str("name: proxy\nextra: true\n").expect("mut doc_de");
    let collected_mut = UnknownCollector::deserialize(&mut mut_de).expect("middleware by &mut");
    assert_eq!(collected_mut.known_name, "proxy");
    assert_eq!(collected_mut.unknown_keys, vec!["extra"]);

    // Bounded reader refuses oversized stream before UTF-8 decode.
    let opts = ParseOptions::new().with_limits(Limits::new().with_max_input_bytes(64));
    let oversized = vec![0xFF; 128];
    let err = from_reader_with::<_, Value>(Cursor::new(oversized), &opts).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Limit);
    assert_eq!(
        err.limit().map(LimitExceeded::kind),
        Some(LimitKind::InputBytes)
    );

    // Alias bomb is rejected deterministically with LimitKind::AliasExpansion.
    let bomb = "\
a: &a [x, x, x, x, x, x, x, x, x]
b: &b [*a, *a, *a, *a, *a, *a, *a, *a, *a]
c: &c [*b, *b, *b, *b, *b, *b, *b, *b, *b]
d: &d [*c, *c, *c, *c, *c, *c, *c, *c, *c]
e: &e [*d, *d, *d, *d, *d, *d, *d, *d, *d]
f: &f [*e, *e, *e, *e, *e, *e, *e, *e, *e]
g: &g [*f, *f, *f, *f, *f, *f, *f, *f, *f]
h: &h [*g, *g, *g, *g, *g, *g, *g, *g, *g]
i: [*h, *h, *h, *h, *h, *h, *h, *h, *h]
";
    let bomb_err = parse_value(bomb).unwrap_err();
    assert_eq!(bomb_err.kind(), ErrorKind::Limit);
    assert_eq!(
        bomb_err.limit().map(LimitExceeded::kind),
        Some(LimitKind::AliasExpansion)
    );

    // Masked errors never leak raw secret scalar values, while exposing span & location.
    let secret = "canary-secret-value-998877";
    let bad_input = format!("token: {secret}\n");
    let err = from_str::<HashMap<String, u32>>(&bad_input).unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidType);
    assert_eq!(
        err.location().map(|loc| (loc.line(), loc.column())),
        Some((1, 8))
    );
    assert_eq!(err.span().and_then(|s| s.slice(&bad_input)), Some(secret));
    let masked = err.masked().to_string();
    assert!(!masked.contains(secret));
    assert!(masked.contains("yaml.invalid-type"));

    // Frontmatter helpers split and parse YAML headers from Markdown bodies.
    let md = "---\nname: guide\n---\n# Heading\n";
    let parts = split_frontmatter(md)
        .expect("split_frontmatter")
        .expect("has frontmatter");
    assert_eq!(parts.yaml(), "---\nname: guide\n");
    assert_eq!(parts.body(), "# Heading\n");
    let parsed = parse_frontmatter::<HashMap<String, String>>(md)
        .expect("parse_frontmatter")
        .expect("present");
    assert_eq!(
        parsed.frontmatter.get("name").map(String::as_str),
        Some("guide")
    );
    assert_eq!(parsed.body, "# Heading\n");
    let fm_opts = ParseOptions::new().with_limits(Limits::new().with_max_depth(8));
    let parsed_with = parse_frontmatter_with::<Value>(md, &fm_opts)
        .expect("parse_frontmatter_with")
        .expect("present");
    assert_eq!(parsed_with.frontmatter["name"].as_str(), Some("guide"));
}

#[test]
fn yaml_rust2_and_saphyr_documents_pull_events_spans_and_emit() {
    // Multi-document parse_documents replaces YamlLoader::load_from_str;
    // missing-key indexing returns Null while `.get()` distinguishes missing from explicit null.
    let docs = parse_documents("kind: ConfigMap\nempty: null\n---\nkind: Secret\n").expect("docs");
    assert_eq!(docs.len(), 2);
    assert_eq!(docs[0]["kind"].as_str(), Some("ConfigMap"));
    assert_eq!(docs[0].get("missing"), None);
    assert!(docs[0]["missing"].is_null());
    assert_eq!(docs[0].get("empty"), Some(&Value::Null));
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

    // SpannedTree::span_at + entry_span_at + LineIndex replace MarkedYaml across column encodings.
    let src = "db:\n  host: localhost\n  pool: 16\n";
    let tree = SpannedTree::parse(src).expect("tree");
    let path = [PathSegment::key("db"), PathSegment::key("pool")];
    let span = tree.span_at(&path).expect("span");
    assert_eq!(span.slice(src), Some("16"));
    let entry_span = tree.entry_span_at(&path).expect("entry_span");
    assert_eq!(entry_span.slice(src), Some("  pool: 16\n"));
    let idx = LineIndex::new(src);
    let loc = Location::from(
        idx.to_position(span.start(), ColumnEncoding::Utf16)
            .expect("pos"),
    );
    assert_eq!((loc.line(), loc.column()), (3, 9));

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

    // Default GlobalTags::Keep preserves non-core global tags as Value::Tagged.
    let keep_opts = compat::ParseOptions::new().with_global_tags(compat::GlobalTags::Keep);
    let kept: HashMap<String, Value> =
        compat::from_str_with(legacy, &keep_opts).expect("compat keep decode");
    assert_eq!(kept["port"].as_u64(), Some(443));
    assert!(matches!(kept["created"], Value::Tagged(_)));

    // Opt-in GlobalTags::Drop strips non-core global tags.
    let drop_opts = compat::ParseOptions::new().with_global_tags(compat::GlobalTags::Drop);
    let dropped: HashMap<String, Value> =
        compat::from_str_with(legacy, &drop_opts).expect("compat drop decode");
    assert_eq!(dropped["port"].as_u64(), Some(443));
    assert_eq!(dropped["created"], Value::String("2026-05-01".into()));

    // DocumentDeserializer::from_str_compat_with bridges Serde middleware with compat rules.
    let compat_de = DocumentDeserializer::from_str_compat_with(
        "name: legacy\nname: legacy\nunknown: 1\n",
        &drop_opts,
    )
    .expect("compat doc_de");
    let collected = UnknownCollector::deserialize(compat_de).expect("compat middleware");
    assert_eq!(collected.known_name, "legacy");
    assert_eq!(collected.unknown_keys, vec!["unknown"]);
}
