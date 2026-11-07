# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- A typed reader's error now names the path to what it rejected, as in
  `tasks[1]: unknown field `efort`, expected `title` or `effort` at line 4 column 5`, on every
  typed entry point. `Error::message` documents how a path reads. The span and `ErrorKind::Serde`
  are as they were, and an error at the document's root gains nothing.
- In compat mode, an alias directly followed by `:` is now a mapping key, as in libyaml, so `*a: 1`
  has the key that `a` stands for. An alias keeps such a colon only when an anchor earlier in the
  same document has that exact name. Default mode is unchanged.

### Fixed

- A mapping can no longer be written so that its hash collides with those of other mappings. The
  entries' hashes combine by XOR under a key anyone could compute, so entries could be chosen to
  cancel out, and 2 MiB of mappings used as keys took 30 seconds to read. The key is now random
  for each process, so the `Hash` of a `Value` holding a non-empty mapping differs from one run to
  the next. Each kind of number now writes its own mark too: numbers of different kinds could write
  the same bytes, so entries could cancel out under any key.
- Equal tags now hash alike under a hasher that reads each write on its own, whatever the split
  between handle and suffix.
- `from_str` and the other default-mode typed readers now see two keys that differ only in the
  order of their entries as duplicates, as `parse_value` does. A typed map could silently lose one
  of them. A typed map in compat mode still keeps the last of them, as it does for any key.
- The check on a block scalar's indentation now reads a flow collection used as a key token by
  token, by the scanner's rules, and finds a comment after that key the same way. It could lose the
  key, for instance over a quote after plain text or in a name, a blank before a `:` after a quoted
  key, or a verbatim tag holding a `]`. An indentation indicator on that scalar is now read from
  the key's indentation, and a scalar that isn't indented enough is an error.
- Floats now hash by value, so many float keys no longer take quadratic time. They used to share
  one bucket, and 40,000 of them, in 0.4 MiB, took 19 seconds to read.
- A depth limit on a collection used as a key now gives its location on `parse_value`,
  `parse_documents`, and `Deserializer`, as it does elsewhere.
- In compat mode, `from_str` no longer reports "more than one document" for a type error on a
  scalar that holds the words `trailing documents`.
- In compat mode, a `:` followed by more text no longer ends an anchor or alias name in a flow
  collection, which matches default mode. `[&a:b]` read as `[":b"]`, which is neither default
  mode's reading nor libyaml's, and `[&a:b, *a:b]` was an error, though both of them accept it.
- An alias now counts the tags of the node it names against the alias budget. It didn't, so an
  alias to a node with a long tag cost far more memory than the budget suggests: about 1 GiB from
  about 1 MiB of input. A document with many aliases to long tags may now need a higher
  `max_alias_expansion_bytes`.
- In compat mode, a scanner error is no longer reported as a recursion limit because the
  input has an anchor followed by an alias of the same name somewhere. `&a *a\n@` now reports
  the `@`, with the scanner's error kind rather than `ErrorKind::Limit`.
- In compat mode, a byte order mark at the start of a line no longer loses the text after
  it. After a root block scalar, the line is now outside the scalar, so `|\nx\n\u{FEFF}y`
  is an error instead of `"x\n"`, and `|+\nx\n\u{FEFF}\n` keeps one line break, not two.
  After a root plain scalar, the line continues the scalar whatever it holds, so
  `n\n\u{FEFF}#:` is an error instead of `n`, and `n\n\u{FEFF} #:` is `"n \u{FEFF}"`.
- An alias inside a collection with the alias's own anchor is now an error, unless a node
  closer to the alias defines that anchor again. It used to read an earlier node with that
  anchor if there was one, so `a: &x 1\nb: &x [*x]` read `b` as `[1]`. It's also a clearer
  error where it was already one: the message now says the alias refers to a collection that
  contains it, not that the anchor is unknown.
- A plain flow mapping key followed by blank lines before its `:` no longer keeps line
  breaks, so the key in `{x\n\n: y}` is now `x`, not `x\n`. The change applies with and
  without an explicit `?`.
- In default mode, a flow collection's or quoted scalar's lines may start at column 0, or be
  indented with tabs alone, when it's a document's root node or is nested in one. So
  `[[x\n, y]]`, `!!seq [x\n, y]`, `  'a\nb'`, and `[\n\ta\n]` now parse. Under a block
  collection the indentation of those lines is now checked, wherever the node starts, so
  `k:\n  [\na\n  ]` and `---k: 'a\nb'` are now errors. Compat mode still reads the first.
  A comment line is exempt in every position, so `k: [\n# c\n  a\n  ]` now
  parses, and so is a closing `]`, `}`, or quote, as it was before, unless tabs alone indent a
  quote. The check doesn't yet cover a plain scalar's own continuation lines inside a flow
  collection, or every indentation of a quoted scalar's lines under nested collections.
- A second tag or anchor on one node is now an error in both modes, so `&a &b c` and
  `!a !b c` are rejected. The second one used to replace the first.
- An explicit key whose properties are alone on the `?` line may now hold an indentless
  sequence, so `? !t\n- a` parses in both modes.
- By default, a byte order mark no longer stops a document marker or directive after it from
  being one, so `a\n...\n\u{FEFF}---\nb` is two documents, `a` and `b`. The second used to
  be the text `--- b`. A `...` after a mark now ends a document too, so `a\n\u{FEFF}...\nb` is
  two documents, where it was an error. A block collection may also follow the mark on any line,
  so `a\n...\n\u{FEFF}b: 1\nc: 2` parses, and a column after it no longer counts the mark. Compat
  mode keeps its reading of all these, because libyaml counts a column for the mark. A mark
  between a directive and its `---` is now an error, as the grammar has no room for one, so
  `%YAML 1.2\n\u{FEFF}# c\n---\na` is rejected.
- In compat mode, a byte order mark's column on a later line now counts for block structure, as it
  already did for error positions. So `# c\n\u{FEFF}- 1\n - 2` is `[1, 2]`, not `["1 - 2"]`,
  and `# c\n\u{FEFF}k: a\n b` is an error, as in libyaml. A quoted scalar's lines aren't held to
  that column, because libyaml has no such check.
- In compat mode, what follows a `---` or `...` after a byte order mark now reads the way the
  marker does, as text, so the block scalar in `\n\u{FEFF}--- k: |\n  # c\n` holds `# c\n` instead
  of being empty, and `\n\u{FEFF}--- k: a\n  b` is `a b` instead of an error. The stream's first
  mark now takes up a column when a marker follows it, as a later line's does, so
  `\u{FEFF}--- k: a\nz: 1` is an error, as in libyaml.
- The non-specific tag `!` is no longer changed by a `%TAG !` directive, in either mode.
- A tag whose handle expands to part of a core-schema name is now that core tag, in both
  modes. Under `%TAG !y! tag:yaml.org,2002:i`, `!y!nt 1` is the integer 1 and `!y!nt x` is
  an error. `Tag::new` splits such a name at the core prefix, so `handle()` and `suffix()`
  return the core parts. A name the core schema doesn't define now reads as it does when
  written with `!!`: under `%TAG !y! tag:yaml.org,2002:app/`, `!y!Foo 1` is the string `1`,
  and `!y!Foo [1]` selects the enum variant `app/Foo`, not `Foo`.
- Scanning no longer takes time quadratic in the input on a long line holding any non-ASCII
  text, on long runs of tabs or many runs on one line, on many quoted scalars on one line of a
  flow collection, on a plain scalar with many lines after a long prefix or after a run of byte
  order marks, or on many byte order marks in a document prefix. An 800 KB input of those could take over a minute,
  and the default limit allows 32 MiB.
- `Limits` now bounds what tag handles expand to. Every tag written with a handle holds its own
  copy of the handle's prefix, so one long `%TAG` prefix and many short tags could use memory far
  beyond the input: about 1 MiB of input could reach 2 GiB, with no limit applying. The new
  `Limits::max_tag_expansion_bytes`, 64 MiB by default, counts each use across a stream, and going
  over it is `LimitKind::TagExpansion`.

## [0.1.1] — 2026-11-04

A performance patch for wide documents made of short block entries. There are
no public API changes.

### Performance

- Parser token storage now uses a shared arena instead of one token vector per
  logical line.
- Single-line plain scalars are borrowed from the input instead of allocating a
  temporary `String`.
- Same-line source spans reuse the starting line's position data.
- Common one-scalar sequence entries and simple explicit key/value pairs use
  direct parser paths.
- Token-arena growth is tuned for small inputs while bounding unused capacity
  on wide inputs.

### Validation

- Default and compatibility conformance are unchanged.
- Tests, package checks, and cross-platform CI pass.

## [0.1.0] — 2026-11-04

The first public release. ayaml is a safe, pure-Rust YAML 1.2.2 parser, event
stream, value model, Serde implementation, and emitter.

### Added

- YAML 1.2.2 parser with block and flow collections, all scalar styles,
  directives, tags, anchors, aliases, document streams, duplicate mapping
  pairs at the event layer, and UTF-8 byte spans.
- Pull-based event stream with one-based locations, presentation styles, tags,
  anchors, aliases, directives, and duplicate mapping entries.
- Ordered `Mapping`, `Value`, `Number`, `Tag`, and `TaggedValue` types with
  indexing, mutation, conversion, and Serde support.
- Native resource limits for input bytes, depth, documents, scalar bytes,
  collection entries, events, anchors, alias dereferences, alias expansion,
  node cost, and output bytes.
- `Limits::default()` with a stream-sized document limit and `Limits::compat()`
  for the single-document compatibility profile, with `with_*` builders.
- Typed `from_str` and `from_slice` using the span-carrying typed path;
  `from_str_borrowed` and `from_slice_borrowed` provide borrowed scalar text.
- A streaming `Deserializer`, `from_value`, `to_value`, `to_string*`,
  `to_writer*`, event emitters, and value emitters.
- The reusable `Emitter` builder, which borrows limits and options and can
  apply per-node styles to values, writers, and document streams.
- An additive `ayaml::compat` module behind the `libyaml-compat` feature for
  callers that need libyaml-compatible decoding. The default parsing profile is
  unchanged when the feature is enabled.
- Synthetic compatibility fixtures and a public YAML Test Suite conformance
  harness.
- Property, regression, real-world, adversarial, and limit-boundary tests,
  fuzzing targets, allocation and peak-memory probes, and benchmark harnesses.

### Changed

- The default parser follows YAML 1.2.2. libyaml-compatible readings are kept
  only in the optional compatibility mode; each deliberate deviation is listed
  in `tests/data/yaml-test-suite-deviations.txt`.
- The typed decoding path preserves source spans and can borrow simple scalar
  strings.
- Integers up to 128 bits are kept exactly. Wider decimal integers become
  finite floats; wider radix integers become strings. `Number` is 24 bytes and
  `Value` is 72 bytes on 64-bit targets.
- Merge keys preserve the mapping's own keys in source order and append merged
  keys in operand order. Generic parsing keeps `<<` literal unless
  `Value::apply_merge_keys` is called explicitly.
- Quoted scalars accept DEL, C1 controls, U+FFFE, and U+FFFF raw, as YAML 1.2.2
  requires; C0 controls remain errors.
- `Limits::default()` allows 10,000 documents; `Limits::compat()` allows one.
  Single-document entry points still reject a second document.
- `max_depth` is capped at 128; a larger configured value is a limit error.
- Alias limits are enforced on every public entry point, including borrowed
  typed decoding, the document cursor, and compatibility visitor paths.
- The parser, composer, typed builder, and event writer use explicit stacks;
  hostile nesting consumes heap rather than the native stack.
- Anchored subtrees are recorded by position and copied only when an alias
  materializes them, under the alias budget.
- Public API hardening: every public item is documented and
  `#![deny(missing_docs)]` is enforced. `Index` is sealed, `Deserializer`
  implements `Debug`, and `Event`, `ScalarStyle`, `CollectionStyle`,
  `PathSegment`, `Chomping`, `Limits`, `EmitterOptions`, and `NodeStyle` are
  `#[non_exhaustive]`.
- `Limits`, `EmitterOptions`, and `NodeStyle` expose `with_*` builders for
  configuration. The convenience emitter functions that accept options take
  `&EmitterOptions`.
- Errors retain the structured `LimitExceeded` value via `Error::limit()` and
  append the line/column to `Display` when a span is available. Parser,
  scanner, composer, typed-builder, and alias-budget limit failures carry a
  source span. Duplicate-key diagnostics print the key's value.
- The direct `thiserror` dependency is gone; errors implement `Display` and
  `std::error::Error` directly.

### Fixed

- Plain scalar and explicit-key continuation indentation in compact nested
  sequences and mappings.
- Block-scalar indentation indicators and document-marker termination.
- Duplicate `...` streams, explicit keys in sequence entries, C0 control
  rejection, percent-escaped tag suffixes and `%TAG` prefixes, and BOM
  handling.
- YAML 1.2 whitespace, tag-character, flow-indicator, anchor/alias, directive,
  and quoted-scalar rules.
- Tagged-value round trips, tag equality by resolved URI, 128-bit integers,
  wide integer string targets, merge ordering, anchor redefinition, and value
  indexing/accessors.
- Emitter handling for ambiguous scalars, empty scalars, chomping, folded and
  single-quoted scalars, anchors/aliases, indentation indicators, nested tags,
  control characters, long implicit keys, document limits, and indentation
  validation.
- Peak-memory handling for anchors, aliases, mappings, and long documents.

### Security

- No unsafe Rust, C/C++, FFI, or native build steps in the required parser
  graph. ayaml-owned crates use `#![forbid(unsafe_code)]`, and dependency
  classification, license checks, and unsafe census run in CI.
- Alias expansion is charged before allocation; alias bombs and dereference
  floods are rejected on every public entry point.
- Depth, document, scalar, entry, event, anchor, input, and output limits fail
  closed with structured limit errors.
- Public entry points are tested on 2 MiB threads in debug and release builds.

### Known limitations

- The first release is UTF-8 only. UTF-16/UTF-32 input, `from_reader`,
  `no_std`, comment-preserving rewrite APIs, and arbitrary object-graph
  serialization are not included.
- Generic parsing keeps YAML 1.1 merge keys literal; merge behavior is an
  explicit value operation.
- Compatibility-mode deviations are listed in
  `tests/data/yaml-test-suite-deviations.txt`.
- Benchmark and memory results are environment-specific; the repository
  includes the harnesses and methodology, not a performance guarantee.

[0.1.1]: https://github.com/asamarts/ayaml/releases/tag/v0.1.1
[0.1.0]: https://github.com/asamarts/ayaml/releases/tag/v0.1.0
