# API reference and caveats

Complete Rustdoc signatures and trait implementations are published on docs.rs:

- Core crate: [docs.rs/ayaml](https://docs.rs/ayaml)
- Surgical editor crate: [docs.rs/ayaml-edit](https://docs.rs/ayaml-edit)

This page maps every public surface, feature flag, and important caveat in one
place so you can pick the right API quickly.

## Choosing the right entry point

| Task | Recommended API | Key behavior |
| --- | --- | --- |
| Decode one document into a Rust struct | `from_str`, `from_str_with`, `from_slice_with` | Streams directly from events above 4 KiB; rejects trailing documents and duplicate keys. |
| Zero-copy `&str` struct decode | `from_str_borrowed`, `from_slice_borrowed`, `from_str_with` | Borrows unescaped scalars straight from the input slice. |
| Bounded I/O stream decode | `from_reader`, `from_reader_with` | Stops reading after `max_input_bytes + 1` bytes before UTF-8 validation. |
| Decode with field-level byte spans | `Spanned<T>` with `from_str*`, `DocumentDeserializer`, `Deserializer`, or `parse_frontmatter*` | Attaches exact byte `Span` to each wrapped field; refuses untrusted deserializers like `from_value`. |
| Multi-document typed stream | `Deserializer::from_str_with` + `next` / `next_borrowed` / `next_seed` | Lazy one-document-at-a-time streaming with shared `AliasBudget`. |
| Serde middleware integration | `DocumentDeserializer::from_str_with` | Implements `serde::Deserializer<'de>` for wrappers like `serde_path_to_error` or `serde_ignored`. |
| Generic dynamic DOM | `parse_value`, `parse_value_with`, `parse_documents`, `parse_documents_with` | Insertion-ordered `Mapping`, exact `Number`, `Value::Tagged`. |
| Pull-based spanned event stream | `parse_events`, `parse_events_with`, `EventStream` | Preserves raw duplicate mapping pairs, anchors, aliases, and spans. |
| Path-based span lookup | `SpannedTree::parse`, `SpannedTree::parse_with`, `LineIndex` | Look up value and entry byte spans by `PathSegment`; convert to UTF-8/scalar/UTF-16 `Position` and `Location`. |
| Markdown / static-site frontmatter | `split_frontmatter`, `parse_frontmatter`, `parse_frontmatter_with` | Bounded closing delimiter scan; preserves original file byte offsets for spans and body. |
| Deterministic YAML emission | `to_string`, `to_string_with`, `to_writer_with`, `to_string_value_with`, `to_string_documents_with`, `to_string_events_with`, `Emitter` | YAML 1.1-safe quoting, `EmitOptions` (`indent`, `flow`, `EmitterStyles`, `Limits`). |
| Comment-preserving edits | `ayaml_edit::replace_scalar`, `remove_entry`, `apply_batch` | Applies verified byte edits while preserving comments and formatting. |

## Feature flags

| Crate | Feature | Default | Stability | Purpose |
| --- | --- | --- | --- | --- |
| `ayaml` | `libyaml-compat` | off | Stable | Enables additive `ayaml::compat` module and `SpannedTree::parse_compat*`; never alters root `ayaml::*` APIs. |
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
- **No `from_value` for `Spanned<T>`:** `Spanned<T>` requires a source-backed
  `ayaml` deserializer so byte spans are guaranteed to lie within the source
  input. Passing `Value` through `from_value::<Spanned<T>>` returns an error
  rather than forging synthetic `0..0` spans.
- **Merge keys and `SpannedTree::entry_span`:** Enabling
  `ParseOptions::with_merge_keys(true)` on `SpannedTree::parse_with` suppresses
  `entry_span()` across the tree so deleting a merged entry cannot mutate the
  referenced anchor definition. `ayaml-edit` similarly refuses `merge_keys`.
- **Deprecated combination helpers:** Pre-1.0 `*_with_limits`, `*_with_options`,
  and `*_with_styles` functions are deprecated in favor of `*_with(..., &ParseOptions)`
  and `*_with(..., &EmitOptions)` (see [Stability and versioning](stability.md)).
  `Value::apply_merge_keys_with_limits` remains stable in `1.0`.
- **Error text on untrusted inputs:** Always log `err.masked()` rather than
  `err.to_string()` when YAML inputs may contain secrets or PII (see
  [Security and untrusted input](security.md) and [Error codes](error-codes.md)).
