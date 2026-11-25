# ayaml-edit

Verified, source-preserving edits for YAML documents built exclusively on [`ayaml`](https://crates.io/crates/ayaml)'s public API.

## Capabilities

- **Scalar replacement** (`replace_scalar`, `replace_scalar_with`)
- **Whole-entry removal** (`remove_entry`, `remove_entry_with`)
- **Verified batch edits** (`apply_batch`, `apply_batch_with`)
- **Post-edit verification:** every returned `VerifiedEdit` is reparsed with `ayaml` and verified against both the expected value tree and the event stream before being returned, preserving byte-order marks, CRLF line endings, comments, and neighboring bytes.
- **Conservative refusals:** anchors, aliases, tags, block scalars, multiple documents, opt-in merge-key expansion, and ambiguous comment placement are refused rather than risking silent corruption.
