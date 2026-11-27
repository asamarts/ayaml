# Resource limits

YAML input can be attacker-controlled. ayaml therefore makes resource limits
part of the parser API rather than an optional wrapper.

## Defaults

`Limits::default()` allows a stream of up to 10,000 documents. `Limits::compat()` uses
the same defaults with a one-document stream. It is not the optional `libyaml-compat`
mode; that mode has its own `ayaml::compat` entry points.

| Limit | Default | Notes |
| --- | ---: | --- |
| `max_input_bytes` | 32 MiB | Checked before scanning (and before UTF-8 validation on byte/reader entry points; `from_reader*` reads at most `max_input_bytes + 1` bytes). |
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

### Count profile for the input limit

The default count limits are a lower profile than `max_input_bytes`: they bound the tokens
an eager read keeps to 32 MB where they are plain scalars and indicators, and to about 150 MB
at the most, where each is a tag with a long name (see [Memory](#memory)). A dense but valid
document can therefore reach `max_events`, `max_collection_entries`, `max_anchors`, or
`max_alias_dereferences` before it reaches the 32 MiB input limit. This is intentional for
hostile input.

When a caller needs the four count limits not to refuse a valid document within
`MAX_INPUT_BYTES`, start from `ParseOptions::for_max_input()` (or
`Limits::for_max_input()`):

```rust
use ayaml::{ParseOptions, parse_value_with};

# fn main() -> Result<(), ayaml::Error> {
let options = ParseOptions::for_max_input();
let value = parse_value_with("[1, 2, 3]\n", &options)?;
# let _ = value;
# Ok(())
# }
```

That profile sets the four count limits to `usize::MAX`; the input, depth, scalar,
tag-expansion, document, and alias budgets still bound the read. The tokens alone can then
take a gigabyte for a 32 MiB input, so use `Limits::default()` for untrusted input.
The four reproduced dense shapes are covered by
`crates/ayaml/tests/limit_profiles.rs`: the default profile refuses each one, and
`Limits::for_max_input()` reads it.

Limits are inclusive at the accepted boundary. Input size, depth, alias expansion,
tag expansion, a block scalar's size, and the least node cost have tests just below,
at, and above their default ceilings. `crates/ayaml/tests/limit_consistency.rs` covers every limit that a
reader checks: each case names the least value that accepts its input, and every reader that
takes limits is asked at that value, one above, and every value below. It covers
`node_cost_bytes`, whose largest accepted value is the boundary, and
`alias_bytes_per_input_byte`. The entry-count limit is checked at each place that counts
entries: block and flow mappings, block and flow sequences, explicit keys, and the short-block
fast paths. `max_output_bytes` applies to emitters rather than readers, and the same file checks
it on every emitter that takes limits. The event writers also hold to `max_anchors`, for the
names of one document's anchors, and every writer to `max_depth`; no limit on what is read
applies to a writer. `Value::apply_merge_keys` and `Value::apply_merge_keys_with_limits`
enforce `max_depth` on collection nesting (traversing `Value::Tagged` wrappers without adding
collection depth) and `max_collection_entries` when merging adds new keys to a mapping, and
validate `Limits` in the same way as the readers and emitters. Opt-in event and typed merge-key
expansion (`ParseOptions::with_merge_keys(true)`) additionally enforces `max_events`,
`max_alias_dereferences`, and the alias expansion budget (`max_alias_expansion_bytes` +
`alias_bytes_per_input_byte`) on merged entries while keeping quoted `"<<"` keys literal. A limit that cannot apply is stated as such: an event
reader does not copy an alias's target, so the alias-budget limits are asked only of readers that
build values.

Every reader that takes limits counts each limit the same way, so at the same limits they agree
on whether an input keeps within them. The readers that build values differ from the event readers
only where an alias copies its target, as the next sections say. An input that breaks one limit at
one place is refused there by every reader, in one mode. With more than one problem, such as a
limit broken in more than one place, two limits, or a limit and a syntax error, the readers can
report different ones, because they don't check in the same order:

- `parse_events` and the `Deserializer` read a stream lazily, counting the stream's start
  before scanning document content. They can stop at an earlier document than readers that
  consume the whole stream before returning.
- `parse_value`, `parse_documents`, and the readers of one document into a type, such as
  `from_str`, scan logical lines incrementally and report the first problem in document order,
  whatever the input's size (whether found by the scanner, parser, or builder). The
  `Deserializer` reads a document's events before it builds from them, so within a document it
  reports a syntax error before an earlier problem in what the events build, such as a
  duplicate key.
- A reader of one document into a type reads an input of more than 4 KiB from its events, and
  reports what is wrong with the document before anything about the target, wherever each
  stands. To do that it validates the remainder of the document on target-side errors and, when
  needed, reads a refused input a second time, building the document as the other readers do.
  By then its target has read what stands before the fault, as it would read that part of a
  document that is accepted, aliases included. So a refusal can cost such a reader two reads:
  one that an accepted document of the same content up to the fault would cost, and one that
  builds the document. The first read's memory is freed before the second begins. A target or
  a seed that writes somewhere else as it reads, such as a seed that fills its caller's
  vector, has written what it read before the fault when the reader returns the error.

## What the limits count

`max_events` bounds two counts, and an input that takes either over the limit is refused:

- The events of the stream, as `parse_events` yields them: its start and end, each document's
  start and end, each directive, collection start and end, scalar, and alias. Every reader counts
  them alike, each event once, including the readers that build values without keeping events.
- The tokens a read scans: each scalar, anchor, alias, tag, document marker, and directive, and
  each indicator, `-`, `?`, `:`, `,`, and the brackets and braces of flow collections. Counting
  every token bounds total scanning work as well as peak memory on single-line flow inputs.

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
the anchors of its own document. The event writers count the names of one document's anchors
against it, a name that is defined again once, since they hold those names to check each alias. So do `max_events`, `max_alias_dereferences`, and
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
lets aliases copy more memory than the budget says. A typed read keeps the text of a null, a
boolean, and a number, which a string target reads, so there an alias to one is charged its
text where that is longer than a node's cost. Only a number spelled at great length is.
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

A read holds the input, a small bounded window of active logical lines and tokens, and what it
builds. Error line/column coordinates are computed lazily from the input slice when an error is
constructed, so successful parses allocate no line-start index.

- A token takes 32 bytes on a 64-bit target, and every token a read scans counts against
  `max_events`. A directive, an escaped or multiline quoted scalar, or a block scalar holds a
  second allocation for its decoded text; single-line unescaped quoted scalars and plain scalars
  slice the input directly. A tag holds its name as a prefix and suffix.
- All readers (`parse_value`, `parse_documents`, `parse_events`, `from_str*`, `SpannedTree`, and
  `Deserializer`) scan logical lines incrementally in small bounded batches (up to 128 lines or
  384 tokens, roughly 12 to 16 KiB on block documents) and reuse or free retired token storage
  between batches. `parse_events` and `Deserializer` buffer at most one bounded event queue
  (~250 events) rather than a whole document's events.
- When a single logical line (such as a large flow collection) exceeds the batch window, the token
  buffer reserves room for no more tokens than `max_events` allows, whatever the input promises.
  Each reservation is made fallibly (`try_reserve_exact`), so an overflowing reservation ends in a
  `LimitKind::Events` error rather than a panic. Past 1,024 tokens on one line, growth estimates
  remaining tokens from the higher of the whole prefix's density and recent density (starting from
  the first content token), capped at eight times the tokens already scanned; once that oversized
  line retires, the buffer is released immediately.
- A reader of one document into a type, such as `from_str`, holds no tree of an input of more
  than 4 KiB beside those: it builds the target's value from the events, and keeps a tree only
  of what an alias may read again, of a collection that keeps its tag, of what an enum reads,
  and of a mapping's key while its value is read. With duplicate keys refused it also holds a
  set of each open mapping's keys, at 24 bytes a slot and the text of a key that isn't taken
  from the source as it stands. A root that is built whole, because it has an anchor or a tag
  that it keeps or because an enum reads it, is the document as a tree: the tokens are freed
  before its target reads it. A document dense in anchored nodes or containing a large non-root
  anchored, kept-tag, or complex-key collection is likewise built as a tree in a single scan
  with its tokens freed before its target reads it; a large non-root node that reaches the end
  of the document (such as an enum) also frees the token arena before the target reads it. A
  read that fails frees its tokens and then builds the document once more to report the error.
- A collection that is read from events tells its target its exact size whenever it is a root
  flow collection (whose entries the parser counts first) or whenever its end is already among
  the events waiting in the lookahead queue (which covers most small block and nested
  collections). Larger collections whose end lies past the lookahead queue tell no size, so a
  `Vec` or a map grows by doubling and can keep up to twice the room its entries need. `Value`
  and `Mapping` skip `shrink_to_fit` when told a size within their initial pre-allocation cap,
  and otherwise give back spare room at each collection's end. Another target keeps the spare
  room unless it does the same, as with `Vec::shrink_to_fit` or `Mapping::shrink_to_fit`.
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

`Limits`, `ParseOptions`, `compat::ParseOptions`, and `EmitOptions` are
`#[non_exhaustive]`. Start with `Limits::new()` (or `Limits::default()` /
`Limits::compat()`), wrap it in `ParseOptions` or `EmitOptions`, and pass it to
the operation's `_with` function:

```rust
use ayaml::{Limits, ParseOptions, parse_value_with};

# fn main() -> Result<(), ayaml::Error> {
let options = ParseOptions::new().with_limits(
    Limits::new()
        .with_max_input_bytes(1024 * 1024)
        .with_max_depth(64)
        .with_max_documents(100),
);
let value = parse_value_with("a: [1, 2, 3]\n", &options)?;
assert_eq!(value["a"][2].as_u64(), Some(3));
# Ok(())
# }
```

The public fields remain available for code that needs to adjust a limit after
construction.

## Errors

A limit failure returns `ErrorKind::Limit` and a specific `ErrorCode`
(see [`error-codes.md`](error-codes.md#resource-limits) and [`security.md`](security.md)).
`Error::limit()` returns the structured `LimitExceeded` value with:

- the `LimitKind`;
- the configured ceiling; and
- the observed or attempted value.

There are three exceptions. A configured `max_depth` above `MAX_DEPTH_CAP`, or a
`node_cost_bytes` of 0, is `ErrorKind::Limit` with no structured value or span,
because no input went over a limit. In compat mode, the readers that build values report the way a libyaml-based
reader does while still preserving `Error::code()` and `Error::limit()`. A depth failure is
`ErrorKind::Serde` (`ErrorCode::DepthLimit`), with the message `recursion limit
exceeded`, the structured `LimitKind::Depth` value, and the span of the collection that went over.
Going over `max_documents` is `ErrorKind::Parser` (`ErrorCode::DocumentCountLimit`), with the
message `more than one document in stream`, the structured `LimitKind::Documents` value, and the
span of the second document's start. The compatibility event readers don't map these:
`compat::parse_events`, `compat::parse_events_with`, and `compat::parse_events_eager_with`
report `ErrorKind::Limit` with the value and the span.

Parser, scanner, composer, typed-builder, document-count, and alias-budget
failures carry a source span when input provides one. Output-emitter limits do
not have a source span because they describe generated output rather than
input.
