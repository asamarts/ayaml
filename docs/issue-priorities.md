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
Issue #127 closed by the streaming and compact-token work. AY-104 is the next planned #178 step.

## Diagnostics

The diagnostics queue is empty.

## API and performance

1. **#178.** Remaining short-scalar peak heap. Compact scalar tokens, typed nodes, and root
   flow reservations (AY-101 through AY-103) cut the measured shapes by more than a third; the
   remaining gap needs composition without the intermediate typed node tree, tracked by AY-104.

## Dependencies

```text
#178 (short-scalar peak heap)
```

The correctness queue is empty. #178 is independent of the remaining plan work.
