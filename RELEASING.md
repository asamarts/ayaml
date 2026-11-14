# Releasing ayaml

A release is published by [`.github/workflows/release.yml`](.github/workflows/release.yml)
when a maintainer pushes a `vMAJOR.MINOR.PATCH` tag, and no one publishes from a workstation.
0.1.0 and 0.1.1 were published by hand, before this workflow, and the crates.io API tokens that
could publish them still exist until step 3 of the one-time setup revokes them.

## What the workflow binds together

1. **Gate.** The whole of `ci.yml` runs on the tag's commit, as it does on a pull request.
2. **Tag.** The package job scans the annotated tag message and name with the leak gate.
   `tools/check_release.py tag` checks that the tag is annotated and names
   `vMAJOR.MINOR.PATCH`, that the checkout is that commit and is clean, that the commit is on
   `main`, that the package declares the tag's version, and that `CHANGELOG.md` has a non-empty
   section headed `## [VERSION] — YYYY-MM-DD`.
3. **Package.** `cargo package`, with the toolchain that `RELEASE_TOOLCHAIN` pins, and then
   `tools/check_release.py package`. The package must hold exactly the commit's files under
   `crates/ayaml`, the workspace `README.md` and `CHANGELOG.md`, and what cargo generates,
   and its `.cargo_vcs_info.json` must name the commit and no dirty state.
4. **Publish.** Only for a tag pushed to `asamarts/ayaml`, in the `release` environment.
   The job packages the tag again and checks that the package's digest is step 3's.
   crates.io Trusted Publishing then mints a token that lasts minutes and is revoked when the
   job ends, and `cargo publish` runs. A version crates.io already has isn't published again.
   Neither command builds the package to verify it, as they would by default: that runs the
   dependencies' build scripts, and this job holds the credentials that mint the token. Step 3
   built the same bytes without them.
5. **Registry.** `tools/check_release.py registry` waits up to ten minutes for the sparse index
   to list the version. It then checks that the index's checksum and the bytes crates.io serves
   are step 3's package.
6. **Release.** The crate is downloaded from crates.io and checked against step 3's digest,
   and its SHA256SUMS line is written. `actions/attest-build-provenance` then signs a SLSA
   build provenance v1 attestation for it through Sigstore, and the attestation is verified
   with the command the release notes give. Last, `gh release create --verify-tag` attaches
   the crate and SHA256SUMS. The notes are the changelog's section and a verification record:
   the digest, repository, ref, commit, signing workflow, certificate identity, and issuer.

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
   token that could.
4. **Immutable releases**, optional: under Settings → General → Releases. A published
   release's assets and tag can then no longer change. `gh release create` keeps the release
   a draft until its assets are uploaded, so the workflow works either way. *On since
   2026-11-07. Releases published before then aren't affected.*

## Cutting a release

1. In a pull request, set the workspace version in `Cargo.toml`, rename the changelog's
   `## [Unreleased]` section to `## [X.Y.Z] — YYYY-MM-DD`, and refresh `Cargo.lock`. CI is
   the gate, as for any change. Change `RELEASE_TOOLCHAIN` only in this pull request, never
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
  Yank the version with `cargo yank --version X.Y.Z ayaml`, record why in the changelog, and
  release the next patch version. crates.io never lets a version be published twice.
- **The release job fails.** Rerun it. It downloads the crate from crates.io and attests it
  again, which adds a second attestation for the same digest. If the release exists, the job
  checks that it holds exactly the crate and SHA256SUMS, with these bytes, and changes nothing.
  `gh` creates a release as a draft, uploads its assets, and then publishes it, so a run stopped
  in between can leave a draft. One that holds both assets with these bytes is published. One
  that holds anything else fails the job: delete the draft and rerun the job. If gh can't say
  whether the release is there, which isn't the same as gh finding none, the job changes nothing
  and fails. Rerun it.

## Verifying a release

Each release's notes give its digest and the exact command. For version X.Y.Z, from the
release's assets or from crates.io:

```sh
curl -sSfLO https://static.crates.io/crates/ayaml/ayaml-X.Y.Z.crate
gh release download vX.Y.Z -R asamarts/ayaml -p SHA256SUMS
sha256sum --check SHA256SUMS
gh attestation verify ayaml-X.Y.Z.crate --repo asamarts/ayaml \
  --cert-identity https://github.com/asamarts/ayaml/.github/workflows/release.yml@refs/tags/vX.Y.Z \
  --cert-oidc-issuer https://token.actions.githubusercontent.com \
  --source-ref refs/tags/vX.Y.Z --source-digest COMMIT \
  --deny-self-hosted-runners
```

`COMMIT` is the commit the release notes name, which `git rev-parse vX.Y.Z^{commit}` also
gives. The attestation's predicate is `https://slsa.dev/provenance/v1`, `gh`'s default.

To rebuild the package from the tag and compare it with crates.io, from a checkout of `main`,
whose `tools/check_release.py` a release before 0.1.2 doesn't have:

```sh
git worktree add ../ayaml-vX.Y.Z vX.Y.Z
(cd ../ayaml-vX.Y.Z && cargo +1.99.1 package -p ayaml --locked)   # that tag's RELEASE_TOOLCHAIN
crate=../ayaml-vX.Y.Z/target/package/ayaml-X.Y.Z.crate
python3 tools/check_release.py package "$crate" "$(git rev-parse 'vX.Y.Z^{commit}')"
python3 tools/check_release.py registry "$crate" X.Y.Z 0
```

Releases up to 0.1.1 were published by hand, before this workflow, so they have no
attestation. Rebuilt with cargo 1.99.1, the 0.1.1 tag gives the bytes crates.io serves, sha256
`3758e32658d9b5ad8a75bfd1593e55130e93a82a95088660c64f9ac93aa8533c`.
