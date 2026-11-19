# ayaml fuzz targets

These targets are a separate cargo-fuzz workspace:

```sh
cargo install cargo-fuzz --locked
cargo +nightly fuzz run parse --target x86_64-unknown-linux-gnu
cargo +nightly fuzz run emit --target x86_64-unknown-linux-gnu
cargo +nightly fuzz run borrowed --target x86_64-unknown-linux-gnu
```

`parse` exercises value/event/document parsing and event collection. `emit`
parses events, renders them, and reparses the rendered YAML; it also round-trips
a composed `Value`. `borrowed` compares the borrowed typed path with the owned
composition path, and the streamed borrowed path with the composed document list,
including whether each accepts the input.

Fuzzing is intentionally not part of the default workspace test gate because it
requires nightly sanitizer support. Run it with an appropriate time budget and
record any crash before changing behavior.

`memcount` isn't a fuzz target. It is the allocation counter behind
`tools/check_alloc_corpus.sh`, which runs on stable and checks three sets of budgets:
`tools/alloc-corpus.tsv` for the value path, `tools/typed-alloc-corpus.tsv` for typed decoding,
and `tools/json-alloc-corpus.tsv` for an untyped decode into a JSON tree. The last is why this
workspace depends on `serde_json`, which the published crate uses in its tests alone:

```sh
tools/check_alloc_corpus.sh
```
