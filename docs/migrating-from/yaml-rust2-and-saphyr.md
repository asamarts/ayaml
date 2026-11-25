# Migrating from `yaml-rust2`, `saphyr`, and `serde-saphyr`

`yaml-rust2`, `saphyr`, and `serde-saphyr` are pure-Rust YAML 1.2 libraries.
Moving to `ayaml` consolidates event scanning, generic `Value` DOM, spanned
trees, owned/borrowed Serde decoding, deterministic emission, and
format-preserving edits into one unified engine — running 1.40x–1.54x faster
than `yaml-rust2`/`saphyr` (with ~3x fewer allocations than `yaml-rust2`) and
3.2x faster than `serde-saphyr` on our pinned benchmark suite.

## Quick API mapping

| `yaml-rust2` / `saphyr` / `serde-saphyr` | `ayaml` | Notes |
| --- | --- | --- |
| `YamlLoader::load_from_str(s)` | `ayaml::parse_documents(s)` or `ayaml::parse_value(s)` | `parse_value` enforces single-document input (`TrailingDocuments` on extra docs). |
| `Yaml::Hash` / `Yaml::Array` / `Yaml::String` | `Value::Mapping` / `Value::Sequence` / `Value::String` | Also `Value::Tagged(Box<TaggedValue>)`, `Value::Number(Number)`, `Value::Bool`, `Value::Null`. |
| `Parser::new_from_str` + `EventReceiver` push trait | `ayaml::parse_events(s)` pull `Iterator` | Pull iterator yielding `Result<Event<'a>, Error>` with exact byte `Span`s. |
| `MarkedYaml` / `saphyr::LoadableYamlNode` | `ayaml::SpannedTree` + `SpannedNode` or `Spanned<T>` | Query spans by `PathSegment` or decode directly into typed `Spanned<T>` fields. |
| `YamlEmitter::new(&mut out).dump(&doc)` | `ayaml::to_string_value(&doc)` / `to_string_value_with(&doc, &EmitOptions)` | Deterministic block/flow emission with per-node `EmitterStyles` and output byte caps. |
| `serde_saphyr::from_str::<T>(s)` | `ayaml::from_str::<T>(s)` / `from_str_borrowed::<T>(s)` | Direct event streaming above 4 KiB; zero-copy `&str` borrowing built in. |
| Manual string slicing for config updates | `ayaml_edit::replace_scalar`, `remove_entry`, `apply_batch` | Preserves comments, blank lines, and indentation; verifies canonical event diff. |

## 1. Replacing `YamlLoader` and `Yaml` indexing

`ayaml::parse_value` and `ayaml::parse_documents` return insertion-ordered
`Value` trees with ergonomic indexing and typed accessors:

```rust
use ayaml::{parse_documents, parse_value};

# fn main() -> Result<(), ayaml::Error> {
// Single document (rejects accidental extra `---` documents).
let doc = parse_value("server:\n  host: 127.0.0.1\n  ports: [8080, 8443]\n")?;
assert_eq!(doc["server"]["host"].as_str(), Some("127.0.0.1"));
assert_eq!(doc["server"]["ports"][1].as_u64(), Some(8443));

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

`ayaml` provides two complementary span APIs:

- Use `SpannedTree::parse` for untyped path lookups (`span_at`, `lookup`,
  `entry_span`), and `LineIndex` to map byte offsets to zero-based `Position`
  (UTF-8, scalar, or UTF-16 columns) or one-based `Location`.
- Use `Spanned<T>` inside Serde structs when you want typed deserialization and
  exact spans in one pass.

```rust
use ayaml::{ColumnEncoding, LineIndex, Location, PathSegment, SpannedTree};

# fn main() -> Result<(), ayaml::Error> {
let input = "database:\n  pool_size: 16\n";
let tree = SpannedTree::parse(input)?;
let span = tree
    .span_at(&[PathSegment::key("database"), PathSegment::key("pool_size")])
    .expect("pool_size span");
assert_eq!(span.slice(input), Some("16"));

let index = LineIndex::new(input);
let pos = index
    .to_position(span.start(), ColumnEncoding::Utf16)
    .expect("utf-16 column");
let loc = Location::from(pos);
assert_eq!((loc.line(), loc.column()), (2, 14));
# Ok(())
# }
```

## 4. Replacing `YamlEmitter` and format-preserving edits

Use `ayaml::to_string_value_with` with `EmitOptions` for canonical re-emission,
or use the companion `ayaml-edit` crate when updating existing configuration
files while preserving comments and formatting:

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

## 5. Key behavioral differences to keep in mind

- **Duplicate keys:** `ayaml` rejects duplicate mapping keys during `Value`
  composition and typed Serde decoding (`ErrorCode::DuplicateKey`), while
  `parse_events` exposes every raw pair for linters.
- **Native limits:** All entry points enforce default ceilings (`Limits`); for
  very large trusted schemas (> 1M events or > 32 MiB), start from
  `ParseOptions::for_max_input()` (see [Sizing resource limits](../limits-sizing.md)).
- **Merge keys:** Plain `<<:` merges are opt-in via
  `ParseOptions::new().with_merge_keys(true)` rather than silently rewriting
  YAML 1.2.2 mappings.
