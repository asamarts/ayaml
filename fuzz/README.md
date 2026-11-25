# ayaml fuzz targets

These targets are a separate `cargo-fuzz` workspace:

```sh
cargo install cargo-fuzz --locked
cargo +nightly fuzz run parse --target x86_64-unknown-linux-gnu -- -rss_limit_mb=2048
cargo +nightly fuzz run emit --target x86_64-unknown-linux-gnu -- -rss_limit_mb=2048
cargo +nightly fuzz run borrowed --target x86_64-unknown-linux-gnu -- -rss_limit_mb=2048
cargo +nightly fuzz run structured --target x86_64-unknown-linux-gnu -- -rss_limit_mb=2048
cargo +nightly fuzz run spanned --target x86_64-unknown-linux-gnu -- -rss_limit_mb=2048
cargo +nightly fuzz run merge_keys --target x86_64-unknown-linux-gnu -- -rss_limit_mb=2048
cargo +nightly fuzz run masked_errors --target x86_64-unknown-linux-gnu -- -rss_limit_mb=2048
cargo +nightly fuzz run event_emit --target x86_64-unknown-linux-gnu -- -rss_limit_mb=2048
cargo +nightly fuzz run typed_equiv --target x86_64-unknown-linux-gnu -- -rss_limit_mb=2048
cargo +nightly fuzz run edit --target x86_64-unknown-linux-gnu -- -rss_limit_mb=2048
```

Targets:

- `parse` exercises raw value, document, and event parsing.
- `emit` parses events, renders them, and reparses the rendered YAML; it also round-trips a
  composed `Value`.
- `borrowed` compares the borrowed typed path with the owned composition path, and compares the
  tree-backed typed reader with the event-stream typed reader on raw bytes.
- `structured` uses structure-aware `arbitrary::Arbitrary` generators to build nested multi-doc
  YAML (anchors, aliases, tags, block scalars, flow/block collections, directives, BOM, CRLF)
  and verifies parsing, cursor reads, and value round trips.
- `spanned` checks `SpannedTree` span bounds, containment, `scalar_text`, verified `entry_span`
  deletion, and `LineIndex` bijectivity and monotonicity across UTF-8, Scalar, and UTF-16 columns.
- `merge_keys` exercises `Value::apply_merge_keys` and `apply_merge_keys_with_limits` on
  structure-aware values and documents, checking limit monotonicity and idempotence.
- `masked_errors` injects generated canary tokens across failure classes and verifies
  `Error::masked()` preserves metadata, hides the canary, and returns `source() == None`.
- `event_emit` feeds arbitrary (balanced and scrambled) `Event` streams to all three event
  writers and verifies identical output/refusals and reparse soundness.
- `typed_equiv` compares `from_str_by_tree_with` and `from_str_by_events_with` over
  structure-aware YAML, default/compat modes, tight limits, and multiple deserialization targets.
- `edit` exercises `ayaml_edit::replace_scalar`, `remove_entry`, and `apply_batch` over
  structure-aware YAML, checking `apply_edits` equivalence, reparsing, and preservation of all
  unrelated values.

The target harness bodies live in `fuzz/src/lib.rs`, so deterministic unit tests cover them on
stable Rust without linking libFuzzer:

```sh
cargo test --manifest-path fuzz/Cargo.toml --lib --locked
```

Fuzzing itself requires nightly sanitizer support and runs on a daily schedule in
`.github/workflows/fuzz.yml`. Run it locally with `-rss_limit_mb=2048` and a time bound, and
record any crash before changing behavior.

`memcount` isn't a fuzz target. It is the allocation counter behind
`tools/check_alloc_corpus.sh`, which runs on stable and checks three sets of budgets:
`tools/alloc-corpus.tsv` for the value path, `tools/typed-alloc-corpus.tsv` for typed decoding,
and `tools/json-alloc-corpus.tsv` for an untyped decode into a JSON tree. The last is why this
workspace depends on `serde_json`, which the published crate uses in its tests alone:

```sh
tools/check_alloc_corpus.sh
```
