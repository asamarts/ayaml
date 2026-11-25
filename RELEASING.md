# Releasing ayaml

A release is published by [`.github/workflows/release.yml`](.github/workflows/release.yml)
when a maintainer pushes a `vMAJOR.MINOR.PATCH` tag, and no one publishes from a workstation.
0.1.0 and 0.1.1 were published by hand, before this workflow. Since step 3 of the one-time
setup, no crates.io API token can publish the crate.

## What the workflow binds together

1. **Gate.** The whole of `ci.yml` runs on the tag's commit, as it does on a pull request. That
   includes the public API comparison (`tools/check_public_api.py` for both `ayaml` and
   `ayaml-edit`), which builds listings of the public API for the default crate, `libyaml-compat`,
   and `ayaml-edit` with the pinned `public-api` tool (`tools/install-public-api.sh`) and its
   paired nightly toolchain, and compares them against the last released `vMAJOR.MINOR.PATCH` tag
   before `HEAD`. Any removed item or changed signature, trait implementation, or associated type
   fails the gate unless the active `### Changed` section of `CHANGELOG.md` names it. The
   `public-api` archive version/checksum and its nightly toolchain move together by hand, like
   `cargo-semver-checks` and its toolchain.
2. **Tag.** The package job scans the annotated tag message and name with the leak gate.
   `tools/check_release.py tag` checks that the tag is annotated and names
   `vMAJOR.MINOR.PATCH`, that the checkout is that commit and is clean, that the commit is on
   `main`, that both `crates/ayaml` and `crates/ayaml-edit` declare the tag's version (with
   `ayaml-edit` pinning `ayaml = "=VERSION"`), and that `CHANGELOG.md` has a non-empty
   section headed `## [VERSION] — YYYY-MM-DD`.
3. **Package.** `cargo package`, with the toolchain that `RELEASE_TOOLCHAIN` pins, and then
   `tools/check_release.py package` for both `ayaml` and `ayaml-edit`. Each package must hold
   exactly the commit's files under its crate directory (plus the workspace `README.md` for
   `ayaml`) and what cargo generates, and its `.cargo_vcs_info.json` must name the commit and no
   dirty state. The unpacked `ayaml-edit` crate is compiled and tested against the freshly packaged
   `ayaml` crate without crates.io credentials.
4. **Publish.** Only for a tag pushed to `asamarts/ayaml`, in the `release` environment.
   The job packages both crates again and checks that each digest matches step 3's.
   crates.io Trusted Publishing mints a short-lived OIDC token and publishes `ayaml` (skipping if
   already published). After step 5 confirms `ayaml` is live on the sparse index, a fresh Trusted
   Publishing token is minted and `cargo publish -p ayaml-edit --locked --no-verify` publishes
   `ayaml-edit` (skipping if already published). Neither command builds the package to verify it:
   that runs dependencies' build scripts beside the credentials that mint the token, and step 3
   already built and tested the identical bytes without them.
5. **Registry.** `tools/check_release.py registry` waits up to ten minutes for the sparse index
   to list `ayaml` (before `ayaml-edit` publishes) and then `ayaml-edit`. For each crate, it checks
   that the index's checksum and the bytes crates.io serves match step 3's package.
6. **Release.** Both crates are downloaded from crates.io and checked against step 3's digests,
   and their `SHA256SUMS` lines are written. `actions/attest-build-provenance` then signs a SLSA
   build provenance v1 attestation for each `.crate` through Sigstore, and each attestation is
   verified with the command the release notes give. Last, `gh release create --verify-tag`
   attaches both `.crate` archives and `SHA256SUMS`. The notes hold the changelog's section and a
   verification record covering both artifacts.

The workflow never publishes from a pull request, a fork, a branch, or a dirty checkout. It
refuses a tag whose commit isn't on `main`. That check runs from the tagged commit's own
workflow and script, so it stops a mistake, not a tag on a commit that changes them: the
`release` environment's approval does. Before approving a publish, check that the run's commit
is on `main`. The tag ruleset "protect release tags" stops anyone from moving or deleting a `v*`
tag.

## One-time setup

These are done in the web interfaces by an owner. Each is recorded here once done.

1. **GitHub environment.** Create the environment `release` under Settings → Environments.
   Under deployment branches and tags, allow only tags matching `v*`. A required reviewer
   is optional. With one, the publish job waits for that person's approval. *Done on
   2026-11-07: only `v*` tags, and the owner approves each publish.*
2. **crates.io Trusted Publisher.** On crates.io, open the `ayaml` crate's Settings → Trusted
   Publishing and add a GitHub publisher:
   - repository owner `asamarts`;
   - repository `ayaml`;
   - workflow `release.yml`;
   - environment `release`.

   *Done on 2026-11-08: crates.io lists that publisher for `ayaml`.*
3. **After the first release through the workflow.** In the same settings, turn on "Trusted
   Publishing only", so that no API token can publish `ayaml`. Then revoke every crates.io API
   token that could. *Done on 2026-11-15: the crates.io API reports `trustpub_only: true` for
   `ayaml`, and legacy tokens can no longer publish it.*
4. **Immutable releases**, optional: under Settings → General → Releases. A published
   release's assets and tag can then no longer change. `gh release create` keeps the release
   a draft until its assets are uploaded, so the workflow works either way. *On since
   2026-11-07. Releases published before then aren't affected.*

## Cutting a release

1. In a pull request, set the workspace version in `Cargo.toml` and in each place that repeats
   it: the current release and the two install lines in `README.md`, the install lines in
   `docs/index.md`, `docs/getting-started.md`, `docs/compatibility.md`, `docs/migration.md`,
   `docs/editing.md`, and `docs/migrating-from/*.md`,
   and the exact `ayaml` / `ayaml-edit` / `ayaml-fixtures` requirements in
   `crates/ayaml-bench/Cargo.toml`, `crates/ayaml-conformance/Cargo.toml`,
   `crates/ayaml-edit/Cargo.toml`, `crates/ayaml-fixtures/Cargo.toml`,
   `crates/xtask/Cargo.toml`, and `fuzz/Cargo.toml`. A policy test fails while one of them
   disagrees. Rename `## [Unreleased]` to `## [VERSION] — YYYY-MM-DD` in `CHANGELOG.md` and in
   its copy under `crates/ayaml/CHANGELOG.md`, and refresh `Cargo.lock` and
   `fuzz/Cargo.lock`. Make sure any breaking or signature change since the last release tag is
   named in that release's `### Changed` section so the public API gate passes both on the release
   pull request and on the tag. For a minor release, say in `SECURITY.md` which versions get fixes.
   CI is the gate, as for any change. Change `RELEASE_TOOLCHAIN` only in this pull request, never
   between a tag and its rerun.
2. After the squash merge, tag the merge commit on `main` and push the tag:

   ```sh
   git fetch origin
   git tag -a vX.Y.Z -m "ayaml X.Y.Z" origin/main
   git push origin vX.Y.Z
   ```

3. Watch the run under Actions → release. With a required reviewer, approve the publish job
   when the gate and the package have passed and the run's commit is the one on `main`.

## Rehearsing

Run the workflow by hand against a published tag:

```sh
gh workflow run release.yml -R asamarts/ayaml -f tag=v0.1.1
```

The gate runs on the branch it was started from. The tag's checks and package run on the
tag, and the package is compared with the bytes crates.io serves for that version. Those bytes
then go into a draft release named `rehearsal-RUN-ATTEMPT`, with the release job's notes but
for the attestation record. The draft is checked as a rerun checks a release: exactly the crate
and SHA256SUMS, with these bytes, and the notes it was given. Then it's deleted (D77). A draft
makes no tag and only the repository's writers can see it. The job's last step deletes it even
when an earlier step failed or the run was cancelled, says what it did, and warns of any other
rehearsal's draft that is still there, as a run whose runner was lost leaves one. Delete such a
draft under Releases once no rehearsal is running.

Nothing is published or released. A rehearsal shows that `RELEASE_TOOLCHAIN` still rebuilds a
published crate byte for byte, and that `tools/github_release.sh`, which the release job runs
too, still downloads and checks the crate and, with a job's token, creates, checks, and deletes
a draft. Started from a pull request's branch, it rehearses that branch's script. A rehearsal
runs apart from releases, so it can't cancel one that waits for approval.

A rehearsal can't run the publish or the attestation, since each writes something public for
good. Nor does it run the script's `release` command: the create that gh publishes, and a
rerun's publishing of a leftover draft. They share the rehearsal's lookups and checks, and the
tests in `tools/test_ci_checks.py` run them against a fake gh. The first release is their first
run. If the release job then fails the same way on every rerun, the version is on crates.io with
no GitHub release, and since a rerun uses the tag's workflow, completing it takes the next patch
version.

## When a run fails

Every job can be rerun from the Actions page for 30 days after the run started. Rerun the failed
jobs, which keeps the jobs that passed and their outputs. Each later job rebuilds or downloads
what it needs and checks it against the package job's digest, so a rerun needs nothing stored.
Re-running all jobs runs the gate again, on toolchains and an advisory database that have moved
on since, so it can fail for reasons the tag doesn't have. After 30 days GitHub reruns nothing,
and the tag can't be pushed again. A version and a tag are never reused, so a release that
can't be completed becomes the next patch version.

- **The gate or the package job fails.** Nothing is published. If the cause is a flaky runner,
  rerun the failed jobs. If the tagged commit is wrong, leave the tag in place, fix `main`, and
  release the next patch version. The changelog says that the burnt version was never
  published.
- **The publish job fails before `cargo publish` uploads.** Rerun it. If Trusted Publishing
  refuses to mint a token, check the publisher's repository, workflow file name, and
  environment against the setup above.
- **The publish job fails after the upload**, for instance while it waits for the sparse
  index. Rerun it. It finds the version already published, skips the upload, and checks the
  registry's bytes again. crates.io can take a few minutes to serve a new version. The wait is
  ten minutes, and a rerun waits again.
- **crates.io serves other bytes than the package.** This shouldn't happen, because the job
  compares two packages of the tag before it publishes. If it does, don't create the release.
  Yank any published version (`cargo yank --version X.Y.Z ayaml`, and
  `cargo yank --version X.Y.Z ayaml-edit` if `ayaml-edit` was published too), record why in
  the changelog, and release the next patch version. crates.io never lets a version be
  published twice.
- **The release job fails.** Rerun it. It downloads the crate(s) from crates.io and attests
  them again, which adds a second attestation for each digest. If the release exists, the job
  checks that it holds exactly the crate(s) and SHA256SUMS, with these bytes, and changes
  nothing. `gh` creates a release as a draft, uploads its assets, and then publishes it, so a
  run stopped in between can leave a draft. One that holds the expected assets with these
  bytes is published. One that holds anything else fails the job: delete the draft and rerun
  the job. If gh can't say whether the release is there, which isn't the same as gh finding
  none, the job changes nothing and fails. Rerun it.

## Verifying a release

Each release's notes give its digest(s) and the exact command(s). For version X.Y.Z, from the
release's assets or from crates.io (include `ayaml-edit` for releases that ship it):

```sh
curl -sSfLO https://static.crates.io/crates/ayaml/ayaml-X.Y.Z.crate
curl -sSfLO https://static.crates.io/crates/ayaml-edit/ayaml-edit-X.Y.Z.crate
gh release download vX.Y.Z -R asamarts/ayaml -p SHA256SUMS
sha256sum --check SHA256SUMS
for crate in ayaml-X.Y.Z.crate ayaml-edit-X.Y.Z.crate; do
  gh attestation verify "$crate" --repo asamarts/ayaml \
    --cert-identity https://github.com/asamarts/ayaml/.github/workflows/release.yml@refs/tags/vX.Y.Z \
    --cert-oidc-issuer https://token.actions.githubusercontent.com \
    --source-ref refs/tags/vX.Y.Z --source-digest COMMIT \
    --deny-self-hosted-runners
done
```

`COMMIT` is the commit the release notes name, which `git rev-parse vX.Y.Z^{commit}` also
gives. The attestation's predicate is `https://slsa.dev/provenance/v1`, `gh`'s default.

To rebuild the package(s) from the tag and compare them with crates.io, from a checkout of
`main`, whose `tools/check_release.py` a release before 0.1.2 doesn't have:

```sh
git worktree add ../ayaml-vX.Y.Z vX.Y.Z
(cd ../ayaml-vX.Y.Z && cargo +1.99.1 package -p ayaml --locked)                 # that tag's RELEASE_TOOLCHAIN
(cd ../ayaml-vX.Y.Z && cargo +1.99.1 package -p ayaml-edit --locked --no-verify)
commit="$(git rev-parse 'vX.Y.Z^{commit}')"
crate=../ayaml-vX.Y.Z/target/package/ayaml-X.Y.Z.crate
edit_crate=../ayaml-vX.Y.Z/target/package/ayaml-edit-X.Y.Z.crate
python3 tools/check_release.py package "$crate" "$commit"
python3 tools/check_release.py registry "$crate" X.Y.Z 0
python3 tools/check_release.py package --package ayaml-edit "$edit_crate" "$commit"
python3 tools/check_release.py registry --package ayaml-edit "$edit_crate" X.Y.Z 0
```

Releases up to 0.1.1 were published by hand, before this workflow, so they have no
attestation. Rebuilt with cargo 1.99.1, the 0.1.1 tag gives the bytes crates.io serves, sha256
`3758e32658d9b5ad8a75bfd1593e55130e93a82a95088660c64f9ac93aa8533c`.

## Publishing `ayaml-edit`

`ayaml-edit` lives in `crates/ayaml-edit`, depends only on `ayaml`'s public API (`ayaml = "=X.Y.Z"`),
and is packaged, verified, published, attested, and attached to the GitHub release automatically by
[`.github/workflows/release.yml`](.github/workflows/release.yml) immediately after `ayaml` is live on
the crates.io sparse index.

### One-time initial bootstrap for `ayaml-edit`

crates.io only allows configuring a Trusted Publisher on a crate that already exists on crates.io
(it does not support pending Trusted Publishers for unregistered crate names). Therefore, the very
first publish of `ayaml-edit` requires a one-time owner bootstrap, after which no API token is ever
used again:

1. **Ensure the matching `ayaml X.Y.Z` version is live on crates.io.** When cutting the first
   release tag that includes `ayaml-edit`, the workflow's `publish` job publishes `ayaml X.Y.Z`
   via Trusted Publishing, verifies it on the sparse index, mints an OIDC token covering only
   crates that already trust `release.yml` (`ayaml`), and then stops on `Publish ayaml-edit`
   because `ayaml-edit` does not exist on crates.io yet.
2. **One-time initial publish from a clean tag checkout.** Create a temporary crates.io API token
   with the narrowest scope (`publish-new` only) and shortest expiry, check out the exact tagged
   commit with `RELEASE_TOOLCHAIN`, verify the packaged bytes match the workflow's `edit_digest`,
   and upload the initial `ayaml-edit` release once:

   ```sh
   git worktree add ../ayaml-bootstrap vX.Y.Z
   (cd ../ayaml-bootstrap && cargo +1.99.1 package -p ayaml-edit --locked --no-verify)
   sha256sum ../ayaml-bootstrap/target/package/ayaml-edit-X.Y.Z.crate   # must match package job's edit_digest
   (cd ../ayaml-bootstrap && CARGO_REGISTRY_TOKEN=... cargo +1.99.1 publish -p ayaml-edit --locked --no-verify)
   ```

3. **Configure Trusted Publishing and lock down token publishing immediately.** Open
   `https://crates.io/crates/ayaml-edit/settings` → Trusted Publishing:
   - Add a GitHub publisher matching `ayaml`:
     - repository owner `asamarts`;
     - repository `ayaml`;
     - workflow `release.yml`;
     - environment `release`.
   - Enable **"Trusted Publishing only"** (`trustpub_only: true`), so no API token can publish
     `ayaml-edit`.
   - **Revoke the temporary crates.io API token immediately.**
4. **Rerun the failed `publish` job in GitHub Actions.** Because `release.yml` checks
   `check_release.py published` for each crate before uploading, rerunning `publish` sees both
   `ayaml` and `ayaml-edit` already on crates.io, verifies both packages byte-for-byte against the
   sparse index and download CDN, and runs the `release` job to sign SLSA provenance attestations
   and publish the GitHub release for both crates.

Every subsequent release publishes both `ayaml` and `ayaml-edit` solely through crates.io Trusted
Publishing with no workstation publish and no stored tokens.
