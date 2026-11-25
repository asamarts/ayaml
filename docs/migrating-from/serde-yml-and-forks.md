# Migrating from `serde_yml`, `serde_yaml_ng`, and `yaml-serde`

`serde_yml`, `serde_yaml_ng`, and `yaml-serde` are maintained forks of
`serde_yaml` backed by `unsafe` C `libyaml` (or C-transpiled `unsafe-libyaml`)
code. Moving to `ayaml` removes `unsafe` FFI from your dependency graph, cuts
heap allocations by ~3.8x and peak heap by ~1.67x vs `yaml-serde`, doubles
parsing throughput, and adds zero-copy borrowed decoding, `Spanned<T>`, and
`DocumentDeserializer` for Serde middleware.

## Quick API mapping

| `serde_yml` / `serde_yaml_ng` / `yaml-serde` | `ayaml` | Notes |
| --- | --- | --- |
| `from_str`, `from_slice`, `from_reader` | `ayaml::from_str`, `from_slice`, `from_reader` | Add `_with(..., &ParseOptions)` for custom `Limits` or merge keys. |
| `from_str` into `&'a str` | `ayaml::from_str_borrowed` / `from_slice_borrowed` | True zero-copy `&str` borrowing for unescaped scalars. |
| `to_string`, `to_writer` | `ayaml::to_string`, `to_writer` | Add `_with(..., &EmitOptions)` for indentation, flow layout, or output byte caps. |
| `Deserializer::from_str` passed to `serde_path_to_error` / `serde_ignored` | `ayaml::DocumentDeserializer::from_str` | Implements `serde::Deserializer<'de>` directly for a single document while keeping `ayaml` spans and limits. |
| `Value`, `Mapping`, `Number` | `ayaml::Value`, `Mapping`, `Number` | Insertion-ordered mapping with deterministic `PartialEq` and exact `i64`/`u64`/`f64` numbers. |
| `Value::apply_merge` | `ParseOptions::new().with_merge_keys(true)` | Expands plain `<<:` merges during streaming/typed decode; `Value::apply_merge_keys` is also available. |
| `Error::location()` | `Error::span()`, `Error::location()`, `Error::code()`, `Error::masked()` | Structured machine-readable `ErrorCode` plus leak-free `MaskedError`. |

## 1. Zero-copy borrowed decoding and `Spanned<T>`

`libyaml`-backed Serde crates materialize intermediate buffers for every scalar
and lose source offsets once deserialized. `ayaml` supports borrowing plain
scalars directly from the input buffer and attaching exact byte spans to fields
with `Spanned<T>`:

```rust
use ayaml::{ColumnEncoding, LineIndex, Location, Spanned, from_str_borrowed};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Route<'a> {
    #[serde(borrow)]
    prefix: Spanned<&'a str>,
    #[serde(borrow)]
    upstream: &'a str,
    timeout_ms: u32,
}

# fn main() -> Result<(), ayaml::Error> {
let input = "prefix: /v1/orders\nupstream: orders.internal\ntimeout_ms: 2500\n";
let route: Route<'_> = from_str_borrowed(input)?;

assert_eq!(*route.prefix, "/v1/orders");
assert_eq!(route.prefix.span().slice(input), Some("/v1/orders"));
assert_eq!(route.upstream, "orders.internal");
assert_eq!(route.timeout_ms, 2500);

let index = LineIndex::new(input);
let pos = index
    .to_position(route.prefix.span().start(), ColumnEncoding::Scalar)
    .expect("prefix position");
assert_eq!(Location::from(pos).line(), 1);
# Ok(())
# }
```

## 2. Replacing `Deserializer` middleware (`serde_path_to_error`, `serde_ignored`)

In `serde_yml` and `yaml-serde`, callers often pass a `Deserializer` into
wrapper crates such as `serde_path_to_error` or `serde_ignored`. In `ayaml`,
use `DocumentDeserializer` for single-document middleware because
`ayaml::Deserializer` is a multi-document stream cursor:

```rust
use ayaml::{DocumentDeserializer, ParseOptions};
use serde::de::{self, Deserialize, MapAccess, Visitor};

#[derive(Debug, Default, PartialEq)]
struct Service {
    name: String,
    replicas: u16,
}

struct WithUnknownFields {
    service: Service,
    unknown: Vec<String>,
}

impl<'de> Deserialize<'de> for WithUnknownFields {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = WithUnknownFields;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a service mapping")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut service = Service::default();
                let mut unknown = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "name" => service.name = map.next_value()?,
                        "replicas" => service.replicas = map.next_value()?,
                        _ => {
                            let _: de::IgnoredAny = map.next_value()?;
                            unknown.push(key);
                        }
                    }
                }
                Ok(WithUnknownFields { service, unknown })
            }
        }
        deserializer.deserialize_map(V)
    }
}

# fn main() -> Result<(), ayaml::Error> {
let yaml = "name: gateway\nreplicas: 3\ndeprecated_opt: true\n";
let de = DocumentDeserializer::from_str_with(yaml, &ParseOptions::new())?;
let parsed = WithUnknownFields::deserialize(de)?;

assert_eq!(parsed.service, Service { name: "gateway".into(), replicas: 3 });
assert_eq!(parsed.unknown, vec!["deprecated_opt"]);
# Ok(())
# }
```

## 3. Bounded reader I/O and alias-bomb safety

Unlike `libyaml` forks that read an entire stream into memory before parsing or
allow unbounded alias amplification, `ayaml::from_reader_with` enforces
`max_input_bytes` while reading and bounds alias expansion via a byte-weighted
budget:

```rust
use std::io::Cursor;
use ayaml::{ErrorKind, LimitKind, Limits, ParseOptions, from_reader_with};

let opts = ParseOptions::new().with_limits(
    Limits::new()
        .with_max_input_bytes(1024)
        .with_max_alias_expansion_bytes(8 * 1024),
);

let oversized = vec![b'x'; 2048];
let err = from_reader_with::<_, ayaml::Value>(Cursor::new(oversized), &opts).unwrap_err();
assert_eq!(err.kind(), ErrorKind::Limit);
assert_eq!(err.limit().map(|l| l.kind()), Some(LimitKind::InputBytes));
```

## 4. Handling libyaml-specific legacy files

When migrating services that consume third-party YAML written for `libyaml`
(last-wins duplicate keys, legacy global tags, or libyaml anchor quirks), keep
`ayaml::from_str` for strict paths and isolate legacy files behind
`ayaml::compat::from_str_with` (requires the additive `libyaml-compat` Cargo
feature). See [Compatibility profile](../compatibility.md) and
[Migrating from YAML 1.1](../migration.md).
