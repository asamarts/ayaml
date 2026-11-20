---
id: M-v0.2.0
title: Publish ayaml 0.2.0 with every break that is decided
phase: 3
status: todo
blocked_by:
- AY-020
- AY-045
- AY-104
- AY-051
- AY-120
- AY-126
- AY-127
- AY-121
tags:
- milestone
- v0.2.0
---

## Why

A breaking release costs each consumer one migration, so 0.2.0 holds every breaking change the
plan knows of, and nothing additive waits in front of it (D90).

## Acceptance criteria

- [ ] every member is closed with the evidence its tags require
- [ ] ayaml 0.2.0 is published through the release flow, with a migration note in the changelog
- [ ] `SECURITY.md` names the versions that get fixes after 0.2.0
