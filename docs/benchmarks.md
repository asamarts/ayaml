# Benchmark methodology

ayaml includes benchmarks and allocation probes so that performance and memory
behavior can be reviewed separately from correctness. Benchmark results are
environment-specific and are not a compatibility guarantee.

## Layers

The harness measures these layers separately:

- scanner/tokenization;
- parsing into events;
- event replay and event emission;
- generic `Value` composition;
- typed Serde decoding; and
- value and document emission.

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

## Time and memory

Timing uses repeated samples and reports medians rather than a single run.
Allocation probes report allocated bytes, allocation count, and peak resident
memory. Peak memory is measured for parsing, alias materialization, and
emission separately.

The release process keeps comparison results private. This document describes
the methodology so that absolute or relative measurements can be reviewed on
their merits when they are published.

## Interpreting results

- Compare only runs from the same host, CPU affinity, compiler, build mode, and
  input bytes.
- Treat noisy-host runs as observations, not release evidence.
- Keep correctness and conformance checks independent of timing results.
- Record any deliberate performance regression and its cause before release.
