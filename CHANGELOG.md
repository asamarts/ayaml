# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Companion crate `ayaml-edit` providing verified, source-preserving YAML edits (`replace_scalar`,
  `replace_scalar_with`, `remove_entry`, `remove_entry_with`, `apply_batch`, `apply_batch_with`,
  `apply_edits`, `TextEdit`, `VerifiedEdit`, `EditOp`, `EditOptions`, `EditError`,
  `EditErrorKind`), backed by value-tree and event-stream verification and checked against the
  shared `tests/vectors/edit-order.json` contract vectors.
- Opt-in YAML 1.1 merge-key expansion (`ParseOptions::merge_keys`,
  `ParseOptions::with_merge_keys`) at the spanned event layer, preserving source spans, borrowed
  slices, literal quoted `"<<"` / `!!str <<` keys, lexical anchor scoping, and pre-allocation
  resource limits across `parse_events_with`, `parse_value_with`, `parse_documents_with`,
  `from_str_with`, `from_str_seed_with`, `from_slice_with`, `from_reader_with`, `Deserializer`,
  `DocumentDeserializer`, `SpannedTree`, and `parse_frontmatter*`.
- Bounded I/O reader entry points (`from_reader`, `from_reader_with`, and deprecated
  `from_reader_with_limits`) that validate limits first and read at most `max_input_bytes + 1`
  bytes before rejecting oversized or infinite streams.
- Frontmatter splitting and parsing helpers (`Frontmatter`, `FrontmatterDelimiter`,
  `ParsedFrontmatter`, `split_frontmatter`, `split_frontmatter_with`, `parse_frontmatter`,
  `parse_frontmatter_with`, `parse_frontmatter_value`, `parse_frontmatter_value_with`) preserving
  exact byte offsets aligned with `LineIndex` and `SpannedTree`.
- `DocumentDeserializer` (implementing `serde::Deserializer` by value and `&mut`) so Serde
  middleware wrappers can wrap `ayaml`'s borrowed, span-preserving source deserializer directly.
- `Spanned<T>` typed deserialization wrapper capturing exact YAML byte spans on both the
  small-tree and event-stream paths while rejecting non-source and buffered deserializers.
- `Value::apply_merge_keys_with_limits` enforcing `max_depth` and `max_collection_entries` on
  post-parse merge-key expansion.
- Documentation guides for YAML 1.1 migration (`docs/migration.md`), recipes (`docs/cookbook.md`),
  resource-limit sizing (`docs/limits-sizing.md`), surgical editing (`docs/editing.md`),
  developer tooling (`docs/tools.md`), API reference and caveats (`docs/api-reference.md`), and
  three end-to-end tested library migration guides (`docs/migrating-from/`).
- Refreshed Astro + Starlight documentation site (`site/`) for GitHub Pages with a modern landing
  page, Pagefind search, `llms.txt` / `llms-full.txt`, and an interactive `/benchmarks/` showcase
  driven by pinned, SHA-256-verified JSONL benchmark artifacts.
- Pre-1.0 additive trait and `const fn` polish: `DerefMut` on `Spanned<T>`, `Hash` on `Chomping`,
  `Frontmatter`, `ParsedFrontmatter`, `ayaml_edit::TextEdit`, and `ayaml_edit::EditErrorKind`,
  and `PartialEq`/`Eq` + `const fn` constructors on `ayaml_edit::EditOptions`.

### Changed

- `SpannedTree::lookup` now returns `Some(alias_node)` (with `SpannedNode::is_alias() == true`
  and `entry_span() == None`) for alias values instead of `None`, and `SpannedTree` now provides
  exact `entry_span` values for compact mapping first keys on sequence-item (`-`) lines (when no intervening
  comment would be deleted) and entries following safe block scalars.
- `SpannedTree::parse_with` with `ParseOptions::with_merge_keys(true)` conservatively suppresses
  `entry_span` across the tree so deleting a merged entry cannot mutate an anchored source
  mapping.
- `Deserializer::next` and `Deserializer::next_seed` now decode directly from spanned events
  without composing an intermediate owned `Value`.

### Deprecated

- Newly added `*_with_limits` shims (`from_str_with_limits`, `from_slice_with_limits`,
  `from_slice_borrowed_with_limits`, `from_reader_with_limits`,
  `DocumentDeserializer::from_str_with_limits`, and
  `DocumentDeserializer::from_str_compat_with_limits`) are deprecated alongside the existing
  `*_with_limits` shims in favor of `*_with` taking `&ParseOptions` / `&compat::ParseOptions`
  and are scheduled for removal in `1.0`. (`Value::apply_merge_keys_with_limits` takes no
  `ParseOptions` and remains stable in `1.0`.)

### Performance

- Faster implicit mapping parsing, line-indent calculation, `EventCursor` line chunking, scanner
  ASCII byte dispatch, plain scalar resolution, emitter scalar lookup tables, lookahead
  collection size hints, token-arena release before large whole-built target reads, non-cloning
  collection key comparisons, and linear-time complex-key resolution in `SpannedTree`.
- Boxed `Tag` interior (`48 B -> 8 B`, shrinking `Event` from `144 B` to `104 B` with zero public
  API changes) and internal `Box<Error>` hot-path results, eliminated the eager `LineStarts`
  pre-pass in favor of inline non-printable validation and zero-allocation lazy error coordinates,
  added safe 8-byte SWAR scanning (`#![forbid(unsafe_code)]`) for scalars and comments, replaced
  the `flow_ends` `HashMap` with inline `u32` partner indices and flow-scalar fast paths (`Cont`
  `<= 64 B`), deferred `Scanner` tag-handle maps, added a `<= 18`-digit `u64` integer fast path,
  and added specialized `EventSink` scalar callbacks bypassing `Event::Scalar` construction for
  internal composers and typed builders.
- Replaced whole-document upfront token/line buffering with a single-pass bounded `LineWindow`
  streaming parser (`<= 128` logical lines / `<= 384` tokens per batch) across `parse_value`,
  `parse_documents`, `parse_events`, `from_str*`, `SpannedTree::parse`, and `Deserializer`, cutting
  transient parser heap by >98% and reducing peak heap by 30% to 58% on large block and OpenAPI
  documents (`1.00x`–`1.05x` peak/live on OpenAPI fixtures); standardized internal hash maps/sets
  and node hashes on the process-wide cryptographic SipHash-1-3 `GlobalHasher` and added inline
  `<= 8`-key `SeenScalarKeys` for zero-allocation small-mapping duplicate checks during typed
  decoding.

## [0.2.3] — 2026-11-21

### Added

- Documentation book (`book.toml`, `docs/SUMMARY.md`, `docs/index.md`, `docs/getting-started.md`,
  `docs/security.md`, `docs/stability.md`, and the error code reference) built and deployed by
  `.github/workflows/docs.yml`, with every Rust example in `README.md` and `docs/` executed as a
  doctest in CI.
- `Error::masked()` and `MaskedError` (`Display`, `Debug`, `std::error::Error`, `Clone`, `Copy`,
  `PartialEq`, `Eq`, plus safe `kind`, `code`, `span`, `location`, and `limit` accessors), giving
  callers a document-text-free formatting view for logging or surfacing errors from sensitive or
  untrusted YAML documents.
- `ErrorCode` (`#[non_exhaustive]`, `as_str()`, `Display`, `AsRef<str>`), `Error::code()`, and
  `Error::with_code()`, exposing stable `yaml.<kebab-name>` failure codes across encoding,
  scanner, parser, composition, alias, duplicate-key, scalar-resolution, resource-limit, Serde,
  emitter, and I/O errors; documented in `docs/error-codes.md` alongside `ErrorKind`.
- `Clone`, `PartialEq`, and `Eq` implementations for `Error`.
- `ParseOptions` (`#[non_exhaustive]`, `new`, `for_max_input`, `with_limits`, `Default`,
  `From<Limits>`) and `EmitOptions` (`#[non_exhaustive]`, `new`, `flow`, `with_limits`,
  `with_flow`, `with_explicit_start`, `with_explicit_end`, `with_scalar_style`, `with_indent`,
  `with_indent_sequences`, `with_styles`, `Default`, `From<Limits>`, `From<EmitterOptions>`,
  `From<EmitterStyles>`) at the crate root, unifying resource limits, emitter presentation
  settings, and per-node style overrides into one options value per operation.
- `const` constructors and additive conversions/traits: `Limits::new`, `EmitterOptions::new`,
  `NodeStyle::new`, `PartialEq`/`Eq` for `EmitterStyles`, `compat::ParseOptions::for_max_input`,
  and `From<Limits>` for `compat::ParseOptions`.
- Unified `*_with` parsing and decoding entry points taking `&ParseOptions`: `parse_events_with`,
  `parse_value_with`, `parse_documents_with`, `from_str_with`, `from_str_seed_with`,
  `from_slice_with`, `Deserializer::from_str_with`, and `SpannedTree::parse_with`. Both
  `from_str_with` and `from_slice_with` accept owned and borrowed `Deserialize<'de>` targets, and
  `from_slice_with` checks `options.limits` before validating UTF-8.
- Unified compatibility `*_with` entry points (behind `libyaml-compat`) taking
  `&compat::ParseOptions`: `compat::parse_events_with`, `compat::parse_events_eager_with`, and
  `SpannedTree::parse_compat_with`.
- Unified `*_with` emitter entry points taking `&EmitOptions`: `to_string_with`, `to_writer_with`,
  `to_string_value_with`, `to_writer_value_with`, `to_string_documents_with`,
  `to_writer_documents_with`, `to_string_events_with`, `to_writer_events_with`, and
  `to_writer_event_stream_with`.

### Deprecated

All older combination functions remain available through `0.x` and are scheduled for removal in
`1.0`:

- Root parsing and decoding: `parse_events_with_limits` (use `parse_events_with`),
  `parse_value_with_limits` (use `parse_value_with`), `parse_documents_with_limits` (use
  `parse_documents_with`), `from_str_borrowed_with_limits` (use `from_str_with`),
  `from_str_seed_with_limits` (use `from_str_seed_with`), `Deserializer::from_str_with_limits`
  (use `Deserializer::from_str_with`), and `SpannedTree::parse_with_limits` (use
  `SpannedTree::parse_with`).
- Compatibility (`libyaml-compat`): `compat::from_str_with_limits` (use `compat::from_str_with`),
  `compat::from_str_seed_with_limits` (use `compat::from_str_seed_with`),
  `compat::deserialize_any_with_limits` (use `compat::deserialize_any_with`),
  `compat::parse_events_with_limits` (use `compat::parse_events_with`),
  `compat::parse_events_eager_with_limits` (use `compat::parse_events_eager_with`), and
  `SpannedTree::parse_compat_with_limits` (use `SpannedTree::parse_compat_with`).
- Emitters: `to_string_with_limits` and `to_string_with_options` (use `to_string_with`);
  `to_writer_with_limits` and `to_writer_with_options` (use `to_writer_with`);
  `to_string_value_with_limits`, `to_string_value_with_options`, `to_string_value_with_styles`,
  `to_string_value_with_limits_and_styles`, and `to_string_value_with_options_and_styles` (use
  `to_string_value_with`); `to_writer_value_with_limits`, `to_writer_value_with_options`,
  `to_writer_value_with_styles`, `to_writer_value_with_limits_and_styles`, and
  `to_writer_value_with_options_and_styles` (use `to_writer_value_with`);
  `to_string_documents_with_limits` (use `to_string_documents_with`);
  `to_writer_documents_with_limits` (use `to_writer_documents_with`);
  `to_string_events_with_limits` (use `to_string_events_with`);
  `to_writer_events_with_limits` (use `to_writer_events_with`); and
  `to_writer_event_stream_with_limits` (use `to_writer_event_stream_with`).

## [0.2.2] — 2026-11-20

### Added

- `compat::GlobalTags`, `compat::ParseOptions`, `compat::from_str_with`,
  `compat::from_str_seed_with`, and `compat::deserialize_any_with` (behind `libyaml-compat`),
  letting compatibility callers opt into `GlobalTags::Drop` to drop non-core global tags (such as
  `!!timestamp`, `!!binary`, verbatim URI tags, and `%TAG` URI handles) as a libyaml-based reader
  does, while keeping core-schema tags, the non-specific `!` tag, and local tags unchanged.
- `from_str_seed` and `from_str_seed_with_limits` at the crate root, mirroring the compatibility
  module's seed readers under the default YAML 1.2.2 rules and limits.

### Changed

- Refusing a node because of its tag now names the tag across typed and value deserializers (for
  example, ``invalid type: tagged value `!!foo`, expected struct S`` instead of `invalid type: map`,
  and ``invalid type: tagged value `!!binary`, expected a sequence`` instead of `invalid type: enum`).
  `deserialize_struct` now also hands a retained tagged node through `visit_enum` before failing,
  matching `deserialize_map` and `deserialize_seq`.

## [0.2.1] — 2026-11-20

First published `0.2.x` release (`0.2.0` was tagged but never published; see below). Carries every
breaking change, addition, fix, security update, and performance improvement listed under `0.2.0`,
plus the fixes below.

### Fixed

- On an input of more than 4 KiB read from events, a root enum target that swallows deserialization
  errors no longer hides a trailing document or a syntax error after the root; it now fails with the
  same document error as a tree read.
- The event writers now emit `~` for an untagged, unanchored empty plain scalar inside a flow
  sequence (such as the event stream of `[!!map ]` after dropping the mismatched core collection
  tag), so both readers reload it.
- The event writers now write alias mapping keys longer than 128 characters in explicit `?` form in
  both block and flow mappings, so aliases past the 1,024-character implicit-key limit emit YAML
  that every reader loads.
- The event writers now accept directives that hold a byte order mark inside a trailing comment
  (as produced by the compatibility event reader), while still refusing a byte order mark in a
  directive name or parameter.
- In compatibility mode, a non-handle local tag containing `@` and `!` (such as `!@!`) now stops at
  non-tag characters such as `[`, `]`, `<`, and `>`, rejecting unseparated flow indicators and
  non-URI characters as libyaml does.
- In compatibility mode, after an unseparated flow `?` (where default mode reads `?!...` as a plain
  scalar), non-word local tags such as `[?!)!,]` and secondary-handle suffixes holding `!` such as
  `[?!!a!b,]` now match libyaml instead of failing as an undefined handle or missing whitespace.

### Performance

- `EventCursor` (used by `parse_events`, `compat::parse_events`, `Deserializer`, and `SpannedTree`)
  now shrinks each parsed document chunk and moves it into the queue directly instead of copying it
  and retaining unused capacity.

## [0.2.0] — 2026-11-19

*Not published.* The `v0.2.0` tag was held before publishing after post-tag audits and differential
fuzzing found the fixes that ship in `0.2.1`; `0.2.1` is the first published `0.2.x` release and
carries every change listed below.

### Changed

The breaking changes come first, each with what a caller has to change.

- **Breaking: a span is two byte offsets.** `Span::start()` and `Span::end()` return `usize`,
  where they returned a `Marker`, and `Span::new` and `Span::empty` take offsets. A span
  converts to and from `Range<usize>`. For a line and column, build a `LineIndex` for the input
  and ask it for the `Position` of an offset in a `ColumnEncoding`. A `Position` counts from
  zero, for a machine; `Location::from(position)` counts from one, as a `Marker` did and as an
  error's location does. With `ColumnEncoding::Scalar` that is the line and column that a
  `Marker` held in 0.1.3, on a line that begins with no byte-order mark, and UTF-8 and UTF-16
  columns are there too. `Marker` remains, deprecated, for code that keeps one; nothing
  returns or takes one. `Location` is as it was.
- **Breaking: an error built by hand has no location until it is given one.**
  `Error::location()` still returns a one-based line and column for every error of a reader
  that has a span; one for input that isn't UTF-8 has neither, as in 0.1.3. `Error::with_span`
  no longer implies a location, and clears one that was set before it:
  set the span, and then the location with the new `Error::with_location`. Without a location
  an error prints no `at line L column C`.
- **Breaking: `compat::parse_events_with_limits` returns a lazy `EventStream`**, as
  `ayaml::parse_events_with_limits` does, where it returned a `Vec<Event>`. The new
  `compat::parse_events` returns the same. A caller that wants the vector calls the new
  `compat::parse_events_eager_with_limits`, or collects the stream. The stream reads one
  document at a time, so it reports a problem of an earlier document before a scanner's
  problem in a later one, which the vector reports first. Within one document both report a
  scanner's problem first, wherever it stands.
- **Breaking: a mapping iterates through this crate's own types.** `Mapping`, `&Mapping`, and
  `&mut Mapping` give `mapping::IntoIter`, `mapping::Iter`, and `mapping::IterMut`, where their
  `IntoIterator` implementations named `indexmap`'s iterator types. The entries, their order,
  and iteration from both ends with a known length are as they were, and the new types have
  the standard traits of the old ones, `Clone` and `Default` among them. A caller that named
  the old types names these, and one that called a method only the old types have, which are
  `as_slice`, `into_slice`, and `as_mut_slice`, collects the entries or uses the standard
  iterator methods. `Mapping::iter` and `Mapping::iter_mut` return the new types too, where
  they returned an unnamed iterator, so `mapping.iter().rev()` and `.len()` now compile; a
  caller with Clippy's pedantic lints is told to write `for entry in &mapping`. `indexmap`'s
  version is no longer part of this crate's API; the requirement is now 2.6, the first release
  whose owning iterator is `Clone`.
- **The writers refuse what no reader of this crate loads**, where they returned success and
  text that failed when it was read. The event writers refuse an alias with no anchor of its
  name earlier in its document, a `%YAML` or `%TAG` directive that neither the default reader
  nor the compatibility reader takes, and a second `%YAML` or a second `%TAG` for one handle
  in a document. An event reader reports an alias as it is written, so events read from a
  document with a dangling alias are refused when they are written back. The event writers
  hold the names of a document's anchors, at most `max_anchors` of them. The value writers
  count an empty collection as a level of depth, as the parser and the event writers do, so
  `- - []` is refused at `max_depth` 2. And they refuse a `!!int`, `!!float`, `!!bool`, or
  `!!null` tag on a scalar that the tag's rule doesn't take, such as `!!int` on the string
  `x`, before any of the document is written; such a value can only be built by hand. A
  caller that got one of these errors was writing text that no reader could load.
- The event size falls from 176 to 144 bytes, a span from 48 to 16, and an error from 112 to 104
  on 64-bit targets.
- An error about a stream that holds no document is now placed at the end of the input, where
  its value would stand; 0.1.3 gave it that byte offset with line 1, column 1.
- An error's `location()` is the place that a `LineIndex` gives for the start of its span,
  counted from one, on every line that begins with no byte-order mark. On a line that begins
  with one the columns can differ: the index counts as an editor does, a default reader leaves
  every mark that begins a line out of its columns, and a compatibility reader the run of
  marks that opens the input, of which it counts one where a document marker follows, as
  libyaml does.
- A reader of one document into a type, such as `from_str`, now reports the first problem in the
  document at every input size, as `parse_value` does: any that the scanner finds, and then the
  first that parsing or building meets. With a duplicate key and a later flow collection that
  never closes it reports the duplicate key, where 0.1.3 reported the syntax error. The
  `Deserializer` still reads a document's events before it builds from them, so there the syntax
  error comes first.
- A reader of one document into a type reads an input of more than 4 KiB from the parser's
  events (see Performance), which a target can notice in two ways. Its `Deserialize`, or a
  seed, may have read the start of a document that the reader then refuses, where 0.1.3 refused
  a document before the target saw any of it: the error is the same, but a seed that writes to
  its caller's data as it reads has written. And a collection tells its visitor no size, since
  its entries are counted only at its end; a root flow collection still does. `Value` and
  `Mapping` give back the room that growing without a size leaves spare. Another target keeps
  what its own growth leaves: a `Vec<u64>` of 1,500 items read from a block sequence has room
  for 2,048, where 0.1.3 gave it room for 1,500.
- Four of the fixes below refuse documents that 0.1.3 read: a bare variant name for a variant
  that holds a sequence or a mapping, a quoted scalar's continuation line that starts with a
  tab, a block scalar header after node properties that is not indented past its parent, and,
  in compatibility mode, a document such as `&a:[: x` followed by a `- y` line, where an
  anchor's name now ends before a `:` and a flow indicator in every check.
- Two of them change what a caller gets for a document that 0.1.3 took the same way. In
  compatibility mode a run of byte-order marks at the start of the stream is skipped as one
  mark is, so `"\u{feff}\u{feff}key: v"` has the key `key`, where 0.1.3 read the second mark as
  the key's first character, as libyaml does. And a block scalar that is not indented past its
  parent, which 0.1.3 refused in some documents with `ErrorKind::Parser`, is refused in all of
  them with `ErrorKind::Scanner`.

### Added

- `SpannedTree` and `SpannedNode` provide path lookup, value and entry byte spans, source scalar
  text, and conservative ambiguity handling for aliases, merge keys, duplicate keys, a mapping
  with an alias among its keys or a merge key inside one, and multiple documents. With the `libyaml-compat` feature,
  `SpannedTree::parse_compat` and its `_with_limits` form read as the compatibility reader
  does. An entry span is given only where deleting it removes that entry and changes
  nothing else: for a block mapping entry or block sequence item that begins its own line,
  through its last line break, with any nested value, block scalar, and trailing comment. A flow
  entry has none, and nor does an entry that shares its first line, has an explicit `?`, defines
  an anchor or stands inside a collection that has one, or is the only entry of its collection.
  A value that the source doesn't write has an empty span right after its key's `:`.
- `LineIndex`, `Position`, `PositionError`, and `ColumnEncoding` are the positions layer of the
  format contract that ayaml shares with adotenv, in its names and by its rules.
  `LineIndex::new(input)` borrows the input; `to_position(offset, encoding)` gives a zero-based
  line and column in UTF-8 bytes, Unicode scalar values, or UTF-16 code units, `to_offset`
  converts back, and `line_count` counts the lines. One byte-order mark that opens the input
  is in the offsets and out of the columns, and every other mark is a column of its line; a
  byte column needs no character boundary, and an offset between a CR and its LF is an error.
  A column in scalar values or UTF-16 code units reads its line up to the offset, so it costs
  more on a long line. The contract's position vectors are vendored under `tests/vectors/`
  and checked in CI. `ColumnEncoding` and `PositionError` are non-exhaustive,
  so a later release can add a unit or a reason without a break.
- `Error::with_location` sets an error's one-based `Location`.
- `compat::parse_events` reads a lazy `EventStream`, and `compat::parse_events_eager_with_limits`
  the vector that `compat::parse_events_with_limits` returned (see Changed).
- `Mapping::shrink_to_fit` gives back the room that a mapping's entries don't need.
- `to_writer_events_with_limits` writes collected events to a writer within a caller's limits,
  as the other event writers can.
- `Limits::for_max_input()` lifts the four count limits for a valid document within
  `MAX_INPUT_BYTES`, while the default profile stays memory-bounded.

### Fixed

- A bare variant name no longer fills a variant that holds a sequence or a mapping with an empty
  one, as D87 decided and `from_value` already said: `List` is an error for a variant
  `List(Vec<u32>)`. `List: []`, `List:`, and `!List` still read as the empty list, and a bare
  name still fills a unit variant and one that holds an `Option`.
- `from_slice` and `from_slice_borrowed` refuse input over the input limit before they check
  that it is UTF-8, so they read none of it.
- Mapping lookups by `&str`, including `get_str`, `get_str_mut`, `get`, `get_mut`, and
  `contains_key`, no longer allocate a `String` per lookup.
- A compatibility indentation mistake now names the collection whose indentation it breaks,
  as in `this line is indented differently from the mapping at line 2 column 1`. It was
  reported as unexpected content after the document, and in some documents as a block entry
  indented less than its parent or as a mapping line with no `:`. The error stands where it
  stood.
- A short quoted continuation with no closing quote now reports the opening quote.
- Serde `invalid_type` errors now name the actual unexpected value for string, enum, and struct
  targets.
- A compact block sequence that crosses `max_depth` is now refused while scanning, rather than
  after the whole input has been read.
- Compatibility flow implicit-key lookahead skips nested collections in one step instead of
  rescanning their content at every enclosing level.
- A plain scalar's event span and raw text now end at its last non-whitespace character, leaving
  comments, line breaks, and flow separators outside the scalar. The spans that ended where it
  did end there too: a collection's end, a document's start and end, and an error's. The blanks
  after a plain scalar no longer count toward `max_scalar_bytes`.
- A comment after a flow `?` in compatibility mode now ends at the same physical line ending as
  every other comment; NEL, LS, and PS remain content.
- A quoted scalar's continuation line that starts with a tab is now rejected even when spaces
  follow the tab; only spaces indent the continuation, as YAML requires.
- In compatibility mode, the line reader now ends an anchor or alias name before a `:` followed
  by a flow indicator, matching the scanner and the continuation indentation check.
- After a leading byte order mark, a compatibility error keeps its line and column, a quoted
  scalar's continuation check still applies, and a second stream-start mark is skipped instead
  of read as content. A run of marks reads as one mark does: a comment may follow it, with a
  plain scalar of several lines after that, and a document marker after it is text one column
  in, as after one mark.
- A block scalar header after node properties on their own line is now rejected when it is not
  indented past its parent, in both modes and after an anchor or a tag. The error is the
  scanner's, with `ErrorKind::Scanner`, and comes before a later problem in the document.
- A visitor that asks a mapping for a value after the mapping has told it the end is refused
  with `map value requested before key`. A reader that builds a tree, and `from_value`, gave
  it the last value that it had passed over.
- A top-level block scalar with indented empty lines now ends before a document marker, leaving
  an empty scalar and continuing the stream after the marker.

### Security

- An alias to a number is charged for the number's text where a typed read keeps it. A typed
  read into strings copies a number's spelling at every alias to it, and the alias budget
  counted one node for each, so a number spelled at great length was copied past the budget.
  It affects 0.1.0 to 0.1.3, where a target reads aliased numbers as strings.

### Performance

- Parsing holds and allocates less. Against 0.1.3, the workloads of the allocation gate
  allocate 25% to 73% fewer bytes in total while they parse: 32% fewer for a large scalar, 68%
  fewer for the generated short map, and 73% fewer for short explicit pairs. Their peak heap
  falls by up to 68%: by 34% on the short sequence, 35% on short explicit pairs, and 32% on the
  short map, while a large scalar's stays at its size. The gate now budgets total bytes per
  read as well as peak bytes and allocation counts, at the new measurements.
- What lowers them: the token arena is sized from the density of the input scanned so far, and
  short-line inputs get a wider first chunk. Scalar tokens keep their raw
  text as a span and box only the values that need decoding; typed nodes box tags, keep scalar
  text as a source slice, and cache hashes in one atomic word. Where a tree of the document is
  built, a root flow collection reserves its entry count before its items are built, so a wide
  flow collection avoids a final vector copy. With duplicate keys refused, the set of an open
  mapping's keys takes 24 bytes a slot, where it took a node's 80.
- A reader of one document into a type, such as `from_str`, builds its value from the parser's
  events when the input is more than 4 KiB, and keeps no tree of the document beside its tokens
  and its value. It reads a scalar and drops it, reads a collection entry by entry, and builds a
  tree only of what is read whole or again: a node with an anchor, a collection that keeps its
  tag, a mapping key, and what an enum reads. A root that is built whole is read with the tokens
  freed. The value and the error are those of a read that builds the tree first, which a
  refused input is read a second time to report.
- An untyped decode into a JSON tree through the compatibility reader, which is how issue #178
  measures, now peaks at 0.52 to 0.96 times the reference that the issue gives for its nine
  shapes: at 24.2 MB on a 1.1 MB service description, 96.9 MB on a 4.4 MB one, and 17.4 MB on a
  megabyte block sequence of short words. A 20 KB configuration fixture decoded into `Value`
  peaks at about 175 KB and keeps about 98 KB.
- Mappings and typed mapping keys cache their structural hash, so a mapping nested inside a
  mapping key is hashed once per level instead of once for every outer level and entry.

## [0.1.3] — 2026-11-15

### Fixed

- In compat mode, a named tag handle holds only word characters. A local tag such as `!@!`
  after a flow `?` is now read as a local tag instead of an undefined handle, matching
  libyaml; default mode still reads the whole `?!@! x` as a plain scalar.

## [0.1.2] — 2026-11-14

### Added

- `Limits::max_tag_expansion_bytes`, `Limits::with_max_tag_expansion_bytes`,
  `Limits::check_tag_expansion`, and `MAX_TAG_EXPANSION_BYTES` bound the memory that `%TAG`
  handles can expand to.
- The compatibility module now exposes `from_str_with_limits`,
  `from_str_seed_with_limits`, and `deserialize_any_with_limits`.

### Changed

- The compatibility entry points now have `_with_limits` forms for typed decoding, seeded
  decoding, and visitor decoding, so a compatibility reader can apply its own budgets.
- The published crate now uses caret requirements for `serde` and `indexmap`, includes its
  changelog, carries docs.rs feature metadata for `libyaml-compat`, and can run its tests from
  the unpacked `.crate`.
- Typed `from_str` now scans each input once for line starts and non-printable characters, skips a
  temporary `Value` for string scalars, and reserves collection capacity from Serde size hints.
  Its default and compatibility readers still return the same values and report the same errors.
- Typed decoding now rejects a sequence for a struct, an explicit null for a sequence, mapping,
  struct, or struct variant, and a non-string value for a string target under `from_value`. A null,
  boolean, or number scalar no longer names an enum variant, even when a variant is renamed to its
  spelling; a quoted scalar or a one-entry mapping names it instead. Tags read from a source keep a
  canonical handle and suffix across readers. Source readers still accept an empty source value as
  an empty collection or a struct's defaults; `from_value` and the owned stream readers treat a
  composed null as an explicit null, because a `Value` cannot distinguish the two.
- Writers now quote a string that looks like a decimal numeral with a leading zero, such as
  `0644`, `-0644`, or `0_644`, because YAML 1.2 keeps it a string but YAML 1.1 readers may read it
  as an octal integer. `docs/compatibility.md` lists the other YAML 1.1 hazards that can remain
  plain, including booleans, underscored and sexagesimal numbers, dates, `=`, and `<<`.
- From this release on, releases are published by a GitHub Actions workflow through crates.io
  Trusted Publishing. Each release's crate gets a SLSA build provenance attestation, and its
  GitHub release holds the crate and a SHA256SUMS file. `RELEASING.md` says how to verify a
  release against its tag and commit.

- A typed reader's error now names the path to what it rejected, as in
  ``servers[1]: unknown field `prot`, expected `host` or `port` at line 4 column 5``, on every
  typed entry point. `Error::message` documents how a path reads. The span and `ErrorKind::Serde`
  are as they were, and an error at the document's root gains nothing. The readers that decode
  from the input build the path only for an error. `from_value`, and the `Deserializer` methods
  built on it, keep each string key until its value has been read, so a key read as a string or
  a field name costs a copy of its text. So does an enum variant's name, which an error inside
  the variant names, but not a tag on a target that isn't an enum.
- In compat mode, an alias directly followed by a `:` that ends its name is now a mapping key, as
  in libyaml, so `*a: 1` has the key that `a` stands for. The `:` ends the name before a blank or
  the end of the line, and in a flow collection before a flow indicator too, as in `{*a:, b}`. An
  alias keeps such a colon only when an anchor earlier in the same document has that exact name.
  In a flow collection a name runs on through a `:` with more text after it, so `{*a:b}` still
  looks for an anchor named `a:b`, and only a JSON-like key takes a value right after its `:`, so
  `{*a:[1]}` is an error, where libyaml reads the value `[1]`. NEL, LS and PS aren't line breaks
  in compat, so `*a:` before one isn't a key. Default mode is unchanged.
- A malformed flow collection's error now says where it failed and where the collection opened,
  as in `unterminated flow sequence at line 3 column 1, opened at line 2 column 5`, in both modes.
  `Error::location` is where it failed, including at the end of the input, where it could be the
  collection's opening or the first line. `Error::message` holds the rest, as in
  `unterminated flow sequence, opened at line 2 column 5`. Compat words these failures as libyaml
  does, as in `did not find expected ',' or ']' at line 3 column 1, while parsing a flow sequence
  at line 2 column 5`, or `did not find expected node content` where a node belongs: where the
  input ends after `[`, `{`, `,`, `?` or `:`, and at a `}` after `?` or `:` in a flow sequence.
  After an empty `?` key in a flow sequence, libyaml takes the `:` or `,` that follows as part of
  the pair and then asks for `,` or `]`, and so does compat. A `?` directly before a flow
  indicator, as in `[?}` or `[?,`, is refused before that, in both modes, with its own error. It takes the opening from the parser,
  so a bracket in a quoted scalar is never named. Its text differs from libyaml's in three ways: where
  libyaml says it is parsing a flow node, compat names the collection; where the input ends
  without a line break, compat names the end of the last line, and libyaml the start of the next;
  and compat names an opening at line 1 column 1, which libyaml's binding leaves out. An error
  about a node itself keeps its wording and names no opening, as `aliases cannot carry tags or
  anchors` does in `{!t *a`.

- Every entry point that takes limits, the emitters included, now refuses a `node_cost_bytes` of
  0, which made each node an alias copies free and so turned the alias budget off. The least cost
  is 1. As for a `max_depth` above `MAX_DEPTH_CAP`, the error is `ErrorKind::Limit` with no
  structured value or span. `docs/limits.md` now says what a read holds in memory, and that a
  block scalar's size, for `max_scalar_bytes`, counts its header and indentation.
- `max_events` now counts each event once, on every reader, and counts the tokens a read keeps
  against the same limit: scalars, anchors, aliases, tags, indicators, document markers, and
  directives. A line break, and so a blank or comment line, counts as neither, where each one
  counted before, so a million of them between two entries refused a document of about 1 MB. The
  stream's start and end now count as the events they are on every reader, as they did on
  `parse_events` and the `Deserializer` alone, so an input exactly at the limit on another reader
  may need 2 more. `docs/limits.md` says what each limit counts.
- `max_anchors` is now documented as counting the anchors of a stream, as every reader already
  enforced it. It said one document.

### Performance

- Tag handles a document doesn't use no longer allocate their built-in prefixes. The smallest
  inputs take about 15% fewer instructions.

### Fixed

- An unknown core-schema tag on a scalar, such as `!!binary` or `!!timestamp`, is now kept as
  an application tag instead of being dropped. Event streams, composed values, and typed readers
  agree on it.
- A core-schema tag on a node of another kind is now dropped in both default and compatibility
  modes, on every reader. `!!str {}` and `a: !!int [1]` read the collection without the tag, and
  `!!seq a` reads the string `a`. Custom tags are still kept.
- In compat mode, inputs that default mode and libyaml both read no longer fail where the two
  rules meet. This covers a flow `?` before `-` or a `!` tag, anchors ending at `?` or `:`, a
  block scalar under an anchored `:` key, NEL comments and anchors, a `...` marker after a
  stream-start BOM, and quoted scalars below a BOM key. The compatibility notes say where a
  reading remains a deliberate deviation.
- Typed readers now compare scalar mapping keys by their resolved kind and rendering, so a null
  key and an empty-string key are distinct, while two spellings of the same scalar remain
  duplicates and keys that differ only in type are rejected before a narrower target can keep
  only one.
- Serializing a deeply nested Rust value now returns a depth error before it can exhaust the
  stack. `to_string`, `to_writer`, and `to_value` use the parser's 128-level limit. A tag counts
  toward the stack bound but not the collection depth, so a value at the cap still serializes.
- The plain-scalar fast path added in 0.1.1 is now checked against the full continuation
  analysis over generated multi-line scalars. Multi-line plain scalars after a whitespace-only
  line and after a property-led key parse as in 0.1.0, and the fast path does not end them early.
- Block scalars now follow the grammar for whitespace-only lines. A scalar with no content lines
  is empty, and the node after it parses; a comment at or below the parent's indentation ends it;
  a tab after the content indentation is content, not an empty line or a comment; and a comment
  line that starts with a tab directly after a scalar is rejected. A root block scalar's
  explicit indentation indicator counts from the root's -1, so `|1\n y\n` keeps its space in
  default mode; compat keeps libyaml's `y\n`.
- Error diagnostics now agree across readers. A typed reader points at the duplicate key rather
  than its first occurrence, and a default-mode typed map reports scalar keys that differ only in
  type, as `1` and `"1"`, instead of keeping one silently. A complex or tagged duplicate key
  prints in flow form and stays short. The parser names a missing `:` line, an entry indented
  less than its sequence, and a block scalar not indented past its parent. Compat maps an error
  by a private failure code rather than by message text, keeps the location of depth and
  document-count failures, and reports an error inside the first document before a later
  document. A scanner depth limit hit while reading ahead no longer hides an earlier token
  error.
- A block scalar is now built as it is read. Its lines were listed first, so a read of a 4 MiB
  block scalar of short or blank lines took 25 to 42 times its size in resident memory at its
  peak, where it now takes about 5 to 10 times. Counting the capacity it reserves without
  touching, a literal scalar of blank lines reaches 12 times. The index of line starts is sized before it is built, at 8 bytes
  for each CR or LF, so it no longer grows by doubling.
- The scanner's token arena now grows by a sixteenth of itself once it holds more than 16 chunks.
  Growing by a fixed chunk alone copied it a number of times quadratic in its length wherever
  `realloc` copies, as for a flow collection of a million tokens.
- An error's text is no longer read to decide whether it says where the error is. A message that
  quoted a value holding the words `at line`, as in `a: "at line 9"` read into a number, lost
  its location.
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
  `[[x\n, y]]`, `!!seq [x\n, y]`, `'a\nb'` indented by two spaces, and `[\n\ta\n]` now parse. Under a block
  collection the indentation of those lines is now checked, wherever the node starts, so
  `k:\n  [\na\n  ]` and `---k: 'a\nb'` are now errors. Compat mode still reads the first.
  A comment line is exempt in every position, so `k: [\n# c\n  a\n  ]` now
  parses, and so is a closing `]`, `}`, or quote, as it was before, unless tabs alone indent a
  quote. A later entry below holds plain scalars' lines and the innermost collection to it too.
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
  and `!y!Foo [1]` selects the enum variant that `!!app/Foo [1]` selects.
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

- `parse_events` and the `Deserializer` counted each event twice against `max_events`, so they
  refused an input at about half the limit that the other readers allowed. They count it once now,
  so they accept what the other readers accept at the same limits. Reading a document at a time,
  they can still stop at an earlier document of a stream than the other readers, as
  `docs/limits.md` says.
- A `key: value` or `? key` entry of a flow sequence now counts toward `max_depth` as the mapping it
  is on the event readers too. `parse_events` read `[a: [1]]` under a `max_depth` of 2, which the
  readers that build values refuse.
- A flow collection that is a block mapping's key now counts toward `max_depth` inside the mapping
  on the event readers too. `parse_events` read `[a]: c` under a `max_depth` of 1, and a key of 128
  nested sequences at the default limits, which the readers that build values refuse.
- `max_scalar_bytes` now bounds a scalar's value as well as its text. A double-quoted `\L` or `\P`
  takes 2 bytes of text and 3 of value, so a scalar of them could hold half as much again as the
  limit.
- The typed readers that decode from the input now charge an alias to a tagged node for the node
  that holds the tag, as `parse_value` and `Deserializer::next` do. They charged `node_cost_bytes` less for
  each tag, so an alias to tagged nodes could copy more than the budget let `parse_value` copy.
- The stream start and end events from `parse_events` now carry the places that the eager event
  reader gives them. The end said line 1, column 1, at the end of the input, and the start, after a
  byte order mark, the mark's place.
- An empty node where the input ends, as after `a:`, and the end of its document, now carry their
  line and column, and so do errors there: `a:` read into a struct with a number
  field said `at line 1 column 1`, and now says `at line 2 column 1`. From `parse_events` and the
  `Deserializer`, an empty node at the end of a document that the next `---` starts also had the
  input's length as its offset.
- A tab now separates wherever it doesn't stand for indentation, in both modes, as YAML 1.2.2
  allows. A tab with a colon and a space anywhere later on its line was refused as block structure, so
  `key:\t"a: b"`, `key:\t{a: 1}`, `a: "1"\t# note: x`, `"a"\t: 1`, `!t\ta: 1`, `[1]\t: x`,
  `-\t'a: b'`, `---\t{a: 1}`, and a tab before a comment holding a colon and a space on its own line were errors.
  A tab is now refused where it starts a line or follows a `-`, `?`, or `:` indicator, `---`, or
  `...`, and a block indicator or an implicit key follows it, as in `\ta: 1` or `-\ta: 1`. A later
  entry below refuses a node that only tabs indent on its own line, as in `k:\n\tv`.
- A `-` or `?` indicator with a tab after it, as in `\t-\tb` or `-\t-\tb`, is now refused after a
  tab, as it is with a space after it. It was read as an entry indented by the tab.
- A closing quote on a line indented only with tabs now closes its scalar, as a closing `]` or `}`
  there does, so `k: 'a\n\t'` reads `a` followed by a space. Only the scalar's own quote counts, so `k: "a\n\t'"` is
  still an error.
- A plain scalar's line may now start with `---` or `...` that isn't a document marker, in both
  modes. Only one at column 0, with whitespace or the line's end after it, is a marker, so
  `k: a\n  ...and so on`, `a\n ---x`, and `a\n---b` now parse, where the line ended the scalar and
  was an error. In compat, a `...` followed by a byte order mark isn't one either, so
  `0\n...\u{FEFF}` reads `"0 ...\u{FEFF}"`, as in libyaml. Default mode refuses a mark there.
- A plain scalar's line may now hold a comment with a `:` in it, in both modes, so
  `k: a\n  b # note: d` reads `a b`. The comment's `:` ended the scalar as if it were a key's.
- By default, a plain scalar after a node property that ends in `:`, as in `&a: b`, may now go
  on at its own line's indentation when the node starts its line or follows `---`, as it may in
  compat. So `&a: b\nc` reads `b c`, where default mode ended the scalar at its first line and
  refused the next.
- A quoted scalar on its own line under a block collection, or in a flow collection that is, may
  now go on at any column past that collection's, whatever the indentation of the line it opens
  on, in both modes. So `k:\n  'a\n b'` and `k: [\n    'a\n  b']` now parse. Its lines are still
  held to that column, and only spaces indent them, so `k:\n  'a\n\t\tb'`, which read `a b`, is
  now an error, as `k: 'a\n\t\tb'` already was.
- A comment may now follow a `%TAG` directive, in both modes, so `%TAG ! tag:x: # c` parses. The
  directive's event keeps the comment in its value, as a `%YAML` directive's already did.
- The typed readers now read a node with a custom tag, one outside YAML's own
  `tag:yaml.org,2002:` names, into a target that isn't an enum, a string, or a `Value` as they
  read it untagged. So `!port 80` reads into a `u16`, and `!list [1, 2]` into a `Vec`, where both
  were type errors. This holds on every typed reader, `from_value` included, and for what an enum
  variant holds, so `Pair: !t [1, 2]` reads into a tuple variant and `Nothing: !t` into a unit one. A
  YAML tag keeps its meaning, so `!!str 5` is still a string, and an error read into a number. An
  `Option` reads a tagged node as `Some`, since what it holds may take the tag, so `!port ~` is an
  error read into an `Option<u32>`. A target that Serde reads through `deserialize_any`, such as
  an untagged or internally tagged enum or a flattened field, still gets the tag as an enum.
- A tag with no value, as in `!Point`, now reads into a struct variant as an empty value reads
  into a struct, so its fields take their defaults, and so does a variant's name as a mapping key
  with an empty value, as in `Point:`. A bare `Point` still names a unit variant, and is an error
  there. So is `!Point ~`, except from `from_value` and the `Deserializer` methods built on it,
  which can't tell `~` from an empty value, tagged or not. They read `!list ~` into a `Vec` as
  empty too, as they read `~`.
- A float target no longer reads from the input text that isn't a number's spelling, such as `inf`
  or `NaN`, a spelling too large for a float, such as `1e999`, or a scalar tagged as a string, such
  as `!!str 1.5` or `! 5`. Each read as infinity, NaN, or a number, and is now an error, as it
  already was from `from_value`. A spelling with a leading zero, as in `01`, still reads.
- `&str` and `Cow<str>` targets now borrow a plain scalar that a comment or blanks follow on its
  line, as in `x: abc # c`. The scalar's text held those blanks, so `&str` failed and `Cow` was
  silently owned.
- These invalid forms are now errors in both modes, as YAML 1.2.2 says, and as libyaml says too:
  - a block scalar header with two chomping or two indentation indicators, as in `|++`, `|+-`, or
    `|11`, or with blanks before an indicator, as in `| -`. The last of each kind counted, and an
    indicator after blanks counted as one;
  - a tag handle with no suffix, as in `!!` or `!e!`, and the empty verbatim tag `!<>`. Each read
    as a tag named by the handle's prefix alone, or by nothing;
  - a second `%TAG` for a handle in one document, even with the same prefix. The last one counted.
- The verbatim tag `!<!>` is now an error in both modes, since `!` alone names no local tag
  (YAML 1.2.2 example 6.25). It read as the non-specific tag `!`, as libyaml reads it.
- By default, a byte order mark in a comment after a block scalar's header is now an error, as it
  is in any other comment. Compat still reads one there, as libyaml does.
- These lines are now held to the indentation YAML 1.2.2 gives them:
  - a node on its own line that only tabs indent, under a key or an entry, as in `k:\n\tv`,
    `k:\n\t{a: b}`, or `-\n\t[a]`, in both modes, as libyaml refuses it too. Only spaces indent,
    and the node read as indented by the tab. The error says the tab doesn't count as
    indentation. A space before the tab still indents it;
  - by default, a flow collection's line under a block collection, which must be indented past the
    innermost one, whether a plain scalar goes on there or an entry starts there, as in
    `k: [a\nb]` or `a:\n  b: [c,\n  d]`. The error now names where the collection opened, since
    one left open is the usual cause. Compat still reads such a line, as libyaml does, unless an
    implicit flow key's `:` sits on it, as in `k: {x\n: y}` or `k: {"x"\n:y}`, or only a tab
    indents a line that a plain scalar goes on to, as in `k: [a\n\tb]`. libyaml refuses both, and
    compat now passes YAML Test Suite case VJP3#0. An explicit key's `:`, as in `k: {? x\n: y}`,
    and a closing line, as in `k: [a\n\t]`, still read in compat, as libyaml reads the first;
  - a quoted scalar's line that starts with the other kind of quote, or with `''` in a
    single-quoted scalar, as in `k: "a\n'b"`, in both modes. Only the scalar's own closing quote
    may sit left of the indent;
  - by default, a quoted scalar's lines after a compact key or entry, which are now held to the
    collection that the key or entry starts, as a plain scalar's are. `- k: 'a\n b'` needs three
    spaces before `b`, and read with one. Compat still reads it, as libyaml does, since real files
    align such lines with the key;
  - by default, an empty line inside a multi-line scalar under a block collection that holds a tab
    before the scalar's indentation, as in `k:\n  a\n\t\n  b`, `k: [a\n\t\n b]`, or `k: "a\n\t\n b"`
    (production 70). Compat refuses it in a plain scalar, as libyaml does, and still reads it in
    a quoted one, as libyaml does. An empty line after the scalar, as in `k: a\n\t\nj: b`, still
    reads in both modes, as YAML 1.2.2 says, where libyaml refuses it after a plain scalar.
- A document marker line now ends a plain scalar in a flow collection, which then refuses it, in
  both modes, as libyaml does. `[a\n--- b]` read `a --- b`.
- These invalid forms are now errors in both modes, as YAML 1.2.2 says:
  - a block collection on a `---` line, as in `--- a: b`, `--- - a`, or `--- ? a`, on every
    entry point. A block collection starts after a line break, and libyaml refuses these too. The
    eager readers read one at the start of the stream, and the `Deserializer` and `parse_events`
    after another document too, where the eager readers' error named the document start as a
    misplaced node. A block mapping now ends at a `---` or `...` line, so the error is the same on
    every entry point. A scalar or a flow collection may still sit on the `---` line;
  - a flow collection's `?` directly before a flow indicator, as in `[?]`, `{?}`, `{?,a}`, or
    `[?[a]]`. It is neither a key's indicator, which needs a space or a line break after it, nor
    a plain scalar. By default the first three read as an empty key, and a `?` before `[` or `{`
    was already an error. Compat read each of them, `[?[a]]` as the key `[a]`, and libyaml reads
    each but `[?]`, so compat now refuses what libyaml reads. Where libyaml refuses such an input
    too, as in `[?}`, compat now gives this error, not libyaml's. `[? ]` and `{? }` still read,
    and compat still reads `[?x]` as libyaml does.
- By default, an empty line indented no more than the line a key starts no longer ends the key's
  multi-line plain value, so `a:\n  k: x\n \n   y` reads `x\ny`, as compat and libyaml read it. It
  was an error.
- A tag now names an enum's variant by its whole name on every typed reader, `from_value`
  included: a local tag by all of it after its first `!`, as readers built on libyaml name it, and
  any other tag by its full name in angle brackets. A tag written with a `%TAG` handle selected the
  variant named by its suffix alone, so under `%TAG !e! tag:x:`, `!e!B 5` read as the variant `B`,
  while `!<tag:x:B> 5`, the same tag, named `tag:x:B`. Both now select the variant named
  `<tag:x:B>`. A core tag names its whole name too, so `!!C {x: 1}` no longer reads as the variant
  `C`. A local tag that a named handle writes is named as when written with `!`, so `!e!B` selects
  `pre-B` under `%TAG !e! !pre-`, and `!e!bang` selects `!bang` under `%TAG !e! !!`, as libyaml
  reads them. A `Value` holding the first, written through Serde as to JSON, names it `!pre-B`,
  where it named `!<!pre-B>`.
- An enum's mapping of one entry whose key is under the non-specific tag `!`, as in `! B: 5`, now
  names the variant `B` on every typed reader, as `B: 5` does, since `!` makes a scalar a string.
  It was the error "enum variant key must be a string". A key under any other tag but `!!str`, as
  in `!x B: 5`, is still that error (D80).
- A stream of several documents written with `with_explicit_start(true)` had `---` twice before
  each later document, which read back as an empty document more. Each document now gets one.
- A tag that a verbatim tag can't hold now reads back as itself from every writer. Under
  `%TAG !e! tag:x:`, `!e!a%20b` names `tag:x:a b`, which was written `!<tag:x:a%20b>` and read
  back as another tag, since a verbatim tag is read as written. A tag whose name has no scheme,
  as `!e!bar` under `%TAG !e! foo`, was written `!<foobar>`, which YAML doesn't allow. The
  writers now write such a tag as a shorthand under a `%TAG` directive that they add to the
  document, as `%TAG !t0! tag:x:` and then `!t0!a%20b` (D82). Every other tag is written as before.
  A tag that is neither local nor core and has fewer than two characters, as `Tag::new("x", "")`
  makes, has no form in YAML, and writing it is now an error, where it was written as `!<x>`,
  which YAML doesn't allow either, and the reader refuses it too. The
  event writer now holds each document until it ends, to put the directives first, so it holds as
  much as the largest document, which `max_output_bytes` bounds.
- A verbatim tag that isn't a tag name is now an error in both modes: a name that is neither local
  nor has a URI scheme, as `!<]>`, `!<b.>`, `!<foo>`, or `!<$:?>`, and a `%` that doesn't start two
  hex digits, as in `!<tag:x:50%>`. The writers already refused a one-character name as a verbatim
  tag and wrote any other name that no verbatim tag can hold under a `%TAG` directive, so the
  reader and the writers now agree on which verbatim tags exist. `!<tag:x:50%25>` still names
  `tag:x:50%25`, and `!<tag:x$:?>` still names `tag:x$:?`.
- A tagged key in a flow mapping is now written with an explicit `?` and a space once it and its tag pass
  128 characters, as libyaml's emitter does. So is a string key that the scalar style option or a
  flow collection quotes, once it passes 128 characters with its quotes. Readers built on libyaml
  refuse an implicit key past 1024 characters, in a flow mapping too.
- Serializing a struct whose flattened fields repeat a key, or a map that gives a key twice, is
  now an error, "duplicate entry with key ...". The value kept one of the entries and dropped the
  other.
- A key or value whose `Display` fails is now an error from `to_value`, `to_string` and the other
  writers. A key's text was cut where the failure came, and `collect_str` panicked.

### Security

- Resource-exhaustion and wrong-value handling in 0.1.0 and 0.1.1 is covered by
  [GHSA-f77x-cp9r-m4p3](https://github.com/asamarts/ayaml/security/advisories/GHSA-f77x-cp9r-m4p3).
  Inputs include a long `%TAG` prefix repeated across tags, an alias to a long-tagged node, and
  many `%TAG` directives; 0.1.2 bounds each under the documented limits. `0.1.0` and `0.1.1`
  remain published and are not yanked; users should upgrade.

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

[0.2.0]: https://github.com/asamarts/ayaml/releases/tag/v0.2.0
[0.1.3]: https://github.com/asamarts/ayaml/releases/tag/v0.1.3
[0.1.2]: https://github.com/asamarts/ayaml/releases/tag/v0.1.2
[0.1.1]: https://github.com/asamarts/ayaml/releases/tag/v0.1.1
[0.1.0]: https://github.com/asamarts/ayaml/releases/tag/v0.1.0
