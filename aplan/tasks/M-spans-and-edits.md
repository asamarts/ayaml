---
id: M-spans-and-edits
title: Let consumers locate and edit YAML values safely
phase: 4
status: todo
blocked_by:
- AY-049
- AY-050
tags:
- milestone
- api
---

## Why

Tools that fix files need a byte span for a path, a scalar encoding, and edits they can
trust. ayaml has none of these yet.

## Acceptance criteria

- [ ] every member is closed with test evidence
- [ ] a downstream multi-format fix layer's YAML set and removal cases pass through these APIs
