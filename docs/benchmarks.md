# Benchmark methodology

> **Looking for charts and results?** See the
> [Benchmarks & Performance](https://asamarts.github.io/ayaml/benchmarks/) page
> for CPU, peak-heap, allocation, emission, v0.2.3 speedup, and 10x scaling
> charts generated directly from pinned JSONL artifacts.

`ayaml` includes deterministic fixtures, timing benchmarks, counting-allocator
probes, steady-state Callgrind checks, and a `cargo xtask` CLI so that
performance and memory behavior can be verified separately from correctness.
Benchmark results are environment-specific and are not a compatibility
guarantee.

## Deterministic fixture catalog

All timing benchmarks, allocation baselines, and instruction/cycle checks draw
from one shared crate, `crates/ayaml-fixtures`. Every fixture is generated from
fixed seeds and verified against a pinned FIPS 180-4 SHA-256 digest before
measurements start, preventing fixture drift from invalidating comparisons.

The catalog spans five categories:

- **Realistic domain workloads (11 families):** multi-document cluster
  manifests, OpenAPI specifications, CI workflows with block scripts, container
  orchestration specs with anchors/aliases/merge keys, bulk small automation
  playbooks, wide lockfiles, custom-tagged templates, escape-heavy route
  configs, tiny frontmatter headers, strict JSON-in-YAML, and multi-byte
  Unicode documents.
- **Strategy boundary fixtures (4 sizes):** structured mappings straddling the
  historical 4 KiB threshold (`boundary-3kb`, `boundary-4kb-below`,
  `boundary-4kb-above`, and `boundary-8kb`) verifying that typed decoding
  streams directly from events across every input size with no heap cliff
  (see D107 in [decisions.md](decisions.md)).
- **Scaling ladders (9 fixtures):** 10 KiB, 100 KiB, and 1 MiB ladders across
  block mappings, block sequences, and nested OpenAPI documents for automated
  10x linear complexity verification.
- **Structural stress shapes (11 fixtures):** deep flow nesting (`depth-64`,
  `depth-128`), repeated aliases (`aliases-1000`), a 1 MiB single-line plain
  scalar (`large-scalar-1mb`), 256 KiB flow collections, dense short block
  entries (`short-seq-20000`, `short-map-20000`, `short-explicit-20000`),
  nested complex mapping keys (`nested-keys`), and late typed-decode mismatches
  (`typed-mismatch-late`).
- **Error paths (5 fixtures):** early and late syntax errors, late duplicate
  keys, depth-limit rejection, and late invalid escapes.

## Layers measured

Reporting a single end-to-end number can hide a slow layer, so the harness
measures each layer independently:

- scanner/tokenization;
- parsing into events;
- generic `Value` composition and multi-document parsing;
- typed Serde decoding (owned and zero-copy borrowed);
- span-preserving `SpannedTree` parsing and `LineIndex` coordinate conversion;
- `ayaml::compat` decoding;
- event emission, `Value` emission (block and flow), and Serde serialization;
  and
- error/rejection paths.

Every Criterion benchmark group validates semantic correctness across all
scenarios before timing begins so broken parses or empty outputs cannot
produce misleading timings.

## Running measurements locally with `cargo xtask`

The workspace provides a `cargo xtask` CLI (`.cargo/config.toml` alias for
`crates/xtask`) that runs every measurement and writes schema-versioned
JSONL records alongside a sibling hardware/toolchain fingerprint JSON file
(`*.fingerprint.json`):

```sh
cargo xtask fixtures --verify                     # verify SHA-256 catalog digests
cargo xtask fixtures --json                       # print deterministic JSON manifest
cargo xtask speed --out-dir target/xtask          # run Criterion and export speed.jsonl
cargo xtask speed --quick --filter frontmatter    # fast filtered smoke run
cargo xtask memory --suite baseline               # counting-allocator JSONL probe (baseline matrix)
cargo xtask memory --suite full --out-dir target/xtask # full 40-fixture counting-allocator probe
cargo xtask det --suite pr --base HEAD~1          # steady-state Callgrind over ayaml-bench
cargo xtask det --suite pr --anchor main          # check cumulative drift against an anchor ref
cargo xtask profile --tool callgrind --bench structural/parse_value/depth-64
cargo xtask profile --tool dhat --fixture openapi-spec --mode parse_value
cargo xtask scale --out-dir target/xtask          # 10x linear time & memory verification
cargo xtask compare --baseline DIR_A --current DIR_B
cargo xtask gate                                  # run allocation & baseline gates
cargo xtask gate --baseline DIR_A --current DIR_B # gate two recorded JSONL directories
cargo xtask report --dir target/xtask             # render Markdown summary report
```

You can also invoke the underlying harnesses directly:

```sh
cargo bench --locked -p ayaml-bench --bench throughput
tools/check_alloc_corpus.sh
python3 tools/check_alloc_baseline.py
python3 tools/check_instructions.py HEAD~1
```

## Artifact schemas and fingerprints

Every JSONL artifact written by `cargo xtask` uses `schema_version: 1` and
embeds each fixture's byte length and pinned `fixture_sha256`. Comparison and
gating commands refuse to compare rows whose `fixture_sha256` differs.

Each JSONL output (`speed.jsonl`, `memory.jsonl`, `callgrind.jsonl`,
`scale.jsonl`) is paired with a sibling `<kind>.fingerprint.json` recording OS,
architecture, kernel release, CPU model, logical CPU count, `rustc --version`,
workspace git SHA, combined fixture manifest SHA-256, and capture timestamp.
Machine hostnames and local filesystem paths are omitted.

## Steady-state Callgrind instruction and cycle gating

`tools/check_instructions.py` (invoked directly or via `cargo xtask det --legacy-probe`) runs
`tools/instruction-probe.rs` under Valgrind Callgrind with
`--cache-sim=yes --branch-sim=yes` and pinned simulated cache geometry
(`--I1=32768,8,64 --LL=8388608,16,64`). Each `(operation, workload)` cell executes
a warmup iteration (`K1 = 1`) followed by `N` steady-state iterations (`4` for
parse operations, `12` for emit, and `64` for `walk` to amortize setup
variance), and computes per-iteration counters from `(counts(K1 + N) - counts(K1))`
divided by `N` with half-up rounding.

The gate checks both:

- instruction count (`Ir`): fails above `+2.0%`; and
- `EstimatedCycles = Ir + 10 * L1_misses + 100 * LL_misses + 20 * branch_mispredicts`:
  fails above `+5.0%`.

Level-1 and last-level cache misses and branch mispredictions are also recorded in
`callgrind.jsonl` for diagnostics. If a rise is deliberate, add a 5-column tab-separated
entry (`OPERATION`, `WORKLOAD`, `IR_PERCENT`, `CYCLES_PERCENT`, `REASON`) to
`tools/instruction-rises.tsv` in the same pull request explaining why, using `-` for any
metric that should remain at its default limit so an `EstimatedCycles` exception never
relaxes `Ir`. When `tools/instruction-baseline.jsonl` is present, `tools/check_instructions.py`
also checks every matching cell against the pinned release baseline (`+10.0%` `Ir` and `+15.0%`
`EstimatedCycles`) to catch multi-PR cumulative creep (refreshable via `--record-baseline`).
CI runs the allocation budgets and steady-state Callgrind comparison on every
pull request; it does not gate wall-clock Criterion timings because shared
hosted runners exhibit run-to-run timing noise.

For local bottleneck investigation and long-horizon regression checks directly
against `crates/ayaml-bench`, `cargo xtask` also provides two Valgrind-backed
commands:

- **`cargo xtask det --suite pr|full [--base REV] [--anchor REV]`:** compiles
  the standard Criterion `ayaml-bench` benchmarks against a lightweight counting
  Criterion shim so Valgrind Callgrind collects only inside steady-state
  iterations (`--collect-atstart=no --toggle-collect=*__ayaml_det_measure*`),
  excluding fixture generation and warmup. Passing `--anchor REV` checks
  cumulative drift against a pinned baseline ref that shares benchmark IDs with
  the suite, in addition to `--base` (`callgrind.jsonl` records schema-v1 base
  deltas while stdout/stderr report both base and anchor gate results). Use
  `--legacy-probe` to invoke `tools/check_instructions.py` directly.
- **`cargo xtask profile --tool callgrind|dhat`:** attributes CPU instructions,
  simulated cache misses, and branch mispredictions per function via
  `callgrind_annotate` (with optional `--inclusive` and `--base REV` diffs) or
  attributes peak heap (`at-t-gmax`), allocation counts, and total allocated
  bytes per call site via Valgrind DHAT (`--fixture ID --mode MODE` or
  `--bench ID`), excluding fixture generation and setup parses.

## Time, memory, and allocation architecture

Timing uses repeated samples and reports medians rather than a single run.
Allocation probes report peak live bytes, retained live bytes, allocation
count, cumulative allocated bytes, and peak-to-input byte ratio.

CI enforces two allocation gates, both measured by the same
counting allocator in `fuzz/src/bin/memcount.rs`:

1. **Structured JSONL baseline (`tools/check_alloc_baseline.py` against
   `tools/alloc-baseline.jsonl`):** gates peak bytes, live bytes, total
   allocated bytes (up to `+10%` or `512 B` floor), and allocation counts (up
   to `+5%` or `4` allocs floor) across the shared `ayaml-fixtures` domains,
   4 KiB boundary, scaling points, stress shapes (`depth-64`, `aliases-1000`),
   and post-0.2.3 APIs (`ayaml-edit`, `frontmatter`, `merge_keys`, `Spanned<T>`,
   `DocumentDeserializer`, `from_reader`, `Tape`/`ParserBuf`, and
   `libyaml-compat`). When actual peak bytes or allocations drop more than `15%`
   below the committed baseline, `check_alloc_baseline.py` prints a ratchet
   warning suggesting `--record`.
2. **Legacy corpus budgets (`tools/check_alloc_corpus.sh`):**
   - `tools/alloc-corpus.tsv` budgets parsing, emission, and serialization of
     the `Value` path (about 1.15x measured parse peak and 1.20x other metrics).
   - `tools/typed-alloc-corpus.tsv` budgets typed decoding across real-world
     configs and stress shapes (about 1.10x measured parse peak and 1.20x other
     metrics).
   - `tools/json-alloc-corpus.tsv` budgets an untyped decode into a JSON tree
     through the compatibility reader on the document shapes of issue #178
     (counting reallocation growth by delta bytes, budgeted at about 1.10x).

What holds those peaks down: the token arena is sized from the density of the
input scanned so far, within the token limit (`docs/limits.md` gives its
bounds). Event vectors reserve four fifths of the token count plus headroom. A
typed read builds its value directly from the parser's events at every input
size (D107), so neither a vector of the document's events nor a tree of its
nodes stands between the tokens and the value: a scalar is read directly from
the event and a collection is read entry by entry. Only what is read whole or
again is built as a tree: a node with an anchor, a collection that keeps its
tag, a complex mapping key, and what an enum reads. Scalar tokens keep their raw text as
a span and box only values that need decoding; typed nodes box tags, keep
scalar text as a source slice, and cache hashes in one atomic word. The parser
counts a root flow collection's entries before it reads them: a root that is
built as a tree reserves that room, and one that is read entry by entry tells
its target the count.

What the events cost: a root flow collection (counted by the parser) and any
collection whose end is already among the events waiting in the lookahead queue
tell their target their exact size; larger collections whose end lies past the
lookahead queue tell no size, so a vector or a map grows by doubling and can
keep up to twice the room its entries need. `Value` and `Mapping` skip
`shrink_to_fit` when told a size within their initial pre-allocation cap
(`1..=1024` for sequences, `<= 4096` for mappings) and give spare room back at
each collection's end otherwise (AY-118). A `Value` of the 4.4 MB service description then
keeps 45 MB and its read peaks at 80 MB, where a read that built the tree
first, and told each collection its size, kept 48 MB and peaked at 120 MB. A
mapping of 1,000 to 4,096 entries on its own still peaks higher while it grows,
counting both blocks of each reallocation: by about 14% in default mode and by
about half in compatibility mode, where no set of keys stood beside the tree
before. A node that is built whole inside the document, because it has an
anchor or a tag that it keeps, an enum reads it, or it is a key that is a
collection, stands as a tree beside the tokens and the value. A megabyte flow
sequence that is a mapping's value peaks at 131 MB with an anchor or a tag, and
at 106 MB with neither, where a read that built the whole tree first peaked at
99 MB; one that is a key peaks at 152 MB against 114 MB. A document whose nodes
mostly have anchors peaks about a quarter above such a read as a JSON tree,
95 MB against 76 MB for 50,000 anchored flow mappings of two entries by
issue #178's measure, and about a third above it as a `Value` (AY-119). A root with
an anchor peaks as it did.

Issue #178 gives a reference for each of its nine shapes, the peak of a
libyaml-based reader: 1.40, 41.5, and 165.9 MB for the three service
descriptions, and 36.6, 35.7, 34.3, 67.8, 18.4, and 18.1 MB for the six
megabyte documents, in the order of the budget file. Against those, the service
descriptions peak at about 22 bytes for each input byte, 0.58 times the
reference, and the six megabyte documents at 13 to 49 bytes, 0.52 to 0.96
times: all within the 1.2 times that the issue asks for. The megabyte block
sequence of short words is the one at 0.96 times. It peaks at 17.4 MB: the
token arena (8.4 MB), the line records (3.1 MB), the line index (1.0 MB), and
the JSON tree (4.8 MB). While a tree of the document was built first, its node
vector (10.5 MB) stood there in place of the JSON tree, and the shape peaked at
23.1 MB, 1.28 times the reference (AY-104).

## Comparative benchmarks and peer methodology

`cargo xtask` inside `asamarts/ayaml` reproduces all pure-Rust `ayaml` timing,
memory, scaling, and Callgrind checks against `crates/ayaml-fixtures` without
introducing C/C++ build dependencies into the core workspace.

Full 11-parser Rust, C, and C++ comparative benchmarks run from the standalone
public harness repository
[`asamarts/ayaml-benchmarks`](https://github.com/asamarts/ayaml-benchmarks)
(pinned by commit SHA in `site/src/data/benchmarks/fingerprint.json`, currently
`95759c29c15596412c31b6c9c850f8602674d6ef`). Before timing or memory probes
start, the harness verifies semantic equivalence against `ayaml` over all 26
shared `ayaml-fixtures` scenarios (including the 10 KiB, 100 KiB, and 1 MiB
scaling ladders) and verifies identical typed Rust struct decodes.

### Pinned peer versions and build flags

- **Rust crates:** `ayaml` (measured commit `0318345798d8` vs `v0.2.3` engine
  baseline `79e1368e7850`, the first commit carrying `ayaml-fixtures` with a
  byte-identical parser and emitter engine to tag `v0.2.3`), `yaml-rust2 0.13.0`,
  `saphyr 0.3.3`, `noyalib 0.0.63`, `fast-yaml-core 0.7.0`, `yaml_serde 0.10.7`,
  `serde-saphyr 2.2.0`, `serde 1.0.229`, `serde_json 1.0.154`, and
  `criterion 0.7.0`.
- **Vendored C/C++ libraries:** `rapidyaml v0.16.0` (`-O3 -std=c++11`),
  `fkYAML v0.5.0` (`-O3 -std=c++17`), `libyaml 0.2.6` (`-O3`), and
  `libfyaml v1.0.0-rc2` (`-O3 -std=gnu11`, built with `HAVE_PORTABLE_TARGET=1`
  and no host-specific SIMD flags, matching default Rust `target-cpu`).
- **Rust build flags:** standard `release` / `bench` profile (`opt-level = 3`,
  `lto = false`, `codegen-units = 16`, default `target-cpu`), compiled with
  `rustc 1.97.0` and `GCC 16.1.1`.
- **Criterion & multi-round aggregation:** `warm_up_time = 500 ms`,
  `measurement_time = 1200 ms`, `sample_size = 30`, executed for `rounds = 3`
  independent rounds pinned to one CPU core with the `performance` governor on
  dedicated `x86_64` Intel Skylake-class Linux hardware. Each published row
  reports the median of round medians alongside cross-round coefficient of
  variation (`round_cv_pct`, summarized in `fingerprint.json`).

### Disclosed caveats and semantic differences

- **Pull events vs. tokenizer isolation:** `ayaml events` benchmarks the public
  pull-based `ayaml::parse_events(input)` iterator and discards each event on
  pull (`live_bytes = 0`), matching the C `libyaml` and raw non-resolving
  `libfyaml` (`FYPCF_QUIET`) event loops. `ayaml scan` uses the `bench-internals`
  feature solely to isolate scanner tokenization.
- **`serde-saphyr` target type:** `serde-saphyr` provides a Serde deserializer
  without a YAML-native `Value` DOM, so Tier 2 deserializes into
  `serde_json::Value` (`BTreeMap`-backed string-only keys, no YAML tags).
- **Per-peer scenario exclusions:** Single-document APIs (`ayaml serde-value`,
  `ayaml compat-serde`, `fkYAML`, `yaml-serde`) exclude multi-document
  `cluster-manifests`; `noyalib` excludes `aliases-1000` and
  `container-compose`; `serde-saphyr` excludes `aliases-1000`;
  `libfyaml events` covers all 26 scenarios using raw non-resolving
  `FYPCF_QUIET` events (no alias expansion); and `libfyaml doc` deterministically
  excludes `cluster-manifests`, `depth-64` (upstream depth limit 64),
  `short-map-20000`, `short-explicit-20000`, `scale-map-100kb`,
  `scale-map-1mb`, and `scale-openapi-1mb` (quadratic flat-map construction)
  across both timing and memory.
- **Duplicate-key and map-ordering differences:** `ayaml` rejects duplicate
  mapping keys and preserves insertion order via `indexmap`. `rapidyaml` and
  `libyaml` allow duplicate keys without hashing; `fkYAML` stores mappings in
  sorted `std::map`; `serde_json::Value` uses `BTreeMap`; `yaml-rust2` and
  `saphyr` use `LinkedHashMap` / `Hash` with differing duplicate policies.
- **Plain-scalar vs. structured workloads:** `large-scalar-1mb` is a single-line
  1,000,000-byte plain scalar (`"x".repeat(1_000_000) + "\n"`), not a literal or
  folded block scalar. Published summaries disclose `libfyaml events` on
  `large-scalar-1mb` alongside `ayaml` and report `libyaml events` geometric
  means both with and without `large-scalar-1mb`.
- **Synthetic fixture caveat:** `ayaml-fixtures` uses deterministic seeded
  generators rather than raw third-party repositories. Re-run measurements on
  your own documents before making production capacity decisions.

## Interpreting results

- Compare only runs from the same hardware, OS, CPU affinity, compiler, build
  mode, and matching `fixture_sha256` digests.
- Treat noisy-host runs as observations, not release evidence.
- Keep correctness and conformance checks independent of timing results.
- Record any deliberate performance regression and its cause in
  `tools/instruction-rises.tsv` before release.
