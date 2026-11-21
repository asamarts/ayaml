# What ayaml offers at each layer of the format contract

ayaml and [adotenv](https://github.com/asamarts/adotenv) are independent parsers that keep their
public surfaces consistent through a
[format contract](https://github.com/asamarts/adotenv/blob/main/docs/design/format-contract.md):
no shared types, no shared trait, and no dependency between them. A crate offers the layers that
its format supports, and where it offers one it uses the contract's names and rules. Conformance
is checked by shared test data, which is vendored here under
[`tests/vectors/`](../tests/vectors/SOURCE.md).

This page says what ayaml offers at each layer today, and which task holds what it doesn't.

| Layer | The contract's names | ayaml today |
| --- | --- | --- |
| Positions | `LineIndex::{new, to_position, to_offset, line_count}`, `ColumnEncoding`, `Position`, `PositionError` | Offered, and checked against the shared position vectors (D96). |
| Syntax stream | `parse_events` | Offered: `parse_events`, and `compat::parse_events` for the compatibility reader. |
| Lossless, recovering | `parse` | Not offered. ayaml has no recovering parser, and leaves the name unused. |
| Lossless, strict | `parse_strict` | Not offered. `SpannedTree` gives spans and source text for one document, and is no lossless document. |
| Native value | `parse_value`, `parse_documents` | Offered. A YAML stream has no flat map, so there is no `parse_values`. |
| Typed, with Serde | `from_str`, `from_slice`, `to_string`, `to_writer`, `from_value`, `to_value` | Offered. |
| Options | one options value per operation | Offered (D98): `ParseOptions`, `compat::ParseOptions`, and `EmitOptions` are `#[non_exhaustive]` structs with `const` presets and `with_*` builders, passed to one `*_with` function per operation. Older `*_with_limits`, `*_with_options`, and `*_with_styles` functions are deprecated through `0.x` until removal in `1.0`. |
| Locate | a byte span for a value or an entry | Offered by `SpannedTree`: `span_at`, `entry_span_at`, `scalar_text_at`, and `scalar_encoding_at`. An entry span is given only where its removal is exact (D92). |
| Edit | `edit`, returning verified edits | Not offered. The `ayaml-edit` crate is planned (AY-050), and will run the shared edit-order vectors. |
| Format | `format` | Not offered. It needs a lossless layer. |
| Compare | `compare`, `portability` | Not offered. |
| Effects | behind a feature or in another crate | ayaml reads no file, environment variable, or network address. Its features add a reader, `libyaml-compat`, and entry points for its own benchmarks, `bench-internals`, and no effect. |
| Tool | `<format>-cli` | Not offered. |

## Positions

- An offset is a count of UTF-8 bytes in the original input, a leading byte-order mark included.
  A `Span` is two of them and converts to and from `Range<usize>`.
- A `Position` is a line and a column counted from zero, for a machine. A `Location` is the same
  place counted from one, for a person: `Location::from(position)`. `Error::location()` returns
  a `Location`.
- A column counts what its `ColumnEncoding` says: UTF-8 bytes, Unicode scalar values, or UTF-16
  code units. A byte column exists for every offset on a line; the other two need the offset at
  a character boundary.
- Lines end at LF, at CRLF, and at a CR alone. NEL, LS, and PS are content, as in YAML 1.2. The
  offset between a CR and its LF is an error.
- Columns leave out one byte-order mark that opens the input, which the contract makes the
  default. An offset inside it shares the first visible column. This index has no form that
  counts the mark's bytes as columns. Every other mark is a character of its line and one
  column, as in an editor: a second mark at the start, and a mark that begins a later line.
- A conversion finds its line by binary search. A column in UTF-8 bytes is then a subtraction,
  and one in scalar values or UTF-16 code units reads the line up to the offset.
- A reader's error stands where the index puts the start of its span, counted from one, on
  every line that begins with no mark. Where a line begins with one, the readers count in
  their own ways: a default reader leaves out every mark that begins a line, and a
  compatibility reader the whole run that opens the input, of which it counts one where a
  document marker follows, as libyaml does.

## Diagnostics

An error has a kind, a message, an optional span, and an optional location. It has no stable
code yet: AY-046 adds one, in the contract's `<format>.<kebab-name>` form, and AY-047 a way to
print an error without text from the document.
