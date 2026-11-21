# Security and untrusted input

`ayaml` is built to parse untrusted YAML input safely, with explicit resource limits and
masked error formatting for secret-bearing documents. This page documents what `ayaml` does
and does not do, how its limits protect callers, and what text error values may contain.

## What `ayaml` never does

Parsing, event streaming, value composition, typed Serde decoding, `SpannedTree` queries,
and YAML emission:

- never read files, environment variables, the working directory, or the network;
- never execute code, evaluate shell or template commands, or instantiate arbitrary types
  from YAML tags;
- never apply YAML 1.1 merge keys (`<<`) implicitly during parsing (`Value::apply_merge_keys`
  is explicit);
- never use `unsafe` code in published crates (`#![forbid(unsafe_code)]`, checked in CI by
  `cargo-geiger`);
- never link against a native C library or third-party YAML parser; and
- never store configuration in process-global mutable state.

## Untrusted input and resource limits

Hostile YAML can try to exhaust stack space, heap memory, or CPU time through deep nesting,
quadratic scanning or hashing, tag-handle expansion, or exponential alias expansion
("billion laughs"). `ayaml` defends against each of these by default:

- **UTF-8 validation.** Byte-slice entry points (`from_slice`, `from_slice_with`,
  `from_slice_borrowed`) reject invalid UTF-8 with `ErrorCode::InvalidUtf8` rather than
  replacing bytes.
- **No input-controlled recursion.** The scanner, parser, composer, typed builder, and
  emitters use explicit heap stacks where input controls nesting, so hostile nesting cannot
  overflow the native call stack. `Limits::max_depth` defaults to 128, which is also the hard
  ceiling (`MAX_DEPTH_CAP`); higher configured values are rejected with
  `ErrorCode::InvalidLimits`.
- **Bounded input, tokens, events, and collections.** `Limits::default()` caps input size at
  32 MiB, individual scalars at 32 MiB, events and retained tokens at 1,000,000, single
  collections at 1,000,000 entries, documents per stream at 10,000, anchors at 100,000,
  alias dereferences at 100,000, tag-handle expansion at 64 MiB, and emitted output at 64 MiB.
- **Pre-allocation alias budgets.** Alias expansion is charged before copying nodes, using
  the byte-weighted budget `max_alias_expansion_bytes + alias_bytes_per_input_byte *
  min(input_bytes, max_input_bytes)` with a minimum `node_cost_bytes` of 1 (default 256).
  Setting `node_cost_bytes` to 0 is rejected with `ErrorCode::InvalidLimits`.
- **Linear-time scanning and hashing.** Scanning, tag-directive lookup, and nested mapping
  key hashing stay linear in input size.
- **Tightening limits for untrusted endpoints.** Default limits are sized for general-purpose
  documents up to 32 MiB. For network APIs, webhooks, or multi-tenant services, construct a
  tighter `ParseOptions` value and pass it to the `*_with` entry points:

```rust
use ayaml::{ErrorCode, ErrorKind, LimitKind, Limits, ParseOptions, parse_value_with};

const UNTRUSTED: ParseOptions = ParseOptions::new().with_limits(
    Limits::new()
        .with_max_input_bytes(256 * 1024)
        .with_max_depth(32)
        .with_max_collection_entries(10_000)
        .with_max_events(50_000)
        .with_max_alias_expansion_bytes(1024 * 1024),
);

let err = parse_value_with(&format!("{}{}", "[".repeat(40), "]".repeat(40)), &UNTRUSTED)
    .unwrap_err();
assert_eq!(err.kind(), ErrorKind::Limit);
assert_eq!(err.code(), ErrorCode::DepthLimit);
assert_eq!(err.limit().map(|l| (l.kind(), l.limit())), Some((LimitKind::Depth, 32)));
```

### Do not relax count limits for untrusted input

`ParseOptions::for_max_input()` (and `Limits::for_max_input()`) raises `max_events`,
`max_collection_entries`, `max_anchors`, and `max_alias_dereferences` to `usize::MAX` so
valid dense documents up to 32 MiB are accepted. That allows token storage alone to reach
roughly a gigabyte for a 32 MiB input. Keep `Limits::default()` or tighter limits whenever
the caller does not trust the input.

### Streaming typed reads and side-effecting seeds

Single-document typed readers (`from_str`, `from_slice`, `from_str_seed`, and their `_with`
forms) stream inputs larger than 4 KiB directly from parser events without materializing an
intermediate document tree. If a late syntax or duplicate-key fault occurs, the reader drops
the first pass and rebuilds the document to report the earliest document error. A custom
`DeserializeSeed` that mutates external state while reading will have observed the prefix
before the error was returned; treat partial state from a failed decode as untrusted.

See [Resource limits](limits.md) for the complete table of limits, memory formulas, and
per-reader accounting rules.

### Testing and fuzzing

- `crates/ayaml/tests/adversarial.rs` runs hostile inputs (alias bombs, deep nesting,
  quadratic-time probes, zero node cost, and memory bounds) in isolated child processes with
  time and address-space ceilings.
- Continuous fuzzing covers `parse`, `emit`, and `borrowed` targets. A bounded fuzz run is
  evidence rather than a proof of absence of bugs.

## What errors may contain

Normal formatting on `ayaml::Error` — `Display`, `Debug`, `Error::message()`, and
`std::error::Error::source()` — is written for developer diagnostics and **may include text
copied from the YAML document or from an underlying sink**:

- **Scalar values:** Serde type and value errors (such as `invalid type: string "..."`),
  radix integers rejected as `f64`, and core-schema tag mismatches in the emitter.
- **Mapping keys and typed paths:** duplicate mapping key errors, Serde unknown-field
  errors, and typed error paths (such as `credentials.api_key: ...` or `["secret key"]: ...`).
- **Enum variants and custom messages:** Serde unknown-variant errors, enum variant path
  prefixes, and messages returned by custom `Deserialize` or `Serialize` implementations.
- **Tags, directives, anchors, and aliases:** `%TAG` handles and prefixes, undefined tag
  handles, non-core tag names refused by typed readers, unwritable tags or directives, and
  anchor or alias names in event emission.
- **Characters and escape sequences:** unexpected block-scalar header characters, reserved
  indicators, unknown double-quoted escapes, and non-printable Unicode code points.
- **Underlying source errors:** `Error::source()` may hold a `std::io::Error` (which can
  include file paths or sink details) or a `std::str::Utf8Error`.

### Use `Error::masked()` for logs, telemetry, and external responses

Whenever YAML input may contain credentials, tokens, private keys, or personal data — or is
supplied by an untrusted party — format `error.masked()` (`MaskedError`) instead of `error`:

```rust
use std::collections::HashMap;
use std::error::Error as _;

let err = ayaml::from_str::<HashMap<String, u32>>("db_password: s3cr3t-value\n").unwrap_err();

// Normal Display and Debug include document text:
assert!(err.to_string().contains("db_password"));
assert!(err.to_string().contains("s3cr3t-value"));

// Masked Display and Debug omit all document text and source errors:
let masked = err.masked();
assert_eq!(masked.to_string(), "serde error (yaml.invalid-type) at line 1 column 14");
assert!(!format!("{masked:?}").contains("db_password"));
assert!(!format!("{masked:?}").contains("s3cr3t-value"));
assert!(masked.source().is_none());
```

`MaskedError` exposes only:

- in `Display`: `ErrorKind`, `ErrorCode` (`yaml.<kebab-name>`), and one-based `Location`
  (when present);
- in `Debug`: `kind`, `code`, `span`, `location`, and numeric `LimitExceeded` metadata;
- via `std::error::Error::source()`: always `None`, preventing error-chain loggers from
  printing an unmasked underlying cause.

### Other types that contain document text

`Debug` and `Display` output on `Value`, `Mapping`, `Entry`, `TaggedValue`, `Event`,
`SpannedTree`, and `SpannedNode` naturally contains document keys, values, tags, anchors,
and raw source slices. Do not log their formatted representations when documents contain
secrets.

## Reporting a vulnerability

Report security vulnerabilities privately through
[GitHub's private advisory form](https://github.com/asamarts/ayaml/security/advisories/new)
rather than a public issue or pull request. See
[`SECURITY.md`](https://github.com/asamarts/ayaml/blob/main/SECURITY.md) for the supported
release line and scope.
