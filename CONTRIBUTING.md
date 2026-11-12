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

The repository lint needs alint 0.22.0. Install the pinned release from its own script:

```sh
work="$(mktemp -d)"
curl -fsSL https://raw.githubusercontent.com/asamarts/alint/v0.22.0/install.sh -o "$work/install.sh"
ALINT_VERSION=v0.22.0 bash "$work/install.sh"
rm -rf "$work"
```

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

## Rust lint policy

The workspace lint policy lives in `Cargo.toml`, with `clippy.toml` for the 200-line
function threshold and `rustfmt.toml` for the 2024 style edition, Unix newlines, field-init
shorthand, and try shorthand. The policy is checked on Rust 1.88 and stable with `-D warnings`.

The Clippy policy enables `all`, `cargo`, and `pedantic`. The only policy-wide allows are
`multiple_crate_versions`, `module_name_repetitions`, and `similar_names`. It also enables
`allow_attributes_without_reason`, `literal_string_with_formatting_args`, `mem_forget`,
`needless_pass_by_ref_mut`, `or_fun_call`, and `redundant_clone`; `dbg_macro`, `todo`, and
`unimplemented` are denied. The nursery and restriction groups stay off: the scanner has many
proven-boundary indexing, slicing, and numeric casts where blanket suppressions would hide the
reasoning instead of improving safety.

Fix a new finding rather than adding a baseline. When a finding is genuinely intentional, use the
narrowest `#[expect(lint, reason = "...")]` or `#[allow(lint, reason = "...")]` that covers it. A
stable release that adds a finding is an ordinary pull request: fix it or record the decision in
the pull request and the lint decision record. Keep the policy on both toolchains; a new lint that
Rust 1.88 does not know is not added until both accept it.

The production library has a stricter panic profile. It denies `unwrap_used`, `expect_used`, and
`panic`, so fallible parsing and writing return errors and a panic is only allowed for a required
trait contract with a narrow reason. Tests and development targets are not part of that profile.

The separate `fuzz` workspace declares Rust 1.88 and mirrors the applicable policy. Its three
fuzz targets forbid unsafe code; `memcount` is the one documented exception because a counting
global allocator needs `unsafe`. Run the fuzz workspace's format and Clippy checks in both
toolchain jobs.

Rustdoc uses `#![warn(rustdoc::all)]` and `RUSTDOCFLAGS=-D warnings`; public `Result` APIs
document their error cases under `# Errors`. The practical local commands are:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy -p ayaml --lib --all-features -- -D warnings -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic
cargo fmt --manifest-path fuzz/Cargo.toml --all -- --check
cargo clippy --manifest-path fuzz/Cargo.toml --all-targets -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --all-features
```

## Repository lint

`.alint.yml` requires alint 0.22.0 and extends the bundled `oss-baseline`, `rust`,
`ci/github-actions`, `hygiene/no-tracked-artifacts`, `tooling/editorconfig`,
`agent-hygiene`, and `agent-context` v1 rulesets. The hosted `repository lint` job runs
the official action pinned to the v0.22.0 commit with `--fail-on-warning`; local CI runs
the same check, and a release run fails if the local binary is missing or a different
version.

```sh
alint check --fail-on-warning
tools/ci-local.sh alint
```

Three source files predate the 2,000-line ceiling and carry a non-growth ratchet in
`.alint.yml`: the scanner, parser, and emitter. Fix the tree or remove a ratchet in the
change that shrinks the file; there is no baseline. `--changed` is not the
gate: it depends on a base revision and lets a shape violation that is already on the base
survive.

To move to another alint version, change the `requires:` value, the action pin in
`.github/workflows/ci.yml`, and the local binary together, then repeat the 2,000/2,001-line
boundary probes for every covered extension, `tools/hooks/*`, and a Rust source outside
today's `crates/`, `fuzz/`, and `tools/` directories.

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
