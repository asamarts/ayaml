# Surgical editing with `ayaml-edit`

When updating an existing YAML configuration file (such as a CI workflow,
manifest, or lockfile), deserializing and re-emitting the entire document strips
comments, blank lines, quoting style, and key alignment.

The companion crate `ayaml-edit` (`#![forbid(unsafe_code)]`) applies verified,
byte-range edits using `ayaml::SpannedTree`, preserving surrounding comments,
BOMs, CRLF/LF line endings, and formatting. Every edit re-parses the resulting
document and compares canonical event streams before returning `Ok(VerifiedEdit)`.

Full Rustdoc reference: [docs.rs/ayaml-edit](https://docs.rs/ayaml-edit).

## Install

```toml
[dependencies]
ayaml = "0.2.3"
ayaml-edit = "0.2.3"
```

## Replacing a scalar value

`replace_scalar` locates the target node by `PathSegment`s, quotes strings
automatically when plain syntax would be ambiguous or flow-unsafe, and verifies
that no unrelated node changed:

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
# Ok(())
# }
```

## Removing a mapping entry or sequence item

`remove_entry` removes the target entry's exact `entry_span()` while keeping
neighboring comments and structure intact:

```rust
use ayaml::PathSegment;
use ayaml_edit::remove_entry;

# fn main() -> Result<(), ayaml_edit::EditError> {
let src = "keep: 1\nobsolete: true\nalso_keep: 2\n";
let verified = remove_entry(src, &[PathSegment::key("obsolete")])?;

assert_eq!(verified.output(), "keep: 1\nalso_keep: 2\n");
# Ok(())
# }
```

## Applying multiple edits atomically (`apply_batch`)

Use `apply_batch` (or `apply_batch_with` with custom `EditOptions`) to apply
several non-overlapping `EditOp` operations in one verified pass:

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

## Conservative refusal guarantees

`ayaml-edit` refuses to guess whenever a byte edit could silently corrupt
document semantics or delete comments unexpectedly, returning
`EditErrorKind::Unsupported`:

- Multi-document streams (`---` / multiple documents).
- Documents containing anchors (`&a`), aliases (`*a`), or explicit tags (`!tag`).
- `ParseOptions` with `merge_keys` enabled (`with_merge_keys(true)`).
- Block scalar replacements (`|` / `>`), complex non-scalar replacements, or
  ambiguous inline entries where removing an entry would strip comments or
  break compact `- key: val` structure.
- Overlapping byte spans across batch operations (`EditErrorKind::InvalidEdit`).

For documents that require structural transformations across anchors, tags, or
multi-document streams, parse with `ayaml::parse_documents`, transform the
`Value` tree, and re-emit with `ayaml::to_string_documents_with`.
