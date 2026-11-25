# Developer tooling and verification

`ayaml` ships with a reproducible verification stack covering YAML 1.2.2
conformance, performance and memory regression gates, fuzzing, formal
verification, and mutation testing.

## Local CI runner (`tools/ci-local.sh`)

Run the same checks that gate pull requests locally before pushing:

```sh
tools/ci-local.sh --list       # list all mirrored CI jobs
tools/ci-local.sh quality      # fmt, clippy, panic-profile clippy, workspace tests, docs
tools/ci-local.sh suite        # pinned YAML Test Suite (default + compat profiles)
tools/ci-local.sh allocation   # JSONL baseline + legacy corpus allocation gates
tools/ci-local.sh docs-lint    # markdownlint, link/coverage checks, site build, book doctests
```

## Unified benchmark & measurement CLI (`cargo xtask`)

The workspace defines a `cargo xtask` alias (`crates/xtask`) that drives all
deterministic fixtures, Criterion timings, counting-allocator memory probes,
Callgrind instruction/cycle diffs, and 10x linear scaling checks:

```sh
cargo xtask fixtures --verify                     # verify pinned SHA-256 fixture digests
cargo xtask speed --out-dir target/xtask          # export Criterion speed.jsonl
cargo xtask memory --suite baseline               # export counting-allocator memory.jsonl
cargo xtask det --base-rev HEAD~1                 # steady-state Callgrind Ir + EstCycles diff
cargo xtask scale --out-dir target/xtask          # verify 10x O(n) time & memory scaling
cargo xtask compare --baseline DIR_A --current DIR_B
cargo xtask gate                                  # run CI allocation & baseline gates
cargo xtask report --dir target/xtask             # render Markdown summary tables
```

See [Benchmark methodology](benchmarks.md) for schema details and hardware
provenance rules.

## YAML Test Suite conformance

`crates/ayaml-conformance` runs the pinned YAML Test Suite under both the
default YAML 1.2.2 profile and the optional `libyaml-compat` profile:

```sh
AYAML_STRICT_SUITE=1 cargo run --locked -p ayaml-conformance -- tests/fixtures tests/data/yaml-test-suite/src
AYAML_STRICT_SUITE=1 cargo run --locked -p ayaml-conformance --features compat -- tests/fixtures tests/data/yaml-test-suite/src
```

Every `README.md` and `docs/**/*.md` Rust snippet is also compiled and executed
through `crates/ayaml-conformance` doctests:

```sh
cargo test --doc -p ayaml-conformance --all-features --locked
```

## Fuzzing (10 libFuzzer targets)

The `fuzz/` workspace defines 10 libFuzzer targets covering parsing, typed
decoding, emission, event-stream emission, `SpannedTree` / `Spanned<T>`, merge
keys, masked error canary non-leakage, `ayaml-edit`, and tree-vs-event
equivalence (`parse`, `emit`, `borrowed`, `structured`, `spanned`,
`merge_keys`, `masked_errors`, `event_emit`, `typed_equiv`, `edit`).

Run the deterministic seed smoke tests or bound a live libFuzzer run:

```sh
cargo test --manifest-path fuzz/Cargo.toml --lib --locked
timeout -k 5 120 nice -n 10 cargo +nightly fuzz run spanned -- -max_total_time=60 -rss_limit_mb=2048
```

## Adversarial tests, Miri, Kani, and mutation testing

- **Adversarial DoS suite:** `crates/ayaml/tests/adversarial.rs` isolates
  hostile inputs inside child processes with a 1 GiB virtual-memory cap and
  wall-clock deadline. Always bound full adversarial runs:

```sh
( ulimit -v 8388608; timeout -k 5 300 nice -n 10 cargo test -p ayaml --all-features --locked )
```

- **Miri:** Scheduled CI checks `crates/ayaml` under Miri with strict provenance
  and isolation.
- **Kani bounded model checking:** Pure arithmetic and bounds invariants are
  verified via `python3 tools/run_kani.py --package ayaml --all-features`
  (which enforces per-harness virtual-memory, peak-RSS, and timeout bounds).
- **Mutation testing:** `.cargo/mutants.toml` and `.github/workflows/mutants.yml`
  shard `cargo-mutants` across 16 bounded single-worker jobs in scheduled CI.
- **Leak gate:** `tools/leak-scan/install-hook.sh` installs the pre-push hook
  mirroring the required `leak scan` CI check.
