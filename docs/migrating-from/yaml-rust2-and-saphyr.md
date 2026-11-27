# Migrating from `yaml-rust2`, `saphyr`, and `serde-saphyr`

`yaml-rust2`, `saphyr`, and `serde-saphyr` are pure-Rust YAML 1.2 libraries.
`ayaml` brings event scanning, generic `Value` DOM, spanned trees, owned and
borrowed Serde decoding, deterministic emission, and source-preserving edits
under one parser. On geometric mean across 23 shared fixtures, `ayaml` runs
1.40x to 1.54x faster than `yaml-rust2` and `saphyr` (with roughly 3x fewer
allocations than `yaml-rust2`), and 3.2x faster than `serde-saphyr` across 22
shared fixtures (see [Benchmarks](https://asamarts.github.io/ayaml/benchmarks/)
and [Benchmark methodology](../benchmarks.md)).

## Updating `Cargo.toml`

```toml
[dependencies]
ayaml = "0.2.3"

# Optional: verified source-preserving edits
ayaml-edit = "0.2.3"
```

## Quick API mapping

| `yaml-rust2` / `saphyr` / `serde-saphyr` | `ayaml` | Notes |
| --- | --- | --- |
| `YamlLoader::load_from_str(s)` | `ayaml::parse_documents(s)` or `ayaml::parse_value(s)` | `parse_value` enforces single-document input (`TrailingDocuments` on extra docs). |
| `Yaml::Hash` / `Array` (`yaml-rust2`) or `Yaml::Mapping` / `Sequence` / `Value(Scalar)` (`saphyr`) | `Value::Mapping` / `Value::Sequence` / `Value::String` / `Value::Number` | `Number` stores exact `i64`/`u64`/`i128`/`u128` integers and IEEE-754 `f64` floats (unlike `yaml-rust2`'s `i64`-only `Integer` and string `Real`); also `Value::Tagged`, `Value::Bool`, `Value::Null`. |
| `doc["missing"].is_badvalue()` (`Yaml::BadValue`) | `doc.get("missing")` (`Option<&Value>`) | `ayaml`'s `doc["missing"]` returns `&Value::Null`, not a distinct `BadValue` variant. Use `.get(...)` to distinguish missing keys from explicit `null`. |
| `yaml_rust2::Parser` + `EventReceiver` (or `saphyr_parser::Parser` `Iterator`) | `ayaml::parse_events(s)` pull `Iterator` | Pull iterator yielding `Result<Event<'a>, Error>` with exact byte `Span`s. |
| `MarkedYaml` / `saphyr::LoadableYamlNode` | `ayaml::SpannedTree` + `SpannedNode` or `Spanned<T>` | Query single-document spans by `PathSegment` (`span_at`, `lookup`, `entry_span_at`) or per-document via `tree.documents()` / `tree.document(i)`, or decode typed `Spanned<T>` fields. |
| `YamlEmitter::new(&mut out).dump(&doc)` | `ayaml::to_string_value(&doc)` / `to_string_value_with(&doc, &EmitOptions)` | `YamlEmitter` writes a leading `---` by default; pass `EmitOptions::new().with_explicit_start(true)` in `ayaml` when you want `---`. |
| `serde_saphyr::from_str::<T>(s)` / `serde_saphyr::Spanned<T>` | `ayaml::from_str::<T>(s)` / `from_str_borrowed::<T>(s)` / `ayaml::Spanned<T>` | Direct event streaming at every input size; `ayaml::Spanned<T>` implements `Deref`/`DerefMut` and exposes byte `Span` (convert with `LineIndex`). `serde-saphyr` expands `<<:` merges by default; pass `ParseOptions::new().with_merge_keys(true)` in `ayaml`. |
| Manual string slicing for config updates | `ayaml_edit::replace_scalar`, `remove_entry`, `apply_batch` | Preserves comments, blank lines, and indentation; verifies canonical event diff (see [Source-preserving editing](../editing.md)). |

## 1. Replacing `YamlLoader` and `Yaml` indexing

`ayaml::parse_value` and `ayaml::parse_documents` return insertion-ordered
`Value` trees with indexing and typed accessors. Use `.get(...)` whenever you
previously checked `.is_badvalue()`:

```rust
use ayaml::{parse_documents, parse_value};

# fn main() -> Result<(), ayaml::Error> {
// Single document (rejects accidental extra `---` documents).
let doc = parse_value("server:\n  host: 127.0.0.1\n  ports: [8080, 8443]\n")?;
assert_eq!(doc["server"]["host"].as_str(), Some("127.0.0.1"));
assert_eq!(doc["server"]["ports"][1].as_u64(), Some(8443));

// Missing keys return `None` via `.get(...)`, whereas `doc["missing"]` returns `&Value::Null`.
assert!(doc.get("missing").is_none());
assert!(doc["missing"].is_null());

// Multi-document stream (equivalent to `YamlLoader::load_from_str`).
let docs = parse_documents("kind: ConfigMap\n---\nkind: Secret\n")?;
assert_eq!(docs.len(), 2);
assert_eq!(docs[0]["kind"].as_str(), Some("ConfigMap"));
assert_eq!(docs[1]["kind"].as_str(), Some("Secret"));
# Ok(())
# }
```

## 2. Replacing push `EventReceiver` with pull `parse_events`

Instead of implementing a stateful callback trait (`EventReceiver`), iterate
directly over `ayaml::parse_events` (or `parse_events_with` for custom
`ParseOptions`):

```rust
use ayaml::{Event, parse_events};

# fn main() -> Result<(), ayaml::Error> {
let input = "name: ayaml\nversion: 1\n";
let mut scalars = Vec::new();
for item in parse_events(input)? {
    if let Event::Scalar { value, span, .. } = item? {
        scalars.push((value.into_owned(), span));
    }
}
assert_eq!(scalars.len(), 4);
assert_eq!(scalars[0].0, "name");
assert_eq!(scalars[1].1.slice(input), Some("ayaml"));
# Ok(())
# }
```

## 3. Replacing `MarkedYaml` with `SpannedTree` and `Spanned<T>`

`ayaml` provides two span APIs:

- Use `SpannedTree::parse` for untyped path lookups (`span_at`, `lookup`,
  `entry_span_at`, plus `SpannedNode::entry_span`), and `LineIndex` to map byte
  offsets to zero-based `Position` (UTF-8, scalar, or UTF-16 columns) or
  one-based `Location`. On multi-document streams, top-level `span_at` /
  `lookup` / `entry_span_at` return `None`; iterate `tree.documents()` or index
  `tree.document(i)` instead.
- Use `Spanned<T>` inside Serde structs when you want typed deserialization and
  exact spans in one pass.

```rust
use ayaml::{ColumnEncoding, LineIndex, Location, PathSegment, SpannedTree};

# fn main() -> Result<(), ayaml::Error> {
let input = "database:\n  host: localhost\n  pool_size: 16\n";
let tree = SpannedTree::parse(input)?;
let path = [PathSegment::key("database"), PathSegment::key("pool_size")];
let span = tree.span_at(&path).expect("pool_size span");
assert_eq!(span.slice(input), Some("16"));
assert_eq!(
    tree.entry_span_at(&path).and_then(|s| s.slice(input)),
    Some("  pool_size: 16\n")
);

let index = LineIndex::new(input);
let pos = index
    .to_position(span.start(), ColumnEncoding::Utf16)
    .expect("utf-16 column");
let loc = Location::from(pos);
assert_eq!((loc.line(), loc.column()), (3, 14));
# Ok(())
# }
```

## 4. Replacing `YamlEmitter` and source-preserving edits

Use `ayaml::to_string_value_with` with `EmitOptions` for canonical re-emission
(add `.with_explicit_start(true)` to match `YamlEmitter`'s leading `---`), or
use the companion `ayaml-edit` crate (see [Source-preserving editing](../editing.md))
when updating existing configuration files while preserving comments and
formatting:

```rust
use ayaml::{EmitOptions, PathSegment, Value, parse_value, to_string_value_with};
use ayaml_edit::replace_scalar;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let val = parse_value("env: prod\nreplicas: 2\n")?;
let flow = to_string_value_with(&val, &EmitOptions::flow())?;
assert_eq!(flow, "{env: prod, replicas: 2}\n");

let config = "# Production replicas\nenv: prod\nreplicas: 2\n";
let edited = replace_scalar(config, &[PathSegment::key("replicas")], &Value::from(4_u64))?;
assert_eq!(edited.output(), "# Production replicas\nenv: prod\nreplicas: 4\n");
# Ok(())
# }
```

## 5. Behavioral differences

- **Missing-key indexing:** `doc["missing"]` returns `&Value::Null` (not
  `Yaml::BadValue`). Check `doc.get("missing")` wherever presence matters.
- **Duplicate keys:** `ayaml` rejects duplicate mapping keys during `Value`
  composition and typed Serde decoding (`ErrorCode::DuplicateKey`), while
  `parse_events` exposes every raw pair for linters.
- **Native limits:** All entry points enforce default ceilings (`Limits`). For
  dense documents within the 32 MiB default input limit (> 1M events or > 1M
  entries), use `ParseOptions::for_max_input()`. For trusted documents **larger
  than 32 MiB**, also raise `max_input_bytes` explicitly via
  `ParseOptions::new().with_limits(Limits::for_max_input().with_max_input_bytes(...))`
  (see [Sizing resource limits](../limits-sizing.md)).
- **Merge keys:** Unlike `serde-saphyr` (which expands `<<:` by default),
  `ayaml` keeps `<<` literal in YAML 1.2.2 mode; opt in explicitly via
  `ParseOptions::new().with_merge_keys(true)`.
