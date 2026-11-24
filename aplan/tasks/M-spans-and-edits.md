---
id: M-spans-and-edits
title: Let consumers locate and edit YAML values safely
phase: 4
status: done
blocked_by:
- AY-049
- AY-050
- AY-116
tags:
- milestone
- api
evidence:
- kind: test
  ref: replace_scalar_preserves_bom_crlf_comments_and_neighbors
- kind: test
  ref: remove_entry_handles_block_mappings_compact_first_keys_and_sequences
- kind: test
  ref: workflow_files_have_exact_entry_spans_with_high_coverage
---

## Why

Tools that fix files need a byte span for a path, a scalar encoding, and edits they can
trust. ayaml has none of these yet.

## Acceptance criteria

- [x] every member is closed with the evidence its tags require
- [x] a fixture of YAML set and removal cases, shaped like what a fix tool needs, passes through these APIs
