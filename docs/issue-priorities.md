# Open issue priorities

This is the working order for the open GitHub issues. It groups issues by root
cause so one change is tested once, rather than fixing symptoms separately.

## Correctness and data safety

No open correctness issues.

Completed: AY-086 — #132, #146, #147 in `608a0b4` (#152);
AY-087 — #133, #137, #140; AY-088 — #138; AY-089 — #139; AY-090 — #141;
AY-083 — #124; AY-082 — #123; AY-045; AY-091 — #165; AY-092 — #160, #161, #162;
AY-093 — #164; AY-070; AY-057; AY-084 — #125; AY-049 — #128; AY-096 — #163;
AY-098, AY-100, AY-101, AY-102, and AY-103 (part of #127 and #178); AY-085 — #126;
Issue #127 closed by the streaming and compact-token work. AY-105 through AY-113 fixed what
audits found after it.

## Diagnostics

The diagnostics queue is empty.

## API and performance

1. **#178.** Short-scalar peak heap. Measured as the issue measures it, eight of its nine shapes
   are within 1.2 times its reference, at 0.61 to 1.06 times, and `tools/json-alloc-corpus.tsv`
   budgets all nine (AY-113). The ninth, a megabyte block sequence of short words, is at 1.28
   times. AY-104, composition without the intermediate typed node tree, is the planned work for
   it.

## Dependencies

```text
#178 (short-scalar peak heap)
```

The correctness queue is empty. The 0.2.0 release waits for #178 (D94); no other task does.
