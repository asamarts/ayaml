# API reference and caveats

Complete Rustdoc signatures and trait implementations are published on docs.rs:

- Core crate: [docs.rs/ayaml](https://docs.rs/ayaml)
- Source-preserving editor crate: [docs.rs/ayaml-edit](https://docs.rs/ayaml-edit)

Use the tables below to choose an entry point, check feature flags, and review
key caveats across `ayaml` and `ayaml-edit`.

## Choosing the right entry point

| Task | Recommended API | Key behavior |
| --- | --- | --- |
| Decode one document into a Rust struct | `from_str`, `from_str_with`, `from_slice`, `from_slice_with`, `from_str_seed`, `from_str_seed_with` | Streams directly from events above 4 KiB; rejects trailing documents and duplicate keys. |
| Zero-copy `&str` struct decode | `from_str_borrowed`, `from_slice_borrowed`, `from_str_with`, `from_slice_with` | Borrows unescaped scalars straight from the input slice; use `Cow<'a, str>` or `String` when escapes or folding may occur. |
| Bounded I/O stream decode | `from_reader`, `from_reader_with` | Stops reading after `max_input_bytes + 1` bytes before UTF-8 validation. |
| In-memory `Value` <-> Serde conversion | `from_value`, `from_value_seed`, `to_value` | Converts between `ayaml::Value` and typed Rust structs without re-parsing text (`from_value` rejects `Spanned<T>` and treats `Value::Null` as explicit null). |
| Decode with field-level byte spans | `Spanned<T>` with `from_str*`, `DocumentDeserializer`, `Deserializer`, or `parse_frontmatter*` | Attaches exact byte `Span` to each wrapped field; refuses untrusted or buffered deserializers. |
| Multi-document typed stream | `Deserializer::from_str`, `Deserializer::from_str_with` + `next` / `next_borrowed` / `next_seed` / `next_seed_borrowed` | Lazy one-document-at-a-time streaming with a shared alias expansion budget across documents. |
| Serde middleware integration | `DocumentDeserializer::from_str`, `from_str_with`, `from_str_compat`, `from_str_compat_with` | Implements `serde::Deserializer<'de>` by value and `&mut` for wrappers like `serde_path_to_error` or `serde_ignored`. |
| Generic dynamic DOM | `parse_value`, `parse_value_with`, `parse_documents`, `parse_documents_with` | Insertion-ordered `Mapping`, exact `i64`/`u64`/`i128`/`u128` + IEEE-754 `f64` `Number`, `Value::Tagged`. |
| Pull-based spanned event stream | `parse_events`, `parse_events_with`, `EventStream` | Preserves raw duplicate mapping pairs, anchors, aliases, and spans. |
| Path-based span lookup | `SpannedTree::parse`, `SpannedTree::parse_with`, `LineIndex` | Look up value (`span_at`, `lookup`) and entry (`entry_span_at`) byte spans by `PathSegment`; convert to UTF-8/scalar/UTF-16 `Position` and `Location`. |
| Markdown / static-site frontmatter | `split_frontmatter`, `split_frontmatter_with`, `parse_frontmatter`, `parse_frontmatter_with`, `parse_frontmatter_value`, `parse_frontmatter_value_with` | Bounded closing delimiter scan; preserves original file byte offsets for spans and body. |
| Deterministic YAML emission | `to_string`, `to_string_with`, `to_writer`, `to_writer_with`, `to_string_value_with`, `to_string_documents_with`, `to_string_events_with`, `Emitter` | YAML 1.1-safe leading-zero numeral quoting (`0644`), `EmitOptions` (`indent`, `flow`, `EmitterStyles`, `Limits`). |
| Comment-preserving edits | `ayaml_edit::replace_scalar`, `replace_scalar_with`, `remove_entry`, `remove_entry_with`, `apply_batch`, `apply_batch_with`, `apply_edits` | Applies verified byte edits while preserving comments and formatting. |

## Feature flags

| Crate | Feature | Default | Stability | Purpose |
| --- | --- | --- | --- | --- |
| `ayaml` | `libyaml-compat` | off | Stable | Enables additive `ayaml::compat` module, `DocumentDeserializer::from_str_compat*`, and `SpannedTree::parse_compat*`; never alters root `ayaml::*` APIs. |
| `ayaml` | `bench-internals` | off | Unstable (outside SemVer) | Internal harness hooks for workspace benchmarks and conformance tests. |

## Options and builder summary

```rust
use ayaml::{EmitOptions, Limits, ParseOptions, from_str_with, to_string_with};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Deserialize, Serialize)]
struct Job {
    name: String,
    retries: u8,
}

# fn main() -> Result<(), ayaml::Error> {
const PARSE: ParseOptions = ParseOptions::new()
    .with_merge_keys(true)
    .with_limits(Limits::new().with_max_depth(32).with_max_input_bytes(64 * 1024));

const EMIT: EmitOptions = EmitOptions::new()
    .with_indent(2)
    .with_limits(Limits::new().with_max_output_bytes(64 * 1024));

let job: Job = from_str_with("base: &b\n  retries: 2\nname: test\n<<: *b\n", &PARSE)?;
let rendered = to_string_with(&job, &EMIT)?;
assert_eq!(from_str_with::<Job>(&rendered, &PARSE)?, job);
# Ok(())
# }
```

## Important caveats

- **UTF-8 only:** `ayaml` accepts valid UTF-8 (`&str` or UTF-8 `&[u8]` / `Read`).
  UTF-16 and UTF-32 inputs must be transcoded before parsing.
- **`Spanned<T>` deserializer and alias semantics:** `Spanned<T>` requires a
  direct source-backed `ayaml` deserializer so byte spans are guaranteed to lie
  within the source input. It is refused by `from_value`, foreign Serde
  deserializers (such as `serde_json`), and buffered Serde attributes
  (`#[serde(flatten)]`, `#[serde(untagged)]`, `#[serde(tag = "...")]`). When
  applied to an alias node (`copy: *anchor`), `Spanned<T>` reports the alias
  use-site span (`*anchor`), while nested `Spanned<U>` fields inside the target
  struct report their definition spans.
- **Merge keys and `SpannedTree::entry_span`:** Enabling
  `ParseOptions::with_merge_keys(true)` on `SpannedTree::parse_with` suppresses
  `entry_span()` across the tree so deleting a merged entry cannot mutate the
  referenced anchor definition. `ayaml-edit` similarly refuses `merge_keys`.
- **Deprecated `0.x` items:** Pre-1.0 `*_with_limits`, `*_with_options`,
  `*_with_styles` functions and the legacy `Marker` type are deprecated in favor
  of `*_with(..., &ParseOptions)`, `*_with(..., &EmitOptions)`, and byte `Span`
  with `LineIndex` (see [Stability and versioning](stability.md)).
  `Value::apply_merge_keys_with_limits` remains stable in `1.0`.
- **Error text on untrusted inputs:** Always log `err.masked()` rather than
  `err.to_string()` when YAML inputs may contain secrets or PII (see
  [Security and untrusted input](security.md) and [Error codes](error-codes.md)).
