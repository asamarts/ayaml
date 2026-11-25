# Migrating from `serde_yaml`

`serde_yaml` (archived upstream) wrapped C-transpiled `unsafe-libyaml` with YAML
1.1 heuristics and unbounded alias expansion by default (while enforcing a
128-level recursion cap). `ayaml` provides a safe, pure-Rust
(`#![forbid(unsafe_code)]`) YAML 1.2.2 replacement with familiar Serde entry
points, native DoS `Limits` (including byte-weighted alias expansion budgets),
byte-accurate spans, and an optional `libyaml-compat` boundary for legacy
documents.

## Quick API mapping

| `serde_yaml` | `ayaml` | Notes |
| --- | --- | --- |
| `serde_yaml::from_str::<T>(s)` | `ayaml::from_str::<T>(s)` | Also `from_str_with(s, &opts)` and zero-copy `from_str_borrowed(s)`. |
| `serde_yaml::from_slice::<T>(b)` | `ayaml::from_slice::<T>(b)` | Also `from_slice_with` and zero-copy `from_slice_borrowed`; rejects inputs over `Limits::max_input_bytes` before UTF-8 validation. |
| `serde_yaml::from_reader::<R, T>(r)` | `ayaml::from_reader::<R, T>(r)` | Also `from_reader_with`; reads at most `max_input_bytes + 1` bytes before returning `ErrorKind::Limit`. |
| `serde_yaml::to_string(&v)` | `ayaml::to_string(&v)` | Deterministic emission; customize via `to_string_with(&v, &EmitOptions)`. |
| `serde_yaml::to_writer(w, &v)` | `ayaml::to_writer(w, &v)` | Also `to_writer_with(w, &v, &EmitOptions)`. |
| `serde_yaml::from_value::<T>(v)` | `ayaml::from_value::<T>(v)` | Same typed shape checks as `from_str`, except `String` targets require `Value::String`, `Value::Null` is explicit null (cannot supply empty collections or defaulted structs), and `Spanned<T>` is refused. |
| `serde_yaml::to_value(&v)` | `ayaml::to_value(&v)` | Converts any `Serialize` value into `ayaml::Value`. |
| `serde_yaml::Deserializer::from_str(s)` | `ayaml::Deserializer::from_str(s)` | Fallible (`Result<Deserializer, Error>`) lazy multi-document cursor (`de.next::<T>()?`); does not implement `Iterator` or `serde::Deserializer` directly (use `DocumentDeserializer` for single-doc middleware; read UTF-8 first for multi-doc `Read`/`&[u8]`). |
| `Value::apply_merge(&mut self)` | `ParseOptions::new().with_merge_keys(true)` or `Value::apply_merge_keys(&mut self)` | `with_merge_keys(true)` preserves quoted `"<<"` keys, spans, and borrowed strings; `Value::apply_merge_keys` / `apply_merge_keys_with_limits` operate post-parse (where quoted `"<<"` and plain `<<` are indistinguishable). |
| `Error::location()` (`Location::index()`) | `Error::span()` + `Error::location()` | `Error::span().start()` replaces `Location::index()`; `Error::location()` gives one-based `(line, column)`; format `err.masked()` for untrusted logs. |

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

Moving from `serde_yaml` to strict YAML 1.2.2 surfaces several intentional
differences:

1. **Duplicate mapping keys are rejected by default.** `serde_yaml` silently
   overwrote earlier keys in mappings. `ayaml` returns `ErrorCode::DuplicateKey`
   so typos and shadowed security policies fail loudly.
2. **YAML 1.1 plain scalars stay strings.** Values like `yes`, `no`, `on`,
   `off`, `0644`, and `2026-05-01` decode as strings in YAML 1.2.2, while
   special floats follow YAML 1.2.2 (`.nan`, `.inf`, `-.inf`; bare `NaN` /
   `Infinity` remain strings; see [Migrating from YAML 1.1](../migration.md)).
3. **Merge keys (`<<:`) are opt-in.** Replace `val.apply_merge()` with
   `ParseOptions::new().with_merge_keys(true)` so merge expansion works directly
   with typed Serde structs and respects alias budgets while keeping quoted
   `"<<"` literal. When mutating an already-parsed `Value`, use
   `Value::apply_merge_keys` or `Value::apply_merge_keys_with_limits`.
4. **Externally tagged Rust enums (`!Variant` and `Variant: payload`).** Like
   `serde_yaml` 0.9, `ayaml` serializes externally tagged data-carrying enum
   variants with `!Variant` YAML tags (and unit variants as plain `Variant`),
   and deserializes **both** `!Variant payload` and single-entry mappings
   (`Variant: payload`). `ayaml` does not ship a `with::singleton_map` helper;
   callers that must *serialize* enums as single-entry mappings (`Variant: payload`)
   should wrap the field in a small custom `Serialize` helper.
5. **Empty documents, `Number`, `Value::Tagged`, and `Mapping`.**
   - An empty document (`""`) decodes as `Value::Null` (`parse_value` /
     `from_str::<Value>`) and fills a `#[serde(default)]` struct via `from_str`,
     whereas `from_value(Value::Null)` treats `Null` as an explicit null.
   - `ayaml::Number` stores exact `i64`/`u64`/`i128`/`u128` integers and
     IEEE-754 `f64` floats, formatting non-finite floats as `.nan`/`.inf`/`-.inf`.
   - `Value` accessors (`as_str`, `as_u64`, indexing) look through `Value::Tagged`,
     `TaggedValue.tag` is a structured `ayaml::Tag` (`handle()`, `suffix()`), and
     serializing `Value::Tagged` to non-YAML Serde serializers (such as
     `serde_json`) encodes as `{"!Tag": value}`.
   - `Mapping` is insertion-ordered with private `indexmap` backing (no
     `indexmap` types in the public API), zero-alloc `&str` lookups, and
     `shift_remove` / `swap_remove`.
6. **Resource limits are always active.** Untrusted payloads cannot trigger
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

// Single-entry mapping enum syntax (`Env: API_TOKEN`) also deserializes cleanly.
let mapped_enum: SecretRef = from_str("Env: API_TOKEN\n")?;
assert_eq!(mapped_enum, SecretRef::Env("API_TOKEN".into()));

// Single-doc readers reject trailing documents cleanly.
let trailing = from_str::<u32>("1\n---\n2\n").unwrap_err();
assert_eq!(trailing.code(), ErrorCode::TrailingDocuments);
# Ok(())
# }
```

## 4. Spans, limits, and safe error logging

`serde_yaml::Error::to_string()` can echo raw YAML fragments into logs. In
`ayaml`, match on `Error::code()`, use `Error::span()` (`span.start()` replaces
`Location::index()`) with `LineIndex` for user-facing editors, and log
`err.masked()` for untrusted inputs:

```rust
use std::collections::HashMap;
use ayaml::{ErrorCode, Limits, ParseOptions, from_str_with};

let opts = ParseOptions::new().with_limits(Limits::new().with_max_depth(32));
let err = from_str_with::<HashMap<String, u16>>("api_key: super-secret\n", &opts).unwrap_err();

assert_eq!(err.code(), ErrorCode::InvalidType);
assert!(err.location().is_some());
assert!(err.span().is_some());
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
