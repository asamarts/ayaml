# Open issue priorities

This is the working order for the open GitHub issues. It groups issues by root
cause so one change is tested once, rather than fixing symptoms separately.

## Correctness and data safety

No open correctness issues.

Completed: AY-086 — #132, #146, #147 in `608a0b4` (#152);
AY-087 — #133, #137, #140; AY-088 — #138; AY-089 — #139; AY-090 — #141;
AY-083 — #124; AY-082 — #123; AY-045; AY-091 — #165.

## Diagnostics

These issues were filed after the original priority pass and still need task planning:

1. **#160** — compatibility indentation errors should name the collection being read.
2. **#161** — an unclosed quoted scalar should name where the quote opened.
3. **#162** — Serde type errors should carry the actual unexpected value.

## API and performance

1. **AY-049 — #128.** Spanned tree with path lookup.
2. **AY-070 / AY-057 — #127** and **#163.** Total allocation and peak heap on
   configuration files.
3. **#164.** Non-allocating `Mapping` lookups by `&str`.
4. **AY-084 — #125.** Lazy compatibility event stream.
5. **AY-085 — #126.** Document that event readers do not validate alias
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
