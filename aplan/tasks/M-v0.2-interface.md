---
id: M-v0.2-interface
title: Align the interface shapes in 0.2.x releases
phase: 3
status: done
blocked_by:
- M-v0.2.0
- AY-046
- AY-047
- AY-048
- AY-052
- AY-122
tags:
- milestone
- api
evidence:
  - kind: url
    ref: https://github.com/asamarts/ayaml/releases/tag/v0.2.2
  - kind: url
    ref: https://github.com/asamarts/ayaml/releases/tag/v0.2.3
  - kind: url
    ref: https://asamarts.github.io/ayaml/
---

## Why

ayaml is pre-1.0, so this is the cheap moment to align its spans, errors, and options
with the shapes adotenv already uses. The spans went into 0.2.0, with every other break that was
decided (D90). What is left adds to the interface and breaks nothing: stable error codes, masked
errors, the options values, the shared position vectors, and the documentation book. Each ships
in a 0.2.x release when it is done.

## Acceptance criteria

- [x] every member is closed with test evidence
- [x] each member is in a published 0.2.x release that breaks nothing: the semver check finds no break, and neither does a comparison of the public API with the release before (AY-124), since the pinned checker reported none of the four breaks of 0.2.0
