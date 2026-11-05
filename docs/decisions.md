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
