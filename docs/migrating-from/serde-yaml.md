# Migrating from `serde_yaml`

`serde_yaml` (archived upstream) wrapped unsafe `libyaml` bindings with YAML 1.1
heuristics and unbounded alias expansion by default. `ayaml` provides a safe,
pure-Rust (`#![forbid(unsafe_code)]`) YAML 1.2.2 replacement with familiar Serde
entry points, native DoS `Limits`, byte-accurate spans, and an optional
`libyaml-compat` boundary for legacy documents.

## Quick API mapping

| `serde_yaml` | `ayaml` | Notes |
| --- | --- | --- |
| `serde_yaml::from_str::<T>(s)` | `ayaml::from_str::<T>(s)` | Also `from_str_with(s, &opts)` and zero-copy `from_str_borrowed(s)`. |
| `serde_yaml::from_slice::<T>(b)` | `ayaml::from_slice::<T>(b)` | Rejects inputs over `Limits::max_input_bytes` before UTF-8 validation. |
| `serde_yaml::from_reader::<R, T>(r)` | `ayaml::from_reader::<R, T>(r)` | Reads at most `max_input_bytes + 1` bytes before returning `ErrorKind::Limit`. |
| `serde_yaml::to_string(&v)` | `ayaml::to_string(&v)` | Deterministic emission; customize via `to_string_with(&v, &EmitOptions)`. |
| `serde_yaml::to_writer(w, &v)` | `ayaml::to_writer(w, &v)` | Also `to_writer_with(w, &v, &EmitOptions)`. |
| `serde_yaml::from_value::<T>(v)` | `ayaml::from_value::<T>(v)` | Uses the same typed shape rules as `from_str`. |
| `serde_yaml::to_value(&v)` | `ayaml::to_value(&v)` | Converts any `Serialize` value into `ayaml::Value`. |
| `serde_yaml::Deserializer::from_str(s)` | `ayaml::Deserializer::from_str(s)` | Lazy multi-document cursor (`de.next::<T>()?`); use `DocumentDeserializer` for single-doc `serde::Deserializer` middleware. |
| `Value::apply_merge(&mut self)` | `ParseOptions::new().with_merge_keys(true)` or `Value::apply_merge_keys(&mut self)` | `with_merge_keys(true)` preserves quoted `"<<"` keys, spans, and borrowed strings. |
| `Error::location()` | `Error::span()` + `Error::location()` | Byte `Span` plus one-based `Location`; format `err.masked()` for untrusted logs. |

## 1. Updating `Cargo.toml`

Replace `serde_yaml` with `ayaml`:

```toml
[dependencies]
ayaml = "0.2.3"
serde = { version = "1", features = ["derive"] }
```

## 2. Typed Serde decode and multi-document streams

Single-document calls map directly, while multi-document streams become a
pull loop on `ayaml::Deserializer::next`:

```rust
use ayaml::{Deserializer, from_str, to_string};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Deserialize, Serialize)]
struct Service {
    name: String,
    port: u16,
}

# fn main() -> Result<(), ayaml::Error> {
let single: Service = from_str("name: billing\nport: 8080\n")?;
assert_eq!(single.port, 8080);

let yaml = to_string(&single)?;
assert_eq!(from_str::<Service>(&yaml)?, single);

let stream = "name: api\nport: 8080\n---\nname: worker\nport: 9090\n";
let mut de = Deserializer::from_str(stream)?;
let mut services = Vec::new();
while let Some(svc) = de.next::<Service>()? {
    services.push(svc);
}
assert_eq!(services.len(), 2);
assert_eq!(services[1].name, "worker");
# Ok(())
# }
```

## 3. Behavioral differences to check

Moving from `serde_yaml` to strict YAML 1.2.2 surfaces five intentional
improvements:

1. **Duplicate mapping keys are rejected by default.** `serde_yaml` silently
   overwrote earlier keys in mappings. `ayaml` returns `ErrorCode::DuplicateKey`
   so typos and shadowed security policies fail loudly.
2. **YAML 1.1 plain scalars stay strings.** Values like `yes`, `no`, `on`,
   `off`, `0644`, and `2026-05-01` decode as strings in YAML 1.2.2 (see
   [Migrating from YAML 1.1](../migration.md)).
3. **Merge keys (`<<:`) are opt-in.** Replace `val.apply_merge()` with
   `ParseOptions::new().with_merge_keys(true)` so merge expansion works directly
   with typed Serde structs and respects alias budgets while keeping quoted
   `"<<"` literal.
4. **Local tags (`!Variant`) map directly to Rust enums.** Adjacent or external
   enum tagging with `!Tag` round-trips cleanly without `serde_yaml::with::singleton_map`.
5. **Resource limits are always active.** Untrusted payloads cannot trigger
   billion-laughs alias expansion or unbounded reader allocation.

```rust
use ayaml::{ErrorCode, ParseOptions, from_str, from_str_with, parse_value};
use serde::Deserialize;

#[derive(Debug, PartialEq, Deserialize)]
enum SecretRef {
    Env(String),
    Literal(String),
}

#[derive(Debug, PartialEq, Deserialize)]
struct AppConfig {
    country: String,
    retries: u8,
    token: SecretRef,
}

# fn main() -> Result<(), ayaml::Error> {
// Duplicate keys are rejected rather than silently overwritten.
let dup_err = parse_value("port: 80\nport: 443\n").unwrap_err();
assert_eq!(dup_err.code(), ErrorCode::DuplicateKey);

// Opt-in merge keys + YAML 1.2.2 strings (`NO` stays "NO") + `!Tag` enums.
let input = "\
defaults: &def
  country: NO
  retries: 3
app:
  <<: *def
  token: !Env API_TOKEN
";

#[derive(Debug, Deserialize)]
struct Root {
    app: AppConfig,
}

let opts = ParseOptions::new().with_merge_keys(true);
let root: Root = from_str_with(input, &opts)?;
assert_eq!(
    root.app,
    AppConfig {
        country: "NO".into(),
        retries: 3,
        token: SecretRef::Env("API_TOKEN".into()),
    }
);

// Single-doc readers reject trailing documents cleanly.
let trailing = from_str::<u32>("1\n---\n2\n").unwrap_err();
assert_eq!(trailing.code(), ErrorCode::TrailingDocuments);
# Ok(())
# }
```

## 4. Spans, limits, and safe error logging

`serde_yaml::Error::to_string()` can echo raw YAML fragments into logs. In
`ayaml`, match on `Error::code()`, use `Error::span()` with `LineIndex` for
user-facing editors, and log `err.masked()` for untrusted inputs:

```rust
use std::collections::HashMap;
use ayaml::{ErrorCode, Limits, ParseOptions, from_str_with};

let opts = ParseOptions::new().with_limits(Limits::new().with_max_depth(32));
let err = from_str_with::<HashMap<String, u16>>("api_key: super-secret\n", &opts).unwrap_err();

assert_eq!(err.code(), ErrorCode::InvalidType);
assert!(err.location().is_some());
let safe_log = err.masked().to_string();
assert!(!safe_log.contains("super-secret"));
assert!(safe_log.contains("yaml.invalid-type"));
```

## 5. Legacy fallback with `ayaml::compat`

If you have legacy inputs that rely on last-wins duplicate keys or non-core
global tags (such as `!!timestamp`), enable the additive `libyaml-compat`
feature and call `ayaml::compat::*` at that boundary without relaxing the rest
of your application:

```toml
[dependencies]
ayaml = { version = "0.2.3", features = ["libyaml-compat"] }
```

See [Compatibility profile](../compatibility.md) for the exact rules in
`ayaml::compat`.
