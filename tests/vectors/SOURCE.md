# Where these files come from

Everything in this directory but this file is vendored, byte for byte, from the shared
format-contract vectors of [adotenv](https://github.com/asamarts/adotenv), at
`tests/vectors/` in commit `b4c82fd3faaa134b3db15e8adc9c2510121e0d3f` (2026-11-16). The contract
they belong to is that repository's `docs/design/format-contract.md`.

One file of that directory is left out: `manifest.json`, which records the SHA-256 of each
vector and schema. This repository's secret scanner takes one of its lines for a key, for the
letters `api` in a file's name beside a digest, and the scanner's rules are not relaxed for
it. The digests are recorded in `tools/test_ci_checks.py` instead, with that of `README.md`,
which the manifest doesn't list, and the test fails when a file here has another digest or
when a file stands beside the recorded ones. To take a newer set, copy the files again, and
update the commit above and the digests in the test in the same change.

What ayaml runs today:

- `positions.json`: `crates/ayaml/tests/shared_vectors.rs` checks `LineIndex` against every case
  in each column encoding. ayaml's index leaves a leading byte-order mark out of the columns,
  which the contract makes the default, so the three sources that open with a mark are checked
  against their `without_bom` answers. The byte-relative form of those three, where the mark's
  bytes are columns, is not offered.
- `edit-order.json`: nothing yet. ayaml has no text-edit API; the `ayaml-edit` crate (AY-050)
  will run it.
- `github-escaping.json` and the two report schemas: nothing. ayaml has no command-line tool.

[`docs/interface.md`](../../docs/interface.md) says what ayaml offers at each layer of the
contract.
