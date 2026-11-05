# Dependency policy

ayaml keeps its runtime dependency graph small and does not delegate YAML
parsing to another parser.

## Runtime dependencies

| Crate | Role |
| --- | --- |
| `serde` | Serde traits and data model; the ecosystem contract. |
| `indexmap` | Insertion-ordered map used by `Mapping`. |

No third-party YAML parser, scanner, composer, or native YAML library is in the
normal dependency graph.

## Unsafe code policy

- ayaml-owned crates use `#![forbid(unsafe_code)]`.
- The YAML-specific parser and composer code is written in safe Rust and does
  not use FFI.
- `serde` and `indexmap` contain reviewed implementations with internal
  `unsafe`; both are allowlisted runtime dependencies.
- A new dependency with unsafe code requires a measured need, a safety review,
  and an explicit policy update.
- Dev-only differential, property-test, fuzzing, and benchmark dependencies are
  not part of the shipped graph.

`cargo tree -e normal` is the shipped graph. The workspace dependency check
enforces the direct allowlist and a complete allowlist for the normal graph, so
any other crate there fails, including a YAML parser.

## Build and test dependencies

The workspace uses dev-only crates for conformance, differential testing,
allocation measurement, and benchmarks. These are not reachable from the
normal runtime graph.

## Minimum supported Rust version

The MSRV is Rust 1.88. CI checks the locked dependency graph, not only the
direct crate metadata, because a transitive dependency can raise the effective
floor. Raising the MSRV requires an explicit release note.
