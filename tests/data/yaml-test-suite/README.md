# YAML Test Suite (pinned)

Vendored from `https://github.com/yaml/yaml-test-suite` at commit
`da267a5c4782e7361e82889e76c0dc7df0e1e870` on 2026-10-21.

- `src/` contains 351 YAML
  metadata documents.
- Content SHA-256 (sorted file digests): `e3a740a23046f3324c9acc833f2d3223a3885589c33ba530710c6bfd620c5148`,
  from `find src -type f | LC_ALL=C sort | xargs sha256sum | sha256sum`.
- License: MIT; see `License`.
- Upstream readme: `ReadMe.md`.

`crates/ayaml-conformance` runs every case. A strict run fails unless `src/` matches the file
count and digest in `REVISION`, and it saw the case counts in that program's `EXPECTED`.
