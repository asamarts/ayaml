---
id: M-site-and-docs
title: Launch refreshed documentation site with benchmarks showcase and tested migration guides
phase: 9
status: done
blocked_by:
- AY-158
- AY-159
- AY-160
- AY-161
- AY-162
tags:
- milestone
- docs
evidence:
- kind: file
  ref: site/astro.config.mjs
- kind: file
  ref: site/src/pages/benchmarks.astro
- kind: test
  ref: docs_book_and_reference_pages_are_complete_and_doctested_in_ci
---

## Why

A polished, modern documentation site with transparent benchmark data and
end-to-end tested migration guides makes `ayaml` 1.0 immediately approachable
for production adopters.

## Acceptance criteria

- [x] every member is closed with passing site builds, link checks, and doctests
- [x] `https://asamarts.github.io/ayaml/` deploys cleanly from `main`
