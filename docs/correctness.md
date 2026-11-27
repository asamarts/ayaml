# Correctness and verification

`ayaml` verifies parser, serializer, and editor correctness across eight layers,
from zero-`unsafe` compilation to fuzzing, Miri, and Kani formal proofs.

YAML 1.2.2 is a subtle format. Indentation rules, block scalars, anchors and aliases,
explicit mapping keys, flow-inside-block nesting, and multi-byte UTF-8 byte offsets create
edge cases where ordinary unit tests alone miss bugs. Relying on a single technique leaves
blind spots: unit tests only check inputs a human thought to write, fuzzers only explore
paths they can reach, and formal proofs only hold inside their modeled bounds.

Instead of betting on one method, `ayaml` layers eight complementary checks so bugs that slip
past one layer are caught by another. Every section below explains what the technique is in
plain terms, what bug class it catches, how `ayaml` runs it, and what it does not prove.

## At a glance

| Layer | Technique | What it catches | Scale in `ayaml` | Cadence |
| --- | --- | --- | --- | --- |
| 1 | Static checks & API freeze | `unsafe` code, hidden panics, SemVer breaks, dep creep | 0 `unsafe`, 2 direct runtime deps, 3 OSes, MSRV 1.88 | Every PR |
| 2 | Conformance & differential oracles | Spec mismatches vs YAML 1.2.2, JSON, and real configs | 405 suite cases (2,240 checks, 0 deviations) | Every PR |
| 3 | Unit, fault-injection & DoS tests | Logic bugs, short writes, alias bombs, stack/heap DoS | 891 tests + 61 doctests (952 total), 37 adversarial tests | Every PR |
| 4 | Property-based testing (`proptest`) | Invariant breaks on generated trees and round trips | 27 properties (up to 16,384 cases each) | Every PR + daily |
| 5 | Structure-aware fuzzing (`libFuzzer` + ASan) | Crashes, hangs, secret leaks, parser/stream drift | 10 fuzz targets + 7 stable PR seed suites | Every PR + daily |
| 6 | Undefined behavior detection (Miri) | Memory/aliasing UB in stdlib, deps, and test allocator | Core, post-0.2.3 APIs, `ayaml-edit`, `memcount` | Daily |
| 7 | Bounded model checking (Kani / CBMC) | Integer overflows, slice panics, sorting-order bugs | 26 `#[kani::proof]` harnesses across 8 files | Daily |
| 8 | Mutation testing, perf gates & supply chain | Weak tests, O(n^2) or heap regressions, tampered releases | 16 mutant shards, Callgrind, Trusted Publishing | Every PR / weekly / release |

---

## 1. Compile-time guarantees and static discipline

**What it is:** Checks that run while compiling the code, before any test binary executes.
Rust's type system and borrow checker prevent whole classes of memory-safety bugs (buffer
overflows, use-after-free, data races) in safe code, provided the crate avoids `unsafe`
blocks. See [Safe and Unsafe Rust in the Rustonomicon](https://doc.rust-lang.org/nomicon/meet-safe-and-unsafe.html)
for background on what `unsafe` unlocks and why avoiding it matters.

**What bugs it catches:** Memory corruption, unintended panics on untrusted input, accidental
breaking API changes, dependency bloat, and platform-specific compilation failures.

**How `ayaml` enforces it:**

- **Zero `unsafe` in published crates:** Both `ayaml` and `ayaml-edit` declare
  `#![forbid(unsafe_code)]`, which makes any `unsafe` block a hard compiler error. Every pull
  request also runs `cargo-geiger` via
  [`tools/check_unsafe_census.py`](https://github.com/asamarts/ayaml/blob/main/tools/check_unsafe_census.py)
  over the full dependency tree to verify that `ayaml` and `ayaml-edit` contain zero `unsafe`
  items and forbid `unsafe_code`, that no unexpected source files went unscanned, and that any
  `unsafe` code in transitive dependencies stays inside an explicit allowlist (`serde`,
  `serde_core`, `indexmap`, `hashbrown`).
- **Panic-free production library lint profile:** Both library crates are checked with
  `cargo clippy --lib ... -- -D warnings -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic`.
  Library code must return structured `Result<_, Error>` values rather than aborting the
  caller's process.
- **Minimal runtime dependencies and scanner isolation:** Direct runtime dependencies of
  `ayaml` are restricted to `serde` and `indexmap` (with only `serde_core`, `hashbrown`, and
  `equivalent` as transitive runtime crates, and `ayaml-edit` depending only on `ayaml`),
  enforced by
  [`tools/check_deps.py`](https://github.com/asamarts/ayaml/blob/main/tools/check_deps.py)
  and `cargo-deny` (see [Dependency policy](dependencies.md)). A dedicated policy test in
  [`crates/ayaml/tests/repository_policy.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/tests/repository_policy.rs)
  verifies no `indexmap` type leaks into `ayaml`'s public API, and `tools/check_deps.py`
  verifies that the internal scanner modules (`crates/ayaml/src/scanner.rs` and
  `crates/ayaml/src/scanner/`) only import approved low-level modules and never pull in higher
  parser or Serde layers.
- **API and SemVer freeze:** Compile-time trait freeze suites
  ([`crates/ayaml/tests/api_freeze.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/tests/api_freeze.rs)
  and [`crates/ayaml-edit/tests/api_freeze.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml-edit/tests/api_freeze.rs))
  pin `Send`, `Sync`, `Unpin`, standard derives, and `const fn` constructors. Every PR runs
  both `cargo-semver-checks` and a rustdoc JSON `public-api` diff
  ([`tools/check_public_api.py`](https://github.com/asamarts/ayaml/blob/main/tools/check_public_api.py))
  against the last released tag (see [Stability and versioning](stability.md)).
- **Toolchain and OS matrix:** CI tests Rust 1.88 (the Minimum Supported Rust Version, MSRV),
  current stable, `-Z minimal-versions` (verifying declared minimum dependency versions
  actually compile and pass), Linux, macOS, Windows, and cross-target checks for Apple Silicon,
  x86_64 macOS, and Windows MSVC/GNU in
  [`.github/workflows/ci.yml`](https://github.com/asamarts/ayaml/blob/main/.github/workflows/ci.yml).

**Honest limits:** Static checks guarantee memory safety and API shape, not that parsed YAML
matches the specification. A parser can be 100% safe Rust and still misread an indented block
scalar.

---

## 2. Standards conformance and differential oracles

**What it is:** Checking `ayaml` against external ground truth rather than only against tests
written by the same author. A *differential oracle* feeds the exact same input to two
independent implementations (or compares against an independently derived reference output)
and flags any disagreement.

**What bugs it catches:** Subtle spec misinterpretations, regressions in rare YAML grammar
productions, drift between fast-path streaming code and general-purpose tree parsing, and
silent formatting corruption during source-preserving edits.

**How `ayaml` runs it:**

- **Pinned YAML 1.2.2 Test Suite (0 deviations):** The official
  [YAML Test Suite](https://github.com/yaml/yaml-test-suite) for the
  [YAML 1.2.2 specification](https://yaml.org/spec/1.2.2/) is vendored at a pinned commit
  under `tests/data/yaml-test-suite/src` and executed by `crates/ayaml-conformance` on every
  PR. In default YAML 1.2.2 mode, `ayaml` runs all **405** suite cases (94 expected-error
  cases and 311 valid documents) across seven check categories (up to seven checks per case):
  accept/reject classification (`405/405`), lazy event tree (`311/311`), eager event tree
  (`311/311`), canonical JSON equivalence (`282/282`), event re-emission (`311/311`), stream
  re-emission (`311/311`), and `Value` re-emission (`309/309`), totaling **2,240** default
  checks with **0 deviations**. The optional `libyaml-compat`
  [compatibility profile](compatibility.md) also runs the full suite with its 12 deliberate
  compatibility deviations explicitly pinned in
  [`tests/data/yaml-test-suite-deviations.txt`](https://github.com/asamarts/ayaml/blob/main/tests/data/yaml-test-suite-deviations.txt)
  (any unexpected pass or fail breaks CI).
- **RFC 8259 JSON differential oracle:** YAML 1.2.2 is defined as a strict superset of JSON
  ([RFC 8259](https://www.rfc-editor.org/rfc/rfc8259)).
  [`crates/ayaml/tests/json_conformance.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/tests/json_conformance.rs)
  compares `ayaml` directly against `serde_json` over a curated corpus, edge cases, and
  generated JSON payloads, verifying flow collections, Unicode surrogate-pair escapes,
  numbers, and quoting while pinning the narrow documented boundary cases between YAML 1.2.2
  and RFC 8259.
- **Real-world reduced oracles:**
  [`crates/ayaml/tests/real_world.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/tests/real_world.rs)
  checks minimal fixtures reduced from real-world YAML configs against independent
  hand-constructed expected documents and canonical event streams stored in sibling
  `.oracle.json` files so a parser bug cannot be accidentally blessed into a golden snapshot.
- **Shared position vectors:**
  [`crates/ayaml/tests/shared_vectors.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/tests/shared_vectors.rs)
  validates `LineIndex` UTF-8, Unicode scalar, and UTF-16 column conversions against shared
  format-contract test vectors under `tests/vectors/`.
- **Event-stream vs. tree oracle equivalence:** Single-document typed Serde decoding streams
  directly from parser events at every input size, while retaining the whole-document tree
  reader as an internal test oracle (`from_str_by_tree`). Differential tests and property tests
  verify both readers agree on every value and error, and every PR runs the full `ayaml` test
  suite under `RUSTFLAGS="--cfg ayaml_typed_from_events"` as well.
- **Runtime self-verification in `ayaml-edit`:** Before `ayaml-edit` returns
  `Ok(VerifiedEdit)`, it re-parses the spliced YAML with `ayaml` and verifies that both the
  resulting `Value` tree and the canonical event stream match the exact intended edit and
  nothing else (see [Source-preserving editing](editing.md)).

**Honest limits:** Conformance suites test known tricky cases, not arbitrary adversarial
combinations or every possible runtime I/O failure.

---

## 3. Unit, contract, fault-injection, and adversarial DoS tests

**What it is:** Targeted tests for individual functions, public API contracts, simulated I/O
faults, and hostile inputs engineered to trigger denial of service (exhausting CPU, stack,
or heap memory).

**What bugs it catches:** Regressions in specific APIs (`ErrorCode`, `MaskedError`,
`Spanned<T>`, `Frontmatter`, `DocumentDeserializer`, opt-in merge keys), broken behavior on
interrupted or short writes, and resource-limit bypasses such as exponential alias expansion
("billion laughs") or quadratic scanning.

**How `ayaml` runs it:**

- **891 workspace Rust tests + 61 doctests (952 total):** Every PR runs all library unit
  tests, 29 integration test suites in `crates/ayaml/tests/`, 3 suites in
  `crates/ayaml-edit/tests/`, and 61 doctests (16 in `crates/ayaml/src/` doc comments plus 45
  `book_doctests` compiled and executed straight out of `README.md`,
  `crates/ayaml-edit/README.md`, and `docs/**/*.md` via
  [`crates/ayaml-conformance/src/lib.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml-conformance/src/lib.rs)).
- **Bidirectional error-code and repository-policy contracts:**
  [`crates/ayaml/tests/error_codes.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/tests/error_codes.rs)
  verifies every `ErrorCode` variant is triggered by real input and documented in
  [Error codes](error-codes.md), and
  [`crates/ayaml/tests/repository_policy.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/tests/repository_policy.rs)
  asserts that critical CI jobs (allocations, event-streaming equivalence, public API, Miri,
  Kani, fuzzing, and docs) remain unconditionally wired.
- **Deterministic state-machine and I/O fault injection:**
  [`crates/ayaml/tests/state_machines.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/tests/state_machines.rs)
  drives emitters, `Deserializer` cursors, and streaming typed reads through randomized
  valid/invalid event sequences and simulated `io::Write` faults (`Interrupted`, `WriteZero`,
  short writes, and mid-stream errors) using a fixed-seed PRNG.
- **Process-isolated adversarial DoS suite:**
  [`crates/ayaml/tests/adversarial.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/tests/adversarial.rs)
  contains **37** active hostile-input tests (plus 5 `#[ignore]` child-process helper entry
  points spawned by parent tests, running in both debug and release profiles in CI), using
  **24** `in_child!` wrappers that run each attack in an isolated child process with a 1 GiB
  virtual-memory cap (`RLIMIT_AS` on Linux) and a hard wall-clock deadline. If an alias bomb,
  deep nesting attack, or quadratic probe ever leaks past `Limits`, the child process fails
  deterministically instead of hanging or exhausting host memory.

**Honest limits:** Hand-written test cases only exercise failure shapes engineers anticipated
in advance.

---

## 4. Property-based testing (`proptest`)

**What it is:** Instead of writing one example input and checking one expected output, you
write a *property* (an invariant that must hold for every valid input) and let a generator
create thousands of random inputs. When a property fails, the framework automatically
*shrinks* the failing input down to the smallest minimal counterexample that still reproduces
the bug. See the [Proptest book](https://proptest-rs.github.io/proptest/intro.html) for an
introduction.

**What bugs it catches:** Subtle interaction bugs between combinations of features (for
example, a quoted string containing control characters inside a flow mapping inside a
document with anchors) that humans rarely write by hand.

**How `ayaml` runs it:**

- **27 properties across 8 test modules** (including
  [`crates/ayaml/tests/properties.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/tests/properties.rs),
  [`crates/ayaml/tests/properties/invariants.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/tests/properties/invariants.rs),
  `scanner/tests.rs`, `frontmatter.rs`, `json_conformance.rs`, `merge_keys.rs`,
  `spanned_typed.rs`, and `crates/ayaml-edit/tests/edits.rs`), powered by a custom YAML 1.2.2
  grammar and value generator.
- Checked invariants include emit-then-parse round-trip preservation across block and flow
  styles, `SpannedTree` span bounds and slice integrity, `LineIndex` UTF-8/scalar/UTF-16
  coordinate round trips, `Tag` equality/ordering/hashing coherence, `Limits` monotonicity,
  tree-vs-event-stream typed decoding equivalence, `MaskedError` secret non-leakage,
  `ayaml-edit` splice equivalence and unrelated-value preservation, and JSON equivalence.
- Every PR runs the default case count; a daily scheduled workflow
  ([`.github/workflows/proptest.yml`](https://github.com/asamarts/ayaml/blob/main/.github/workflows/proptest.yml))
  scales up to **16,384** generated cases per property in `properties` (release mode, plus
  8,192 release cases with `RUSTFLAGS="--cfg ayaml_typed_from_events"` exercising event-stream
  typed decoding at every input size) and **2,048** cases per property across `frontmatter`,
  `spanned_typed`, `merge_keys`, and `ayaml-edit`.

**Honest limits:** Random generation explores the distribution defined by the generator; it
does not observe branch coverage inside the parser to steer toward deep or malformed code
paths.

---

## 5. Coverage-guided and structure-aware fuzzing (`libFuzzer` + ASan)

**What it is:** A *coverage-guided fuzzer* mutates inputs continuously while instrumenting
the compiled binary to observe which code branches each input hits. Whenever a mutation
reaches a new branch, the fuzzer keeps it in a corpus and mutates it further. Pairing fuzzing
with [AddressSanitizer (ASan)](https://clang.llvm.org/docs/AddressSanitizer.html) turns any
out-of-bounds read/write or memory error in any dependency into an immediate crash.
*Structure-aware* targets use the fuzzer's raw bytes to build structured inputs (such as
arbitrary `Value` trees, `EmitOptions`, or `EditOp` sequences) so the fuzzer reaches deep
logic instead of bouncing off early syntax checks. See
[LLVM libFuzzer](https://llvm.org/docs/LibFuzzer.html) and the
[Rust Fuzz Book](https://rust-fuzz.github.io/book/cargo-fuzz.html).

**What bugs it catches:** Panics, infinite loops, quadratic slowdowns, malformed-UTF-8 or
slicing edge cases, secret leakage in `Error::masked()`, and silent disagreements between
different APIs on bizarre inputs.

**How `ayaml` runs it:**

- **10 fuzz targets** under
  [`fuzz/fuzz_targets/`](https://github.com/asamarts/ayaml/tree/main/fuzz/fuzz_targets):
  `parse`, `emit`, `borrowed`, `structured`, `spanned`, `merge_keys`, `masked_errors`
  (embeds a recognizable fake secret string, called a "canary", and asserts `Error::masked()`
  never emits it), `event_emit`, `typed_equiv` (asserts tree-backed and event-streamed typed
  decoding agree), and `edit`.
- **Every PR:** The 7 structure-aware harnesses (`structured`, `spanned`, `merge_keys`,
  `masked_errors`, `event_emit`, `typed_equiv`, `edit`) run deterministic seed-corpus tests
  under stable Rust (`cargo test --manifest-path fuzz/Cargo.toml --lib --locked`), and
  `cargo check --bins` verifies all 10 fuzz targets (including the 3 raw-byte targets `parse`,
  `emit`, and `borrowed`) still compile.
- **Daily scheduled fuzzing:**
  [`.github/workflows/fuzz.yml`](https://github.com/asamarts/ayaml/blob/main/.github/workflows/fuzz.yml)
  runs all 10 targets under nightly `cargo-fuzz` + ASan with per-target time caps
  (`-max_total_time=60`) and libFuzzer's 2 GiB default RSS ceiling. Any discovered crash input
  is encrypted with the `age` encryption tool before upload so an unfixed DoS or correctness
  trigger is never published to public CI logs before a fix ships.

**Honest limits:** A bounded fuzz run is strong empirical evidence, never a proof of absence
of bugs. A rare condition guarded by a narrow magic sequence can still evade random mutation.

---

## 6. Undefined behavior detection with Miri

**What it is:** In Rust, *undefined behavior (UB)* means violating the language's memory or
aliasing rules (for example, dereferencing a dangling pointer, reading uninitialized memory,
or using a pointer after another mutable reference invalidated its borrowing permissions under
Rust's Stacked Borrows model). Once UB occurs, the optimizer may miscompile the program in
unpredictable ways. [Miri](https://github.com/rust-lang/miri) is an interpreter for Rust's
Mid-level Intermediate Representation (MIR, the compiler's simplified form of Rust code before
optimization) that executes tests while tracking pointer origin and permissions (pointer
provenance), alignment, initialization, and aliasing dynamically. See the
[Rust Reference on Undefined Behavior](https://doc.rust-lang.org/reference/behavior-considered-undefined.html).

**Why run Miri when `ayaml` forbids `unsafe`?** Even when a crate has zero `unsafe` of its
own, it runs on top of the Rust standard library and its two direct runtime dependencies
(`serde` and `indexmap`), and the workspace includes an unpublished global counting allocator
(`fuzz/src/bin/memcount.rs`) used by memory benchmarks. Running Miri checks the combined
execution of `ayaml`, its dependencies, and the counting allocator for subtle UB or memory
leaks.

**How `ayaml` runs it:**

- Daily bounded runs in
  [`.github/workflows/miri.yml`](https://github.com/asamarts/ayaml/blob/main/.github/workflows/miri.yml)
  execute `cargo +nightly miri test` (under Miri's default Stacked Borrows model) with an
  8 GiB virtual-memory cap (`ulimit -v 8388608`) and strict timeouts across four suites:
  1. `ayaml` library unit tests (`--all-features`, skipping 21 multi-megabyte and 20k-item
     stress tests so the ~235 core library unit tests complete within timeout),
  2. post-0.2.3 integration suites (`frontmatter`, `merge_keys`, `document_deserializer`,
     `spanned_typed`, `options`),
  3. `ayaml-edit` unit and integration tests, and
  4. the unpublished counting allocator in `fuzz/src/bin/memcount.rs` under Miri's default
     isolation (blocking OS and filesystem calls). Suites 1 through 3 pass
     `-Zmiri-disable-isolation` (with `PROPTEST_CASES=2` on suites 2 and 3) so file-backed
     fixtures and `proptest` work under the interpreter.

**Honest limits:** Miri only checks the specific execution paths taken by the tests it runs,
and runs much slower than native execution, so it complements rather than replaces fuzzing
and formal proofs.

---

## 7. Formal verification with Kani bounded model checking

**What it is:** Testing and fuzzing check individual inputs one at a time. *Bounded model
checking* translates Rust code into a mathematical formula and hands it to a mathematical
logic solver (SAT/SMT solver, via [Kani](https://model-checking.github.io/kani/) and the
[CBMC](https://www.cprover.org/cbmc/) verification engine) that searches exhaustively over
**all possible values** of symbolic inputs (every `u64`, `usize`, `f64`, or bounded byte
sequence) up to a stated bound. If any input can trigger an arithmetic overflow, out-of-bounds
index, assertion failure, or invariant break, the solver returns a concrete counterexample; if
the solver proves no counterexample exists, the property holds for every input in that domain.

**What bugs it catches:** Subtle integer overflows at `usize::MAX` / `u64::MAX`,
off-by-one UTF-8 char-boundary slicing panics, broken `Eq`/`Ord`/`Hash` coherence across edge
values like `-0.0` or `i128` boundaries, and non-monotonic capacity or budget calculations
that random inputs almost never hit.

**How `ayaml` runs it:**

`ayaml` maintains **26** `#[kani::proof]` harnesses across **8** source files, executed daily
in [`.github/workflows/kani.yml`](https://github.com/asamarts/ayaml/blob/main/.github/workflows/kani.yml)
via [`tools/run_kani.py`](https://github.com/asamarts/ayaml/blob/main/tools/run_kani.py)
(which runs harnesses sequentially under a 3 GiB per-harness `RLIMIT_AS` ceiling, a 2 GiB
physical-memory budget measured as peak RSS, and per-harness timeouts):

| Module | Harnesses | Invariants proved symbolically |
| --- | --- | --- |
| [`crates/ayaml/src/error.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/src/error.rs) | 6 | Path byte accounting saturates without overflow; quoted-key escaping produces valid UTF-8 and escapes control chars; path truncation preserves UTF-8 boundaries and is idempotent |
| [`crates/ayaml/src/position.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/src/position.rs) | 5 | `LineIndex` line counts, UTF-8/scalar/UTF-16 block counters, exact coordinate checks for `to_position` and `to_offset`, surrogate bounds, and cursor cache equivalence to binary search |
| [`crates/ayaml/src/limits.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/src/limits.rs) | 3 | Alias-expansion budget formula never overflows and stays capped; `Limits::is_valid` and `Limits::check` boundaries; budget charging is saturating and monotonic |
| [`crates/ayaml/src/number.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/src/number.rs) | 3 | Exact integer conversions are canonical; `Eq`/`Ord`/`Hash` coherence holds across signed/unsigned integer types and IEEE-754 floats (including `-0.0` and `NaN` handling) |
| [`crates/ayaml/src/parser/kani_proofs.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/src/parser/kani_proofs.rs) | 3 | Prefix token capacity estimation stays bounded and overflow-free across bounded input regimes and saturation boundaries up to `usize::MAX`; token arena growth helpers never overflow across arbitrary `usize` inputs |
| [`crates/ayaml/src/frontmatter.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/src/frontmatter.rs) | 2 | Frontmatter line-length and delimiter-matching helpers stay strictly within input byte bounds across multi-byte UTF-8 inputs |
| [`crates/ayaml/src/span.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/src/span.rs) | 2 | `Span` start/end invariants guarantee safe `&str` slicing; `Location::from(Position)` saturates cleanly at `usize::MAX` |
| [`crates/ayaml-edit/src/lib.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml-edit/src/lib.rs) | 2 | Edit span ordering is a consistent, cycle-free sorting order (strict total order); `apply_edits` span validation guarantees every splice stays in bounds on valid UTF-8 boundaries and never overlaps |

**Honest limits:** Kani is *bounded* model checking, not a whole-program proof of the entire
YAML grammar. Proofs over loops or strings hold up to the configured loop iteration limit
(unwind bound) or concrete buffer length, so Kani targets the arithmetic, indexing, ordering,
and boundary helpers where exhaustive mathematical verification is strongest.

---

## 8. Mutation testing, performance gates, watchdog, and release integrity

Assuring correctness also means checking that the tests themselves are strong, that
performance and memory bounds do not silently regress, that scheduled CI actually keeps
running, and that published crates match the reviewed Git commit.

- **Testing the tests (`cargo-mutants`):** *Mutation testing* deliberately injects small bugs
  ("mutants", such as flipping `<=` to `<`, deleting a boundary check, or returning a default
  value) into library code and runs the test suite. If the tests still pass, the mutant
  "survived", revealing a gap in test coverage. See [cargo-mutants](https://mutants.rs/).
  `ayaml` runs weekly bounded mutation testing across **16** shards in
  [`.github/workflows/mutants.yml`](https://github.com/asamarts/ayaml/blob/main/.github/workflows/mutants.yml)
  using [`.cargo/mutants.toml`](https://github.com/asamarts/ayaml/blob/main/.cargo/mutants.toml)
  over five core `ayaml` modules (`span.rs`, `position.rs`, `options.rs`, `spanned.rs`, and
  `emitter.rs`, excluding DoS-limit guards). Survivors and timeouts are published as workflow
  warnings and trend artifacts (`summary.json`) rather than failing PR CI, and regression tests
  for previously caught mutants are tracked in
  [`crates/ayaml/tests/mutation_targets.rs`](https://github.com/asamarts/ayaml/blob/main/crates/ayaml/tests/mutation_targets.rs).
- **Deterministic allocation and instruction gates:** Wall-clock timings fluctuate on shared
  CI machines, so every PR runs two deterministic checks instead:
  - Counting-allocator peak-heap, live-heap, total-byte, and allocation-count gates
    (`tools/check_alloc_baseline.py` and `tools/check_alloc_corpus.sh`).
  - Steady-state [Valgrind Callgrind](https://valgrind.org/docs/manual/cl-manual.html) diffs
    against the base commit, failing above `+2.0%` executed CPU instructions (`Ir`) or `+5.0%`
    simulated cycles (`EstimatedCycles`, which adds simulated cache and branch misses) via
    `tools/check_instructions.py` (see [Benchmark methodology](benchmarks.md)).
- **Scheduled-workflow watchdog:** GitHub disables scheduled workflows after 60 days of repo
  inactivity, and a broken nightly workflow is easy to overlook. Every day,
  [`.github/workflows/scheduled-watch.yml`](https://github.com/asamarts/ayaml/blob/main/.github/workflows/scheduled-watch.yml)
  runs [`tools/check_scheduled_runs.py`](https://github.com/asamarts/ayaml/blob/main/tools/check_scheduled_runs.py)
  to confirm that fuzzing, Miri, Kani, nightly proptest, mutation testing, and security
  advisory scans (`advisories.yml`) are still enabled and healthy.
- **Release supply-chain integrity:** Releases are never published from a developer laptop.
  [`.github/workflows/release.yml`](https://github.com/asamarts/ayaml/blob/main/.github/workflows/release.yml)
  re-runs the full CI gate on the annotated release tag, verifies packaged `.crate` source
  files against the Git commit (`tools/check_release.py package`), publishes via
  [crates.io Trusted Publishing](https://crates.io/docs/trusted-publishing) (short-lived OpenID
  Connect identity tokens only; legacy API tokens are disabled on `ayaml`, and `ayaml-edit`
  adopts the same setting after its initial publish), verifies the `.crate` served by crates.io
  against both the packaged archive and crates.io's HTTP package index (sparse index) checksum
  (`tools/check_release.py registry`), and signs a
  [SLSA Build Provenance v1](https://slsa.dev/spec/v1.0/provenance) build receipt recorded in
  Sigstore's public transparency log for every release archive.

---

## Where to go next

- Run any of these checks locally: [Developer tooling and verification](tools.md)
- Read the threat model and masked-error guarantees: [Security and untrusted input](security.md)
- Review exact resource ceilings and memory formulas: [Resource limits](limits.md)
- Inspect pinned CPU and memory results: [Benchmarks](https://asamarts.github.io/ayaml/benchmarks/)
