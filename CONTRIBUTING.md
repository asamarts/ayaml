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
nightly and `cargo-fuzz`, the instruction check needs `valgrind`, and plan changes need
`aplan` (`cargo install aplan --version 0.6.0 --locked`).

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

- `crates/ayaml/tests/adversarial.rs` runs each hostile test in a child process with a 300 s
  deadline and, on Linux, 1 GiB of address space. A runaway test fails instead of exhausting
  the machine. These tests normally peak near 220 MiB, and one that passes 512 MiB fails. macOS
  and Windows don't get the address-space limit.
- `tools/ci-local.sh` runs test steps with 8 GiB of address space per process and a 30-minute
  timeout that kills the step's whole process group. A cold `cargo test --workspace
  --all-features` passes with 4 GiB. The script needs Linux and GNU `timeout`, and its test
  steps fail where it can't set these bounds. The `bounds` job checks that they work.

## Checks that run before a release

Maintainers run two further sets of checks before a release, and tasks in `aplan/tasks/`
refer to them:

- The **performance gates** compare timing and peak memory with earlier measurements.
  They need a machine with nothing else running, called a quiet host in the tasks.
- The **comparison gates** check parsing results against a larger private set of inputs.

Their results aren't published. CI's allocation budgets and instruction counts are the
public checks of the same concerns.

## Releasing

Until the automated workflow in task AY-004 exists, a release is done by hand: the version
bump and changelog land through a pull request, a maintainer tags that commit on `main` as
`vX.Y.Z`, and then publishes from a clean checkout of the tag with `cargo publish`.

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
- **Plan:** tasks live under `aplan/tasks/` and `aplan check` validates them.
  Decisions that outlive a change go in [`docs/decisions.md`](docs/decisions.md).
- **Security fixes:** state what to build in public text. Reproduction inputs for an
  unfixed problem wait until the fix ships.

## License

Unless you say otherwise, your contribution is dual-licensed under MIT and
Apache-2.0, as the project is, with no additional terms.
