# Getting started

Parse YAML into `Value` trees or typed Serde structs, stream multiple documents,
inspect spans, emit YAML, and enforce resource limits with masked errors.

## Adding `ayaml`

```toml
[dependencies]
ayaml = "0.2.3"
serde = { version = "1", features = ["derive"] }
```

Enable the optional `libyaml-compat` feature only when you need `ayaml::compat`; see the
[compatibility profile](compatibility.md).

## Parsing values and document streams

Use `parse_value` for a single YAML document and `parse_documents` for a multi-document stream:

```rust
use ayaml::{parse_documents, parse_value};

# fn main() -> Result<(), ayaml::Error> {
let value = parse_value("host: localhost\nport: 5432\n")?;
assert_eq!(value["host"].as_str(), Some("localhost"));
assert_eq!(value["port"].as_u64(), Some(5432));

let docs = parse_documents("first: 1\n---\nsecond: 2\n")?;
assert_eq!(docs.len(), 2);
assert_eq!(docs[1]["second"].as_u64(), Some(2));
# Ok(())
# }
```

Single-document readers reject a second document with `ErrorCode::TrailingDocuments`.

## Typed Serde decoding

Use `from_str`, `from_slice`, `from_str_borrowed`, or `from_value` for a single document,
and `Deserializer` to read a multi-document stream lazily:

```rust
use serde::Deserialize;

#[derive(Debug, PartialEq, Deserialize)]
struct Entry<'a> {
    key: &'a str,
    count: u32,
}

# fn main() -> Result<(), ayaml::Error> {
let borrowed: Entry<'_> = ayaml::from_str_borrowed("key: alpha\ncount: 3\n")?;
assert_eq!(borrowed, Entry { key: "alpha", count: 3 });

let mut stream = ayaml::Deserializer::from_str("10\n---\n20\n")?;
assert_eq!(stream.next::<u32>()?, Some(10));
assert_eq!(stream.next::<u32>()?, Some(20));
assert_eq!(stream.next::<u32>()?, None);
# Ok(())
# }
```

## Spanned events, positions, and `SpannedTree`

When source locations matter (for policy checks, diagnostics, or locating keys and values),
use `parse_events`, `LineIndex`, and `SpannedTree`:

```rust
use ayaml::{ColumnEncoding, Event, LineIndex, Location, PathSegment, SpannedTree, parse_events};

# fn main() -> Result<(), ayaml::Error> {
let input = "service:\n  port: 8080\n";
let events: Vec<Event<'_>> = parse_events(input)?.collect::<Result<_, _>>()?;
assert!(!events.is_empty());

let tree = SpannedTree::parse(input)?;
let port_span = tree
    .span_at(&[PathSegment::key("service"), PathSegment::key("port")])
    .expect("port span");
assert_eq!(&input[port_span.range()], "8080");

let index = LineIndex::new(input);
let pos = index.to_position(port_span.start(), ColumnEncoding::Scalar).expect("position");
let loc = Location::from(pos);
assert_eq!((loc.line(), loc.column()), (2, 9));
# Ok(())
# }
```

## Emitting YAML

Use `to_string`, `to_string_value`, `to_writer`, or their `_with` forms taking `&EmitOptions`:

```rust
use ayaml::{EmitOptions, parse_value, to_string_value_with};

# fn main() -> Result<(), ayaml::Error> {
let value = parse_value("service:\n  port: 8080\n")?;

let flow = to_string_value_with(&value, &EmitOptions::flow())?;
assert_eq!(flow, "{service: {port: 8080}}\n");

let mut buf = Vec::new();
ayaml::to_writer_with(&mut buf, &value, &EmitOptions::new().with_indent(4))?;
assert_eq!(
    String::from_utf8(buf).expect("utf-8"),
    "service:\n    port: 8080\n"
);
# Ok(())
# }
```

## Options, limits, and masked errors

Pass `ParseOptions` to any `*_with` reader to tighten `Limits`, match on `Error::code()`
rather than message text, and format `error.masked()` whenever logs or responses must not
contain YAML document text:

```rust
use std::collections::HashMap;
use ayaml::{ErrorCode, Limits, ParseOptions, from_str_with};

const OPTIONS: ParseOptions = ParseOptions::new().with_limits(
    Limits::new()
        .with_max_input_bytes(64 * 1024)
        .with_max_depth(32),
);

let err = from_str_with::<HashMap<String, u32>>("token: top-secret\n", &OPTIONS).unwrap_err();
assert_eq!(err.code(), ErrorCode::InvalidType);

let masked = err.masked().to_string();
assert!(!masked.contains("top-secret"));
assert_eq!(masked, "serde error (yaml.invalid-type) at line 1 column 8");
```

See [Error codes](error-codes.md), [Security and untrusted input](security.md), and
[Resource limits](limits.md) for the full contracts.
