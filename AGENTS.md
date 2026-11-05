# ayaml agent notes

Guidance for coding agents working in this repository. People should start with
[`CONTRIBUTING.md`](CONTRIBUTING.md), which this file assumes. A local `CLAUDE.md`
is ignored by Git and can hold notes specific to one machine.

## Layout

- `crates/ayaml`: the library, including the optional `ayaml::compat` module.
- `crates/ayaml-conformance`: the YAML Test Suite runner.
- `crates/ayaml-bench`: timing benchmarks.
- `tests/`: fixtures, the pinned suite, and real-world files.
- `fuzz/`: `cargo-fuzz` targets and seeds.
- `tools/`: local CI, allocation and instruction checks, the dependency policy check,
  and the leak gate.
- `aplan/tasks/`: the task plan.
- `docs/`: design, limits, compatibility, and decisions.

## Making a change

Every change goes through a squash-merged pull request, and `main` refuses direct
pushes, including from admins. Don't try to get around a failing check. Fix the cause
or stop and say what's blocking you.

- Install the leak gate with `tools/leak-scan/install-hook.sh` before the first push
  from a clone. See `tools/leak-scan/README.md` for the rest of its setup.
- Never use `git push --no-verify`.
- Commit or open a pull request only when asked.

## Public text

Every push is public at once and can't be fully recalled. That covers commit
messages, branch names, pull request titles and bodies, and code. Keep out machine
names, runner names, local paths, private repository names, and anything from a
private planning document. Name no parser other than libyaml, and only for what
`ayaml::compat` imitates. If the leak gate rejects a commit, rewrite that commit,
because removing the text in a later commit doesn't clear it.

Task text says what to build. Reproduction inputs for an unfixed resource or
correctness problem wait until the fix ships.

## Plan

```sh
aplan check    # validate the plan
aplan ready    # tasks with nothing blocking them
aplan verify   # resolve evidence against the repository
```

Close a task in the pull request that finishes it, citing test evidence. Decisions
that outlive a change go in `docs/decisions.md`.

## Tests that feed hostile input

Adversarial, fuzz, property, and `--all-features` runs can use a lot of memory if a
limit regresses, so always bound them:

```sh
( ulimit -v 8388608; timeout -k 5 300 nice -n 10 cargo test -p ayaml --all-features --locked )
```

- A sanitizer build can't run under `ulimit -v`, because it reserves a huge address
  space. Bound a fuzz run with libFuzzer's `-rss_limit_mb=2048`, plus `timeout` and
  `nice`.
- Never run a mutant or experiment that disables or zeroes an alias, depth, or
  expansion limit on a shared machine.
- If an adversarial test's memory passes a few GiB on unmodified code, stop it and
  file a task. Don't let it run to completion.
- When a machine is slow, look for the process with the largest resident memory,
  not the highest CPU. The busy processes are often reclaim victims.

## Gates

Hosted CI on the pull request decides. `tools/ci-local.sh --list` shows jobs you can
run first. Don't weaken a gate, a limit, or a test's assertion to make a change pass.
