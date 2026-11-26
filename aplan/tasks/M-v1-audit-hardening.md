---
id: M-v1-audit-hardening
title: Resolve pre-1.0 adversarial audit findings across correctness, security, verification, API cleanup, and benchmarks
phase: 9
status: todo
blocked_by:
- AY-169
- AY-170
- AY-171
- AY-172
- AY-173
- AY-174
- AY-175
- AY-177
- AY-178
- AY-179
- AY-180
- AY-181
tags:
- milestone
- v1.0
- correctness
- performance
---

## Why

Before running the final `AY-150` extended fuzz soak and tagging `1.0.0`, resolve every
blocking issue surfaced by the pre-1.0 adversarial correctness, verification, and benchmark
audits and land the full performance, memory, and zero-copy API upgrade ahead of the 1.0
benchmark capture: resource-bound bugs in `SpannedTree`, `merge/events.rs`, and
`ValueWriter::emit_flow`; semantic edge cases in merge keys, `ayaml-edit`, and `compat`;
deterministic Callgrind/DHAT profiling (`xtask det` / `profile`); compact 32 B `Value` with
linear small `Mapping` and unboxed quoted tokens; boxed `Tag`/`Error`, lazy `LineStarts`,
flow fast paths, and safe SWAR scanning; single-pass bounded-line-window streaming parsing;
unified streaming typed Serde at all sizes; future-proof flat-array `Tape<'a>` DOM and
reusable `ParserBuf` scratch APIs; dead/narrow fuzz, adversarial, Miri, Kani, and mutation
coverage; 1.0 API cleanup (`#[deprecated]` removals, `ErrorKind::Alias`,
`FrontmatterDelimiter`, and `ayaml-edit` `^1.0.0`); `xtask`/`memcount` and CI baseline fixes;
and comparative benchmark fairness, disclosure, and modern-hardware refresh on the 1.0
release candidate.

## Acceptance criteria

- [ ] `AY-169` through `AY-175` and `AY-177` through `AY-181` are closed with full test, policy, profiling, and verification evidence
- [ ] `ayaml` and `ayaml-edit` are ready to enter the `AY-150` 24+ core-hour soak and 1.0.0 release qualification
