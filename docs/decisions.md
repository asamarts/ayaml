# ayaml decisions

Technical decisions made since this repository became the development home. Each
record states what was decided and why, so a later reader can tell whether the
reason still holds. Numbering continues from earlier records that aren't published,
which is why it starts at D65 and skips numbers.

## D65 — Development happens here, through pull requests

**Date:** 2026-11-05
**Status:** Accepted

**Decision.** Source, tests, CI, releases, and the task plan under `aplan/` live in
this repository. Every change reaches `main` through a squash-merged pull request,
with required checks and a leak scan, and that includes maintainers.

**Why.** One repository owns the code, so there's no second copy of it to keep in
sync. `aplan verify` checks that evidence commits and tests exist in the repository
holding the tasks, so the tasks have to sit beside the code.

**Consequences.** Each change waits for CI. Every push is public at once, so the
leak gate in `tools/leak-scan/` runs before a commit is pushed and on the pull request. It
can't read a squash commit's message until that message is public, so write it from text
that has already passed the gate, and check the run on `main` after merging.

## D66 — The leak gate judges a change from the base commit

**Date:** 2026-11-05
**Status:** Accepted

**Decision.** In CI, `gate.py` and its configuration come from the pull request's base
commit, and the head is read as data. The head's copy of the gate runs only its tests,
last, with no secret. Locally, the installed hook runs the gate from `origin/main`.

**Alternatives.** Keep running the head's copy, which is simplest, but then a change can
turn off its own check. Move to `pull_request_target`, which would also take the workflow
file from the base, but runs with secrets and a write token next to untrusted changes,
the combination GitHub's security guidance warns about.

**Why.** The gate exists to stop a mistaken or careless change, so that change shouldn't
be able to edit what judges it.

**Consequences.** Editing the workflow file can still weaken the job, and only review
catches that. A change to the gate takes effect after it lands. GitHub gives no secrets to
pull requests from forks, so the job fails on them by design, and a maintainer who has read
the change brings it onto a branch here. Dependabot's pull requests read only Dependabot's
secrets, and since 2026-11-07 the pattern list is one of those too, so the gate runs on them.

## D67 — Dependencies are checked against an allowlist

**Date:** 2026-11-05
**Status:** Accepted

**Decision.** `tools/check_deps.py` fails on any package in the shipped graph that isn't
listed with a reason. It reads `cargo tree` for the `ayaml` package with every feature on.

**Alternatives.** Keep a list of rejected parsers, which names other projects in this
repository and misses anything not on it.

**Why.** An allowlist fails closed on what nobody has looked at, and it names only what
ayaml uses.

**Consequences.** Adding a dependency, even a transitive one, takes a line here.

## D68 — CI checks allocations and instruction counts, not timing

**Date:** 2026-11-05
**Status:** Accepted

**Decision.** CI enforces allocation budgets and fails on a rise of more than 5% in the
instructions any operation executes on a fixed input, compared with the pull request's
base. A deliberate rise goes in `tools/instruction-rises.tsv` in the same change, with its
reason. Timing is compared only on a quiet machine, before a release.

**Alternatives.** Check nothing between releases, so a slowdown is found late and is hard
to trace. Time on hosted runners, where the limit has to be so loose that it misses real
slowdowns and still fails at random.

**Why.** Counts repeat to within about 1% from run to run on a loaded machine, and they
compare ayaml only with itself, so nothing private is involved.

**Consequences.** One more required job. Counts miss cache and branch effects, so a
slowdown of that kind still waits for the release check.

## D69 — The fuzz workspace may hold a counting allocator's `unsafe`

**Date:** 2026-11-05
**Status:** Accepted

**Decision.** `unsafe` stays forbidden in this workspace. `fuzz/src/bin/memcount.rs` may use
it, because a global allocator can't be written without it. `fuzz/` isn't published, and the
parser's dependency graph can't reach it.

**Alternatives.** Measure through a third-party allocator crate, which adds a dependency and
still has `unsafe`.

**Why.** Allocation budgets need an exact count, and the code is about 30 lines.

**Consequences.** Claims about `unsafe` name the published crates or this workspace, not
everything in the repository.

## D70 — A fuzz target's skip for a known bug is temporary and exact

**Date:** 2026-11-05
**Status:** Accepted

**Decision.** A fuzz target may skip inputs that hit a bug already tracked in a task. Its
comment states the exact condition and names the task, and the skip is removed when that task
closes. All fuzz targets compile on every pull request.

**Alternatives.** Let the nightly run fail until the fix ships, which hides new failures
behind the known one. Drop the assertion, which loses it for every other input.

**Why.** A skip that's wider than the bug hides other bugs, and one that's narrower breaks the
nightly run. Both have happened.

**Consequences.** A known-bug skip in `borrowed` stays until AY-025 closes.

## D71 — No `cargo-vet` until the dependency graph changes hands or grows

**Date:** 2026-11-05
**Status:** Accepted

**Decision.** ayaml doesn't use `cargo-vet` for now. Adopt it if an outside pull request adds a
runtime dependency, or if the runtime graph grows.

**Alternatives.** Adopt it now, or drop the idea.

**Why.** The runtime graph is `serde` and `indexmap` and their support crates, and
`cargo-deny` and `tools/check_deps.py` fail closed on anything else.

**Consequences.** Nobody attests to dependency code in the meantime.

## D72 — The name `parse` is reserved for a recovering parser

**Date:** 2026-11-05
**Status:** Accepted

**Decision.** ayaml builds no recovering parser until a YAML language server or linter is
committed to. If one comes, `parse` recovers and `parse_strict` returns `Result`, as in adotenv.
Until then ayaml leaves the name `parse` unused, and it never adds `parse_recovering`.

**Alternatives.** Build it after 0.2, or choose `parse_recovering` when the time comes.

**Why.** Recovery is much harder in YAML, where an indentation error can change the meaning of
what follows, and nothing needs it yet.

**Consequences.** Broken YAML still stops at the first error.

## D73 — Tag expansion has its own limit

**Date:** 2026-11-06
**Status:** Accepted

**Decision.** `Limits::max_tag_expansion_bytes`, 64 MiB by default, counts the whole prefix at each
use of a tag handle across a stream. Going over it is `LimitKind::TagExpansion`.

**Alternatives.** Charge tags to the alias budget, which would add no field but would make `!!str`
fail for anyone who lowered that budget to forbid aliases. Cap the length of a `%TAG` prefix, which
leaves about 100 times the input at a 1 KiB cap. Share one prefix among tags, which stops the copies
held by tags but not the copies that Serde names and the split at the core prefix make.

**Why.** A count of expansions bounds what handles cost on every path, and it can't reject a
document for anything but this cost. A tag that an alias copies counts against the alias budget, with
the rest of what the alias copies. `max_events` stops a stream first, at between 250,000 and 500,000
tagged nodes depending on how they're written, so 64 MiB holds every use it allows of a prefix up to
about 134 bytes, more than any real document needs. A stream at both limits, written as a flow sequence
of empty tagged nodes, has a peak resident size of about 470 MiB from less than 4 MiB of input on
`next_borrowed`, the costliest reader, where the same stream takes about 365 MiB with a short
prefix. On `parse_value` the two are about 370 and 260 MiB. Live heap peaks about 30 to 60 MiB below
each of those.

**Consequences.** `Limits` and `LimitKind` each gain a member. Both are `#[non_exhaustive]`, so this
isn't a breaking change. A document with more expansions than this needs a higher limit.

## D74 — A hosted fuzz run publishes no input

**Date:** 2026-11-06
**Status:** Accepted

**Decision.** `tools/fuzz-hosted.sh` keeps a hosted fuzz run's output off the public log, which shows
only the target, the exit status, the run count, how many files a green run left, and the hash of an
encrypted file. On a failure, or a green run that left files such as a slow unit, the fuzzer's log and
those files upload encrypted to the key in `tools/fuzz-crash-recipient.txt`, and only the maintainer
holds the private key. If encrypting fails, nothing is kept.

**Alternatives.** Filter the log and upload nothing, which needs no key but can lose a rare crash for
good. Fuzz on a private host, which gives up free hosted runs. Publish the inputs on the grounds that
anyone can run the same fuzzer, which publishes a reproduction before a fix exists.

**Why.** `CONTRIBUTING.md` keeps reproduction inputs for an unfixed resource or correctness problem out
of public text, and a crash would otherwise break that by itself: libFuzzer and `cargo-fuzz` both
print the input, and the artifact held it. Hosted runs cost nothing and catch regressions every day.
A crash file's name, and any hash of its contents, isn't published either, because a short input could
be guessed from it.

**Consequences.** A failed run shows that a target failed, and no more. Reading what it found takes the
key: `gh run download RUN_ID`, then `age -d -i KEY FILE | tar -xz`. Losing the key loses access to past
crashes only: generate another and replace the recipient file. Local fuzz runs still print everything.
The scheduled watch leaves its issue open until someone closes it, because a later green run doesn't
read what an earlier failed one sealed, and the artifact expires. It also reports a green run that
left a sealed file, and a green run whose file couldn't be sealed fails.

The corpus cache stays plain text, and a fork's workflow can restore it. It holds only inputs that
didn't stop the fuzzer, without any file the run left, such as a slow unit, and anyone can grow the
same corpus by running the same fuzzer, so encrypting it would add a secret and more steps for little
gain. One run from before this decision printed an
input, and its log and artifact were deleted on 2026-11-06.

## D77 — A rehearsal creates and deletes a draft release, and attests nothing

**Date:** 2026-11-07
**Status:** Accepted

**Decision.** Started by hand, `release.yml` rehearses the GitHub release too. It downloads the
crate crates.io serves for the rehearsed tag, creates a draft release named `rehearsal-RUN-ATTEMPT`
with that crate, `SHA256SUMS`, and the release notes, checks the draft as a rerun checks a release,
and deletes it. The release job and the rehearsal run the same script, `tools/github_release.sh`.
A rehearsal makes no attestation.

**Alternatives.** Stop at the comparison with the registry, as before, which leaves the release
job's first run to the first release. Attest the rehearsed crate as well, which would run the
attestation step, but each rehearsal would add a permanent public record for a published crate,
signed by a branch's workflow where the release notes name a tag's. Release a throwaway tag, which
can't be removed once immutable releases are on.

**Why.** The release job is the step that can leave a version on crates.io with no GitHub release,
and it had never run. A draft is visible only to the repository's writers, makes no tag, and can be
deleted with immutable releases on, so a rehearsal can run the job's download, checks, and gh
calls on a draft and leave nothing. A
public repository's attestations go to a transparency log that can't be edited, so an attestation
can't be rehearsed and then taken back.

**Consequences.** The rehearsal's job has `contents: write` for its draft. It builds nothing and runs
only the scripts of the branch it was started from, and only a writer can start one. A run
cancelled between creating and deleting the draft could leave it, so the job's last step deletes
it, and only a draft with a rehearsal's name. The draft targets the default branch: GitHub's REST
reference says a token that can't change workflows, as a job's can't, can't create a release
whose target commit adds or changes workflow files relative to the default branch, and the first
rehearsal, at `v0.1.1`'s commit, was refused with a 403. The release job's create names no
target, so GitHub checks the default branch's own head. The attestation and its check, and the
create that publishes a release, still run for the first time in the first release through the
workflow.

## D78 — `max_events` bounds events and kept tokens, and counts no line break

**Date:** 2026-11-08
**Status:** Accepted

**Decision.** `max_events` stays one limit over two counts, and an input that takes either over it
is refused. One is the stream's events as `parse_events` yields them, its start and end included,
each counted once on every reader. The other is the tokens a read keeps: scalars, anchors,
aliases, tags, indicators, document markers, and directives. Line breaks, and so blank and comment
lines, count as neither. `docs/limits.md` defines both counts.

**Alternatives.** Count events alone, and add a `max_tokens` limit for the tokens: each name would
say what it counts, but a patch release would grow the API, and a caller who raised one limit for
a large stream would have to find and raise the other. Count events alone with no bound on tokens,
which could then outnumber events without bound: a second `...` line in a row is a token and no
event, so a read could hold the tokens of a whole 32 MiB input, about 800 MB, where it now holds
about 100 MB.

**Why.** The eager readers keep every token of the stream, about 100 bytes each, and the token
count is what bounds that memory. No reader keeps a line break, so counting them bounded nothing,
and a million blank lines between two entries refused a document of about 1 MB. The lazy readers
counted each event twice, once as its document was parsed and once as it was handed on, so the same
input needed about twice the limit there.

**Consequences.** The limit's name says less than it bounds, so its documentation says both, and
either count over it is reported as `LimitKind::Events`. Every reader that takes limits counts both
alike, so at the same limits they agree on whether an input keeps within it, and
`tests/limit_consistency.rs` checks that on each of them. Where they refuse an input can still
differ when it breaks the limit in more than one place: `parse_events` and the `Deserializer` read
a stream a document at a time, and can stop in an earlier document. `docs/limits.md` says when.

## D79 — `max_anchors` counts the anchors of the whole stream

**Date:** 2026-11-08
**Status:** Accepted

**Decision.** `max_anchors` counts every anchor in the stream, as every reader already enforced it.
The documentation, which said one document, now says one stream.

**Alternatives.** Count per document, as the documentation said, by resetting the count at each
document's start. That matches the anchors an alias can see, but it loosens what every reader
enforced: a stream of many documents could hold up to `max_events` anchors instead of 100,000.

**Why.** No reader changes, and no limit loosens. `max_events`, `max_alias_dereferences`, and
`max_tag_expansion_bytes` count per stream too, and so does the alias budget a reader of several
documents shares between them.

**Consequences.** A long stream of documents that each hold many anchors may need a higher
`max_anchors`.

## D80 — An enum reads from one mapping form: a single entry whose key is a string

**Date:** 2026-11-08
**Status:** Accepted

**Decision.** Besides a tag and a scalar that names the variant, a typed reader takes an enum from
one mapping form: a mapping of one entry whose key is a string naming the variant, as in `B: 5` or
`C: {x: 1}`. A key is a string when it reads as one: plain or quoted, explicit, under `!!str` or the
non-specific `!`, or an alias of one. A whole mapping read through an alias is read as the mapping
is. Every other mapping is an error, on every typed reader and in both modes: an empty one, one of
more entries, and one whose key is null, a boolean, a number, a collection, or under a custom tag,
as in `!x B: 5`. Every reader did this already, except that it refused a key under `!`, and
`an_enum_reads_one_mapping_form` now holds it.

**Alternatives.** Refuse every mapping form, as Serde readers built on libyaml do, so that only a
tag or a scalar names a variant. That breaks files written in Serde's externally tagged form, which
`serde_json` and most Serde formats write, and AY-021's criteria expect it, as in `Point: ~`. Or
name the variant by a null, boolean, or number key's text, as a struct's field is named, so that
`true: 5` selects a variant renamed `true`, as the scalar `true` does.

**Why.** The single-entry form is how Serde writes an externally tagged enum to most formats, so
YAML converted from them reads. A second entry has no variant to belong to. A key that resolves to
something other than a string is refused, though a field, and a variant named by a bare scalar,
are named by its text. Refusing it can be relaxed later without breaking a file that reads, while
reading it could not be taken back. AY-021 settles how every reader names a variant from a scalar,
and the key can follow that rule then.

**Consequences.** Compat reads these enums differently from a Serde reader built on libyaml, which
refuses an untagged mapping. `! B: 5` now names `B`, where it was an error. `docs/compatibility.md`
says which forms read.

## D82 — A writer gives a tag that no verbatim tag can hold a `%TAG` directive of its own

**Date:** 2026-11-09
**Status:** Accepted

**Decision.** The value writer, the Serde writer, and the event writer write each tag in the first
of these forms that reads back as the same tag:

- `!name` for a local tag and `!!name` for a core tag, unless the document redefines that handle,
  with each character that isn't a URI character %-escaped;
- a verbatim tag, `!<name>`, when the name is local or starts with a URI scheme, holds only URI
  characters, and has a `%` only before two hex digits, since a verbatim tag is read as written;
- otherwise a shorthand under a `%TAG` directive that the writer adds to the document, as
  `%TAG !t0! tag:x:` and then `!t0!a%20b` for the tag `tag:x:a b`. The prefix and the suffix are
  %-escaped, and read back unescaped. The tag is split where its handle's prefix ended, if it
  keeps one, or else after its last `:`, `/`, or `#`, or else before its last character. A tag
  that went through Serde keeps only its whole name, so the Serde writer may split it elsewhere
  than the value writer, and both read back. The handles are `!t0!`, `!t1!`, and so on, past any
  handle that the event writer's input declares.

A tag that is neither local nor core and has fewer than two characters, as `Tag::new("x", "")`
makes, has no form, since a shorthand needs a character on each side of its handle, and writing it
is an error. The event writer holds each document's text until the document ends, so that its
directives come first.

**Alternatives.** Refuse to write such a tag. That is simple, but some documents ayaml reads could
not be written back. Or decode %-escapes inside a verbatim tag when reading it, as libyaml does.
That fixes `%20` without a directive, but YAML 1.2.2 reads a verbatim tag as written, it changes
how existing documents read, and a prefix without a scheme would still need one of the other two.

**Why.** Every tag that YAML can hold then reads back as itself, and AY-025 refuses verbatim tags
that aren't tag names, as YAML 1.2.2's example 6.25 does, without making any such tag unwritable.
The reader and the writers agree on which verbatim tags exist: `!<x>` and `!<foo>` are errors,
because a verbatim name is local or has a URI scheme, and each `%` starts a two-digit escape. Output changes only for tags whose old form
didn't read back as the same tag, or wasn't a tag name.

**Consequences.** A document that holds such a tag starts with the directives it needs, and a
document after another one gets `...` before them, which YAML asks for. The lazy event writer,
`to_writer_event_stream`, needs memory for one document's text, which `max_output_bytes` bounds,
where it needed memory for the nesting depth alone. A tag with no form, which was written as an
invalid verbatim tag, is an error. The value writer and the Serde writer look through a document's
value for tags that need a directive before they write its first line. That look stops where the
writer would, at the depth limit, and costs about 60 instructions a collection and 75 a document,
which `tools/instruction-rises.tsv` accepts for the smallest and the deepest inputs that CI counts.

## D83 — Documentation lint and plan verification are required gates

**Date:** 2026-11-10
**Status:** Accepted

**Decision.** Hosted CI runs `markdownlint-cli2` over the maintained Markdown files, and the plan
job runs `aplan verify` with full history and `aplan version` as well as `aplan check`. The local
gate mirrors all three.

**Why.** A markdown regression landed because only the local gate ran `markdownlint`, and `aplan
check` is structural: it does not resolve the evidence citations in the plan. A cited test or
commit can rot while the hosted plan job stays green.

**Consequences.** A documentation-only change now waits for the lint job, and the plan job fetches
the whole history because a shallow clone cannot resolve commit evidence. A new single-line
`run:` command in a workflow still needs its local counterpart, which the `sync` job checks.

## D84 — Repository shape is gated by alint 0.22.0

**Date:** 2026-11-11
**Status:** Accepted

**Decision.** `.alint.yml` requires alint 0.22.0 and extends the bundled `oss-baseline`,
`rust`, `ci/github-actions`, `hygiene/no-tracked-artifacts`, `tooling/editorconfig`,
`agent-hygiene`, and `agent-context` v1 rulesets. The hosted `repository lint` job runs the
official action pinned to the v0.22.0 commit `8d7a60f6642cb1b59537202ae81df7d951a078ad` with
`--fail-on-warning`; local CI runs `alint check --fail-on-warning`, and a release run fails
when the binary is missing or a different version. Every deterministic error and warning is
blocking; the advisory infos, such as CODEOWNERS, a code of conduct, and one pinned Rust
toolchain, are not.

**Why.** Repository structure, portable names, text hygiene, workflow pins, and source-file
growth were enforced only by scattered checks and reviewer knowledge. alint makes them one
declarative, offline check with inline annotations, and the full-tree run catches a violation
that a diff-scoped run would let survive. Pinning the action and the local binary keeps the
gates on the same ruleset.

**Consequences.** AY-076 split the two test modules and removed their ratchets; AY-073 through
AY-075 split the scanner, parser, and emitter, and AY-077 removed the remaining exemptions. The
source-size rule now has no path exclusion, baseline entry, or non-growth ratchet. The pinned upstream YAML suite's
`README.md` / `ReadMe.md` collision is excluded from the case-conflict rule alone, not from
the rest of the tree. `.editorconfig` states UTF-8, LF, a final newline, trailing-whitespace
removal, four-space Rust and Python indentation, two-space YAML indentation, and tab-indented
make recipes; `editorconfig_conforms` covers `.aplan.yml` and `.github/**`, while rustfmt owns
Rust indentation and the byte-exact parser fixtures and fuzz corpora stay outside content
conformance. A version update moves `.alint.yml`'s `requires`, the action pin, and the local
binary together, and repeats the 2,000/2,001-line boundary probes for every covered extension,
`tools/hooks/*`, and a Rust source outside today's `crates/`, `fuzz/`, and `tools/` layouts.

## D85 — A core tag on a node of another kind is dropped

**Date:** 2026-11-11
**Status:** Accepted

**Decision.** Default mode and compatibility mode both drop a core-schema tag when the node it is
on has another kind. A collection tag on a scalar, such as `!!seq a`, is dropped and the scalar is
read normally. A scalar tag on a collection, such as `!!str {}` or `a: !!int [1]`, is dropped and
the collection is read normally. A core collection tag on a matching collection is still dropped
from the value model, as it only restates the kind; an event stream keeps that tag as input
presentation. Custom tags are kept. `parse_value`, the typed readers, and the event streams drop a
mismatched tag alike.

**Alternatives.** Refuse such a node as invalid, as YAML 1.2.2 section 3.3.3 could be read to
require. That would make `!!str {}` and `!!seq a` errors in both modes, where today they read
successfully, and libyaml drops the tag instead. Dropping keeps the reading lenient and makes
compat match libyaml.

**Why.** The old split was accidental: collection tags were dropped, while scalar tags stayed on
collections. That made `!!map [a]` a sequence and `!!str {}` a tagged mapping, and compat differed
from libyaml without saying so. One rule makes every reader agree.

**Consequences.** `!!str {}` reads `{}`, `a: !!int [1]` reads `{a: [1]}`, `!!null []` reads `[]`,
and `!!seq a` reads `a`. A custom tag on a collection still wraps it. The event streams no longer
carry the dropped tag, so the composed value and the events agree.

## D86 — Rust linting is strict on the MSRV and stable, with a separate panic profile

**Date:** 2026-11-12
**Status:** Accepted

**Decision.** The workspace enables `all`, `cargo`, and `pedantic` Clippy at group priority -1,
plus the selected `allow_attributes_without_reason`, `literal_string_with_formatting_args`,
`mem_forget`, `needless_pass_by_ref_mut`, `or_fun_call`, and `redundant_clone` lints. It allows
only `multiple_crate_versions`, `module_name_repetitions`, and `similar_names` policy-wide, and
denies `dbg_macro`, `todo`, and `unimplemented`. Rust lints include `missing_docs`, the 2018
idioms, and the other checks listed in `Cargo.toml`. The nursery and restriction groups stay off.
The `fuzz` workspace mirrors the applicable policy and declares Rust 1.88; its three fuzz targets
forbid unsafe code, and its counting allocator is the documented exception. A production-library
Clippy command denies `unwrap_used`, `expect_used`, and `panic`. Rustdoc enables `rustdoc::all`
and retains `-D warnings`. `clippy.toml` sets the function-size threshold to 200 lines without
duplicating MSRV metadata.

**Alternatives.** Enable every nursery and restriction lint, or allow the noisy pedantic lints
policy-wide. The first produces unstable or contradictory findings, and the second hides the
findings this policy exists to catch. Keep the panic profile on tests too: tests legitimately use
`expect` and `panic` to report a failed assertion, so the profile belongs to the production
library only.

**Why.** The MSRV and stable toolchains must agree on what a clean tree is. The selected groups
catch stale public documentation, lossy conversions, panic-prone shortcuts, and stale
suppressions while leaving the scanner's deliberate indexing and numeric operations readable.
A separate fuzz workspace is necessary for the allocator, and the fuzz targets still get the
same compile-time policy. Documenting errors and running rustdoc with all lints keeps the public
API usable from docs.rs.

**Consequences.** Clippy is blocking on both toolchains and all targets and features, including
both fuzz workspaces. A new finding is fixed or gets a narrow reasoned suppression, never a
silent baseline. A stable release that adds a lint may require a policy change; it is made in a
pull request with this record updated. The production panic profile is checked in both toolchain
jobs, and any new `unwrap`, `expect`, or `panic` in the library must be a checked invariant or a
required panicking trait behavior.

## D87 — Typed decoding checks the node shape, scalar kind, and exact variant names

**Date:** 2026-11-12
**Status:** Accepted

**Decision.** A typed sequence target reads only a sequence or tuple, and a struct reads only a
mapping. A sequence no longer fills a struct by position. An explicit null is an error for a
sequence, mapping, struct, or struct-variant target, whether the null is bare or under a tag; an
empty source value still supplies an empty collection or a struct's defaults to the source-based
readers. A string target under `from_value` reads only a string or tagged string, not a number,
boolean, null, or collection. A bare enum variant is named only by a string scalar or a tag; a
null, boolean, or number scalar names no variant, even when a variant is renamed to its spelling,
and a bare variant name holds nothing: it fills no variant that holds a sequence, a mapping, a
struct, or a scalar. A mapping names a variant only
through its one string key. Readers store a tag in a canonical full-name split, so `handle()` and
`suffix()` do not depend on which reader produced the tag.

**Alternatives.** Keep the old coercions and document them. That leaves `from_value::<String>`
inventing `"5"` from `5`, lets `[1, 2]` fill `{a, b}` by position, and makes a null mean an empty
collection or a default value, which is not what the caller wrote. Keep plain `true` and `1` as
variant names for source readers. That would make `from_str` and `from_value` disagree after the
value path stopped stringifying non-strings. Preserve each reader's original tag split. That
cannot be reproduced from a tag's full name alone after typed decoding, so the same tag would
have a different `handle()` or `suffix()` depending on the reader.

**Why.** A Serde target describes the shape and scalar kind it accepts. Coercing a different shape
produces a value the caller did not write and hides a bad document. Exact string names make
`from_str`, `from_str_borrowed`, `from_value`, seeded readers, and compat agree, and they match
D80's rule for a variant's mapping key. The full tag name is what the tag means; the split is
presentation, so a canonical split is safe.

**Consequences.** Source-based readers still accept an empty source value as an empty collection
or a struct's defaults, and still accept a bare tag with no value there. A composed `Value` cannot
distinguish that empty value or bare tag from `~`, so `from_value` and the owned stream readers
treat it as an explicit null and reject it for those targets. That difference is documented in
`docs/compatibility.md` and the `from_value` API docs. A tag built by hand keeps the explicit
split its constructor was given; tags read from a source use the canonical split.

## D88 — Default count limits stay memory-bounded; `Limits::for_max_input` lifts them

**Date:** 2026-11-16
**Status:** Accepted

**Decision.** `Limits::default()` keeps `max_events`, `max_collection_entries`,
`max_anchors`, and `max_alias_dereferences` at their memory-bounded values. Add
`Limits::for_max_input()`, which sets those four to `usize::MAX` and keeps the input, depth,
scalar, tag-expansion, document, and alias budgets. `docs/limits.md` states that the defaults
are a lower profile than `max_input_bytes`, and that the new profile is the way to read a
dense document the input limit admits when the other budgets allow it.

**Alternatives.** Raise the defaults themselves until they cover the worst case within 32 MiB:
the token arena could then reach several gigabytes, undoing D78's memory bound. Scale the count
limits with input length: every reader would need a new input-sized count rule, it would still
permit the same memory, and explicitly lower limits would become ambiguous. Document the lower
profile without a preset: callers would still have to discover and lift four fields by hand.

**Why.** The input limit was being read as a promise about every dense document, but the count
limits exist to bound memory, not to restate the input size. Naming the permissive profile makes
that trade explicit and leaves the default fail-closed for hostile input.

**Consequences.** Callers that need the input bound opt in and accept the memory cost; callers
that lower any of the four counts keep their stricter behavior. The other budgets, including
depth and alias expansion, still apply even with `for_max_input()`, so a syntactically valid
document can still be refused for those reasons.

## D89 — Spans are byte ranges, and line columns come from a public index

**Date:** 2026-11-16
**Status:** Accepted

**Decision.** `Span` holds two byte offsets and converts to `Range<usize>`. The old `Marker` remains as a deprecated compatibility value type, but
spans no longer store it. `LineIndex`, `Position`, `PositionError`, and `ColumnEncoding` are public;
columns can count UTF-8 bytes, Unicode scalar values, or UTF-16 code units. Conversions between
an offset and a position reject out-of-bounds offsets and offsets inside a UTF-8 character
instead of clamping them. `ColumnEncoding` and `PositionError` are non-exhaustive, so
either can gain a variant without a break. A leading
byte-order mark is included in byte offsets and excluded from columns. `Error::location()` still
returns a one-based location, filled from the reader's line index when the error reaches a
boundary. D96 later gave the public index the format contract's names and its zero-based
positions, before any release had the index.

**Alternatives.** Keep eager markers in every span: that is 48 bytes per span and a line/column
calculation on the scanner's hot path. Derive locations only when formatting an error: an error
does not own its input, so it would have to copy the input or lose its location. Retain UTF-8
columns only: consumers such as language servers need UTF-16 columns and would keep converting
themselves.

**Why.** Events and tokens are produced in bulk, while line/column positions are needed only for
errors and editor consumers. Byte ranges make the common path smaller; an on-demand index gives
each consumer the encoding it needs.

**Consequences.** This is a breaking API migration: `Span::start` and `Span::end` return
`usize`, spans no longer use `Marker`, and a manually constructed `Error` uses `with_location` when it needs
a location. On a 64-bit release build the event size falls from 176 to 144 bytes, `Span` from 48
to 16, and `Error` from 112 to 104. The hosted performance gates decide whether the scanner and
parser remain within their instruction and allocation budgets.

## D90 — 0.2.0 carries every known break, and the additive interface work follows it

**Date:** 2026-11-18
**Status:** Accepted

**Decision.** The next release is 0.2.0, cut from `main` once the limit and typed-reader
regressions found after 0.1.3 are fixed and the short-scalar peak in #178 is settled. It holds
every breaking change the plan knows of: byte-offset spans (D89), the lazy
`compat::parse_events` readers, and the `Mapping` iterator newtypes that AY-044 planned, which move
ahead of the rest of AY-048. Stable error codes, masked errors, the options values, the shared
position vectors, and the documentation book are additive, so they follow in 0.2.x releases.

**Alternatives.** Hold 0.2.0 until the whole interface milestone is done: one coherent release, but
about seven more working days before fixes that are already merged reach a user. Release 0.2.0
without the iterator newtypes: sooner, but they would then force a 0.3.0, a second breaking
release. Backport the fixes to a 0.1.4: the memory work is built on byte-offset spans, so its bound
can't be backported, and the release procedure tags `main` only.

**Why.** `main` has carried breaking changes since #166, so no 0.1.x release can come from it, and
every fix merged since 0.1.3 waits for the next minor release. A breaking release costs each
consumer one migration, so it should hold every break that is already decided, and nothing
additive needs to wait in front of it.

**Consequences.** The plan's 0.2 milestone is re-cut in the change that adds the release's own
task: the iterator newtypes become a task of their own that blocks 0.2.0, and the additive tasks
move to a later milestone. `SECURITY.md` names the versions that get fixes when 0.2.0 ships. The
release is qualified as 0.1.2 and 0.1.3 were, with the timing comparison D68 asks for.

## D91 — A typed reader of one document reports its first problem at every input size

**Date:** 2026-11-18
**Status:** Accepted

**Decision.** `from_str`, `from_slice`, their borrowed forms, and the compatibility readers of one
document report the problem that comes first in the document: any that the scanner finds, then
the first that the parser or the builder meets, then decoding into the target. That is what
`parse_value` reports. An input of at most 4 KiB is still parsed whole and then built, which is
faster when it succeeds. When that fails, the input is read again the way a larger one is, built
as it is parsed, and that answer is the one reported.

**Alternatives.** Report the syntax error first at every size, as 0.1.3 did, by holding back a
builder's error until the parser reaches the end: no change for a small document, but a large one
with an early duplicate key is parsed to its end before it is refused, and the typed readers keep
disagreeing with `parse_value`. Leave both behaviors and document that the answer depends on the
input's size: adding a comment to a document could then change which error it reports, and no
test could compare the paths.

**Why.** Since #179 an input over 4 KiB is built as it is parsed, so the same document reported a
different problem once it grew past that size. One rule for every size makes the three paths
testable against each other, and agreeing with `parse_value` leaves one order to document.

**Consequences.** A small document with two problems can report a different one than 0.1.3 did,
which the changelog states. An input of at most 4 KiB that fails while it is scanned or parsed is
read twice. One that is accepted, or that fails only while it is built, is read once, as before.
The `Deserializer` is unchanged: it reads a document's events before it builds from them, at
every size, so within a document it reports a syntax error first. D95 later changed what an
input of more than 4 KiB costs: it is read from its events, and read twice when it is refused.

## D92 — An entry span is given only where its removal is exact

**Date:** 2026-11-18
**Status:** Accepted

**Decision.** `SpannedNode::entry_span` returns a span only when deleting those bytes leaves a
document that reads as the original without that entry, and with nothing else changed. A block
mapping entry or a block sequence item has one when all of this holds:

- it begins its own line: only indentation before a key, and only indentation, a `-`, and
  spaces before an item;
- a key and its `:` are on one line, so no `?` begins the entry on that line or an earlier one,
  and the key has text;
- nothing follows the entry on its last line but a comment, and its end is known: an entry that
  ends in a value the source doesn't write after a `:` on its key's line ends at that `:`, and
  one that ends in any other scalar with no text has no known end. A key that is a block scalar
  ends with its last line, so a `:` on the line after it is not on its line;
- no line that begins with `:` follows it, which would give its value to a key before the entry;
- the input holds no byte-order mark past the marks that open it, since what such a mark means
  depends on the lines before it: the compatibility reader counts one toward its line's
  indentation unless it opens the input;
- it defines no anchor and stands inside no collection that has one, since an alias reads what
  it names;
- it is not the only entry of its collection, which would then be no collection;
- where a block scalar is the last thing written before it, content follows it directly, since
  the scalar would otherwise take the blank and comment lines after the entry.

The span runs from the start of the entry's first line through its last line break, so it takes
a nested value, a block scalar, and a trailing comment, and leaves a byte-order mark. Every other
entry returns `None`, a flow entry among them. A value that the source doesn't write after its
key's `:`, on the key's line, has an empty span right after that `:`; whether the next line could
hold it is judged from the mapping's column, not the key's, which a tag before the key moves.

The span answers for what the document reads as. A limit that a document keeps within only by
its length, as the alias allowance grows with the input, can be passed once the document is
shorter, and merge keys that are applied afterwards can bring back a merged entry that the
removed one overrode.

**Alternatives.** Give a span for single-line block entries only: the least code, but a nested
entry could not be removed through the API. Give flow entries a span that takes one neighboring
separator: complete, but which comma and which blanks belong to an entry is a rule about edits,
and it belongs with the edits that are checked by reading their result. Give the only entry of a
collection a span and say that the collection then reads as an empty value: what it reads as
depends on a tag before it, and under `!!int` it doesn't read at all.

**Why.** The first version computed a block entry's span from its key alone, so a nested entry's
span was its key line, and a flow entry's was its key. The second gave whole block entries a
span, and deleting about 3% of them still changed something else: a key under a `?` on the line
before, an entry inside an anchored collection, an entry before a line that begins with `:`, and
an entry after a block scalar. What is exact for an entry depends on the lines around it. A span
that is absent sends the caller to another way of editing; a span that is wrong corrupts a file.

**Consequences.** `None` is conservative: it means that no span is known to be exact, not that
none exists. The rule is tested by what it promises: on generated documents that put every kind
of entry next to every other, each span a tree gives is deleted and the rest read, in both modes.
Flow entries, only entries, and entries that share a line wait for the edit crate's verified
edits (AY-050).

## D93 — The allocation gate budgets an issue's shapes by the issue's own measure

**Date:** 2026-11-18
**Status:** Accepted

**Decision.** When an issue reports memory on generated document shapes, the allocation gate
budgets those shapes measured as the issue measures them. For #178 that is an untyped decode into
a JSON tree through the compatibility reader, with the count limits lifted, and a reallocation
counted by the bytes it adds. The counter in the fuzz workspace generates the nine shapes and
decodes them that way, and `tools/json-alloc-corpus.tsv` holds a peak, an allocation count, and a
total for each. The fuzz workspace takes `serde_json` for the tree, at the version the tests use.
A budget is about 1.10 times the measured value, and a shape that is over the issue's target has
a budget about 1% over its measured value, so that it may not grow.

**Alternatives.** Decode into a JSON-like tree written in the counter: no dependency, but its
sizes would be close to the issue's and not equal, so a regression near the target could be
misjudged. Budget nothing here and leave the measure to whoever filed the issue: a regression is
then found after it merges.

**Why.** The gate's typed row decoded into ayaml's own `Value`, another tree than the issue's.
Figures from that row were set beside the issue's reference in the plan and in the issue's thread,
and they overstated the gap: by the issue's measure eight of the nine shapes were within its
target while the plan still called for a three-day rewrite to reach it. A figure can only be
compared with a reference that was measured the same way.

**Consequences.** The fuzz workspace's lockfile gains `serde_json` and what it needs; nothing that
is published depends on them. The ten percent of room keeps a shape that is within 1.2 times the
issue's reference there, since none is over 1.06 times today; a row is lowered in the change that
lowers its measurement. The gate script no longer passes over a budget file that is missing.

## D94 — The last shape of #178 is closed by composing without the intermediate tree

**Date:** 2026-11-18
**Status:** Accepted

**Decision.** AY-104 keeps its scope: an untyped decode builds its value as the events arrive,
without a complete typed node tree in between. That is the work that brings the ninth shape of
issue #178, a megabyte block sequence of short words, from 1.28 times the issue's reference to
within 1.2 times.

**Alternatives.** A narrower change, estimated at a day: line records of 12 bytes in place of 24,
and room reserved for a root block collection from its line count. It was estimated to bring that
shape to about 1.16 times, and it would refuse an input of more than four billion tokens. Or
accept 1.28 times as a documented exception, which leaves the issue's target unmet.

**Why.** At that shape's peak the typed node vector is the largest thing live, 10.5 MB of 23.1 MB,
beside the token arena, the line records, and the line index, and the JSON tree is not yet built.
Building the value directly takes the node vector out of every untyped decode's peak, where the
narrower change trims one shape and leaves little room under the target.

**Consequences.** #178 stays open until AY-104 lands, and by D90 the 0.2.0 release waits for it.
The narrower change is not planned. The budgets in `tools/json-alloc-corpus.tsv` are lowered in
the change that closes AY-104.

## D95 — A typed read builds its value from events, and reports what a tree of the document reports

**Date:** 2026-11-18
**Status:** Accepted

**Decision.** A reader of one document into a type reads an input of more than 4 KiB from the
parser's events, which it takes as its target asks for values. A scalar is one node that the
target reads and drops, and a collection is read entry by entry. A tree is built only of a node
that is read whole or again: a node with an anchor and what an alias refers to, a collection that
keeps its tag, a mapping key, and what an enum reads. The checks that the tree's builder makes are
made as the events pass: depth, duplicate keys, anchors and the alias budget, and a single
document. A root that is built whole is the document as a tree, so it is built before its target
reads, the stream is read to its end, and the tokens are freed first, as when the tree reader
builds it.

Such a read's own error is not always the one to report. After any error the document is built as
a tree, and that tree's error, if it has one, is the answer. Only a document that builds reports
the error that was found in reading it, which is then the target's. An input of at most 4 KiB, and
the `Deserializer`, build the tree and read from it as before.

**Alternatives.** Report the first error in document order, of either kind: one read, but a value
that doesn't suit its target would then hide a duplicate key or a syntax error after it, which
every release has reported first and D91 keeps first. Read every input from events: one path, but
about 1,300 more instructions for a document of a few bytes, whose tree costs nothing to hold. Tell
a target the size of each collection by looking ahead at its lines: a target could then reserve
its room as it can with a tree, at the cost of a second pass over the lines of every nested
collection.

**Why.** D94 asked for it: the tree was the largest thing live at the peak of an untyped decode.
Serde's targets ask for values, so the parser had to give its events on demand, and its grammar
already ran on an explicit stack that can stop between two steps. Reading scalars and whole nodes
through the tree's own deserializer keeps one statement of how each target reads each node. Over
the 76,861 files of the fuzz corpus the two readers gave the same value or the same error in
14.4 million reads into 47 targets, and in 3.1 million into targets whose visitors take an error
and go on, read nothing, or leave values behind.

**Consequences.** A refused input of more than 4 KiB is read twice, unless the scanner refuses
it, which reads the whole input before any event. Its target has read what stands before the
fault by then, where the tree reader refuses a document before its target sees any of it: the
value and the error are the same, but a seed that writes to its caller's data as it reads has
written, and the first read costs what an accepted document of that content costs. A collection
that is read from events tells its target no size, but for a root flow collection, whose entries
the parser counts: a vector or a map then grows by doubling, peaks higher, and keeps up to twice
the room its entries need, which `Value` and `Mapping` give back at each collection's end. A
visitor that asks a mapping for a value after the mapping has told it the end is refused by
both readers; the tree gave it the last value it had left unread, until that was found. A node
that is built whole and is not the root, because it has an anchor or a tag that it keeps, an
enum reads it, or it is a key that is a collection, is kept as a tree beside the tokens and the
value: a megabyte flow sequence with an anchor peaks about a third above a read that builds
the tree first, and a document made mostly of anchored nodes about a quarter above it as a
JSON tree. A typed read of many short entries executes 4% to 7% more instructions than one
that builds the tree, which the instruction gate accepts for its three such workloads. The tree reader stays: it
reads small inputs and anchored nodes, and it words every error. The tests read documents both
ways and compare, the `borrowed` fuzz target does the same, and a build with
`--cfg ayaml_typed_from_events` runs the whole suite on events at every size.

## D96 — The positions layer is the format contract's, by its names, its rules, and its vectors

**Date:** 2026-11-19
**Status:** Accepted

**Decision.** `LineIndex`, `Position`, `PositionError`, and `ColumnEncoding` follow the format
contract that ayaml shares with adotenv. `LineIndex::new(input)` borrows the input.
`to_position(offset, encoding)` returns a zero-based `Position`, `to_offset(position, encoding)`
converts back, and `line_count` counts the lines. `PositionError` has four reasons, with the names
that the vectors and adotenv give them: `OutOfBounds`, `InvalidUtf8Boundary`, `SplitSurrogate`, and
`InsideCrLf`. `Location` is its own type again, one-based, for display: it is what
`Error::location()` returns, and `Location::from(position)` gives it for a position. The contract's
vectors are vendored under `tests/vectors/`, a test runs the index against every position case, and
CI fails when a vendored file no longer has the digest recorded for it.

Three answers changed to the vectors'. A UTF-8 byte column needs no character boundary, so an
offset inside a multi-byte character has one. An offset between a CR and its LF is an error. And an
offset inside a byte-order mark that the columns leave out shares the first visible column, the one
case where `to_position` gives a position for an offset that isn't its own.

The index has the contract's default form only: one byte-order mark that opens the input is in the
offsets and out of the columns, as in an editor. Every other mark is a character of its line and a
column, a second mark at the start and a mark that begins a later line among them. The vectors also
give the byte-relative form of the three sources that open with a mark, which an index may offer
and this one doesn't.

**Alternatives.** Keep D89's shape, `position(input, offset, encoding)` returning a one-based
position, and list the differences as stated exceptions: no work now, but the contract's names
would mean other numbers here, and conforming later would take another breaking release for
everyone who had adopted the index. Match the names and keep one-based positions: the same call
would then return different numbers in the two crates, which is worse than a different name. Offer
both forms of a leading mark, as adotenv does with `without_bom`: nine more vector answers would
run, for a form that the contract calls the old default and that no ayaml reader uses. Leave out of
the columns every mark that the default readers leave out, a run at the start and a mark that
begins a later line, so that an error's location is always the index's position: the first version
of this change did, and on such lines it gave other columns than adotenv's index and than an
editor, which the 30 vectors don't reach and a comparison with adotenv's own code on 200,000 texts
did.

**Why.** The index was new and unreleased, so its names and numbers could still change at no cost
to anyone. The contract checks conformance by data and not by a shared type, so the vectors decide
what an answer is, and where they are silent the contract's words and adotenv's answers do. A
machine position counts from zero in every editor protocol the index is for, and a person's from
one, which is why the two are two types.

**Consequences.** `LineIndex::position` and `LineIndex::line_start` are gone before any release had
them. The readers keep an index of their own, which doesn't hold the input and remembers where it
last looked, so their cost is unchanged. A conversion finds its line by binary search; a column in
scalar values or UTF-16 code units then reads the line up to the offset, so it costs more on a long
line, as adotenv's does. A reader's error stands where the index puts the start of its span,
counted from one, on every line that begins with no mark. On a line that begins with one the
readers count in their own ways: a default reader leaves out every mark that begins a line, and a
compatibility reader the whole run that opens the input, of which it counts one where a document
marker follows, as libyaml does. A test holds every reader to that rule on every short text.
`Location::from` adds one to a line and a column and stops at the largest number, where a count
can't go. `docs/interface.md` says what ayaml offers at each layer of the contract.

## D97 — A writer refuses what no reader of the crate loads, from 0.2.0 on

**Date:** 2026-11-19
**Status:** Accepted

**Decision.** The event writers refuse an alias that has no node with its anchor earlier in its
document, a `%YAML` or `%TAG` directive that the scanner reads in neither of its modes, and a
second `%YAML` directive or a second `%TAG` directive for one handle in a document. The value
writers count an empty collection as a level of depth, and refuse a `!!int`, `!!float`, `!!bool`,
or `!!null` tag on a scalar that the tag's rule doesn't take, such as `!!int` on the string `x`.
Each returned success before and wrote text that every reader of the crate refuses. Nothing that a
reader loads is newly refused. The change ships in 0.2.0 with the other changes that a caller can
notice, and not in a patch release after it.

**Alternatives.** Ship it in 0.2.x as a fix, which is what it is: 0.2.0 would have gone out sooner,
but a patch release would then have turned successes into errors. Leave the writers permissive and
document that they write what they are given: the crate would keep producing documents that its own
readers refuse. Check an alias in the event readers too, so that reading and writing agree: an
event stream is the document's syntax, where a dangling alias is well formed, and tools that only
move events about would lose such documents. Refuse every core tag on a payload of another kind,
such as `!!str` on a number or `!!seq` on a scalar: the first version of this change did, but a
reader loads each of those, as the value that the tag makes of the text or with the tag dropped,
and 0.1.3 wrote them, so that would have refused more than what no reader loads.

**Why.** A caller should find out that its output can't be loaded when it writes it, not when
someone reads it. What a directive may say is the scanner's rule: the writer has the scanner read
the one directive line, so the two can't drift apart. It asks the default mode and then the
compatibility mode, which takes a little more, such as a `%TAG` prefix that starts with a flow
indicator: events that the compatibility reader gave are written back, as text that it reads. What
a document may hold once is the parser's rule, which the writer counts for itself and the tests
hold to the parser by reading back what is written. A reader resolves a scalar beneath one of the
four tags by that tag's rule, in any style, so the writer asks that rule about the text it is about
to write.

**Consequences.** The event writers hold the names of the anchors of the document they are writing,
at most `max_anchors` of them, so that an alias can be checked, and `to_writer_events_with_limits`
lets a caller raise that limit as the other event writers do. Events that were read from a document
with a dangling alias can't be written back; the event readers still read such a document. The
event writers check what no reader loads and not what a stream means: an alias inside the node it
names, a tag that doesn't suit its node, and a key that a mapping has twice are written as they are
given. A tag is refused before the first line of its document, by the scan that finds the tags that
need a directive, so a writer that is handed the output has none of that document. A tag that is written on a scalar that fits it still reads back as what the tag makes of the text:
`!!int` on the string `1` reads back as the number, as it did.

## D98 — Options values are `non_exhaustive` structs with `const` builders and `*_with` entry points

**Date:** 2026-11-20
**Status:** Accepted

**Decision.** Parsing options follow one shape across the crate, introduced first by
`compat::ParseOptions` (AY-122) and followed by the root `ParseOptions` and `EmitOptions`
(AY-048):

- The options type is a `#[non_exhaustive]` struct (`Clone`, `Debug`, `PartialEq`, `Eq`,
  `Default`) with public fields for each setting, a `pub const fn new() -> Self` preset matching
  `Default::default()`, and `pub const fn with_*` builders.
- Multi-way policy knobs such as `compat::GlobalTags` are `#[non_exhaustive]` enums with
  `Default` selecting the existing behavior (`GlobalTags::Keep`).
- Operations taking an options value are named `<operation>_with` (such as
  `compat::from_str_with`, `compat::from_str_seed_with`, and `compat::deserialize_any_with`) and
  take `&ParseOptions` in the same position that `*_with_limits` takes `&Limits`.

**Alternatives.** Name the functions `*_with_options` and keep `limits` as a separate parameter:
that repeats the combinatorial suffix problem AY-048 exists to end and would require deprecating
the new functions as soon as AY-048 lands. Put `global_tags` on a root `ParseOptions` shared with
the default readers: that would either expose a compatibility-only policy on default readers that
ignore it, or change default-mode semantics that keep non-core global tags.

**Why.** The shape matches the shared format contract (`docs/interface.md`) and existing
`Limits` and `EmitterOptions` structs in this crate, allows `const` configuration values such as
`ParseOptions::new().with_global_tags(GlobalTags::Drop)`, and lets AY-048 extend the same
pattern to the remaining entry points without deprecating or renaming AY-122's API.

**Consequences.** `compat::from_str_with`, `compat::from_str_seed_with`, and
`compat::deserialize_any_with` accept both custom `Limits` and `GlobalTags` in one value. AY-048
completes the pattern across the rest of the crate with root `ParseOptions` and `EmitOptions`
(holding `Limits`, layout/indentation flags, and optional `EmitterStyles`), `const` presets
(`Limits::new`, `ParseOptions::for_max_input`, `compat::ParseOptions::for_max_input`,
`EmitOptions::flow`, `EmitterOptions::new`, `NodeStyle::new`), one `*_with` entry point per
operation (including `from_str_with` and `from_slice_with`, both accepting owned and borrowed
`Deserialize<'de>` targets, and `compat::parse_events_with` / `compat::parse_events_eager_with` /
`SpannedTree::parse_compat_with`), and deprecates all 32 older `*_with_limits`, `*_with_options`,
and `*_with_styles` combination functions through `0.x` for removal in `1.0`.

## D99 — CI compares public API listings per feature set against the last release tag

**Date:** 2026-11-20
**Status:** Accepted

**Decision.** CI builds rustdoc JSON listings of `ayaml`'s public API for every feature set that
changes it (the default crate and `libyaml-compat`), verifies that every feature declared in
`crates/ayaml/Cargo.toml` is classified and that `bench-internals` exposes no public items, and
compares the listings with the last released `vMAJOR.MINOR.PATCH` tag strictly before `HEAD`
(`tools/check_public_api.py`). Any removed item or changed signature, trait implementation, or
associated type fails the check unless the active `### Changed` section of `CHANGELOG.md`
(`## [Unreleased]` before a release pull request, or `## [X.Y.Z]` when the crate version is newer
than the base tag) names the item. Purely additive changes pass without a `### Changed` entry.

The listing tool (`tools/public-api-cli.rs`, installed by `tools/install-public-api.sh`) compiles
directly against the pinned `public-api` library crate (`0.52.0`) downloaded from crates.io,
verified by SHA-256 checksum, and built `--locked --no-default-features` using the `Cargo.lock`
inside that archive and the paired nightly toolchain (`nightly-2026-05-21`, rustdoc JSON format
version 57). `tools/ci-local.sh` runs the exact same check and regression test (`public-api` job).

**Alternatives.** Rely on `cargo-semver-checks` alone: its pinned release missed all four breaking
changes between `0.1.3` and `0.2.0` (changed return types on `Span::start`/`Span::end` and
`compat::parse_events_with_limits`, changed parameter types on `Span::new`/`Span::empty`, and
changed `IntoIterator::IntoIter` associated types on `Mapping`, `&Mapping`, and `&mut Mapping`).
Build the `cargo-public-api` CLI crate instead of `public-api`: `cargo-public-api` publishes no
prebuilt GitHub release binaries and its locked graph pulls in `curl-sys` and `openssl-sys 0.9.88`,
which fails to build against OpenSSL 4.x on modern Linux hosts. Parse raw rustdoc JSON in Python:
duplicates the rendering, re-export expansion, and trait-impl traversal that `public-api` already
maintains.

**Why.** The 0.2.x series promises additive-only evolution, and a whole-API listing diff catches
every signature, trait implementation, and associated type change across both default and
compatibility feature sets. Using `public-api` with `--no-default-features` keeps the pinned tool
pure Rust (no C or OpenSSL dependencies), fast to build and cache, and identical between hosted CI
and `tools/ci-local.sh`.

**Consequences.** Every pull request and release tag runs `public-api` in CI, including a live
regression test confirming that comparing `v0.2.0` against `v0.1.3` reports all four documented
breaks unless named in `### Changed`. Compiler-internal unstable marker traits (`Freeze`,
`StructuralPartialEq`, `StructuralEq`) are filtered out while stable auto traits (`Send`, `Sync`,
`Unpin`, `UnwindSafe`, `RefUnwindSafe`) stay enforced. The `public-api` crate version/checksum and
its paired nightly toolchain must move together by hand.

## D100 — Stable `ErrorCode` (`yaml.<kebab-name>`) alongside `ErrorKind`, plus `Clone` and `PartialEq` on `Error`

**Date:** 2026-11-20
**Status:** Accepted

**Decision.** Publish `ErrorCode` (`#[non_exhaustive]`, `Clone`, `Copy`, `Debug`, `PartialEq`,
`Eq`, `Hash`, `Display`, `AsRef<str>`, with `const fn as_str(self) -> &'static str` returning a
stable `yaml.<kebab-name>` string) at the crate root, expose `Error::code(&self) -> ErrorCode` and
`Error::with_code(self, ErrorCode) -> Self`, keep `ErrorKind` intact for coarse pipeline matching,
and implement `Clone`, `PartialEq`, and `Eq` on `Error`.

`Error` stores `source` as `Option<Arc<dyn StdError + Send + Sync + 'static>>` instead of `Box`,
keeping `size_of::<Error>() == 104` and preserving all auto traits (`Send`, `Sync`, `Unpin`). Two
`Error` values are equal when their `kind`, `code`, path/context offsets, `span`, `location`,
`message`, `limit`, and source (by `Arc::ptr_eq` or display text) match. All seven
`serde::de::Error` methods (`invalid_type`, `invalid_value`, `invalid_length`, `unknown_variant`,
`unknown_field`, `missing_field`, `duplicate_field`) are overridden on `Error` so typed readers and
derived `Deserialize` implementations emit exact `ErrorCode`s while preserving Serde's wording
byte for byte. A dedicated reference page (`docs/error-codes.md`) documents every variant, its
`yaml.<kebab-name>` string, its usual `ErrorKind` (including `libyaml-compat` kind remappings), and
its meaning, checked bidirectionally by unit and integration tests.

**Alternatives.** Replace `ErrorKind` with `ErrorCode`: breaking change in `0.2.x` and loses
coarse pipeline-stage matching. Expose codes as `&'static str` only: gives up compile-time
exhaustiveness/typo checking and diverges from the format contract (`docs/interface.md`). Drop
`source` on `Error::clone()` or ignore `source` in `PartialEq`: silently loses underlying I/O or
UTF-8 causes on clone or treats sourced and unsourced errors as equal.

**Why.** Callers need a stable contract for matching specific failures without coupling to
human-readable message text, and tests and diagnostic wrappers need `Error: Clone + PartialEq`.
Using `Arc` for the rare `source` field keeps `Error` at 104 bytes with zero hot-path overhead.

**Consequences.** Every encoding, scanner, parser, composition, alias, duplicate-key, resolver,
resource-limit, Serde, emitter, and I/O failure carries a pinned `ErrorCode`, and
`Error::with_limit` refines `ErrorCode::LimitExceeded` to the specific `LimitKind` code. In
`libyaml-compat` mode, `ErrorKind` remappings (such as `ErrorKind::Serde` for depth limits and
`ErrorKind::Parser` for document limits) keep their specific `ErrorCode`. Adding a new `ErrorCode`
variant requires updating `docs/error-codes.md` and `crates/ayaml/tests/error_codes.rs`, enforced
by tests.

## D101 — Masked error wrapper (`Error::masked()` / `MaskedError`)

**Date:** 2026-11-21
**Status:** Accepted

**Decision.** Add `Error::masked(&self) -> MaskedError<'_>` and export `MaskedError` at the crate
root (`ayaml::MaskedError` and `ayaml::error::MaskedError`). `MaskedError<'a>` is a `Clone + Copy`
borrowing view whose `Display` prints `<kind> (<code>)` (followed by `at line <L> column <C>` when a
location is present), whose `Debug` prints only `kind`, `code`, `span`, `location`, and `limit`,
whose `std::error::Error::source()` always returns `None`, and whose `PartialEq`/`Eq` compares
only those five non-document fields.

**Alternatives.** Keep YAML mapping keys or typed paths in masked output (as `.env` formats do for
plain environment variable names): in YAML, mapping keys can be arbitrary scalars or collections
and may themselves hold sensitive data (identifiers, tokens, or user-supplied strings), so masking
must withhold the entire `message` and `source`. Mutate `Error` into a redacted `Error` value:
forces allocation and makes it harder for a caller to keep the full diagnostic internally while
formatting only the masked view to a log sink.

**Why.** Normal `Display` and `Debug` on `Error` intentionally include helpful context from the
document (values, keys, tags, directives, anchor/alias names, and source errors), which is unsafe
to log verbatim from untrusted or secret-bearing inputs. A zero-allocation borrowing wrapper with
no `message()` or `source()` surface lets callers safely format errors for logs and telemetry while
preserving `ErrorKind`, `ErrorCode`, and `Location` for triage.

**Consequences.** Callers logging or surfacing errors from sensitive or untrusted documents can
format `error.masked()` without leaking document contents, and `crates/ayaml/tests/masked_errors.rs`
pins every error path in the crate that can interpolate document text against regressions.
