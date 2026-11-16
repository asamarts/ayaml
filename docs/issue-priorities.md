# Open issue priorities

This is the working order for the open GitHub issues. It groups issues by root
cause so one change is tested once, rather than fixing symptoms separately.

## Correctness and data safety

1. **AY-086 — #132, #146, #147.** Block-scalar boundaries: a header after
   properties must be indented past its parent, and a document marker after a
   top-level scalar's indented empty lines must end the scalar.
2. **AY-087 — #133, #137, #140.** Leading byte-order-mark handling in compat:
   error locations, the quoted-scalar continuation check, and the second leading
   mark.
3. **AY-089 — #139.** The compatibility scanner and line reader must end an
   anchor name at the same place before a flow indicator.
4. **AY-088 — #138.** A quoted scalar's continuation line beginning with a tab
   followed by spaces is not indented; reject it.
5. **AY-090 — #141.** A comment after a flow `?` must use the same line-ending
   rule as every other comment; NEL is content.
6. **AY-083 — #124.** Default count limits currently refuse valid documents far
   below the 32 MiB input limit. Define and test the intended default profile.
7. **AY-082 — #123.** Plain-scalar spans and raw text include trailing
   separation, which makes span-based edits unsafe.

## API and performance

1. **AY-070 / AY-057 — #127.** Total allocation and peak heap on configuration
   files.
2. **AY-045 — #123 and the span/line-index interface.** Byte-spanned events,
   public line index, and column encodings.
3. **AY-049 — #128.** Spanned tree with path lookup; blocked by AY-045 and by
    AY-082 for safe plain-scalar extents.
4. **AY-084 — #125.** Lazy compatibility event stream.
5. **AY-085 — #126.** Document that event readers do not validate alias
    targets.

## Dependencies

```text
AY-082 (plain scalar extent) ─┐
AY-045 (byte spans) ──────────┴─> AY-049 (spanned tree, #128)

AY-086 (#132/#146/#147)
AY-087 (#133/#137/#140)
AY-089 (#139)
AY-088 (#138)
AY-090 (#141)
AY-083 (#124)
AY-070 / AY-057 (#127)
AY-084 (#125)
AY-085 (#126)
```

The correctness group is ordered by severity and overlap. The block-scalar
issues share scanner/parser state, so AY-086 and AY-087 should land before the
smaller indentation and anchor-name fixes. AY-083 and AY-082 are independent and
can run in parallel with the block-scalar work.
