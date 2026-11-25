# Benchmark methodology

ayaml includes deterministic fixtures, timing benchmarks, counting-allocator
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
  4 KiB threshold (`boundary-3kb`, `boundary-4kb-below`, `boundary-4kb-above`,
  and `boundary-8kb`) where typed decoding switches from building an
  intermediate tree to streaming directly from events.
- **Scaling ladders (9 fixtures):** 10 KiB, 100 KiB, and 1 MiB ladders across
  block mappings, block sequences, and nested OpenAPI documents for automated
  10x linear complexity verification.
- **Structural stress shapes (11 fixtures):** deep flow nesting (`depth-64`,
  `depth-128`), repeated aliases (`aliases-1000`), a 1 MiB scalar, 256 KiB flow
  collections, dense short block entries (`short-seq-20000`, `short-map-20000`,
  `short-explicit-20000`), nested complex mapping keys (`nested-keys`), and
  late typed-decode mismatches (`typed-mismatch-late`).
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
cargo xtask memory --suite baseline               # counting-allocator JSONL probe
cargo xtask det --base-rev HEAD~1                 # steady-state Valgrind Callgrind check
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

`tools/check_instructions.py` (invoked directly or via `cargo xtask det`) runs
`tools/instruction-probe.rs` under Valgrind Callgrind with
`--cache-sim=yes --branch-sim=yes`. Each `(operation, workload)` cell executes
a warmup iteration (`K1 = 1`) followed by `N` steady-state iterations (`4` for
parse operations, `12` for emit, and `64` for `walk` to amortize setup
variance), and computes per-iteration counters as
`(counts(K1 + N) - counts(K1)) / N`.

The gate checks both:

- instruction count (`Ir`): fails above `+2.0%`; and
- `EstimatedCycles = Ir + 10 * L1_misses + 100 * LL_misses + 20 * branch_mispredicts`:
  fails above `+5.0%`.

Level-1 and last-level cache misses and branch mispredictions are also recorded in
`callgrind.jsonl` for diagnostics. If a rise is deliberate, add a tab-separated
entry to `tools/instruction-rises.tsv` in the same pull request explaining why.
CI runs the allocation budgets and steady-state Callgrind comparison on every
pull request; it does not gate wall-clock Criterion timings because shared
hosted runners exhibit run-to-run timing noise.

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
   4 KiB boundary, scaling points, and stress shapes.
2. **Legacy corpus budgets (`tools/check_alloc_corpus.sh`):**
   - `tools/alloc-corpus.tsv` budgets parsing, emission, and serialization of
     the `Value` path (about 1.25x measured values; short-node workloads stay
     at most 130x input bytes in total).
   - `tools/typed-alloc-corpus.tsv` budgets typed decoding of a 20 KB
     configuration fixture (peaking around 175 KB and retaining around 98 KB).
   - `tools/json-alloc-corpus.tsv` budgets an untyped decode into a JSON tree
     through the compatibility reader on the document shapes of issue #178
     (counting reallocation growth by delta bytes, budgeted at about 1.10x).

What holds those peaks down: the token arena is sized from the density of the
input scanned so far, within the token limit (`docs/limits.md` gives its
bounds). Event vectors reserve four fifths of the token count plus headroom. A
typed read of more than 4 KiB builds its value from the parser's events as it
takes them, so neither a vector of the document's events nor a tree of its
nodes stands between the tokens and the value: a scalar is one node that the
target reads and drops, and a collection is read entry by entry. Only what is
read whole or again is built as a tree: a node with an anchor, a collection
that keeps its tag, a mapping key, and what an enum reads. A smaller read
builds the tree, which costs a document of a few bytes about 1,300 fewer
instructions, 7,000 against 8,300 for `a`. Scalar tokens keep their raw text as
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

## Interpreting results

- Compare only runs from the same hardware, OS, CPU affinity, compiler, build
  mode, and matching `fixture_sha256` digests.
- Treat noisy-host runs as observations, not release evidence.
- Keep correctness and conformance checks independent of timing results.
- Record any deliberate performance regression and its cause in
  `tools/instruction-rises.tsv` before release.
