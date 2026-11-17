# Open issue priorities

This is the working order for the open GitHub issues. It groups issues by root
cause so one change is tested once, rather than fixing symptoms separately.

## Correctness and data safety

No open correctness issues.

Completed: AY-086 — #132, #146, #147 in `608a0b4` (#152);
AY-087 — #133, #137, #140; AY-088 — #138; AY-089 — #139; AY-090 — #141;
AY-083 — #124; AY-082 — #123; AY-045.

## API and performance

1. **AY-049 — #128.** Spanned tree with path lookup.
2. **AY-070 / AY-057 — #127.** Total allocation and peak heap on configuration
   files.
3. **AY-084 — #125.** Lazy compatibility event stream.
4. **AY-085 — #126.** Document that event readers do not validate alias
    targets.

## Dependencies

```text
AY-049 (spanned tree, #128)

AY-070 / AY-057 (#127)
AY-084 (#125)
AY-085 (#126)
```

The correctness queue is empty. AY-049 is the next interface task; AY-070 /
AY-057 and the remaining API work are independent.
