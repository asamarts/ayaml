# Leak gate

Every push to this repository is public at once and can't be fully recalled, so
two mechanical checks stand in front of it: a pre-push hook and the required
`leak scan` CI job. Both run [`gate.py`](gate.py), which rejects:

- credentials, using [gitleaks](https://github.com/gitleaks/gitleaks), pinned by
  version and archive digest in [`gitleaks.env`](gitleaks.env);
- personal filesystem paths, using patterns built into the gate;
- anything matching a private pattern list held outside this repository.

It reads each new commit's raw metadata and message, changed paths, and added
lines, as well as the whole tree at the tip. A commit that adds a line and a later
commit that removes it are still rejected, because both commits stay readable. In
CI it also reads the pull request's title and body, and re-runs when they're edited.
Review comments and issues aren't scanned.

The gate fails closed: if the list or the scanner is missing, or the list looks
truncated, it exits 2 and the push or check fails. Findings give a location and a
pattern id, never the matching text.

## Setup for a new clone

```sh
git config core.hooksPath tools/hooks
tools/leak-scan/install-gitleaks.sh DIR           # prints the binary's path
git config ayaml.gitleaks DIR/gitleaks
git config ayaml.leakPatterns /path/to/pattern-list
```

Run these once per clone. Linked worktrees share the clone's configuration, so
`git worktree add` needs no extra setup. A new clone starts with no hook, so CI
is the backstop. Don't bypass the hook with `git push --no-verify`.

The install script covers Linux on x86-64. Elsewhere, install the pinned gitleaks
version yourself; the gate refuses any other version.

The hook also refuses to publish anything except branches and `v*` tags, so notes
and other refs stay local.

## The pattern list

Maintainers hold the list privately, as a local file and as the
`AYAML_LEAK_PATTERNS` repository secret. Keep the two the same. Each line has four
fields, and `#` starts a comment line:

```text
id class scope regex
```

- `id` is a short label that appears in findings, in place of the pattern.
- `class` is a name for the kind of content. `host`, `runner`, `repo`, and
  `comparison` must each appear at least once.
- `scope` is `tree` to reject a match anywhere, or `added` to reject it only in
  new commits, for content that's still being removed from the tree.
- `regex` is a case-insensitive Python regular expression, and it can't begin with
  whitespace.

To check a list without printing it:

```sh
python3 tools/leak-scan/gate.py check-list
```

## Pull requests from forks and bots

GitHub doesn't give repository secrets to workflows on pull requests from forks or
from Dependabot, so the job fails there by design. A maintainer who has read the
change brings it onto a branch in this repository, keeping the author's commits,
and the check runs there.

## Running it by hand

```sh
python3 tools/leak-scan/gate.py scan --head HEAD --not origin/main
python3 tools/leak-scan/test_gate.py
```

The tests use made-up names only. They exercise the real gitleaks binary when
`AYAML_GITLEAKS` is set, as it is in CI.
