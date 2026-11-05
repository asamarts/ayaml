---
id: M-v0.1.2
title: Publish the hardened, aplan-compatible ayaml 0.1.2 release
phase: 1
status: todo
blocked_by:
- AY-001
- AY-002
- AY-003
- AY-004
- AY-005
- AY-031
- AY-032
- AY-033
- AY-034
- AY-038
- AY-014
- AY-015
- AY-016
- AY-017
- AY-018
- AY-019
- AY-021
- AY-022
- AY-023
- AY-024
- AY-025
- AY-026
- AY-027
- AY-028
- AY-029
- AY-011
- AY-013
- AY-043
tags:
- milestone
- v0.1.2
---

## Why

Aplan A2-009 can replace its translated libyaml bridge only with an immutable
ayaml release that preserves the consumer's typed errors, compatibility forms,
diagnostic context, and supply-chain evidence. This cut contains the ayaml-side
work needed to produce that release. It also fixes defects and gate gaps found
in 0.1.1, so that aplan and other users qualify one release, not two.

## Acceptance criteria

- [ ] every member is closed with test evidence
- [ ] ayaml 0.1.2 is published from the attested public tag
- [ ] the immutable release evidence is sufficient for aplan A2-009 to resume
