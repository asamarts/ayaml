# Aplan A2-009 ayaml-side unblocking plan

**Recorded:** 2026-11-04  
**Ayaml release audited:** `v0.1.1`, commit
`18e0203581cc2c3b1882da2369c1c7f448f724d2`  
**Aplan revision audited:** public `main` at `817e603`  
**Status:** implementation is tracked by the `M-v0.1.2` cut under
`aplan/tasks/`. This document records requirements and evidence, not task
status.

## Decision

Do not qualify ayaml 0.1.1 as the final backend for aplan A2-009. The public
release clears the old licensing, publication, Rust-version, API-fit, safe-Rust,
and most semantic blockers, but one definite typed-error gap and two narrower
compatibility/diagnostic gaps remain. Close those gaps in ayaml, harden the
release chain, publish an immutable patch release, and then hand the exact
release back to aplan for its product-level cutover.

The target is ayaml 0.1.2. The work is backward-compatible and does not justify
a new minor version. Default YAML 1.2.2 semantics remain the default. Libyaml
quirks remain opt-in through `ayaml::compat`; enabling the feature must not
change root entry points.

## What the public release already satisfies

The 0.1.1 release is a valid basis for a patch rather than another parser
qualification from scratch:

- crates.io publishes an unyanked `ayaml 0.1.1` with `MIT OR Apache-2.0` and
  `rust-version = 1.88`;
- the crates.io package, GitHub release asset, and registry record share SHA-256
  `3758e32658d9b5ad8a75bfd1593e55130e93a82a95088660c64f9ac93aa8533c`;
- `.cargo_vcs_info.json` names commit `18e0203`, and the packaged source and
  tests match that tag;
- the runtime graph contains only `serde`, `indexmap`, and their support crates,
  with no third-party YAML parser, native parser, C/C++, or FFI;
- ayaml-owned source forbids unsafe code;
- Rust 1.88 and stable checks, tests, Clippy, docs, package smoke, minimal
  versions, semver, dependency policy, macOS, Windows, and cross-target checks
  passed on the release commit;
- independent local validation passed 359 tests and doctests;
- strict default conformance passed all 405 classifications and every tree,
  JSON, event, stream, and value check;
- compatibility conformance had only the 13 committed deviations, with no
  unexpected or stale deviation; and
- the historical aplan blockers for unit-enum serialization, custom tags,
  empty typed configuration, YAML 1.2 scalar resolution, duplicate locations,
  value access, mapping lookup, and deterministic emission are fixed.

These facts should be retained as the 0.1.2 baseline. The patch should add
evidence only for the deltas below, then replay the complete release gate.

## Current aplan integration evidence

A clean public aplan worktree at `817e603` changed only the dependency boundary:

```toml
yaml_compat = { package = "ayaml", version = "=0.1.1" }
```

The resolver-only pin for the earlier bridge was removed. No Rust caller was adapted.
The result was:

- workspace check with all targets and a locked graph: pass;
- standalone TUI evaluator: 59 passed, 0 failed;
- normal product graph: `ayaml` present, the earlier bridge absent;
- complete nextest inventory: 3,611 passed, 18 failed, 8 skipped.

The 18 failures are bounded:

| Class | Failed tests | Disposition |
| --- | ---: | --- |
| Unseparated alias key | 4 | Ayaml compatibility work and an explicit syntax disposition |
| Nested Serde path | 1 | Ayaml defect; must be fixed before handoff |
| Diagnostic contract | 7 | One ayaml context improvement plus aplan-side normalization or explicit wording dispositions |
| Migration bookkeeping | 6 | Aplan-only dependency policy and generated-file work |

The full failing inventory was:

- `close_gate::initial_close_refuses_alternate_evidence_keys_without_writing`;
- `close::tests::initial_close_refuses_alternate_evidence_keys_before_candidate_construction`;
- `close::tests::initial_close_refuses_empty_version_fields_before_candidate_construction`;
- `version_record::empty_version_fields_are_refused_without_writing`;
- `blueprint::tests::unknown_fields_name_the_accepted_set_at_each_level`;
- `cascade::the_parse_error_says_what_to_do_about_it`;
- `load::tests::an_unquoted_colon_is_named_as_such_and_the_field_identified`;
- `cli::cli_corpus`;
- `coverage::the_rule_catalogue_is_committed_and_current`;
- `capture::tests::loader_refuses_noncanonical_members_duplicate_yaml_and_id_disagreement`;
- `yaml_contract::duplicate_keys_and_trailing_documents_are_rejected_with_locations`;
- `validation::tests::validation_inventory_rejects_duplicate_and_unknown_entries`;
- four validation-selection tests made stale by `Cargo.lock`; and
- two dependency-audit tests that still require the old two-layer bridge.

The last six are expected cutover bookkeeping. They do not indicate an ayaml
defect. Duplicate-key and trailing-document wording also remain precise and
located; aplan must deliberately update or normalize those assertions rather
than ask ayaml to imitate every historical sentence.

## U1 nested Serde paths

### U1 finding

For a nested blueprint typo, ayaml 0.1.1 reports:

```text
unknown field `efort`, expected `title` or `effort` at line 9 column 5
```

The bridge included the typed path, such as `tasks[0]`. Aplan uses that path to
select the schema for the nested flattened task entry. Without it, aplan offers
the blueprint's top-level fields instead of the valid task fields. The source
location and immediate Serde expectation are correct, but the missing path
makes the product remedy wrong.

### U1 implementation

- Carry the typed path through struct fields, sequence indices, maps, enum
  variants, newtypes, and flattened structs.
- Prefix or structurally expose the path on Serde errors without changing
  `ErrorKind::Serde`.
- Preserve the primary source span of the field or value that caused the error.
- Apply the behavior to owned, borrowed, seeded, root, and compatibility typed
  entry points.
- Avoid path duplication when a nested deserializer propagates an existing
  ayaml error.
- Define deterministic path rendering for identifiers, sequence indices, and
  mapping keys that are not identifier-like.

### U1 tests

- Unknown field in a sequence of structs reports `tasks[0]`.
- Unknown field in a flattened nested struct retains the outer path.
- Type error in a nested sequence reports both field and index.
- Nested map, tuple, enum, option, and newtype failures retain their path.
- Root-level errors do not gain a spurious prefix.
- Root and `ayaml::compat` paths agree unless a documented compatibility
  behavior changes the parse before Serde runs.
- Location assertions name the offending token, not merely the document start.

The aplan blueprint regression must pass without changing its accepted-field
assertions.

## U2 unseparated alias keys in compatibility mode

### U2 finding

The current bridge accepts this alternate spelling:

```yaml
owner: &evidence_key evidence
*evidence_key: []
```

Ayaml 0.1.1, in both default and compatibility entry points, returns
`unexpected content after document`. The separated form
`*evidence_key : []` works. YAML's anchor-name grammar makes the unseparated
colon ambiguous, so this must not silently broaden default YAML 1.2.2 mode.
It is nevertheless observable libyaml behavior already exercised by aplan's
field-integrity tests.

### U2 implementation

- Teach only the libyaml-compatible scanner/parser path to terminate an alias
  name at a mapping-value colon in an implicit block key where libyaml does.
- Keep root/default parsing unchanged.
- Ensure the alias resolves to the anchored scalar before mapping-key duplicate
  or typed-field handling.
- Preserve unknown-anchor, immediate-cycle, alias-dereference, expansion-byte,
  event, depth, and input limits.
- Record the behavior in the compatibility documentation and deviation list if
  it changes a conformance classification.
- Do not change root functions when `libyaml-compat` is merely enabled.

### U2 tests

- Generic compatibility parsing accepts the exact aplan `evidence` and
  `version` fixtures.
- Typed compatibility parsing reaches duplicate-field handling rather than a
  document-level parse error.
- An alias resolving to an existing generic key is rejected as a duplicate at
  the expected location.
- The separated standards form continues to work in both modes.
- Default mode's decision for the unseparated form is pinned explicitly.
- Flow mappings, explicit keys, unknown aliases, aliases whose names contain
  otherwise legal punctuation, and limit boundaries do not regress.

Aplan must still opt into the compatibility entry point at its narrow YAML
boundary. Ayaml must not turn a feature-unification event into a global semantic
mode change.

## U3 flow diagnostic context

### U3 finding

For a malformed flow sequence, the bridge and ayaml compatibility mode report
both the failure and the opening collection location:

```text
did not find expected ',' or ']' at line 3 column 6,
while parsing a flow sequence at line 2 column 8
```

The ayaml root path reports only:

```text
expected ',' or ']' at line 3 column 6
```

The primary span is correct, but removing the opening context weakens an
existing aplan CLI diagnostic. Ayaml already computes equivalent context in
the private compatibility error mapper.

### U3 implementation

- Refactor flow-error context discovery so root and compatibility errors can
  name the opening collection without sharing parsing semantics.
- Preserve the failure span as the error's primary location.
- Add the opening line and column to display output for unterminated or malformed
  flow sequences and mappings.
- Keep messages deterministic across LF, CRLF, BOM-prefixed input, multibyte
  UTF-8 before the opener, and nested flow collections.
- Do not turn human wording into a stable public code contract in this patch.
  A future structured secondary-span API belongs with the diagnostics roadmap.

### U3 tests

- Malformed flow sequence and flow mapping name both locations.
- Nested flow errors name the innermost relevant opener.
- Quoted `[` and `{` bytes are not mistaken for an opener.
- CRLF and multibyte input produce correct one-based positions.
- Compatibility wording and default wording may differ, but neither may lose
  the opening context.

The unquoted-colon remedy remains aplan-owned. Ayaml's default
`unexpected token in mapping value` and compatibility
`mapping values are not allowed in this context` are both sufficiently typed
and located for the consumer to normalize.

## U4 release provenance and workflow hardening

### U4 finding

The 0.1.1 package has a consistent checksum, tag, VCS record, release asset,
and hosted CI run. That is adequate source integrity, but the publication path
is manual and not independently authenticated:

- no GitHub artifact attestation exists for the `.crate` asset;
- crates.io publication is not performed by an OIDC Trusted Publisher workflow;
- workflow dependencies use mutable tags, `master`, or `main`; and
- the public repository has no release workflow binding tag, package, publish,
  release asset, and provenance into one run.

### U4 implementation

- Pin every action in CI, fuzz, and release workflows to a full commit SHA,
  retaining a version comment for maintainability.
- Add a least-privilege release workflow with GitHub OIDC and crates.io Trusted
  Publishing. Do not add or retain a long-lived crates.io token.
- Validate that the tag is `v<workspace-version>`, points at the checked-out
  commit, and names a version not already published.
- Run the complete release gate before publication, including MSRV, stable,
  all features, conformance, dependency policy, package smoke, and semver.
- Produce the package, record its SHA-256 and VCS identity, and verify packaged
  source against the tag.
- Publish, wait for the exact sparse-index record, download the registry crate,
  and compare its digest with the release asset.
- Attach the matching `.crate` and checksums to the GitHub release.
- Generate a GitHub artifact attestation using the SLSA provenance v1 predicate
  for the exact release asset.
- Record the workflow, issuer, repository, ref, commit, artifact name, and
  digest needed by a consumer to verify the attestation.
- Update required checks when workflow job names change.
- Document the operator steps and recovery boundaries in `RELEASING.md` or the
  equivalent maintained release document.

The release workflow must not publish from pull requests, forks, mutable branch
heads, or an unreviewed dirty checkout. A failed post-publish GitHub-release
step cannot roll crates.io back, so reruns must reuse and verify the immutable
registry bytes rather than rebuild a replacement.

## U5 qualify and publish ayaml 0.1.2

The patch release is the integration point for U1 through U4. It must not be a
version bump wrapped around targeted tests only.

### Required local evidence

- Formatting, all-target Clippy with warnings denied, all-feature tests,
  doctests, and rustdoc pass on Rust 1.88 and stable.
- Default and compatibility conformance run in strict mode with no unexpected
  or stale deviation.
- Release-mode adversarial tests and all public limit-boundary tests pass.
- Property tests run at the release case count.
- Parse, emit, and borrowed fuzz targets complete an explicit campaign on the
  release commit; any new differential target covering U1/U2 runs too.
- The pre-release comparison gates report every reading change, with an
  explicit disposition for expected compatibility-only changes.
- The quiet-host parser, emitter, allocation, and peak-heap gates pass from
  the release candidate.
- `cargo package --locked` and downstream smoke use only packaged files.
- The package contains both licenses, notice, readme, changelog, repository
  metadata, and the intended source/tests.

### Required hosted evidence

- Linux Rust 1.88 and stable quality jobs pass.
- macOS and Windows tests/docs pass.
- Cross-target checks, minimal versions, semver, cargo-deny, dependency
  classification, unsafe census, secret scan, package/downstream smoke, and
  strict conformance pass on the same public commit.
- The scheduled fuzz workflow is manually dispatched for the release commit if
  no scheduled run covers it.
- The release workflow publishes and attests the exact tag after all required
  jobs pass.

### Release record

Record in the changelog and the release record:

- the nested-path behavior and proof tests;
- the exact scope of the alias-key compatibility extension;
- the flow-context diagnostic improvement;
- any conformance deviation-list change;
- benchmark and corpus dispositions;
- tag, full commit, crates.io checksum, GitHub asset checksum, attestation
  verification command, CI run, docs.rs result, and publication time; and
- confirmation that 0.1.1 remains immutable and is not yanked merely because a
  patch exists.

## Work that remains in aplan, not ayaml

Do not expand ayaml to absorb consumer-specific cutover work. After 0.1.2,
aplan still owns:

- selecting `ayaml::compat` behind its logical `yaml_compat` boundary;
- recognizing both old and new unquoted-colon errors and producing the
  task-author remedy;
- deciding its public wording for duplicate mapping keys and trailing
  documents without weakening category/location assertions;
- exact-pinning the release and removing the earlier bridge's resolver pin;
- updating dependency audits, license/source policy, both lockfiles, validation
  selection, generated rule catalogue, design notes, and changelog;
- running its fixture differential and every hostile boundary through real
  product entry points;
- measuring default/headless binary size and parser-heavy product latency; and
- passing complete local and GitHub-hosted profiles from one aplan commit.

Ayaml may provide fixtures and release evidence for those steps, but it must not
gain a permanent checkout of aplan, aplan-specific field knowledge, or a hidden
global compatibility mode.

## Sequence and task mapping

| Order | Aplan task | Deliverable |
| ---: | --- | --- |
| 1 | `AY-001` | Nested typed-error paths across every typed entry point |
| 1 | `AY-002` | Libyaml-compatible unseparated alias keys, default unchanged |
| 1 | `AY-003` | Flow errors retain opening collection context |
| 1 | `AY-004` | SHA-pinned CI plus trusted, attested release automation |
| 2 | `AY-005` | Full 0.1.2 qualification, publication, and evidence handoff |

`AY-001` through `AY-004` can proceed independently. `AY-005` waits for all
four. `M-v0.1.2` closes only when the patch is published and every member has
test evidence.

## Handoff criteria for aplan A2-009

Ayaml is ready to unblock the consumer task when all of these are true:

- 0.1.2 is immutable, licensed, checksummed, Rust 1.88-compatible, and
  attestable to its public tag and release workflow;
- nested typed errors retain the field/index path and source location;
- the unseparated alias-key behavior has an explicit default/compat decision
  and executable tests;
- malformed flow collections retain their opening context;
- no unexpected conformance, corpus, fuzz, limit, performance, or memory
  regression remains;
- all local and hosted ayaml gates pass on the tag commit; and
- the handoff names exact commands and immutable identifiers rather than a
  branch or sibling checkout.

Rollback before publication is a normal patch revision. After publication,
0.1.2 is immutable: a defect produces a later patch and aplan remains on its
last qualified exact pin.
