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

- The **scanner** validates UTF-8 and YAML lexical structure, produces tokens,
  and tracks byte offsets and one-based line/column positions.
- The **parser** turns tokens into stream, document, collection, scalar, tag,
  anchor, alias, and directive events. Collections use explicit stacks, so
  hostile nesting consumes heap rather than the native call stack.
- The **composer** registers anchors, measures anchored subtrees, charges alias
  expansion before allocating a copy, and builds the generic value model.
- The **resolver** implements the documented scalar profile. Events always
  retain the original scalar text, style, tag, and span before resolution.
- The **Serde layer** decodes from the same event stream and preserves source
  spans through typed errors. An error below the document's root also names the
  path to what failed, as in `tasks[0].limits.cpu`. The path is built only as an
  error travels outwards, so a read from the input that succeeds allocates and
  formats nothing for it. A read from a `Value` keeps each string key until its
  value has been read, so a key read as a string or a field name is copied.
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

Spans are half-open UTF-8 byte ranges over the original input. `Marker` carries
a byte index plus one-based line and column values. Block and flow collections,
scalars, tags, anchors, aliases, and directives expose source spans in the
event stream.

Errors have a stable `ErrorKind`, an optional span and location, a message, and
an optional source. Resource-limit errors additionally expose a structured
`LimitExceeded` value through `Error::limit()`. Human-readable `Display` output
includes the location when one is known without duplicating a location already
present in the message.

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
