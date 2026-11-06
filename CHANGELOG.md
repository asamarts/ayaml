# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

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
