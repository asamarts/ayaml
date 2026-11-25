# Stability and versioning

`ayaml` follows [Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html). During the
`0.2.x` series, every patch release is additive and non-breaking for callers that use the
stable API. This page states which surfaces are stable, which are extensible, which are
deprecated, and which sit outside SemVer.

## Release line and compatibility checks

- **Current line (`0.2.x` -> `1.0.0` freeze).** Every pull request and release tag runs both
  `cargo-semver-checks` and a `public-api` diff against the previous released `vMAJOR.MINOR.PATCH`
  tag across default features and `libyaml-compat`. Any removed item or signature/trait break
  fails CI unless explicitly recorded as a breaking change for a minor (`0.x`) or major (`1.0`)
  release in `CHANGELOG.md`.
- **1.0 stability promise.** Once `1.0.0` is tagged, all `1.x` releases are strictly additive and
  backwards-compatible for every surface marked **Stable** or **Stable (additive)** below.
  Deprecated `0.x` shims (`*_with_limits`, `*_with_options`, `*_with_styles`, and `Marker`) are
  scheduled for removal in `1.0.0` (see D104 in [Design decisions](decisions.md#d104-10-public-api-trait-error-code-and-semver-freeze-audit)).
- **Previous line (`0.1.x`).** The `0.1.x` line ended with `0.1.3` and does not receive
  backports.
- **Minimum supported Rust version (MSRV).** Rust 1.88, verified in CI against the locked
  dependency graph (`Cargo.lock`). In `1.x`, raising the MSRV requires a minor version bump and an
  explicit release note.

## Surface stability matrix

| Surface | Status | Contract |
| --- | --- | --- |
| Crate-root functions, submodules, constants, and core data types (`error`, `event`, `limits`, `mapping`, `number`, `span`, `tag`, `value`; `parse_value*`, `parse_documents*`, `parse_events*`, `from_str*`, `from_slice*`, `from_reader*`, `from_value*`, `to_value`, `to_string*`, `to_writer*`, `split_frontmatter*`, `parse_frontmatter*`, `Value`, `Mapping`, `Entry`, `Number`, `Tag`, `TaggedValue`, `Event`, `EventStream`, `Anchor`, `ScalarStyle`, `CollectionStyle`, `Deserializer`, `DocumentDeserializer`, `Span`, `Spanned`, `Position`, `Location`, `LineIndex`, `ColumnEncoding`, `PositionError`, `Frontmatter`, `FrontmatterDelimiter`, `ParsedFrontmatter`, `SpannedTree`, `SpannedNode`, `PathSegment`, `Chomping`, `NodeStyle`, `Emitter`, `EmitterOptions`, `EmitterStyles`, `Error`, `MaskedError`, `LimitExceeded`, default limit constants, `VERSION`) | **Stable** | Covered by SemVer, compile-time trait freeze tests (`api_freeze`), and automated public API diff checks. |
| Companion editing crate `ayaml-edit` (`TextEdit`, `VerifiedEdit`, `EditError`, `EditErrorKind`, `EditOptions`, `EditOp`, `apply_edits`, `replace_scalar*`, `remove_entry*`, `apply_batch*`) | **Stable (additive)** | Covered by SemVer and compile-time trait freeze tests; `EditErrorKind`, `EditOptions`, and `EditOp` are `#[non_exhaustive]`. |
| `#[non_exhaustive]` enums and option/style structs (`ErrorKind`, `ErrorCode`, `LimitKind`, `Event`, `ScalarStyle`, `CollectionStyle`, `ColumnEncoding`, `PositionError`, `PathSegment`, `Chomping`, `compat::GlobalTags`, `Limits`, `ParseOptions`, `compat::ParseOptions`, `EmitOptions`, `EmitterOptions`, `NodeStyle`, `ayaml_edit::EditErrorKind`, `ayaml_edit::EditOptions`, `ayaml_edit::EditOp`) | **Stable (additive)** | Existing variants, `ErrorCode::as_str()` (`yaml.<kebab-name>`) strings, and builder methods are not renamed or removed within a major line. New variants or fields may be added in any `0.2.x` or `1.x` release. |
| `libyaml-compat` feature (`ayaml::compat::*`, `SpannedTree::parse_compat*`) | **Stable (opt-in feature)** | Additive feature that does not change default-profile functions. Covered by the public API diff check. Deliberate deviations from YAML 1.2.2 are documented in [Compatibility profile](compatibility.md). |
| Legacy `*_with_limits`, `*_with_options`, `*_with_styles` functions and `Marker` | **Deprecated** | Kept working through `0.x` so existing callers do not break; scheduled for removal in `1.0`. Prefer the `*_with` functions taking `&ParseOptions`, `&compat::ParseOptions`, or `&EmitOptions`, and use `Location` / `Position` instead of `Marker`. |
| `bench-internals` feature and `ayaml::internals` module | **Unstable** | Outside SemVer. Exists only for `ayaml`'s own benchmark and conformance harnesses; items may change or be removed in any release. |
| `Error::message()` prose wording and normal `Error` `Display` text | **Unstable wording** | Human-readable wording may be clarified across releases. Match on `Error::code()` (`ErrorCode`) or `Error::kind()` (`ErrorKind`), and inspect `Error::limit()` / `Error::location()` programmatically. |
| `MaskedError` `Display` and `Debug` guarantee | **Stable contract** | Never includes YAML document text or underlying `source()` strings; `Display` shows kind, code, and location (when present), and `source()` always returns `None`. |
| Internal `Mapping` backing store (`indexmap`) | **Private** | No `indexmap` type appears in `ayaml`'s public API (enforced by tests), so dependency updates do not break downstream callers. |
| Workspace helper crates (`ayaml-conformance`, `ayaml-bench`, `fuzz`, `tools/`) | **Not published** | Internal testing, benchmarking, fuzzing, and release tooling; not part of the published crate API. |

## Writing forward-compatible code

Because `ErrorKind`, `ErrorCode`, `LimitKind`, `ParseOptions`, `EmitOptions`, and `Limits`
are `#[non_exhaustive]`:

- construct options and limits via their `new()` / preset constructors and `with_*` builders
  rather than struct literals; and
- include a wildcard `_` arm when matching on `ErrorCode`, `ErrorKind`, or `LimitKind`.

```rust
use ayaml::{ErrorCode, ErrorKind, Limits, ParseOptions, parse_value_with};

const OPTIONS: ParseOptions =
    ParseOptions::new().with_limits(Limits::new().with_max_documents(1));

let err = parse_value_with("a: 1\n---\nb: 2\n", &OPTIONS).unwrap_err();

let handled = match err.code() {
    ErrorCode::TrailingDocuments | ErrorCode::DocumentCountLimit => "too many documents",
    _ if err.kind() == ErrorKind::Limit => "resource limit exceeded",
    _ => "other yaml error",
};
assert_eq!(handled, "too many documents");
assert_eq!(err.code().as_str(), "yaml.document-count-limit");
```

## Dependency and unsafe-code stability

- Runtime dependencies of `ayaml` are restricted to `serde` and `indexmap`, enforced by
  `tools/check_deps.py` and `cargo-deny` in CI.
- Published workspace crates carry `#![forbid(unsafe_code)]`, verified by `cargo-geiger` in
  CI. See [Dependency policy](dependencies.md).
