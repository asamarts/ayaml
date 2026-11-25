# ayaml

A safe, pure-Rust YAML 1.2.2 parser, event stream, value model, Serde
implementation, and emitter.

ayaml is built for applications that need a complete YAML pipeline without
unsafe Rust or a native YAML dependency. One scanner and grammar back the
spanned event stream, the generic value model, typed Serde decoding, and the
emitter, so every surface observes the same syntax and resolution rules.

The current release is `0.2.3`. The crate is UTF-8 only, has an MSRV of
Rust 1.88, and is dual-licensed under MIT or Apache-2.0.

## Highlights

- **YAML 1.2.2 parser** with block and flow collections, all scalar styles,
  directives, tags, anchors, aliases, document streams, duplicate mapping
  pairs at the event layer, and byte-accurate spans.
- **Spanned event stream** for policy checks, source-aware tooling, and
  consumers that should not materialize a full document.
- **Generic value model** with insertion-ordered mappings, tagged values,
  exact wide integers, and deterministic equality.
- **Serde support** through borrowed and owned typed decoding, a
  multi-document `Deserializer`, `DeserializeSeed`, `from_value`, and
  `to_value`. `from_value` applies the typed path's shape checks, and its string
  target requires a string; a composed null is an explicit null.
- **Deterministic emitter** for values, Serde types, event streams, and
  document streams, with per-node styles and output limits.
- **Native resource limits** for input size, depth, documents, scalars,
  collections, events, anchors, aliases, alias expansion, tag expansion, and output.
- **No `unsafe`** in the crates ayaml publishes and no third-party YAML parser in
  the runtime dependency graph.
- **Optional libyaml-compatible mode** behind the additive `libyaml-compat`
  feature, without changing the default parsing profile.

## Install

```toml
[dependencies]
ayaml = "0.2.3"
```

For typed decoding, enable Serde's derive feature in your own crate:

```toml
[dependencies]
ayaml = "0.2.3"
serde = { version = "1", features = ["derive"] }
```

## Quick start

Parse a document into the generic value model:

```rust
use ayaml::{parse_value, to_string};

fn main() -> Result<(), ayaml::Error> {
    let value = parse_value("name: example\nitems:\n  - one\n  - two\n")?;
    assert_eq!(value["name"].as_str(), Some("example"));

    let output = to_string(&value)?;
    assert!(output.contains("name: example"));
    Ok(())
}
```

Decode into a typed value:

```rust
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Config {
    name: String,
    retries: u32,
}

fn main() -> Result<(), ayaml::Error> {
    let config: Config = ayaml::from_str("name: example\nretries: 3\n")?;
    assert_eq!(config.retries, 3);
    Ok(())
}
```

Walk the event stream when source spans matter:

```rust
fn main() -> Result<(), ayaml::Error> {
    for event in ayaml::parse_events("a: 1\n")? {
        let event = event?;
        println!("{event:?} at {:?}", event.span());
    }
    Ok(())
}
```

A span is a byte range. When a line and column are needed, `LineIndex::new(input)` converts an
offset to a zero-based `Position` in UTF-8, scalar, or UTF-16 columns, and back;
`Location::from(position)` is the one-based place that a person reads.

Apply stricter limits or emit options with `ParseOptions` and `EmitOptions`:

```rust
use ayaml::{EmitOptions, Limits, ParseOptions, parse_value_with, to_string_value_with};

const PARSE: ParseOptions =
    ParseOptions::new().with_limits(Limits::new().with_max_depth(64));

fn main() -> Result<(), ayaml::Error> {
    let value = parse_value_with("a: [1, 2, 3]\n", &PARSE)?;
    assert_eq!(value["a"][1].as_u64(), Some(2));

    let flow = to_string_value_with(&value, &EmitOptions::flow())?;
    assert_eq!(flow, "{a: [1, 2, 3]}\n");
    Ok(())
}
```

## API overview

| Need | Entry points |
| --- | --- |
| One document as a value | `parse_value`, `parse_value_with` (with opt-in `ParseOptions::with_merge_keys`) |
| A document stream as values | `parse_documents`, `parse_documents_with` |
| Spanned events | `parse_events`, `parse_events_with`, `EventStream` |
| Compatibility events | `compat::parse_events`, `compat::parse_events_with`, and the eager `compat::parse_events_eager_with` |
| Spanned tree & typed spans | `SpannedTree::parse`, `SpannedTree::parse_with`, `SpannedNode`, `Spanned<T>`, `LineIndex` |
| Typed decoding & middleware | `from_str`, `from_str_with`, `from_str_borrowed`, `from_slice`, `from_slice_with`, `from_slice_borrowed`, `from_reader`, `from_reader_with`, `from_str_seed`, `from_str_seed_with`, `Deserializer`, `DocumentDeserializer` |
| Markdown frontmatter | `split_frontmatter`, `split_frontmatter_with`, `parse_frontmatter`, `parse_frontmatter_with`, `parse_frontmatter_value`, `parse_frontmatter_value_with` |
| Value/Serde conversion | `from_value`, `from_value_seed`, `to_value` |
| Emission | `to_string`, `to_string_with`, `to_writer`, `to_writer_with`, `to_string_value_with`, `to_string_documents_with`, `to_string_events_with`, `Emitter` |
| Surgical editing | Companion crate `ayaml-edit`: `replace_scalar`, `remove_entry`, `apply_batch`, `apply_edits` |
| Compatibility | `ayaml::compat` with `compat::ParseOptions` and the `libyaml-compat` feature |

`ParseOptions`, `compat::ParseOptions`, `EmitOptions`, and `Limits` are
`#[non_exhaustive]` structs with `const` presets (`new()`, `for_max_input()`,
`EmitOptions::flow()`) and `with_*` builders. `EmitOptions` combines resource
limits, layout and indentation settings, and optional per-node `EmitterStyles`
in one value. `Emitter` remains available to reuse borrowed `Limits` and
`EmitterOptions` across calls. The older `*_with_limits`, `*_with_options`, and
`*_with_styles` combination functions remain available and deprecated through
`0.x` until removal in `1.0`.

## Compatibility profile

The default parser follows YAML 1.2.2. In particular:

- `yes`, `no`, and date-like text remain strings;
- `0123` remains a string; `0b11` and `0x1f` are integers;
- `1e3` is a floating-point number;
- merge keys remain literal `<<` entries in the generic value model;
- duplicate mapping pairs are retained at the event layer and rejected
  deterministically during generic composition;
- custom tags are preserved.

The `libyaml-compat` feature adds `ayaml::compat` for callers that need the
observable behavior of a libyaml-compatible stack. It is additive: enabling it
does not change the root parsing functions. The deliberate compatibility
deviations are listed in
[`tests/data/yaml-test-suite-deviations.txt`](https://github.com/asamarts/ayaml/blob/main/tests/data/yaml-test-suite-deviations.txt).

See [docs/compatibility.md](https://github.com/asamarts/ayaml/blob/main/docs/compatibility.md) for the full profile.

## Limits

Every parse, decode, and emit entry point accepts explicit `Limits` through its
`_with` form taking `&ParseOptions`, `&compat::ParseOptions`, or `&EmitOptions`
(the in-memory `from_value`, `from_value_seed`, and `to_value` conversions do
not take limits). The defaults allow 32 MiB of input, depth 128, 10,000 documents per
stream, and a byte-weighted alias budget; the single-document APIs still reject a
second document. The default count limits are intentionally lower than the input
limit, to bound memory for untrusted input; use `ParseOptions::for_max_input()`
(or `Limits::for_max_input()`) when a
dense document within the input limit is refused by a count limit, at the cost of
much larger memory use. Exceeding a limit returns `ErrorKind::Limit`, and
`Error::limit()` exposes the limit kind, ceiling, and observed value. Limit errors
carry source spans where the input provides one.

See [docs/limits.md](https://github.com/asamarts/ayaml/blob/main/docs/limits.md) for defaults, boundaries, and the alias
budget formula.

## Feature flags

| Feature | Default | Purpose |
| --- | --- | --- |
| `libyaml-compat` | no | Adds the additive `ayaml::compat` decoding module, and `SpannedTree::parse_compat` with its `_with` form. |
| `bench-internals` | no | Unstable hooks for ayaml's benchmark and conformance harnesses; outside SemVer. |

## Documentation

- [Documentation site](https://asamarts.github.io/ayaml/)
- [Interactive benchmarks & performance showcase](https://asamarts.github.io/ayaml/benchmarks/)
- [Getting started](https://asamarts.github.io/ayaml/docs/getting-started/)
- [Cookbook](https://asamarts.github.io/ayaml/docs/cookbook/)
- [Surgical editing with `ayaml-edit`](https://asamarts.github.io/ayaml/docs/editing/)
- Migration guides:
  - [Migrating from `serde_yaml`](https://asamarts.github.io/ayaml/docs/migrating-from/serde-yaml/)
  - [Migrating from `serde_yml`, `serde_yaml_ng`, and `yaml-serde`](https://asamarts.github.io/ayaml/docs/migrating-from/serde-yml-and-forks/)
  - [Migrating from `yaml-rust2`, `saphyr`, and `serde-saphyr`](https://asamarts.github.io/ayaml/docs/migrating-from/yaml-rust2-and-saphyr/)
  - [Migrating from YAML 1.1](https://asamarts.github.io/ayaml/docs/migration/)
- [API reference and caveats](https://asamarts.github.io/ayaml/docs/api-reference/) ([docs.rs/ayaml](https://docs.rs/ayaml), [docs.rs/ayaml-edit](https://docs.rs/ayaml-edit))
- [Error codes and masked errors](https://asamarts.github.io/ayaml/docs/error-codes/)
- [Security and untrusted input](https://asamarts.github.io/ayaml/docs/security/)
- [Stability and versioning](https://asamarts.github.io/ayaml/docs/stability/)
- [Compatibility profile](https://asamarts.github.io/ayaml/docs/compatibility/)
- [Resource limits](https://asamarts.github.io/ayaml/docs/limits/) and [sizing guide](https://asamarts.github.io/ayaml/docs/limits-sizing/)
- [Developer tooling and verification](https://asamarts.github.io/ayaml/docs/tools/)
- [Benchmark methodology](https://asamarts.github.io/ayaml/docs/benchmarks/)
- [Changelog](https://github.com/asamarts/ayaml/blob/main/CHANGELOG.md)

## Minimum supported Rust version

Rust 1.88. The MSRV is checked in CI against the locked dependency graph.
Raising it requires an explicit release note.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. See [NOTICE](NOTICE) for the project notice.
