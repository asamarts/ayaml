---
id: M-correctness-baseline
title: Establish a thorough, multi-layered correctness baseline
phase: 5
status: done
blocked_by:
- AY-129
- AY-130
- AY-131
- AY-132
- AY-133
- AY-134
- AY-135
- AY-156
tags:
- milestone
- testing
evidence:
- kind: commit
  ref: 3dad058
- kind: commit
  ref: 42be965
- kind: test
  ref: error::tests::path_truncation_and_key_quoting_preserve_utf8_and_bounds
- kind: test
  ref: dos::deep_flow_collections_stay_bounded
---

## Why

Before optimizing hot loops or freezing a 1.0 interface, every public layer needs
layered verification: structured property generation, cross-layer invariants, an
independent JSON-superset oracle, structure-aware fuzzing across newer surfaces,
deterministic fault injection, Miri, Kani bounded model checking on pure kernels,
and scheduled mutation testing.

## Acceptance criteria

- [x] every member is closed with the evidence its tags require
- [x] all CI correctness, property, adversarial, and fuzz compilation checks pass cleanly
