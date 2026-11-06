# Resource limits

YAML input can be attacker-controlled. ayaml therefore makes resource limits
part of the parser API rather than an optional wrapper.

## Defaults

`Limits::default()` uses the compatibility profile except that a document
stream may contain 10,000 documents. `Limits::compat()` uses a one-document
stream.

| Limit | Default | Notes |
| --- | ---: | --- |
| `max_input_bytes` | 32 MiB | Checked before scanning. |
| `max_depth` | 128 | Also the accepted maximum (`MAX_DEPTH_CAP`). |
| `max_alias_expansion_bytes` | 256 MiB | Fixed alias allowance. |
| `alias_bytes_per_input_byte` | 16 | Additional allowance per input byte. |
| `node_cost_bytes` | 256 | Estimated cost of one composed node. |
| `max_documents` | 10,000 | `Limits::compat()` uses 1. |
| `max_scalar_bytes` | 32 MiB | Maximum bytes in one scalar. |
| `max_collection_entries` | 1,000,000 | Maximum entries in one collection. |
| `max_events` | 1,000,000 | Maximum events or tokens in one stream. |
| `max_anchors` | 100,000 | Maximum anchors in one document. |
| `max_alias_dereferences` | 100,000 | Maximum alias dereferences in one stream. |
| `max_output_bytes` | 64 MiB | Maximum bytes written by one emitter call. |
| `max_tag_expansion_bytes` | 64 MiB | Maximum bytes that tag handles expand to in one stream. |

Limits are inclusive at the accepted boundary. Input size, depth, and alias
expansion have tests just below, at, and above their ceilings. AY-015 adds the same
for the other limits.

The alias-expansion allowance is:

```text
max_alias_expansion_bytes
  + alias_bytes_per_input_byte * min(input_bytes, max_input_bytes)
```

The input contribution is capped at `max_input_bytes`, so padding a document
cannot multiply the alias budget without bound.

Every tag written with a handle holds its own copy of the prefix that handle stands for, so
`max_tag_expansion_bytes` counts the whole prefix at each use, in `%TAG` handles and the built-in
`!` and `!!` alike. A tag written verbatim costs what its text costs, so it doesn't count. An alias
copies the tags of the node it names, and those bytes count against the alias budget with the rest
of the copy.

## Single document versus stream

Single-document entry points such as `parse_value` and `from_str` reject a
second document even when `max_documents` would allow one. Use
`parse_documents` or the `Deserializer` for streams.

## Configuring limits

`Limits` is `#[non_exhaustive]`. Start with `Limits::default()` or
`Limits::compat()` and use the `with_*` methods:

```rust
use ayaml::{parse_value_with_limits, Limits};

# fn main() -> Result<(), ayaml::Error> {
let limits = Limits::default()
    .with_max_input_bytes(1024 * 1024)
    .with_max_depth(64)
    .with_max_documents(100);
let value = parse_value_with_limits("a: [1, 2, 3]\n", &limits)?;
assert_eq!(value["a"][2].as_u64(), Some(3));
# Ok(())
# }
```

The public fields remain available for code that needs to adjust a limit after
construction.

## Errors

A limit failure returns `ErrorKind::Limit`. `Error::limit()` returns the
structured `LimitExceeded` value with:

- the `LimitKind`;
- the configured ceiling; and
- the observed or attempted value.

There are three exceptions. A configured `max_depth` above `MAX_DEPTH_CAP` is
`ErrorKind::Limit` with no structured value or span, because no input went over a
limit. In compat mode, the readers that build values report the way a libyaml-based
reader does. A depth failure is `ErrorKind::Serde`, with the message `recursion limit
exceeded` and no structured value or span. Going over `max_documents` is
`ErrorKind::Parser`, with the message `more than one document in stream` and no
structured value or span. `compat::parse_events_with_limits` doesn't map these: it
reports `ErrorKind::Limit` with the value and the span.

Parser, scanner, composer, typed-builder, document-count, and alias-budget
failures carry a source span when input provides one. Output-emitter limits do
not have a source span because they describe generated output rather than
input.
