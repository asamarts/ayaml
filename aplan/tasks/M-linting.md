---
id: M-linting
title: Enforce repository and Rust lint policy before 0.1.2
phase: 1
status: todo
blocked_by:
- AY-072
- AY-073
- AY-074
- AY-075
- AY-076
- AY-077
- AY-078
tags:
- milestone
- linting
- maintainability
---

## Why

Repository structure and Rust policy are enforced only by scattered checks and reviewer
knowledge. Five hand-written Rust files already exceed the requested 2,000-line ceiling, and
the workspace enables only three opt-in Clippy lints. Establish the gate before more parser,
scanner, typed-reader, and release work adds to that debt.

## Acceptance criteria

- [ ] every member is closed with its required evidence
- [ ] full-tree alint, MSRV and stable Clippy, rustdoc, rustfmt, and the production panic-prone-operation profile are blocking local and hosted checks
- [ ] every maintained Rust, Python, shell, and hook source file is at most 2,000 lines, with no baseline, exclusion, or temporary ratchet left for the size rule
- [ ] the policy, intentional exceptions, update procedure, and local commands are documented
