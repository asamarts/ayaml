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
and total bytes, for parsing, emission, and serialization or for one decode.

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

The allocation gate has three sets of budgets, all counted by the same allocator:

- `tools/alloc-corpus.tsv` budgets the value path: parsing, emission, and serialization of
  fixtures and generated workloads. Each budget is about 1.25 times its measured value. Against
  0.1.3 these workloads peak up to 68% lower while they parse, and allocate 25% to 73% fewer
  bytes in total: the short-sequence workload peaks 34% lower, short explicit pairs 35%, and the
  short map 32%. Emission and serialization are as they were. The short-node workloads may
  allocate at most 130 times their input bytes in total.
- `tools/typed-alloc-corpus.tsv` budgets typed decoding of a 20 KB configuration fixture, which
  peaks at about 250 KB and allocates about 470 KB in total.
- `tools/json-alloc-corpus.tsv` budgets an untyped decode into a JSON tree through the
  compatibility reader, as a consumer that reads YAML into JSON does, on the nine document shapes
  of issue #178. It counts a reallocation by the bytes it adds, as the issue does, so its figures
  are the issue's, and are not to be set beside the other two files', which count a
  reallocation as a new block. Each budget is about 1.10 times its measured value, but for a
  shape that is over the issue's target, whose budget is about 1% over its measurement.

What holds those peaks down: the token arena is sized from the density of the input scanned so
far, within the token limit (`docs/limits.md` gives its bounds). Event vectors reserve four
fifths of the token count plus headroom. A typed read of more than 8 KiB streams events straight
into the builder, so no document event vector is kept. Scalar tokens keep their raw text as a
span and box only values that need decoding; typed nodes box tags, keep scalar text as a source
slice, and cache hashes in one atomic word. Root flow collections reserve their entry count
from the parser before their items are built.

Issue #178 gives a reference for each of its nine shapes, the peak of a libyaml-based reader:
1.40, 41.5, and 165.9 MB for the three service descriptions, and 36.6, 35.7, 34.3, 67.8, 18.4,
and 18.1 MB for the six megabyte documents, in the order of the budget file. Against those, the
service descriptions peak at about 30 bytes for each input byte, 0.80 to 0.81 times the
reference, and five of the six megabyte documents at 19 to 72 bytes: all within the 1.2 times
that the issue asks for, at 0.61 to 1.06 times. The ninth, a megabyte block sequence of short
words, peaks at 23.1 MB, where 1.2 times allows 21.7 MB. At that peak the token arena (8.4 MB),
the typed node vector (10.5 MB), the line records (3.1 MB), and the line index (1.0 MB) are all
live, and the JSON tree, 4.8 MB, is not yet built. AY-104 tracks composition without the
intermediate node tree, which takes the node vector out of that peak. Earlier figures for these
shapes in this document and in the issue's thread, such as 98 MB for a flow sequence of `1`,
measured a decode into ayaml's own `Value`, another tree than the issue's, so they were not
figures to set beside its reference.

The release process keeps the results of its timing and comparison gates private; `CONTRIBUTING.md`
says what those gates are. The figures here are the ones that CI budgets. Where an issue reports
memory against a libyaml-based reader, its reference is quoted from the issue.

## Interpreting results

- Compare only runs from the same host, CPU affinity, compiler, build mode, and
  input bytes.
- Treat noisy-host runs as observations, not release evidence.
- Keep correctness and conformance checks independent of timing results.
- Record any deliberate performance regression and its cause before release.
