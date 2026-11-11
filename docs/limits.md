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
| `node_cost_bytes` | 256 | Estimated cost of one composed node, at least 1. |
| `max_documents` | 10,000 | `Limits::compat()` uses 1. |
| `max_scalar_bytes` | 32 MiB | Maximum bytes in one scalar, its text or its value, whichever is larger. |
| `max_collection_entries` | 1,000,000 | Maximum entries in one collection. |
| `max_events` | 1,000,000 | Maximum events in one stream, and maximum tokens kept for one stream. |
| `max_anchors` | 100,000 | Maximum anchors in one stream. |
| `max_alias_dereferences` | 100,000 | Maximum alias dereferences in one stream. |
| `max_output_bytes` | 64 MiB | Maximum bytes written by one emitter call. |
| `max_tag_expansion_bytes` | 64 MiB | Maximum bytes that tag handles expand to in one stream. |

Limits are inclusive at the accepted boundary. Input size, depth, alias expansion,
tag expansion, a block scalar's size, and the least node cost have tests just below,
at, and above their default ceilings. `tests/limit_consistency.rs` sets `max_events`,
`max_depth`, `max_anchors`, `max_scalar_bytes`, the fixed alias allowance,
`max_alias_dereferences`, `max_documents`, `max_collection_entries`, `max_tag_expansion_bytes`,
and `max_input_bytes` to small values, and checks each at the least value that accepts its input,
one above, and every value below, on every reader that takes limits. AY-034 extends that to each
place that counts, to `node_cost_bytes` and `alias_bytes_per_input_byte`, and to the readers that
take the default limits only.

Every reader that takes limits counts each limit the same way, so at the same limits they agree
on whether an input keeps within them. The readers that build values differ from the event readers
only where an alias copies its target, as the next sections say. An input that breaks one limit at
one place is refused there by every reader, in one mode. With more than one problem, such as a
limit broken in more than one place, two limits, or a limit and a syntax error, the readers can
report different ones, because they don't check in the same order:

- `parse_events` and the `Deserializer` read a stream a document at a time, and count its start
  before they scan anything. They can stop at an earlier document than the readers that scan the
  whole stream before they parse it.
- `parse_value` and `parse_documents` build values as events arrive, and the typed readers build
  them from a document's events once it is read, so they can meet two limits in a different order.

## What the limits count

`max_events` bounds two counts, and an input that takes either over the limit is refused:

- The events of the stream, as `parse_events` yields them: its start and end, each document's
  start and end, each directive, collection start and end, scalar, and alias. Every reader counts
  them alike, each event once, including the readers that build values without keeping events.
- The tokens a read keeps: each scalar, anchor, alias, tag, document marker, and directive, and
  each indicator, `-`, `?`, `:`, `,`, and the brackets and braces of flow collections. The eager
  readers keep the tokens of the whole stream, so this count is what bounds their memory.

A line break, a blank line, and a comment count as neither, so lines without content don't use up
the limit. `a: 1` is 8 events, from the stream's start to its end, and 3 tokens. Tokens can
outnumber events: each line of `- &a !t x` is 4 tokens and 1 event, and a second `...` line in a row
is a token and no event. Either count over the limit is reported as `LimitKind::Events`.

`max_depth` bounds the collections open at once. A `key: value` entry of a flow sequence is a
mapping of one pair inside it, so `[a: [1]]` is 3 deep. A mapping's keys are inside it, as its
values are, so `[a]: c` is 2 deep. The readers that build values count an
alias as the copy of its target that it becomes, so the target's collections count where the alias
is. An event reader sees the alias alone: `a: &x [[1]]` then `b: [*x]` is 3 deep there and 4 deep
where values are built.

`max_anchors` counts every anchor in the stream, across its documents, although an alias sees only
the anchors of its own document. So do `max_events`, `max_alias_dereferences`, and
`max_tag_expansion_bytes`, and a reader of several documents charges all their aliases to one
alias budget. `max_documents` and `max_collection_entries` count what their names say.

The alias-expansion allowance is:

```text
max_alias_expansion_bytes
  + alias_bytes_per_input_byte * min(input_bytes, max_input_bytes)
```

The input contribution is capped at `max_input_bytes`, so padding a document
cannot multiply the alias budget without bound.

An alias is charged `node_cost_bytes` for each node it copies, on top of the text it
copies, and every reader that builds values charges it the same. A tag that a value keeps,
as `Value::Tagged` does, is a node of its own, and its text counts too: an alias to
`!t abc` is charged two nodes, the 2 bytes of `!t`, and the 3 of `abc`. A core tag that
only restates the value, as `!!str` and `!!seq` do, adds nothing. A composed node takes
about 100 bytes of memory, so the default of 256 covers it with room, and a smaller cost
lets aliases copy more memory than the budget says.
Against a 2 MiB budget, nested aliases of empty nodes built by `parse_value` held
about 0.4 times the budget at the default cost, 1.2 times at 64, 2.7 times at 16, and
65 times at 1, the least accepted cost, before the budget stopped them. At the default
budget of 256 MiB, the same factor comes to about 16 GiB. Every entry
point that takes limits refuses a cost of 0, which would make a copied collection of
empty nodes free and so turn the alias budget off.

Every tag written with a handle holds its own copy of the prefix that handle stands for, so
`max_tag_expansion_bytes` counts the whole prefix at each use, in `%TAG` handles and the built-in
`!` and `!!` alike. A tag written verbatim costs what its text costs, so it doesn't count. An alias
copies the tags of the node it names, and those bytes count against the alias budget with the rest
of the copy.

A scalar's size, for `max_scalar_bytes`, is the larger of its text in the input and the
value it reads as. A quoted scalar's text includes its quotes. A block scalar's runs from
its `|` or `>` through its last line, so its header, indentation, and trailing blank
lines count. A value is never longer than its text, except where a double-quoted `\L` or
`\P` takes 2 bytes of text and 3 of value.

## Memory

A read holds the input, an index of its line starts, and its tokens, besides what it
builds. `parse_events` and the `Deserializer` read one document at a time and hold
that document's tokens. Every other reader holds the tokens of the whole stream.

- The index takes 8 bytes for each CR or LF in the input, so input of blank lines, a
  byte each, costs 8 times its size there. That is 8 bytes a line, or under 16 where
  lines end in CRLF. An error at the end of the input is placed without a second index.
- A token takes about 100 bytes, and every token a read keeps counts against `max_events`,
  so at the default limit a read holds about 100 MB of tokens at most. The arena that holds
  them grows by a chunk of up to 8,192 tokens, or by a sixteenth of itself once that
  is more. Its copying stays linear in the tokens, and its spare capacity is at most a
  chunk or a sixteenth of it, whichever is larger.
- A block scalar is built as it is read. At its peak a read of one takes about 5 times
  its input in resident memory for short lines, and about 10 times for blank lines,
  where the index costs most. Counting the capacity it reserves without touching, a
  literal scalar of blank lines reaches 12 times.
- Before it scans, a read notes the place of each DEL, C1 control, U+FFFE, and U+FFFF
  in the input, since only a quoted scalar may hold them. Each takes 8 bytes, so a
  run of DEL, a byte each, costs 8 times its size there.

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

There are three exceptions. A configured `max_depth` above `MAX_DEPTH_CAP`, or a
`node_cost_bytes` of 0, is `ErrorKind::Limit` with no structured value or span,
because no input went over a limit. In compat mode, the readers that build values report the way a libyaml-based
reader does. A depth failure is `ErrorKind::Serde`, with the message `recursion limit
exceeded` and the span of the collection that went over. Going over `max_documents`
is `ErrorKind::Parser`, with the message `more than one document in stream` and the
span of the second document's start. `compat::parse_events_with_limits` doesn't map
these: it reports `ErrorKind::Limit` with the value and the span.

Parser, scanner, composer, typed-builder, document-count, and alias-budget
failures carry a source span when input provides one. Output-emitter limits do
not have a source span because they describe generated output rather than
input.
