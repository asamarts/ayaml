# ayaml fuzz targets

These targets are a separate cargo-fuzz workspace:

```sh
cargo install cargo-fuzz
cargo fuzz run parse --target x86_64-unknown-linux-gnu
cargo fuzz run emit --target x86_64-unknown-linux-gnu
cargo fuzz run borrowed --target x86_64-unknown-linux-gnu
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
`tools/check_alloc_corpus.sh`, which checks the budgets in
`tools/alloc-corpus.tsv` and runs on stable:

```sh
tools/check_alloc_corpus.sh
```
