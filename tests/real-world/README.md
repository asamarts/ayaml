# Real-world fixtures

Minimal fixtures reduced from public YAML files. Each `.yml` file keeps the
shape of its source, and its `.json` file holds the documents ayaml composes as
serde_json text. `crates/ayaml/tests/real_world.rs` checks every fixture:

- the golden documents;
- the reference implementation against a listed expectation;
- the typed path, the document cursor, and the compatibility path;
- value and event round trips.

| Fixture | Source shape (public path) | ayaml reading |
| --- | --- | --- |
| `compact-nested-sequence` | rust-lang/stdarch, `crates/stdarch-gen-arm/spec/{neon,sve,sve2}/*.spec.yml` | nested compact sequence entries remain separate |
| `repeated-document-end` | llvm-project, `llvm/test/ObjectYAML/MachO/DWARF-debug_info.yaml` | repeated `...` does not create a null document |
| `quoted-delete-character` | llvm-project, `llvm/test/tools/llvm-dlltool/Inputs/llvm_foo_dll_{1,3}.yaml` | quoted DEL is accepted |
| `wide-integer-content` | llvm-project, `lldb/test/Shell/{Minidump,Commands}/*.yaml` and `llvm/test/tools/dsymutil/Inputs/reflection_metadata.yaml` | wide integers remain exact |
| `empty-file` | llvm-project, `llvm/test/tools/llvm-remarkutil/size-diff/Inputs/empty-file.yaml` | an empty file has no documents |

`quoted-delete-character.yml` holds a raw U+007F inside the third symbol's
quoted name, and so does its `.json` file.

These fixtures sit outside `tests/fixtures`: the fixture loops in
`ayaml-conformance` and `ayaml-diff` walk that tree and require the reference
implementation to accept every file that is not an expected error.

Licenses: rust-lang/stdarch is MIT OR Apache-2.0; llvm-project is Apache-2.0
WITH LLVM-exception. The fixtures reproduce only the structure and a few
identifiers of their sources.
