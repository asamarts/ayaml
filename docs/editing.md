# Surgical editing with `ayaml-edit`

When updating an existing YAML configuration file (such as a CI workflow,
manifest, or lockfile), deserializing and re-emitting the entire document strips
comments, blank lines, quoting style, and key alignment.

The companion crate `ayaml-edit` (`#![forbid(unsafe_code)]`) applies verified,
byte-range edits using `ayaml::SpannedTree`, preserving surrounding comments,
BOMs, CRLF/LF line endings, and formatting. Every edit re-parses the resulting
document and verifies both the expected `Value` tree and the canonical event
stream before returning `Ok(VerifiedEdit)`.

Full Rustdoc reference: [docs.rs/ayaml-edit](https://docs.rs/ayaml-edit).

## Install

```toml
[dependencies]
ayaml = "0.2.3"
ayaml-edit = "0.2.3"
```

## Replacing a scalar value

`replace_scalar` (and `replace_scalar_with` for custom `EditOptions`) locates
the target node by `PathSegment`s, quotes strings automatically when plain
syntax would be ambiguous or flow-unsafe, and verifies that no unrelated node
changed:

```rust
use ayaml::{PathSegment, Value};
use ayaml_edit::replace_scalar;

# fn main() -> Result<(), ayaml_edit::EditError> {
let src = "# Service settings\nservice:\n  host: localhost # local bind\n  port: 8080\n";
let path = [PathSegment::key("service"), PathSegment::key("port")];
let verified = replace_scalar(src, &path, &Value::from(9090_u64))?;

assert_eq!(
    verified.output(),
    "# Service settings\nservice:\n  host: localhost # local bind\n  port: 9090\n"
);
assert_eq!(verified.edits().len(), 1);
# Ok(())
# }
```

## Removing a mapping entry or sequence item

`remove_entry` (and `remove_entry_with`) removes the target entry's exact
`entry_span()` while keeping neighboring comments and structure intact:

```rust
use ayaml::PathSegment;
use ayaml_edit::remove_entry;

# fn main() -> Result<(), ayaml_edit::EditError> {
let src = "keep: 1\nobsolete: true\nalso_keep: 2\n";
let verified = remove_entry(src, &[PathSegment::key("obsolete")])?;

assert_eq!(verified.into_output(), "keep: 1\nalso_keep: 2\n");
# Ok(())
# }
```

## Applying multiple edits atomically (`apply_batch`)

Use `apply_batch` (or `apply_batch_with` with custom `EditOptions`) to apply
several non-overlapping `EditOp` operations in one verified pass. Lower-level
callers can also splice explicit byte ranges via `apply_edits` with `TextEdit`:

```rust
use ayaml::{PathSegment, Value};
use ayaml_edit::{EditOp, apply_batch};

# fn main() -> Result<(), ayaml_edit::EditError> {
let src = "service:\n  name: api\n  port: 8080\n  debug: true\n";
let ops = [
    EditOp::ReplaceScalar {
        path: vec![PathSegment::key("service"), PathSegment::key("port")],
        value: Value::from(8443_u64),
    },
    EditOp::RemoveEntry {
        path: vec![PathSegment::key("service"), PathSegment::key("debug")],
    },
];

let verified = apply_batch(src, &ops)?;
assert_eq!(verified.output(), "service:\n  name: api\n  port: 8443\n");
# Ok(())
# }
```

## Conservative refusal guarantees and error kinds

`ayaml-edit` refuses to guess whenever a byte edit could silently corrupt
document semantics or delete comments unexpectedly, returning an `EditError`
categorized by `EditErrorKind`:

- **`EditErrorKind::Parse`**: source YAML failed to parse under the configured
  `ParseOptions`.
- **`EditErrorKind::NotFound`**: target `PathSegment` path does not exist.
- **`EditErrorKind::InvalidEdit`**: invalid replacement value, empty path, or
  overlapping / out-of-bounds byte spans across batch `TextEdit` operations.
- **`EditErrorKind::VerificationFailed`**: the spliced document failed
  post-edit re-parse or semantic/event verification.
- **`EditErrorKind::Unsupported`**: document-wide or target-level constructs
  that cannot be edited conservatively:
  - Multi-document streams (`---` / multiple documents).
  - Documents containing anchors (`&a`), aliases (`*a`), explicit tags
    (`!tag`), or block scalars (`|` / `>`) anywhere in the document.
  - `ParseOptions` with `merge_keys` enabled (`with_merge_keys(true)`).
  - Non-scalar targets, unwritten empty scalars (`key:`), multiline
    plain/quoted scalars, replacement scalars that cannot format on one line,
    flow entries, removing the document root or the sole entry of a collection,
    or `#` comments immediately adjoining a scalar without whitespace.

For documents that require structural transformations across anchors, tags,
block scalars, or multi-document streams, parse with `ayaml::parse_documents`,
transform the `Value` tree, and re-emit with `ayaml::to_string_documents_with`.
