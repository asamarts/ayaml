# Open issue priorities

This is the working order for the open GitHub issues. It groups issues by root
cause so one change is tested once, rather than fixing symptoms separately.

## Correctness and data safety

No open correctness issues.

Completed: AY-086 — #132, #146, #147 in `608a0b4` (#152);
AY-087 — #133, #137, #140; AY-088 — #138; AY-089 — #139; AY-090 — #141;
AY-083 — #124; AY-082 — #123.

## API and performance

1. **AY-045 — #123 and the span/line-index interface.** Byte-spanned events,
   public line index, and column encodings.
2. **AY-049 — #128.** Spanned tree with path lookup; blocked by AY-045.
3. **AY-070 / AY-057 — #127.** Total allocation and peak heap on configuration
   files.
4. **AY-084 — #125.** Lazy compatibility event stream.
5. **AY-085 — #126.** Document that event readers do not validate alias
    targets.

## Dependencies

```text
AY-045 (byte spans) ──> AY-049 (spanned tree, #128)

AY-070 / AY-057 (#127)
AY-084 (#125)
AY-085 (#126)
```

The correctness queue is empty. AY-045 and AY-049 form the next interface
sequence; AY-070 / AY-057 and the remaining API work are independent.
