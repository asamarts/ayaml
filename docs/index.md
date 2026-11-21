# ayaml

`ayaml` is a safe, pure-Rust YAML 1.2.2 parser, spanned event stream, generic value model,
Serde deserializer/serializer, and deterministic emitter.

One scanner and grammar back every public surface, so event streams, `Value` trees,
typed Serde reads, `SpannedTree` lookups, and emitted YAML share the same syntax rules,
byte-accurate spans, and resource limits. Published workspace crates forbid `unsafe` code
and depend only on `serde` and `indexmap`.

## Install

```toml
[dependencies]
ayaml = "0.2.3"
```

For derived Serde types, enable Serde's `derive` feature in your crate:

```toml
[dependencies]
ayaml = "0.2.3"
serde = { version = "1", features = ["derive"] }
```

The minimum supported Rust version is Rust 1.88.

## Quick start

Parse into `Value` or decode directly into typed Rust structs, then emit deterministic YAML:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Deserialize, Serialize)]
struct Service {
    name: String,
    ports: Vec<u16>,
}

# fn main() -> Result<(), ayaml::Error> {
let input = "name: api\nports:\n  - 8080\n  - 8443\n";
let service: Service = ayaml::from_str(input)?;
assert_eq!(service.name, "api");
assert_eq!(service.ports, vec![8080, 8443]);

let rendered = ayaml::to_string(&service)?;
assert_eq!(ayaml::from_str::<Service>(&rendered)?, service);
# Ok(())
# }
```

## Where to go next

| Topic | Page |
| --- | --- |
| Parsing, typed Serde reads, events, spans, options, and emitting | [Getting started](getting-started.md) |
| Format contract layers, spans, positions, and locations | [Format contract and interface](interface.md) |
| YAML 1.2.2 profile and optional `libyaml-compat` mode | [Compatibility profile](compatibility.md) |
| Stable `ErrorCode` catalog and masked error output | [Error codes](error-codes.md) |
| Threat model, untrusted input, resource limits, and error text | [Security and untrusted input](security.md) |
| Stable, `#[non_exhaustive]`, deprecated, and unstable API surfaces | [Stability and versioning](stability.md) |
| Detailed resource limit ceilings, accounting rules, and memory bounds | [Resource limits](limits.md) |
| Generated Rust API reference | [docs.rs/ayaml](https://docs.rs/ayaml) |
