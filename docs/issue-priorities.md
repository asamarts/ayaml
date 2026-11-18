# Open issue priorities

This is the working order for the open GitHub issues. It groups issues by root
cause so one change is tested once, rather than fixing symptoms separately.

## Correctness and data safety

No open correctness issues.

Completed: AY-086 — #132, #146, #147 in `608a0b4` (#152);
AY-087 — #133, #137, #140; AY-088 — #138; AY-089 — #139; AY-090 — #141;
AY-083 — #124; AY-082 — #123; AY-045; AY-091 — #165; AY-092 — #160, #161, #162;
AY-093 — #164; AY-070; AY-057; AY-084 — #125; AY-049 — #128; AY-096 — #163;
AY-098, AY-100, and AY-101 (part of #127 and #178); AY-085 — #126.

## Diagnostics

The diagnostics queue is empty.

## API and performance

1. **#178.** Remaining short-scalar peak heap. Compact scalar tokens and typed nodes (AY-101)
   cut the measured shapes by roughly a third; the remaining gap needs composition without the
   intermediate typed node tree.
2. **#127.** Remaining libyaml-relative total allocation comparison. The large-document
   peak work (AY-096), mid-size token-arena reserve (AY-098), streaming typed reads with a
   prefix token estimate (AY-100), and compact tokens and nodes (AY-101) are complete; the
   large typed total is now below the reference, while the configuration comparison remains.

## Dependencies

```text
#127 (total allocation)
AY-085 (#126)
```

The correctness queue is empty. The remaining peak-heap comparisons and AY-085 are independent.
