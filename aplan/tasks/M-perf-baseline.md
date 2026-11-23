---
id: M-perf-baseline
title: Build a reproducible multi-scenario benchmark and regression-gating baseline
phase: 6
status: done
blocked_by:
- AY-136
- AY-137
- AY-138
- AY-139
- AY-140
tags:
- milestone
- performance
evidence:
- kind: test
  ref: tests::jsonl_and_fingerprint_schemas_roundtrip_and_reject_malformed_rows
---

## Why

Current benchmarks and instruction checks cover a narrow set of small synthetic
inputs and duplicate fixture generators across three tools. A single deterministic
fixture crate, broad domain and boundary coverage, structured JSONL memory gates,
steady-state Callgrind instruction and cycle gates, and a unified `xtask` CLI give
reliable before-and-after measurements for optimization work.

## Acceptance criteria

- [x] every member is closed with the evidence its tags require
- [x] all benchmark, allocation, and Callgrind checks share one deterministic fixture manifest
