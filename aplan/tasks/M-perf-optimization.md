---
id: M-perf-optimization
title: Close known memory and throughput bottlenecks against the performance baseline
phase: 7
status: done
blocked_by:
- AY-141
- AY-142
tags:
- milestone
- performance
evidence:
- kind: commit
  ref: 05ba22b
- kind: commit
  ref: 7305cc5
---

## Why

Once the correctness and performance baselines are in place, close the open arena,
size-hint, anchored-node, nested-key, document-cursor, and baseline-identified hot
loop bottlenecks with deterministic instruction and allocation evidence.

## Acceptance criteria

- [x] every member is closed with before-and-after instruction and allocation evidence
- [x] no correctness or limit invariant regresses
