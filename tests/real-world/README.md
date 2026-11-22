# Real-world fixtures

Minimal fixtures reduced from public YAML files. Each `.yml` file keeps the
shape of its source, its `.json` file holds the expected composed documents in
`serde_json` format, and its companion `.oracle.json` file sits beside it to
record the independent oracle, specification references, canonical YAML Test
Suite event stream (`expected_events`), and any documented reason why the
`.json` representation cannot come purely from a generic JSON or `libyaml`
oracle.

`crates/ayaml/tests/real_world.rs` checks every fixture against:

- the independent `.oracle.json` event stream (`expected_events`) and provenance metadata;
- an independently constructed Rust expected-value oracle (`expected_documents`) before comparing `.json` goldens (so `AYAML_BLESS=1` cannot bless a parser regression);
- the golden `.json` documents;
- the typed path, the document cursor, and the compatibility path;
- value and event round trips.

| Fixture | Source shape (public path) | Independent oracle | `.json` origin / notes |
| --- | --- | --- | --- |
| `compact-nested-sequence` | rust-lang/stdarch, `crates/stdarch-gen-arm/spec/{neon,sve,sve2}/*.spec.yml` | YAML 1.2.2 §8.2.1 / §7.4 + `libyaml` (`W42U`) | Pure independent JSON; nested compact sequence entries remain separate |
| `empty-file` | llvm-project, `llvm/test/tools/llvm-remarkutil/size-diff/Inputs/empty-file.yaml` | YAML 1.2.2 §9.2 + `libyaml` (`AVM7`, `98YD`) | Pure independent JSON (`[]`); an empty file has no documents |
| `repeated-document-end` | llvm-project, `llvm/test/ObjectYAML/MachO/DWARF-debug_info.yaml` | YAML 1.2.2 §9.2 / §10.3.2 + `libyaml` (`RTP8`, `UT92`, `6ZKB`, `7Z25`) | Events & values independent; `.json` uses `ayaml`'s `{"!mach-o": ...}` local-tag wrapper |
| `quoted-delete-character` | llvm-project, `llvm/test/tools/llvm-dlltool/Inputs/llvm_foo_dll_{1,3}.yaml` | YAML 1.2.2 §5.1 (`[2] nb-json`) / §7.3.1 (`RTP8`, `UT92`) | `libyaml` rejects U+007F DEL anywhere; `.json` also uses `{"!COFF": ...}` local-tag wrapper |
| `wide-integer-content` | llvm-project, `lldb/test/Shell/{Minidump,Commands}/*.yaml` and `llvm/test/tools/dsymutil/Inputs/reflection_metadata.yaml` | YAML 1.2.2 §10.3.2 / §7.3.2 + `libyaml` (`RTP8`, `UT92`) | Events & hex/quoted scalars independent; `.json` encodes `!minidump`, 105-bit exact `u128`, >128-bit `f64` overflow, and leading-zero string rule (`docs/compatibility.md`) |

`quoted-delete-character.yml` holds a raw U+007F inside the third symbol's
quoted name, and so does its `.json` file. Its `.oracle.json` escapes that
byte as `\x7f` in `expected_events`.

These fixtures sit outside `tests/fixtures`, so the conformance fixture loops
don't pick them up.

Licenses: rust-lang/stdarch is MIT OR Apache-2.0; llvm-project is Apache-2.0
WITH LLVM-exception. The fixtures reproduce only the structure and a few
identifiers of their sources.
