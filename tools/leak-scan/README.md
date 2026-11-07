# Leak gate

Every push to this repository is public at once and can't be fully recalled, so
two mechanical checks stand in front of it: a pre-push hook and the required
`leak scan` CI job. Both run [`gate.py`](gate.py), which rejects:

- credentials, using [gitleaks](https://github.com/gitleaks/gitleaks), pinned by
  version and archive digest in [`gitleaks.env`](gitleaks.env);
- personal filesystem paths, using patterns built into the gate;
- anything matching a private pattern list held outside this repository.

It reads each new commit's raw metadata and message, changed paths, and added
lines, any annotated tag messages, the branch or tag name, and the whole tree at the
tip. Text in UTF-16 or UTF-32 is read too. A commit that adds a line and a later commit
that removes it are still rejected, because both commits stay readable. In CI it also
reads the pull request's title and body, and re-runs when they're edited.

The gate can't see these:

- Review comments and issues.
- The squash commit's message, which is written at merge time, after the pull request's
  checks have run. The `leak scan` run on `main` reads it, but by then it's public, so
  use text that has already passed `gate.py text`, and check that run after merging.

The gate fails closed: if the list or the scanner is missing, or the list looks
truncated, it exits 2 and the push or check fails. Findings give a location and a
pattern id, never the matching text. Comments that tell gitleaks to skip a line are
rejected, and gitleaks ignore files in the repository have no effect.

## Setup for a new clone

```sh
tools/leak-scan/install-hook.sh
tools/leak-scan/install-gitleaks.sh DIR           # prints the binary's path
git config ayaml.gitleaks DIR/gitleaks
git config ayaml.leakPatterns /path/to/pattern-list
```

Run these once per clone. Linked worktrees share the clone's hook and configuration,
so `git worktree add` needs no extra setup. Don't bypass the hook with
`git push --no-verify`.

The installed hook runs the gate as it is on `origin/main`, so it still applies on a
branch that predates the gate or edits it. It fails closed if `origin/main` is missing.

A new clone starts with no hook, and CI is the only check until it's installed. CI scans
every pushed branch and `v*` tag, but a push runs only the workflows in the pushed commit,
so a branch cut from before the gate existed isn't scanned until it has a pull request.
Anything pushed has already been published by the time CI reads it.

The install script covers Linux on x86-64. Elsewhere, install the pinned gitleaks
version yourself; the gate refuses any other version.

The hook also refuses to publish anything except branches and `v*` tags, so notes
and other refs stay local.

## The pattern list

Maintainers hold the list privately, as a local file and as the
`AYAML_LEAK_PATTERNS` repository secret. Keep the two the same, but leave comments and
blank lines out of the secret: GitHub masks every line of a secret in logs, and a line
holding only `#` would mask that character everywhere. Each line has four fields, and
`#` starts a comment line:

```text
id class scope regex
```

- `id` is a short label that appears in findings, in place of the pattern.
- `class` is a name for the kind of content. `host`, `runner`, `repo`, and
  `comparison` must each appear at least once, and the last pattern must have the class
  `end`, so a list that was cut short is noticed. Its regex can be `(?!)`, which never
  matches.
- `scope` is `tree` to reject a match anywhere, or `added` to reject it only in
  new commits, for content that's still being removed from the tree.
- `regex` is a case-insensitive Python regular expression, and it can't begin with
  whitespace.

To check a list without printing it:

```sh
python3 tools/leak-scan/gate.py check-list
```

## How CI runs it

The `leak scan` job takes `gate.py` and its configuration from the base commit and reads
the change as data, so a change can't weaken the gate that judges it. A change to the gate
takes effect once it has landed. The change's own copy of the gate runs only its tests,
after the scans and with no secret.

GitHub takes the workflow file itself from the change, so an edit to
`.github/workflows/leak-gate.yml` could still weaken the job. Read any such edit closely.

## Pull requests from forks and bots

GitHub doesn't give repository secrets to workflows on pull requests from forks, so
the job fails there by design. A maintainer who has read the change brings it onto a
branch in this repository, keeping the author's commits, and the check runs there.

Dependabot's pull requests read only Dependabot's secrets, so the pattern list is kept
there as well, under the same name. Update both copies whenever the list changes.

## Running it by hand

```sh
python3 tools/leak-scan/gate.py scan --head HEAD --not origin/main
python3 tools/leak-scan/test_gate.py
```

The tests use made-up names only. They exercise the real gitleaks binary when
`AYAML_GITLEAKS` is set, as it is in CI.
