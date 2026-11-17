# Open issue priorities

This is the working order for the open GitHub issues. It groups issues by root
cause so one change is tested once, rather than fixing symptoms separately.

## Correctness and data safety

No open correctness issues.

Completed: AY-086 — #132, #146, #147 in `608a0b4` (#152);
AY-087 — #133, #137, #140; AY-088 — #138; AY-089 — #139; AY-090 — #141;
AY-083 — #124; AY-082 — #123; AY-045; AY-091 — #165; AY-092 — #160, #161, #162;
AY-093 — #164; AY-070; AY-057; AY-084 — #125; AY-049 — #128.

## Diagnostics

The diagnostics queue is empty.

## API and performance

1. **#127** and **#163.** Remaining peak-heap and libyaml-relative allocation
   comparisons. The short-node peak work (AY-057) and total-allocation budget (AY-070) are
   complete.
2. **AY-085 — #126.** Document that event readers do not validate alias
    targets.

## Dependencies

```text
#127 and #163 (peak heap)
AY-085 (#126)
```

The correctness queue is empty. The remaining peak-heap comparisons and AY-085 are independent.
