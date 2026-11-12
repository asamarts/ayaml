# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

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

### Fixed

- A core-schema tag on a node of another kind is now dropped in both default and compatibility
  modes, on every reader. `!!str {}` and `a: !!int [1]` read the collection without the tag, and
  `!!seq a` reads the string `a`. Custom tags are still kept.
- In compat mode, inputs that default mode and libyaml both read no longer fail where the two
  rules meet. This covers a flow `?` before `-` or a `!` tag, anchors ending at `?` or `:`, a
  block scalar under an anchored `:` key, NEL comments and anchors, a `...` marker after a
  stream-start BOM, and quoted scalars below a BOM key. The compatibility notes say where a
  reading remains a deliberate deviation.
- Typed readers now compare scalar mapping keys by their resolved value, so a null key and an
  empty-string key are distinct, while two spellings of the same scalar remain duplicates.
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
