#!/usr/bin/env bash
# Local mirror of .github/workflows/{ci,fuzz}.yml, so that failures show up before a
# push. Hosted CI on the pull request is the gate; this script is a convenience.
#
# Usage: tools/ci-local.sh [--list] [job ...]
#
#   --list    print the job names and exit
#
# With no job names, every job runs. Commands run under `nice`; the script
# does not change build parallelism. Logs go to target/ci-local/<sha>/.
#
# Every single-line `run:` command in those workflows must appear verbatim in
# this file (the `sync` job enforces it), so hosted and local CI cannot drift.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 2

MSRV="1.88.0"
STABLE="stable"
ALL_JOBS=(sync quality-msrv quality-stable features policy harness suite docs-lint package cross fuzz-smoke leak-gate)

jobs=()
for arg in "$@"; do
  case "$arg" in
    --list) printf '%s\n' "${ALL_JOBS[@]}"; exit 0 ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    -*) echo "unknown option: $arg" >&2; exit 2 ;;
    *) jobs+=("$arg") ;;
  esac
done
[ "${#jobs[@]}" -eq 0 ] && jobs=("${ALL_JOBS[@]}")

dirty=0
[ -n "$(git status --porcelain --untracked-files=no)" ] && dirty=1

sha="$(git rev-parse HEAD)"
short="$(git rev-parse --short HEAD)"
logdir="$ROOT/target/ci-local/$short"
mkdir -p "$logdir"
started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
t0=$SECONDS
declare -A status
current=""
job_failed=0

# step NAME CMD... — run one command, log it, and mark the job failed on error.
step() {
  local name="$1"
  shift
  local log="$logdir/$current.log"
  printf '  %-58s ' "$name"
  local s=$SECONDS
  {
    printf '\n### %s\n$ %s\n' "$name" "$*"
  } >>"$log"
  if nice -n 10 "$@" >>"$log" 2>&1; then
    printf 'ok   (%ss)\n' "$((SECONDS - s))"
  else
    printf 'FAIL (%ss) — see %s\n' "$((SECONDS - s))" "${log#"$ROOT"/}"
    job_failed=1
  fi
}

# shstep NAME 'shell command' — like step, for commands that need a shell.
shstep() {
  local name="$1"
  step "$name" bash -c "$2"
}

skip() {
  status[$current]="skip($1)"
  printf '  skipped: %s\n' "$1"
}

have_toolchain() { rustup run "$1" rustc --version >/dev/null 2>&1; }
have_target() { rustup target list --toolchain "$1" --installed 2>/dev/null | grep -qx "$2"; }

job_sync() {
  shstep "workflow run: lines are mirrored here" '
    missing=0
    for wf in .github/workflows/ci.yml .github/workflows/fuzz.yml; do
      while IFS= read -r cmd; do
        case "$cmd" in
          "cargo install cargo-geiger --locked"|"cargo install cargo-fuzz --locked") continue ;;
        esac
        if ! grep -qF -- "$cmd" tools/ci-local.sh; then
          echo "not mirrored ($wf): $cmd"; missing=1
        fi
      done < <(sed -n "s/^ *run: \(.*\)$/\1/p" "$wf" | grep -v "^[|>]" \
        | sed "s/\${{ matrix.target }}/TARGET/g; s/\${RUNNER_TEMP}/\$TMPDIR/g; s/\$GITHUB_WORKSPACE/\$PWD/g")
    done
    exit $missing'
}

quality() {
  local tc="$1"
  if ! have_toolchain "$tc"; then skip "toolchain $tc missing"; return; fi
  step "fmt ($tc)" cargo "+$tc" fmt --all -- --check
  step "clippy ($tc)" cargo "+$tc" clippy --workspace --all-targets --locked --all-features -- -D warnings
  step "test ($tc)" cargo "+$tc" test --workspace --locked --all-features
  # The depth cap is measured in both profiles.
  step "adversarial release ($tc)" cargo "+$tc" test -p ayaml --locked --release --all-features --test adversarial
  step "doc ($tc)" cargo "+$tc" doc --workspace --locked --no-deps --all-features
  # Mirrors: cargo fmt --all -- --check
  # Mirrors: cargo clippy --workspace --all-targets --locked --all-features -- -D warnings
  # Mirrors: cargo test --workspace --locked --all-features
  # Mirrors: cargo test -p ayaml --locked --release --all-features --test adversarial
  # Mirrors: cargo doc --workspace --locked --no-deps --all-features
}
job_quality-msrv() { quality "$MSRV"; }
job_quality-stable() { quality "$STABLE"; }

# Feature combinations: the compat path is what downstream compatibility suites run.
job_features() {
  local f
  for f in "--all-features" "--features libyaml-compat" "--features bench-internals"; do
    # shellcheck disable=SC2086
    step "clippy ayaml $f" cargo "+$STABLE" clippy -p ayaml --all-targets --locked $f -- -D warnings
    # shellcheck disable=SC2086
    step "test ayaml $f" cargo "+$STABLE" test -p ayaml --locked $f
  done
  step "test ayaml --all-features ($MSRV)" cargo "+$MSRV" test -p ayaml --locked --all-features
  step "doc ayaml --all-features" cargo "+$STABLE" doc -p ayaml --locked --no-deps --all-features
}

job_policy() {
  step "classify dependencies" python3 tools/check_deps.py
  # Mirrors: python3 tools/check_deps.py
  if command -v cargo-deny >/dev/null; then
    step "cargo-deny" cargo deny check
  else
    echo "  cargo-deny not installed"; job_failed=1
  fi
  if cargo geiger --version >/dev/null 2>&1; then
    local TMPDIR="$logdir"
    shstep "unsafe census" "cargo geiger --manifest-path \"\$PWD/crates/ayaml/Cargo.toml\" --all-features --output-format Json > \"$TMPDIR/geiger.json\""
    # Mirrors: cargo geiger --manifest-path "$PWD/crates/ayaml/Cargo.toml" --all-features --output-format Json > "$TMPDIR/geiger.json"
  else
    echo "  cargo-geiger not installed: unsafe census skipped"
  fi
}

job_harness() {
  step "conformance fixtures and pinned suite" cargo run --locked -p ayaml-conformance -- tests/fixtures tests/data/yaml-test-suite/src
  step "fuzz targets compile" cargo check --manifest-path fuzz/Cargo.toml --locked --target x86_64-unknown-linux-gnu
  step "allocation budgets" tools/check_alloc_corpus.sh
  step "benchmarks compile" cargo bench --locked -p ayaml-bench --no-run
}

# Strict suite in both parser modes.
job_suite() {
  step "strict suite (default mode)" env AYAML_STRICT_SUITE=1 cargo run --locked -p ayaml-conformance -- tests/fixtures tests/data/yaml-test-suite/src
  step "strict suite (compat mode)" env AYAML_STRICT_SUITE=1 cargo run --locked -p ayaml-conformance --features compat -- tests/fixtures tests/data/yaml-test-suite/src
  # Mirrors: AYAML_STRICT_SUITE=1 cargo run --locked -p ayaml-conformance -- tests/fixtures tests/data/yaml-test-suite/src
  # Mirrors: AYAML_STRICT_SUITE=1 cargo run --locked -p ayaml-conformance --features compat -- tests/fixtures tests/data/yaml-test-suite/src
}

job_docs-lint() {
  if command -v npx >/dev/null; then
    step "markdownlint" npx --yes markdownlint-cli2@0.23.3 "README.md" "docs/*.md"
  else
    skip "npx not installed"
  fi
}

# cargo package plus a fresh downstream consumer built from the .crate.
job_package() {
  local allow=()
  [ "$dirty" -eq 1 ] && allow=(--allow-dirty)
  step "cargo package" cargo package -p ayaml --locked "${allow[@]}"
  # Mirrors: cargo package -p ayaml --locked
  local crate
  crate="$(ls -t target/package/ayaml-*.crate 2>/dev/null | head -1)"
  if [ -z "$crate" ]; then echo "  no packaged crate"; job_failed=1; return; fi
  shstep "packaged crate contents" "tar -tzf '$crate' | sed 's|^[^/]*/||' | sort > '$logdir/package-files.txt'; cat '$logdir/package-files.txt'"
  local work
  work="$(mktemp -d "$ROOT/target/ayaml-smoke.XXXXXX")"
  tar -xzf "$crate" -C "$work"
  local src
  src="$(find "$work" -maxdepth 1 -mindepth 1 -type d -name 'ayaml-*' | head -1)"
  mkdir -p "$work/consumer/src"
  cat >"$work/consumer/Cargo.toml" <<EOF
[package]
name = "ayaml-smoke"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
ayaml = { path = "$src" }

[workspace]
EOF
  cat >"$work/consumer/src/main.rs" <<'EOF'
use std::collections::BTreeMap;

fn main() {
    let input = "name: smoke\nitems:\n  - 1\n  - 2\nnested:\n  - - a\n    - b\n";
    let value = ayaml::parse_value(input).expect("parse_value");
    let rendered = ayaml::to_string_value(&value).expect("to_string_value");
    assert_eq!(ayaml::parse_value(&rendered).expect("reparse"), value);
    let typed: BTreeMap<String, ayaml::Value> = ayaml::from_str(input).expect("from_str");
    assert_eq!(typed.len(), 3);
    let events = ayaml::parse_events(input).expect("parse_events").count();
    assert!(events > 0);
    println!("downstream smoke ok: {} events, ayaml {}", events, ayaml::VERSION);
}
EOF
  cp Cargo.lock "$work/consumer/Cargo.lock" 2>/dev/null || true
  step "downstream consumer smoke" cargo run --quiet --offline --manifest-path "$work/consumer/Cargo.toml" --target-dir "$work/target"
  rm -rf "$work"
}

# The crate type-checks for macOS and Windows targets.
job_cross() {
  local t
  for t in aarch64-apple-darwin x86_64-apple-darwin x86_64-pc-windows-msvc x86_64-pc-windows-gnu; do
    if have_target "$STABLE" "$t"; then
      step "check --all-features --target $t" cargo "+$STABLE" check --locked --all-features --target "$t"
      # Mirrors: cargo check --locked --all-features --target TARGET
    else
      echo "  target $t not installed for $STABLE: skipped"
    fi
  done
}

# Short fuzz smoke (the nightly workflow runs 60 s per target).
job_fuzz-smoke() {
  # Ask cargo, not PATH: cargo also finds subcommands in $CARGO_HOME/bin,
  # which agent shells often leave off PATH (the job used to skip there).
  if ! have_toolchain nightly || ! cargo +nightly fuzz --version >/dev/null 2>&1; then
    skip "cargo-fuzz or nightly missing"
    return
  fi
  local TARGET
  for TARGET in parse emit borrowed; do
    step "fuzz $TARGET (60 s)" cargo +nightly fuzz run "$TARGET" --target x86_64-unknown-linux-gnu -- -max_total_time=60
    # Mirrors: cargo fuzz run TARGET --target x86_64-unknown-linux-gnu -- -max_total_time=60
  done
}

# Hosted-only: minimal versions needs a throwaway lockfile, so run it in CI.
# Mirrors: cargo update -Z minimal-versions
# Mirrors: cargo test --workspace --all-features

job_leak-gate() {
  step "leak gate tests" python3 tools/leak-scan/test_gate.py
  if git rev-parse --verify --quiet origin/main >/dev/null; then
    step "leak scan since origin/main" python3 tools/leak-scan/gate.py scan --head HEAD --not origin/main
  else
    skip "origin/main is not available"
  fi
}

failed=0
for job in "${jobs[@]}"; do
  if ! declare -F "job_$job" >/dev/null; then
    echo "unknown job: $job (see --list)" >&2
    exit 2
  fi
  current="$job"
  job_failed=0
  : >"$logdir/$job.log"
  echo "== $job"
  "job_$job"
  if [ -z "${status[$job]:-}" ]; then
    if [ "$job_failed" -eq 0 ]; then status[$job]=pass; else status[$job]=FAIL; failed=1; fi
  fi
done

result=PASS
[ "$failed" -eq 1 ] && result=FAIL
summary="$(
  echo "ci-local: $result"
  echo "commit: $sha$([ "$dirty" -eq 1 ] && echo ' (dirty tree)')"
  echo "started: $started"
  echo "duration: $((SECONDS - t0))s"
  echo "host: $(uname -srm)"
  echo "rustc $STABLE: $(rustc "+$STABLE" --version 2>/dev/null)"
  echo "rustc msrv: $(rustc "+$MSRV" --version 2>/dev/null)"
  for job in "${jobs[@]}"; do echo "job $job: ${status[$job]}"; done
)"
echo
echo "$summary" | tee "$logdir/summary.txt"

exit "$failed"
