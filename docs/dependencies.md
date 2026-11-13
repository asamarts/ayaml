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

- The crates in this workspace forbid `unsafe` code. Only the fuzz workspace, which
  isn't published, has any: the counting allocator in
  `fuzz/src/bin/memcount.rs`.
- The YAML-specific parser and composer code is written in safe Rust and does
  not use FFI.
- `serde` and `indexmap` contain reviewed implementations with internal
  `unsafe`, as do `serde_core` and `hashbrown`, which they bring in. All four are
  allowlisted runtime dependencies.
- A new dependency with unsafe code requires a measured need, a safety review,
  and an explicit policy update.
- CI runs an unsafe census with `cargo-geiger`, and `tools/check_unsafe_census.py` holds the
  allowlist. It fails if ayaml has any unsafe code or stops forbidding it, or if any other crate
  in the census has unsafe code.
- Dev-only property-test, fuzzing, allocation, and benchmark dependencies are not
  part of the shipped graph.

`cargo tree -p ayaml -e normal,build --all-features` is the shipped graph.
`tools/check_deps.py` reads it, and any crate not on its allowlists fails, including
a YAML parser.

## Build and test dependencies

The repository uses dev-only crates for conformance, property testing,
fuzzing, allocation measurement, and benchmarks. These are not reachable
from the normal runtime graph.

## Minimum supported Rust version

The MSRV is Rust 1.88. CI checks the locked dependency graph, not only the
direct crate metadata, because a transitive dependency can raise the effective
floor. Raising the MSRV requires an explicit release note.
