# ayaml

`ayaml` is a pure-Rust YAML 1.2.2 parser, spanned event stream, generic value model,
Serde codec, and deterministic emitter.

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
| Streaming, tags, merges, `Spanned<T>`, frontmatter, and middleware | [Cookbook](cookbook.md) |
| Comment-preserving verified edits with `ayaml-edit` | [Source-preserving editing (`ayaml-edit`)](editing.md) |
| Migrating from `serde_yaml` (archived libyaml Serde crate) | [Migrating from `serde_yaml`](migrating-from/serde-yaml.md) |
| Migrating from `serde_yml`, `serde_yaml_ng`, or `yaml-serde` | [Migrating from libyaml forks](migrating-from/serde-yml-and-forks.md) |
| Migrating from `yaml-rust2`, `saphyr`, or `serde-saphyr` | [Migrating from `yaml-rust2` / `saphyr`](migrating-from/yaml-rust2-and-saphyr.md) |
| Moving from YAML 1.1 scalars, duplicate keys, and merge keys | [Migrating from YAML 1.1](migration.md) |
| Choosing `Limits` profiles for untrusted payloads and large schemas | [Sizing resource limits](limits-sizing.md) |
| Format contract layers, spans, positions, and locations | [Format contract and interface](interface.md) |
| YAML 1.2.2 profile and optional `libyaml-compat` mode | [Compatibility profile](compatibility.md) |
| Complete public API map, feature flags, and caveats | [API reference and caveats](api-reference.md) |
| Stable `ErrorCode` catalog and masked error output | [Error codes](error-codes.md) |
| Threat model, untrusted input, resource limits, and error text | [Security and untrusted input](security.md) |
| Stable, `#[non_exhaustive]`, deprecated, and unstable API surfaces | [Stability and versioning](stability.md) |
| Detailed resource limit ceilings, accounting rules, and memory bounds | [Resource limits](limits.md) |
| Benchmark results and comparison tables | [Benchmarks](https://asamarts.github.io/ayaml/benchmarks/) |
| Reproducible harness methodology and `cargo xtask` | [Benchmark methodology](benchmarks.md) |
| Conformance, fuzzing, Kani, Miri, and mutation tooling | [Developer tooling and verification](tools.md) |
| Generated Rust API reference | [docs.rs/ayaml](https://docs.rs/ayaml) &middot; [docs.rs/ayaml-edit](https://docs.rs/ayaml-edit) |
