# Design

ayaml exposes one YAML implementation through several APIs. The goal is not a
minimal "YAML-like" configuration reader: the parser, resolver, composer,
typed decoder, and emitter share the same grammar, source spans, and resource
limits.

## Pipeline

```text
UTF-8 input
  -> scanner (tokens and byte spans)
  -> parser (spanned events)
  -> composer (anchors, aliases, and budgets)
  -> resolver (scalar profile)
  -> Value or Serde deserializer
  -> optional Serde serializer or emitter
```

Each layer has a narrow responsibility:

- The **scanner** validates UTF-8 and YAML lexical structure and produces tokens
  with byte offsets. It builds the index of line starts that gives an error its
  one-based line and column on request (D89). A caller builds a `LineIndex` of
  its own for an input, which answers in zero-based positions (D96).
- The **parser** turns tokens into stream, document, collection, scalar, tag,
  anchor, alias, and directive events. Collections use explicit stacks, so
  hostile nesting consumes heap rather than the native call stack. Event readers
  stop at this layer: an alias is reported as written and is not checked against
  earlier anchors. The composer below is what resolves and checks aliases.
- The **composer** registers anchors, measures anchored subtrees, charges alias
  expansion before allocating a copy, and builds the generic value model.
- The **resolver** implements the documented scalar profile. Events always
  retain the original scalar text, style, tag, and span before resolution.
- The **Serde layer** decodes from the same event stream and preserves source
  spans through typed errors. A read of one document of more than 4 KiB takes
  the events from the parser as its target asks for values, and builds no tree
  of the document: a scalar is read and dropped, and a collection is read entry
  by entry. It builds a tree only of what is read whole or again: a node with an
  anchor, a collection that keeps its tag, a mapping key, and what an enum
  reads. A target can tell the two readers apart in two ways only: a collection
  read from events tells it no size, and it has read the start of a document
  that is then refused (D95). A smaller read, and the `Deserializer`, build the document's tree and
  read from that, and every read reports the error that the tree gives (D95).
  An error below the document's root also names the
  path to what failed, as in `tasks[0].limits.cpu`. The path is built only as an
  error travels outwards, so a read from the input that succeeds allocates and
  formats nothing for it. A read from a `Value` keeps each string key until its
  value has been read, so a key read as a string or a field name is copied, and
  so is an enum variant's name, which an error inside the variant names.
- The **emitter** writes deterministic YAML with safe quoting and optional
  per-node presentation styles.

The public entry points are thin wrappers over this pipeline. There is no
second scanner with different rules for values, Serde, or emission.

## Safety

- The crates in this workspace forbid `unsafe` code. Only the fuzz workspace, which
  isn't published, has any: the counting allocator behind the allocation budgets.
- The parser does not depend on a third-party YAML parser, native library, or
  FFI bridge.
- Parser, composer, typed builder, and event writer state machines keep
  explicit stacks where input can control nesting.
- Every limit check uses checked or saturating arithmetic and fails closed.
- Alias expansion is charged before allocation, including on borrowed typed
  paths and the document cursor.
- The default collection-depth ceiling is also the accepted maximum; a larger
  configured value is rejected.
- Error paths are deterministic, panic-free, and safe to retain.

The dependency policy is described in [dependencies.md](dependencies.md); the
resource model is described in [limits.md](limits.md).

## Spans and diagnostics

Spans are half-open UTF-8 byte ranges over the original input. A `Span` holds
only its byte offsets and converts to `Range<usize>`. `LineIndex` turns an
offset into a zero-based `Position` and back, counting UTF-8 bytes, Unicode
scalar values, or UTF-16 code units with `ColumnEncoding`; a leading byte-order
mark is outside the columns and inside the offsets. An error's `Location` is
the one-based form of the same place. These are the names and rules of the
format contract's positions layer ([interface.md](interface.md), D96). Block and flow collections, scalars, tags,
anchors, aliases, and directives expose source spans in the event stream. A
plain scalar's span and raw text end at its last non-whitespace character, so
the separation before a comment, line break, or flow indicator stays outside it.

`SpannedTree` builds a read-only tree from the same events and answers path lookups with a
node's byte span, its entry span, and the original scalar text. Merge keys, duplicate keys,
multi-document streams, and paths that step through an alias return `None` rather than guessing;
an alias value itself is found at its own path (`is_alias() == true`). An entry span is the bytes
whose removal removes that entry and changes nothing else. A block entry that begins its own line
can have one running through its last line break, and the first key of a block mapping on a
sequence's `-` line can have one running to the next key's start so the `-` stays in place. A flow
entry has none, and nor does a block entry whose removal the lines around it would make inexact;
D92 lists the conditions. A value that the source doesn't write after its key's `:` has an empty
span right after that `:`.

Errors have a coarse `ErrorKind` that names the layer that refused the input, a stable
machine-readable `ErrorCode` (`yaml.<kebab-name>` via `Error::code()`), an optional span and
location, a message, and an optional source. Resource-limit errors additionally expose a
structured `LimitExceeded` value through `Error::limit()`. Human-readable `Display` output
includes the location when one is known without duplicating a location already present in the
message, and may contain text from the document; use `Error::masked()` (`MaskedError`) when
formatting errors for logs or untrusted callers. See [error-codes.md](error-codes.md),
[security.md](security.md), and [stability.md](stability.md).

## Determinism and concurrency

- Mapping order follows insertion order.
- Emitted output and error messages do not depend on hash iteration order.
- Parsing configuration is an explicit `Limits` value, not process-global
  state.
- Owned values and independent parses are safe to use concurrently where their
  contents allow it.

## Non-goals for the first release

- UTF-16 or UTF-32 input decoding;
- `no_std` or allocator-free parsing;
- comment-preserving AST mutation;
- automatic YAML 1.1 merge-key application during generic parsing;
- schema languages beyond the documented resolution profile; and
- preserving arbitrary object graphs through serialization.
