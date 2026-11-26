---
id: M-site-polish
title: Polish website copy, add visual benchmark charts, and publish Correctness page before 1.0
phase: 9
status: done
blocked_by:
- AY-166
- AY-167
- AY-168
tags:
- milestone
- docs
- site
evidence:
- kind: commit
  ref: 670fdeb
- kind: commit
  ref: d6a216a
- kind: commit
  ref: 2517bc1
- kind: test
  ref: docs_book_and_reference_pages_are_complete_and_doctested_in_ci
- kind: url
  ref: https://asamarts.github.io/ayaml/docs/correctness/
---

## Why

Before tagging `1.0.0`, the public website (`https://asamarts.github.io/ayaml/`) should read in a
natural, consistent engineering voice with zero em dashes, present visual CPU and memory
comparison charts directly on the benchmarks pages without bloating page length, and give
engineers a clear top-level `Correctness` page explaining the full testing, fuzzing, and formal
verification stack.

## Acceptance criteria

- [x] `AY-166`, `AY-167`, and `AY-168` are closed with green site builds, link checks, doctests, and policy checks
- [x] `https://asamarts.github.io/ayaml/` deploys cleanly from `main` with all three improvements live
