# Benchmark methodology

ayaml includes benchmarks and allocation probes so that performance and memory
behavior can be reviewed separately from correctness. Benchmark results are
environment-specific and are not a compatibility guarantee.

## Layers

The harness measures these layers separately:

- scanner/tokenization;
- parsing into events;
- event emission;
- generic `Value` composition;
- typed Serde decoding; and
- value emission.

Reporting a single end-to-end number can hide a slow layer, so layer results
are reported independently.

## Workloads

The benchmark suite covers:

- a representative compatibility document with mixed block and flow
  collections, tags, anchors, aliases, and merge keys;
- depth-bound nested collections;
- mapping-heavy documents;
- sequence-heavy documents;
- small documents and empty input;
- large scalars; and
- alias-heavy documents.

All workloads use fixed input bytes and the same build mode when comparing two
revisions.

## Running them

The timing harness is `crates/ayaml-bench/benches/throughput.rs`. Allocation budgets
come from `tools/check_alloc_corpus.sh` and its counting allocator,
`fuzz/src/bin/memcount.rs`; instruction counts use `tools/check_instructions.py`
with `tools/instruction-probe.rs`. Each allocation row budgets peak bytes, allocation count,
and total bytes for parsing, emission, and serialization.

```sh
cargo bench -p ayaml-bench                     # timing, with criterion
tools/check_alloc_corpus.sh                    # allocation budgets
python3 tools/check_instructions.py BASE_REV   # instruction counts, needs valgrind
```

CI checks the allocation budgets, compares instruction counts with the pull request's
base, and checks that the benchmarks compile. It doesn't check timing, because a shared
runner's timings aren't comparable from run to run.

The instruction check fails when any operation on any input rises more than 5%. Counts
are repeatable to within about 1% from run to run. They catch a slower algorithm or an
added pass, but not cache or branch effects, so maintainers also compare timing on a
quiet machine before a release. If a rise is deliberate, add a line to
`tools/instruction-rises.tsv` in the same change, saying why.

## Time and memory

Timing uses repeated samples and reports medians rather than a single run.
Allocation probes report allocated bytes, allocation count, and peak allocated
bytes. Peak memory is measured for parsing, emission, and serialization separately.

The token arena doubles after its first chunk, and inputs dense in short lines start with a
wider chunk. The generated short-node budgets currently allow at most 250 times the input bytes
in total; their measured values are in `tools/alloc-corpus.tsv`. Large-scalar parsing allocates
about 53% fewer bytes in total, the 20,000-entry short map about 38% fewer, and the short
explicit-pair workload about 31% fewer than before this change. Peak heap falls about 13% on
short sequences and 19% on short explicit pairs, and the affected budgets are tightened to the
new measurements. Event vectors reserve four fifths of the token count plus headroom; on the
1 MB nested test document this lowers the typed parse peak from about 64.8 MB to 49.4 MB.

The release process keeps comparison results private. This document describes
the methodology so that absolute or relative measurements can be reviewed on
their merits when they are published.

## Interpreting results

- Compare only runs from the same host, CPU affinity, compiler, build mode, and
  input bytes.
- Treat noisy-host runs as observations, not release evidence.
- Keep correctness and conformance checks independent of timing results.
- Record any deliberate performance regression and its cause before release.
