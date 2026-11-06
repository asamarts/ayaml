# ayaml decisions

Technical decisions made since this repository became the development home. Each
record states what was decided and why, so a later reader can tell whether the
reason still holds. Numbering continues from earlier records that aren't published,
which is why it starts at D65.

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
pull requests from forks or from Dependabot, so the job fails on them by design, and a
maintainer who has read the change brings it onto a branch here.

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
the rest of what the alias copies. 64 MiB holds 640,000 uses of a 100-byte prefix, more than any
real document needs, and it stays below the hostile tests' 256 MiB peak.

**Consequences.** `Limits` and `LimitKind` each gain a member. Both are `#[non_exhaustive]`, so this
isn't a breaking change. A document with more expansions than this needs a higher limit.
