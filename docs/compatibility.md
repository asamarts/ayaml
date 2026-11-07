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
- A node has at most one tag and one anchor; a second one is an error, in both
  modes.
- An explicit key's properties may be followed by an indentless sequence, in both
  modes.
- The non-specific tag `!` isn't changed by a `%TAG !` directive, and a tag whose
  handle expands to part of a core-schema name is that core tag, in both modes.

### Unicode and line breaks

Valid UTF-8 is required. LF, CR, and CRLF are accepted as line breaks. NEL,
line separator, and paragraph separator are content under YAML 1.2.2. A UTF-8
byte-order mark is handled according to the specification: it may start any
document prefix, and it takes up no column, so a document marker or directive
right after one is still a marker or directive. Raw C0 control characters are
rejected; quoted scalars accept the printable characters that YAML permits,
including DEL, C1 controls, U+FFFE, and U+FFFF.

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

The compatibility module reads YAML the way a libyaml-compatible stack does, for
callers that need it, apart from the differences listed below. Enabling the
feature does not change the root parsing functions; choose the compatibility
entry points explicitly.

libyaml counts a column for a byte-order mark at the start of a line, so in
compatibility mode a `---` or `...` right after one is text, not a marker. On a
later line, the mark's column also counts for block structure, so a block
collection after it is one column deeper. The stream's first mark takes up no
column, as when libyaml detects the encoding itself, unless a marker follows it.
Then the marker is text, and the line reads like a later line.

A closing `]`, `}`, or quote may sit at any indentation in both modes, including
under a block collection, where the grammar asks for indentation. libyaml accepts
these too, and files commonly close a flow collection this way:

```yaml
key: [
  a,
]
```

The exception is a closing quote on a line indented only with tabs, when the quoted scalar
starts on its key's or `-`'s line, which is still an error.

Compatibility mode isn't an exact copy of libyaml. It follows YAML 1.2.2 instead in
these places, on purpose:

- NEL, LS, and PS are content; libyaml reads them as line breaks.
- A byte-order mark past the start of the stream behaves as described above.
- Anchor and alias names may hold any `ns-anchor-char`, where libyaml takes only
  alphanumerics, `_`, and `-`, so `&a: b` differs. As in libyaml, though, a `?` ends a
  name, and outside a flow collection so does a `:` followed by more text, so `&a:b` is
  the string `:b` here and an empty node in default mode.
- Global, verbatim, and non-specific tags are kept, with their escapes as written.
  Tag forms that YAML 1.2.2 allows and libyaml's scanner refuses are accepted, such as
  `!#`, `!<tag:a,1:b#c>`, or `!` right before a flow indicator. `!` isn't a tag character, so
  `!.!` and `!!!` are errors.
- In a flow collection, `[:a]` holds a plain scalar starting with `:`, `a:]` holds a key,
  and `a:?` is a plain scalar. `{?:{}}` is an error because `?:` starts a plain scalar, and
  `{?\n:{}}` because only a JSON-like key allows an adjacent value. A flow mapping key may
  span lines or have a line break before its `:`. `[-]` is an error.
- An empty implicit key, such as `: a`, is valid.
- Tabs are separation wherever they aren't indentation. A nested flow collection's or
  quoted scalar's lines still need space indentation, so `a: [\n\tb]` is an error.
- A top-level block scalar may hold content at column 0. A `#` line past the scalar's parent and
  indented less than a blank line before it is that scalar's first content, so the blank line is
  too long and it's an error, where libyaml ends the scalar there.
- A block scalar or a quoted scalar's continuation must be indented past its parent, so
  `a:\n|` and `a: "b\nc"` are errors.
- A comment must be separated from what precedes it, so `""#` and `|#` are errors.
- An unknown directive is ignored, and a stream may start with `...`.
- Integers are exact up to 128 bits.
- A quoted scalar may hold any `nb-json` character, such as DEL.

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
configured ceiling and observed value through `Error::limit()`, with the exceptions
in [`limits.md`](limits.md#errors).
