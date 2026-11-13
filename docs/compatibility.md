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

A core-schema tag on a node of another kind is dropped in both modes: `!!seq a` reads the
string `a`, while `!!str {}`, `a: !!int [1]`, and `!!null []` read the collection without the
tag. In the value model, a core collection tag on a collection is dropped too, since it only
restates the kind. A custom tag is kept.

### YAML 1.1 readers

The writers quote a string that looks like a decimal numeral with a leading zero, such as
`0644`, `-0644`, or `0_644`, because YAML 1.2 keeps it a string but a YAML 1.1 reader may
read it as an octal integer. The writers leave the following plain strings as they are, so a
YAML 1.1 reader can still misread them:

| Text | YAML 1.1 reading |
| --- | --- |
| `yes`, `no`, `on`, `off` | boolean |
| underscored numbers such as `1_000` | integer |
| sexagesimal values such as `1:20` or `1:20:30.5` | integer or float |
| dates such as `2001-12-15` | timestamp |
| `=` | the special value key |
| `<<` | merge key |

A quoted or block scalar stays a string in both profiles. An explicit tag also settles the
reading, so the event writer may leave a tagged scalar plain.

### Collections and keys

- Mapping order is insertion order.
- Duplicate mapping pairs remain visible in the event stream.
- Generic value composition rejects duplicate semantic keys deterministically,
  including non-string keys that compare equal after resolution.
- Typed decoding follows the target Serde visitor. In compatibility mode,
  duplicate mapping pairs reach it: generated structs reject duplicate fields,
  and a map-like visitor keeps the last value of an exact duplicate. Default
  mode rejects a duplicate semantic key before the visitor, including scalar
  keys that differ only in type, as `1` and `"1"`.
- In typed decoding, a custom tag, one outside YAML's own `tag:yaml.org,2002:`
  names, names an enum's variant, and a `Value` keeps it. So does any of those
  names on a collection but a collection tag such as `!!map`, which is dropped, as
  in `!!C {x: 1}` or `!!str {x: 1}`, while a scalar under one that the core schema
  doesn't define for a scalar, as in `!!C 5` or `!!seq a`, is a string. A tag
  names the variant by its whole name:
  a local tag by all of it after its first `!`, as `B` for `!B`, and any other tag
  by its full name in angle brackets, as `<tag:x:B>` for `!<tag:x:B>`, which `!e!B`
  under `%TAG !e! tag:x:` is too, or `<tag:yaml.org,2002:C>` for `!!C`.
  A reader stores the tag in a canonical full-name split, so `handle()` and
  `suffix()` agree across readers; the full name is what the tag means.
  Serialization writes a variant that holds a value under the local tag of its
  name, as `!%3Ctag:x:B%3E` for one named `<tag:x:B>`, which selects it too, and
  a unit variant as its name. An enum also reads from a scalar that names a
  variant, and from a mapping of one entry whose key is a string naming it, as
  in `B: 5` or `! B: 5`, in both modes. Any other mapping is an error, such as
  one of two entries, or one whose key is `1`, `true`, or under a custom tag, as
  in `!x B: 5` (D80). A null, boolean, or number scalar does not name a variant,
  even when a variant is renamed to its spelling; use a quoted scalar or a
  one-entry mapping for such a name. A sequence does not fill a struct by position.
  A string target under `from_value` accepts only a string or a tagged string;
  the source-based readers still read a scalar's text into a string.
  A null is not an empty sequence, map, or struct; an empty source value still
  supplies an empty collection or a struct's defaults, but a composed
  `Value::Null` is treated as an explicit null by `from_value`. A target that Serde
  reads by its type, such as a number, a sequence, a struct, or what an enum
  variant holds, reads the node under a custom tag, so `!port 80` reads into a
  `u16`, while a collection under a YAML tag that it keeps, as in `!!C {x: 1}`,
  reaches the target as an enum. An
  `Option` reads a tagged node as `Some`, since what it holds may take the tag.
  A target that Serde reads through `deserialize_any`, such as an untagged or
  internally tagged enum or a flattened field, gets the tag as an enum. A tag
  with no value, as in `!Point`, fills a struct variant as an empty value fills
  a struct.
- A float target reads a number, and from the input a number's spelling with
  a leading zero too, as in `01`, which resolves to a string. Other text, such
  as `inf`, a spelling too large for a float, such as `1e999`, and a scalar
  tagged as a string, such as `!!str 1.5`, are errors read into an `f64`.
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
ayaml = { version = "0.1.1", features = ["libyaml-compat"] }
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
compatibility mode a `---` right after one, or a `...` followed by more text on
its line, is text, not a marker. A `...` that ends its line is a marker, as in
default mode, so `\u{feff}...\n...\u{2028}` reads the second line's text. On a
later line, the mark's column also counts for block structure, so a block
collection after it is one column deeper. The stream's first mark takes up no
column, as when libyaml detects the encoding itself, unless a marker follows it.
Then the marker is text, and the line reads like a later line.

A closing `]`, `}`, or quote may sit at any indentation in both modes, including
under a block collection, where the grammar asks for indentation, and after tabs alone.
libyaml accepts these too, but for a tab right after a plain scalar's line, as in
`k: [a\n\t]`, and files commonly close a flow collection this way:

```yaml
key: [
  a,
]
```

Only a quoted scalar's own quote closes it there, so in `k: "a\n\t'"` the line is
indented by a tab alone, which is an error.

By default, any other line of a flow collection under a block collection is indented
past the innermost one, whether a plain scalar goes on there or an entry starts there,
so `k: [a\nb]` and `a:\n  b: [c,\n  d]` are errors. Compatibility mode reads such
lines, as libyaml does, except where an implicit flow key's `:` sits on one, as in
`k: {x\n: y}`, or only a tab indents a line that a plain scalar goes on to, as in
`k: [a\n\tb]`. libyaml refuses those too. It reads an explicit key's `:` there, as in
`k: {? x\n: y}`, and so does compatibility mode.

Compatibility mode isn't an exact copy of libyaml. It follows YAML 1.2.2 instead in
these places, on purpose:

- NEL, LS, and PS are content; libyaml reads them as line breaks. A NEL in a comment
  after a flow `?` still ends that comment, as libyaml's line break does.
- A core-schema tag on a node of another kind is dropped, as libyaml drops it. Default mode
  drops it too, so `!!str {}` and `!!seq a` read as `{}` and `a` in both modes.
- A byte-order mark past the start of the stream behaves as described above.
- Anchor and alias names may hold any `ns-anchor-char`, where libyaml takes only
  alphanumerics, `_`, and `-`, so `&a: b` differs. As in libyaml, a `?` ends a name
  when text follows it; a trailing `?` before a flow indicator or the end stays in the
  name, as default mode reads it. Outside a flow collection, a `:` followed by text or
  a flow indicator ends a name, so `&a:b` is the string `:b` here and `&r:}` is `:}`;
  a `:` before NEL, LS, or PS stays, as default mode reads it.
- As in libyaml, an alias directly followed by `:` is a mapping key, so `*a: 1` has the key
  that `a` stands for, in a flow collection too. Default mode reads `a:` as the name, as
  YAML 1.2.2 says, so there it's an unknown anchor unless an anchor has that name. Compat keeps
  the colon in one case: an anchor earlier in the same document has that exact name, as in
  `&a: x` then `*a:`. `*a : 1` means the same in both modes. In a flow collection a name runs on
  through a `:` with more text after it, so `{*a:b}` looks for an anchor named `a:b`, where libyaml
  reads the key that `a` stands for and the value `b`. Only a JSON-like key takes a value right
  after its `:` there, so `{*a:[1]}` is an error, where libyaml reads the value `[1]`.
- Global, verbatim, and non-specific tags are kept, with their escapes as written.
  Tag forms that YAML 1.2.2 allows and libyaml's scanner refuses are accepted, such as
  `!#`, `!<tag:a,1:b#c>`, or `!` right before a flow indicator. `!` isn't a tag character, so
  `!.!` and `!!!` are errors, except that `!!!` after a flow `?` is the core tag `!!` with
  suffix `!`, which libyaml reads. A tag needs a name, so `!<!>` is an error too, where libyaml reads
  the non-specific tag. A verbatim tag names a local tag or a tag with a URI scheme, and each `%`
  in it starts a two-digit escape, so `!<foo>`, `!<]>`, and `!<tag:x:50%>` are errors.
- In a flow collection, `[:a]` holds a plain scalar starting with `:`, `a:]` holds a key,
  and `a:?` is a plain scalar. `{?:{}}` is an error because `?:` starts a plain scalar, and
  `{?\n:{}}` because only a JSON-like key allows an adjacent value. A flow mapping key may
  span lines or have a line break before its `:`. `[-]` is an error, and so is a `?` right
  before a flow indicator, as in `{?}` or `[?[a]]`, where libyaml reads an empty key or the
  key `[a]`. A flow `?` followed by `-` and a flow indicator reads as an explicit key
  whose key is the scalar `-`, as in `[?-]` or `{?-}`; a comment may follow the `?`;
  and `!!!` after it is the core tag `!!` with suffix `!`, read as libyaml does.
- An implicit key in a flow mapping may run past 1024 characters, as YAML 1.2.2 allows, where
  libyaml refuses it. Elsewhere an implicit key takes at most 1024, as in libyaml. The writers use
  `?` followed by a space for a key past 128 characters, as libyaml's emitter does.
- An empty implicit key, such as `: a`, is valid.
- Tabs are separation wherever they aren't indentation, as in `key:\t"a: b"`, `"a"\t: 1`, or
  `-\t[1]`. Only spaces indent. A tab stands where indentation would before a block indicator or an
  implicit key, at a line's start or after a `-`, `?`, or `:` indicator, `---`, or `...`, so
  `\ta: 1` and `-\t- b` are errors. A node that only tabs indent on its own line isn't indented
  under the key or entry before it, so `k:\n\tv` is an error, as in libyaml. A flow collection's or
  quoted scalar's lines under a block collection need space indentation too, so `a: [\n\tb]` is
  an error. A tab on an empty line after a plain scalar is separation, as in `k: a\n\t\nj: b`,
  where libyaml refuses it.
- A top-level block scalar may hold content at column 0. A `#` line past the scalar's parent and
  indented less than a blank line before it is that scalar's first content, so the blank line is
  too long and it's an error, where libyaml ends the scalar there.
- A root block scalar's explicit indentation indicator counts from the root's -1, so `|1\n y\n`
  keeps its space in default mode, as productions 170 and 174 say. Compat counts from 0, as
  libyaml does, and reads `y\n`.
- A block scalar or a quoted scalar's continuation must be indented past its parent, so
  `a:\n|` and `a: "b\nc"` are errors. A block scalar under a key with an anchor may sit at
  the parent's column, as in `&a::\n|`, which libyaml reads as the key `:` with an empty
  value. That holds for a quoted line that the other kind of quote
  or `''` starts, as in `k: "a\n'b"`. By default, after a compact key or entry the parent is the
  collection that it starts, so `- k: 'a\n b'` is an error. Compatibility mode measures those
  lines against the line's indent, as libyaml reads files that align them with the key.
- A byte-order mark at the start of a key's line doesn't tighten a quoted scalar below
  it: the mark's column counts for block structure but not for the quoted scalar's own
  continuation check, as libyaml has no such check.
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

A typed error's path is written the same way in both modes, as `Error::message`
describes: a key that isn't a short name is quoted in brackets, as in
`ports["80"]` or `["a.b"]`, where readers built on libyaml write `ports.80` and
`a.b`.
