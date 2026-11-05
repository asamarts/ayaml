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
leak gate in `tools/leak-scan/` runs before it lands.

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
