---
id: M-perf-optimization
title: Close known memory and throughput bottlenecks against the performance baseline
phase: 7
status: todo
blocked_by:
- AY-141
- AY-142
tags:
- milestone
- performance
---

## Why

Once the correctness and performance baselines are in place, close the open arena,
size-hint, anchored-node, nested-key, document-cursor, and baseline-identified hot
loop bottlenecks with deterministic instruction and allocation evidence.

## Acceptance criteria

- [ ] every member is closed with before-and-after instruction and allocation evidence
- [ ] no correctness or limit invariant regresses
