# Shared format-contract vectors

These files are the shared vectors for the format contract. Keep them byte-for-byte when
vendoring them into another parser, and update `manifest.json` whenever a vector changes.

- `positions.json` covers byte offsets, line endings, a leading byte-order mark, astral
  characters, NEL, LS, PS, and expected position errors. `expected` uses counted BOM columns;
  `without_bom` uses `LineIndex::without_bom`.
- `edit-order.json` covers sorted text edits, including an insertion and a replacement that
  start at the same offset.
- `github-escaping.json` covers GitHub Actions property and data escaping.
- `schema/check-report.v1.json` is the JSON Schema for `adotenv-cli check --format json`.
- `schema/diff-report.v1.json` is the JSON Schema for `adotenv-cli diff --format json`.

The schemas are published under `https://asamarts.github.io/adotenv/schema/` by the docs
workflow.

`manifest.json` records the SHA-256 of each file. The vector test fails if a file changes
without a matching manifest update.
