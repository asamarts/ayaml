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
