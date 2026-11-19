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

No open performance issues. #178, short-scalar peak heap: measured as the issue measures it,
its nine shapes peak at 0.52 to 0.96 times its reference, within the 1.2 times it asks for, and
`tools/json-alloc-corpus.tsv` budgets all nine (AY-113). AY-104 brought the last of them there,
by reading a typed value from the parser's events without a tree of the document in between.

## Dependencies

No open issue waits for another. The correctness queue is empty.
