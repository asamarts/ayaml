---
id: M-post-v1-perf-and-hardening
title: Post-1.0 incremental limit streaming and iterative ValueWriter emission
phase: 10
status: done
blocked_by:
- AY-176
tags:
- milestone
- post-v1
- performance
- security
evidence:
- kind: commit
  ref: "0a9c427"
- kind: test
  ref: emitter::writer_tests::iterative_value_writer_and_tag_scan_handle_deep_nesting_on_small_stack
---

## Why

Track non-blocking post-1.0 defense-in-depth enhancements (`1.0.x`) separately from the `1.0.0`
release gate: incremental `max_scalar_bytes` and `max_output_bytes` streaming checks and
iterative `ValueWriter` / `TagDirectives::scan_at` emission.

## Acceptance criteria

- [x] `AY-176` is closed with full conformance, adversarial, allocation, and Callgrind evidence
