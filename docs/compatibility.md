# Compatibility profile

ayaml has one default parsing profile and one optional compatibility mode.

## Default profile

The default profile follows YAML 1.2.2. The first release is UTF-8 only.

### Scalar resolution

Untagged plain scalars resolve as follows:

| Input | Result |
| --- | --- |
| empty, `~`, `null`, `Null`, `NULL` | null |
| `true`, `True`, `TRUE` | boolean true |
| `false`, `False`, `FALSE` | boolean false |
| `0b11`, `0o17`, `0x1f` | integer |
| decimal integers up to 128 bits | exact integer |
| wider decimal integers | finite float |
| wider radix integers | string |
| `1e3`, `1.5`, `.inf`, `-.inf`, `.nan` | float |
| `yes`, `no`, `0123`, date-like text | string |

Quoted and block scalars remain strings unless an explicit core tag requests
conversion. A custom tag preserves its payload and the tag; the non-specific
`!` tag makes a scalar a string.

Explicit core tags override implicit resolution:

- `!!str`
- `!!int`
- `!!float`
- `!!bool`
- `!!null`

An invalid explicit core scalar is a resolution error.

### Collections and keys

- Mapping order is insertion order.
- Duplicate mapping pairs remain visible in the event stream.
- Generic value composition rejects duplicate semantic keys deterministically,
  including non-string keys that compare equal after resolution.
- Typed decoding follows the target Serde visitor: generated structs reject
  duplicate fields, while map-like visitors may keep the last value.
- A plain or quoted `<<` remains a literal key. Merge-key application is an
  explicit `Value::apply_merge_keys` operation, not an implicit parser effect.
- Tags, anchors, aliases, directives, and document markers are preserved in the
  event stream.

### Unicode and line breaks

Valid UTF-8 is required. LF, CR, and CRLF are accepted as line breaks. NEL,
line separator, and paragraph separator are content under YAML 1.2.2. A UTF-8
byte-order mark is handled according to the specification. Raw C0 control
characters are rejected; quoted scalars accept the printable characters that
YAML permits, including DEL, C1 controls, U+FFFE, and U+FFFF.

## Compatibility mode

The additive `libyaml-compat` feature exposes `ayaml::compat`:

```toml
[dependencies]
ayaml = { version = "0.1.0", features = ["libyaml-compat"] }
```

```rust
# #[cfg(feature = "libyaml-compat")]
# fn example() -> Result<(), ayaml::Error> {
let value: ayaml::Value = ayaml::compat::from_str("key: value\n")?;
assert_eq!(value["key"].as_str(), Some("value"));
# Ok(())
# }
```

The compatibility module reproduces the observable behavior of a
libyaml-compatible stack for callers that need it. Enabling the feature does
not change the root parsing functions; choose the compatibility entry points
explicitly.

The maintained deviation list is
[`tests/data/yaml-test-suite-deviations.txt`](../tests/data/yaml-test-suite-deviations.txt).
Each entry names a YAML Test Suite case, the affected mode and check, and the
reason. Compatibility-mode deviations are intentional; default-mode entries
are treated as bugs.

## Error behavior

Parsing errors use stable categories:

- invalid encoding;
- scanner/token error;
- grammar/parser error;
- unknown, cyclic, or over-budget alias;
- duplicate key;
- tag or scalar-resolution error;
- resource-limit violation;
- composition/value error;
- Serde type/data error; and
- emitter or I/O error.

When input provides a location, the error carries a span and `Error::location()`
returns the one-based line and column. Resource-limit failures expose the
configured ceiling and observed value through `Error::limit()`.
