# Contributing to ayaml

## How a change lands

Every change reaches `main` through a pull request, including changes from
maintainers. Pull requests are squash-merged, so the title and body become the
commit message, and they need to stand on their own. A merge needs all required
checks to pass on a branch that's up to date with `main`.

1. Create a branch.
2. Make the change, with tests and a `CHANGELOG.md` entry if users can see it.
3. Open a pull request and wait for CI.

To report a vulnerability, follow [`SECURITY.md`](SECURITY.md) instead.

## Setting up a clone

You need stable Rust and Rust 1.88, the oldest version ayaml supports. Fuzzing needs
nightly and `cargo-fuzz`, the instruction check needs `valgrind`, the unsafe census needs
`cargo-geiger` 0.14.0, and plan changes need `aplan`
(`cargo install aplan --version 0.6.0 --locked`).

Maintainers also set up the leak gate, which needs a private pattern list. See
[`tools/leak-scan/README.md`](tools/leak-scan/README.md). Pull requests from forks
don't get that list, so a maintainer runs the check by bringing the change onto a
branch here.

## Running the checks locally

`tools/ci-local.sh` runs most of what the hosted workflows run, and its header lists what
differs:

```sh
tools/ci-local.sh --list          # the job names
tools/ci-local.sh quality-stable  # one job
```

Hosted CI decides, but running the relevant job first saves a round trip.

The adversarial, property, and `--all-features` test runs feed hostile input to the
parser, so bound them on a shared machine:

```sh
( ulimit -v 8388608; timeout -k 5 300 nice -n 10 cargo test -p ayaml --all-features --locked )
```

Don't run an experiment that disables or zeroes an alias, depth, or expansion limit
without a memory bound. It can turn those tests into real memory exhaustion.

Two bounds also apply on their own:

- `crates/ayaml/tests/adversarial.rs` runs each test that reads YAML in a child process with a
  300 s deadline and, on Linux, 1 GiB of address space. A runaway test fails instead of
  exhausting the machine, and a child also ends itself 30 s after its deadline, in case the test
  binary died first. These tests normally peak below 210 MiB of memory, and one that passes
  256 MiB fails on Linux. macOS and Windows don't get the address-space limit or that check.
- `tools/ci-local.sh` runs test steps with 8 GiB of address space per process and a 30-minute
  timeout that kills the step's whole process group, and a signal to the script ends the running
  step too. A cold `cargo test --workspace --all-features` passes with 4 GiB. Fuzz steps, the
  instruction counts, and the consumer smoke get only the timeout. The script needs Linux and GNU
  `timeout`, and those steps fail where it can't set these bounds. The `bounds` job checks that
  they work.

## Scheduled checks

Fuzzing and an advisory scan of the dependencies run every day. A fuzz run prints no input, and
what it finds uploads encrypted (D74). `tools/check_scheduled_runs.py`
runs later each day, and it opens an issue if either workflow failed, didn't run, or passed and left
a sealed file. Close that issue by hand after reading what the run sealed, since a later green run
doesn't read it, and closing it doesn't file the same problem again. GitHub turns off every schedule in a repository after 60 days with no activity, the check's own included,
so run `tools/ci-local.sh scheduled` before a release.

## Checks that run before a release

Maintainers run `tools/ci-local.sh --release`, which fails if any of its checks had to be
skipped, and two further sets of checks, which tasks in `aplan/tasks/` refer to:

- The **performance gates** compare timing and peak memory with earlier measurements.
  They need a machine with nothing else running, called a quiet host in the tasks.
- The **comparison gates** check parsing results against a larger private set of inputs.

Their results aren't published. CI's allocation budgets and instruction counts are the
public checks of the same concerns.

## Releasing

A release is published by the release workflow when a maintainer pushes a `vX.Y.Z` tag on
`main`, after the version bump and changelog land through a pull request. Nobody publishes
with `cargo publish` from a workstation. [`RELEASING.md`](RELEASING.md) gives the steps, the
recovery from a failed run, and how to verify a release.

## Conventions

- **Dependencies:** `tools/check_deps.py` and `deny.toml` classify every dependency,
  and [`docs/dependencies.md`](docs/dependencies.md) gives the policy. Adding a
  runtime dependency is a decision to discuss first.
- **Unsafe code:** this workspace forbids it. The fuzz workspace's counting allocator
  is the one exception.
- **Compatibility:** the default mode follows YAML 1.2.2. Behavior that imitates
  libyaml belongs only in `ayaml::compat`, and libyaml is the one other parser the
  documentation names. Describe behavior on its own terms, with literal expected
  values in tests.
- **Limits:** a new way to spend CPU or memory needs a limit, with tests just below,
  at, and above it.
- **Plan:** tasks live under `aplan/tasks/`; `aplan check` validates their structure and
  `aplan verify` resolves their evidence against the repository. Both run in CI.
  Decisions that outlive a change go in [`docs/decisions.md`](docs/decisions.md).
- **Security fixes:** state what to build in public text. Reproduction inputs for an
  unfixed problem wait until the fix ships.

## License

Unless you say otherwise, your contribution is dual-licensed under MIT and
Apache-2.0, as the project is, with no additional terms.
