---
id: M-ecosystem-and-dx
title: Complete ecosystem interoperability, ergonomic helpers, and safe editing before 1.0
phase: 8
status: done
blocked_by:
- AY-143
- AY-144
- AY-145
- AY-146
- AY-147
- AY-148
tags:
- milestone
- api
evidence:
- kind: test
  ref: docs_book_and_reference_pages_are_complete_and_doctested_in_ci
- kind: commit
  ref: 1397fad
---

## Why

Real-world YAML consumers across configuration, schema validation, CI pipelines,
container specs, documentation frontmatter, and linters need Serde middleware
compatibility, bounded reader entry points, span-preserving typed decoding,
frontmatter splitting, opt-in typed merge keys, safe edits, and clear migration
guides.

## Acceptance criteria

- [x] every member is closed with tests and book documentation examples passing
- [x] all new APIs are additive and pass SemVer and public-docs checks
