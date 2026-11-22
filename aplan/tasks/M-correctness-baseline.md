---
id: M-correctness-baseline
title: Establish a thorough, multi-layered correctness baseline
phase: 5
status: todo
blocked_by:
- AY-129
- AY-130
- AY-131
- AY-132
- AY-133
- AY-134
- AY-135
tags:
- milestone
- testing
---

## Why

Before optimizing hot loops or freezing a 1.0 interface, every public layer needs
layered verification: structured property generation, cross-layer invariants, an
independent JSON-superset oracle, structure-aware fuzzing across newer surfaces,
deterministic fault injection, Miri, Kani bounded model checking on pure kernels,
and scheduled mutation testing.

## Acceptance criteria

- [ ] every member is closed with the evidence its tags require
- [ ] all CI correctness, property, adversarial, and fuzz compilation checks pass cleanly
