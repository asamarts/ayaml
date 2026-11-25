# Sizing resource limits

`ayaml` applies resource limits on every parse and emit call so untrusted YAML
cannot exhaust memory or stack space. The defaults are chosen for general
configuration files and hostile inputs; this guide explains how to size
`Limits` for your workload and diagnose limit errors.

See [Resource limits](limits.md) for the complete reference of every ceiling,
accounting rule, and memory bound.

## Quick profiles by workload

| Workload | Starting point | Key knobs to adjust |
| --- | --- | --- |
| Public API / untrusted webhook | `Limits::new()` tightened | Lower `max_input_bytes`, `max_depth`, `max_events`, `max_alias_expansion_bytes`; use `from_reader_with`. |
| Trusted application config | `Limits::default()` | Usually none; enable `with_merge_keys(true)` only if configs use `<<:`. |
| Large trusted schema (OpenAPI, CRDs, lockfiles) | `ParseOptions::for_max_input()` or selective bumps | Raise `max_events`, `max_collection_entries`, `max_anchors`; raise `max_input_bytes` if > 32 MiB. |
| Multi-document manifest stream | `Limits::default()` + `Deserializer` | Set `max_documents` to stream size; stream lazily instead of `parse_documents`. |
| Alias- or merge-heavy templates | `Limits::default()` | Tune `max_alias_dereferences`, `max_alias_expansion_bytes`, `alias_bytes_per_input_byte`. |

## 1. Untrusted web or API payloads

For network-facing endpoints, tighten limits to match your maximum accepted
request body and nesting depth, and always read through `from_reader_with` (or
`from_slice_with`) so oversized bodies fail before UTF-8 validation or
scanning:

```rust
use std::io::Cursor;
use ayaml::{Limits, ParseOptions, from_reader_with};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Webhook {
    event: String,
}

const UNTRUSTED_OPTIONS: ParseOptions = ParseOptions::new().with_limits(
    Limits::new()
        .with_max_input_bytes(256 * 1024)
        .with_max_depth(32)
        .with_max_documents(1)
        .with_max_events(20_000)
        .with_max_collection_entries(4_096)
        .with_max_anchors(256)
        .with_max_alias_dereferences(256)
        .with_max_alias_expansion_bytes(1024 * 1024),
);

# fn main() -> Result<(), ayaml::Error> {
let body = Cursor::new(b"event: deploy.succeeded\n");
let hook: Webhook = from_reader_with(body, &UNTRUSTED_OPTIONS)?;
assert_eq!(hook.event, "deploy.succeeded");
# Ok(())
# }
```

## 2. Large trusted schemas and dense documents

The default count limits (`max_events = 1_000_000`,
`max_collection_entries = 1_000_000`, `max_anchors = 100_000`,
`max_alias_dereferences = 100_000`) intentionally cap peak token memory for
untrusted input. A dense, trusted 10 to 32 MiB document (for example a large
OpenAPI specification or generated catalog) can exceed `max_events` even when
it is well under `max_input_bytes`.

For trusted inputs where you want the four count limits lifted while keeping
input size, nesting depth, scalar size, tag expansion, document count, and
alias expansion bounded, start from `ParseOptions::for_max_input()`:

```rust
use ayaml::{Limits, ParseOptions, parse_value_with};

# fn main() -> Result<(), ayaml::Error> {
// Trusted large-schema profile: count limits lifted, input/depth/alias budgets active.
let trusted = ParseOptions::for_max_input().with_limits(
    Limits::for_max_input().with_max_input_bytes(64 * 1024 * 1024),
);
let value = parse_value_with("openapi: 3.1.0\n", &trusted)?;
assert_eq!(value["openapi"].as_str(), Some("3.1.0"));
# Ok(())
# }
```

If you prefer a finite ceiling instead of `usize::MAX`, estimate
`max_events` at roughly `1` token per 3 to 5 bytes of dense YAML (each token is
32 bytes on 64-bit targets, so 2,000,000 tokens is roughly 64 MiB of token
arena memory).

## 3. Alias expansion and opt-in merge keys

Every alias dereference (including `<<: *anchor` merge-key expansion when
`ParseOptions::with_merge_keys(true)` is enabled) charges the shared alias
budget:

```text
budget = max_alias_expansion_bytes
       + alias_bytes_per_input_byte * min(input_bytes, max_input_bytes)
```

Each copied node costs `node_cost_bytes` (default `256`) plus its copied text
and non-core tag bytes. Guidelines:

- Keep `node_cost_bytes` at its default (`256`) unless you have measured real
  allocation ratios; lowering it weakens memory protection against empty-node
  alias bombs (`0` is rejected as invalid).
- For CI or container configs with many legitimate `<<:` template merges,
  raise `max_alias_dereferences` and `max_alias_expansion_bytes` modestly
  rather than disabling bounds.
- Keep `max_depth` at or below `MAX_DEPTH_CAP` (`128`); higher values are
  rejected during limit validation before parsing begins.

## 4. Reporting limit diagnostics

When a limit fires, inspect `Error::limit()` to tell operators which ceiling
was hit and by how much without exposing document contents:

```rust
use ayaml::{ErrorKind, LimitKind, Limits, ParseOptions, parse_value_with};

# fn main() {
let opts = ParseOptions::new().with_limits(Limits::new().with_max_depth(2));
let err = parse_value_with("a: { b: { c: 1 } }\n", &opts).unwrap_err();

assert_eq!(err.kind(), ErrorKind::Limit);
let limit = err.limit().expect("structured limit details");
assert_eq!(limit.kind(), LimitKind::Depth);
assert_eq!(limit.limit(), 2);
assert_eq!(limit.observed(), 3);

// Safe to log even for untrusted input:
assert!(err.masked().to_string().contains("yaml.depth-limit"));
# }
```
