# ayaml

A safe, pure-Rust YAML 1.2.2 parser, event stream, value model, Serde
implementation, and emitter.

ayaml is built for applications that need a complete YAML pipeline without
unsafe Rust or a native YAML dependency. One scanner and grammar back the
spanned event stream, the generic value model, typed Serde decoding, and the
emitter, so every surface observes the same syntax and resolution rules.

The current patch release is `0.1.2`. The crate is UTF-8 only, has an MSRV of
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
ayaml = "0.1.2"
```

For typed decoding, enable Serde's derive feature in your own crate:

```toml
[dependencies]
ayaml = "0.1.2"
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

Apply stricter limits:

```rust
use ayaml::{parse_value_with_limits, Limits};

fn main() -> Result<(), ayaml::Error> {
    let limits = Limits::default().with_max_depth(64);
    let value = parse_value_with_limits("a: [1, 2, 3]\n", &limits)?;
    assert_eq!(value["a"][1].as_u64(), Some(2));
    Ok(())
}
```

## API overview

| Need | Entry points |
| --- | --- |
| One document as a value | `parse_value`, `parse_value_with_limits` |
| A document stream as values | `parse_documents`, `parse_documents_with_limits` |
| Spanned events | `parse_events`, `parse_events_with_limits`, `EventStream` |
| Typed decoding | `from_str`, `from_slice`, `from_str_borrowed`, `Deserializer` |
| Value/Serde conversion | `from_value`, `from_value_seed`, `to_value` |
| Emission | `to_string`, `to_writer`, `Emitter`, `to_string_events` |
| Compatibility | `ayaml::compat` with the `libyaml-compat` feature |

`Limits` is `#[non_exhaustive]`; configure it with `Limits::default()`,
`Limits::compat()`, and the `with_*` builder methods. `EmitterOptions`,
`NodeStyle`, and `EmitterStyles` provide presentation control. `Emitter`
borrows a limits value and a set of options so one configuration can be reused.

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

`Limits` can be passed to the parser and document readers, the borrowed typed
decoder, the `Deserializer`, and the emitters that support an explicit limit, through
their `_with_limits` forms. The convenience `from_str` and `from_slice` typed readers
use the defaults. The defaults allow 32 MiB of input, depth 128, 10,000 documents per
stream, and a byte-weighted alias budget; the single-document APIs still reject a
second document. Exceeding a limit returns `ErrorKind::Limit`, and `Error::limit()`
exposes the limit kind, ceiling, and observed value. Limit errors carry source spans
where the input provides one.

See [docs/limits.md](https://github.com/asamarts/ayaml/blob/main/docs/limits.md) for defaults, boundaries, and the alias
budget formula.

## Feature flags

| Feature | Default | Purpose |
| --- | --- | --- |
| `libyaml-compat` | no | Adds the additive `ayaml::compat` decoding module. |
| `bench-internals` | no | Unstable hooks for ayaml's benchmark and conformance harnesses; outside SemVer. |

## Documentation

- [Design](https://github.com/asamarts/ayaml/blob/main/docs/design.md)
- [Compatibility profile](https://github.com/asamarts/ayaml/blob/main/docs/compatibility.md)
- [Resource limits](https://github.com/asamarts/ayaml/blob/main/docs/limits.md)
- [Dependency policy](https://github.com/asamarts/ayaml/blob/main/docs/dependencies.md)
- [Benchmark methodology](https://github.com/asamarts/ayaml/blob/main/docs/benchmarks.md)
- [Changelog](https://github.com/asamarts/ayaml/blob/main/CHANGELOG.md)

## Minimum supported Rust version

Rust 1.88. The MSRV is checked in CI against the locked dependency graph.
Raising it requires an explicit release note.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. See [NOTICE](NOTICE) for the project notice.
