#!/usr/bin/env python3
"""Checks that bind a release together: its tag, version, commit, package, registry bytes, and asset.

usage:
  tools/check_release.py tag TAG
      HEAD is the commit of TAG, an annotated tag; the checkout is clean; the commit is on
      origin/main; the package's version is TAG's; and CHANGELOG.md has a section for it.
  tools/check_release.py package CRATE COMMIT
      CRATE, as `cargo package` wrote it, holds exactly COMMIT's files of the package's
      directory, the workspace README and CHANGELOG, and what cargo generates, and it names COMMIT.
  tools/check_release.py published VERSION
      Exits 0 if crates.io has VERSION, 1 if it doesn't, and 2 for any other answer, an error
      in this script included.
  tools/check_release.py registry CRATE VERSION [SECONDS]
      Waits up to SECONDS, 300 by default, for the sparse index to list VERSION, then checks
      that its checksum and the bytes crates.io serves are CRATE's.
  tools/check_release.py notes VERSION
      Prints the CHANGELOG.md section for VERSION.
  tools/check_release.py record TAG COMMIT DIGEST [ATTESTATION_URL]
      Prints what a consumer needs to verify the release's attestation: the workflow, issuer,
      repository, ref, commit, artifact name, and digest, and the command that checks them.

Each check says what it compared, and exits 1 on the first difference.
"""

from __future__ import annotations

import hashlib
import http.client
import io
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tarfile
import time
import tomllib
from typing import Callable
import urllib.error
import urllib.request

PACKAGE = "ayaml"
PACKAGE_DIR = "crates/ayaml"
REPOSITORY = "asamarts/ayaml"
WORKFLOW = ".github/workflows/release.yml"
ISSUER = "https://token.actions.githubusercontent.com"
# What the package holds from outside its directory, by packaged name.
FROM_WORKSPACE = {"README.md": "README.md"}
# What cargo writes rather than copies. The manifest it copies is packaged as Cargo.toml.orig.
GENERATED = {".cargo_vcs_info.json", "Cargo.toml", "Cargo.lock"}
TAG = re.compile(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
USER_AGENT = "ayaml-release-check (https://github.com/asamarts/ayaml)"
INDEX = f"https://index.crates.io/{PACKAGE[:2]}/{PACKAGE[2:4]}/{PACKAGE}"
DOWNLOAD = "https://static.crates.io/crates/{name}/{name}-{version}.crate"
VERSION_API = "https://crates.io/api/v1/crates/{name}/{version}"


class Refused(Exception):
    """A check found a difference."""


def git(root: Path, *args: str) -> str:
    done = subprocess.run(["git", "-C", str(root), *args], capture_output=True, text=True)
    if done.returncode != 0:
        raise Refused(f"git {' '.join(args)} failed: {done.stderr.strip()}")
    return done.stdout.strip()


def git_blob(root: Path, commit: str, path: str) -> bytes:
    done = subprocess.run(["git", "-C", str(root), "show", f"{commit}:{path}"], capture_output=True)
    if done.returncode != 0:
        raise Refused(f"{path} isn't in {commit}")
    return done.stdout


def git_has_path(root: Path, commit: str, path: str) -> bool:
    """Whether COMMIT's tree contains PATH."""
    done = subprocess.run(
        ["git", "-C", str(root), "cat-file", "-e", f"{commit}:{path}"],
        capture_output=True,
    )
    return done.returncode == 0


def package_version(root: Path) -> str:
    manifest = tomllib.loads((root / PACKAGE_DIR / "Cargo.toml").read_text(encoding="utf-8"))
    version = manifest["package"]["version"]
    if isinstance(version, dict) and version.get("workspace") is True:
        workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
        version = workspace["workspace"]["package"]["version"]
    if not isinstance(version, str):
        raise Refused(f"{PACKAGE_DIR}/Cargo.toml has no version")
    return version


def changelog_section(text: str, version: str) -> str:
    """The body of the section headed `## [VERSION] — DATE`, which must hold something."""
    heading = re.compile(rf"^## \[{re.escape(version)}\] (?:—|-) [0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}}$", re.M)
    found = heading.search(text)
    if found is None:
        raise Refused(f"CHANGELOG.md has no section headed `## [{version}] — YYYY-MM-DD`")
    rest = text[found.end() :]
    following = re.search(r"^## ", rest, re.M)
    body = (rest[: following.start()] if following else rest).strip()
    if not body:
        raise Refused(f"CHANGELOG.md's section for {version} is empty")
    return body


def check_tag(root: Path, tag: str) -> str:
    if TAG.fullmatch(tag) is None:
        raise Refused(f"{tag!r} isn't a release tag, which is vMAJOR.MINOR.PATCH")
    version = tag[1:]
    if git(root, "cat-file", "-t", f"refs/tags/{tag}") != "tag":
        raise Refused(f"{tag} is a lightweight tag; a release tag is annotated")
    commit = git(root, "rev-parse", f"refs/tags/{tag}^{{commit}}")
    head = git(root, "rev-parse", "HEAD")
    if head != commit:
        raise Refused(f"the checkout is at {head}, and {tag} is {commit}")
    status = git(root, "status", "--porcelain", "--untracked-files=all")
    if status:
        raise Refused(f"the checkout isn't clean:\n{status}")
    on_main = subprocess.run(
        ["git", "-C", str(root), "merge-base", "--is-ancestor", commit, "refs/remotes/origin/main"],
        capture_output=True,
    )
    if on_main.returncode != 0:
        raise Refused(f"{commit} isn't on origin/main")
    declared = package_version(root)
    if declared != version:
        raise Refused(f"{tag} names {version}, and {PACKAGE_DIR} declares {declared}")
    changelog_section((root / "CHANGELOG.md").read_text(encoding="utf-8"), version)
    print(f"{tag}: annotated, commit {commit} on origin/main, clean, version {version}, in CHANGELOG.md")
    return commit


def crate_files(crate: bytes) -> tuple[str, dict[str, bytes]]:
    """The package's version and its files by path, refusing anything but plain files in one directory."""
    files: dict[str, bytes] = {}
    top: str | None = None
    with tarfile.open(fileobj=io.BytesIO(crate), mode="r:gz") as archive:
        for member in archive.getmembers():
            path = PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts or len(path.parts) < 2:
                raise Refused(f"the package holds {member.name!r}, outside its directory")
            if top is None:
                top = path.parts[0]
            if path.parts[0] != top:
                raise Refused(f"the package holds {member.name!r}, outside {top}")
            if not member.isfile():
                raise Refused(f"the package holds {member.name!r}, which isn't a plain file")
            inner = str(PurePosixPath(*path.parts[1:]))
            if inner in files:
                raise Refused(f"the package holds {inner} twice")
            extracted = archive.extractfile(member)
            assert extracted is not None
            files[inner] = extracted.read()
    prefix = f"{PACKAGE}-"
    if top is None or not top.startswith(prefix):
        raise Refused(f"the package's directory is {top!r}, not {prefix}VERSION")
    return top[len(prefix) :], files


def check_package(root: Path, crate_path: Path, commit: str) -> str:
    crate = crate_path.read_bytes()
    version, files = crate_files(crate)
    if crate_path.name != f"{PACKAGE}-{version}.crate":
        raise Refused(f"{crate_path.name} holds {PACKAGE}-{version}")
    vcs = json.loads(files.get(".cargo_vcs_info.json", b"null"))
    wanted = {"git": {"sha1": commit}, "path_in_vcs": PACKAGE_DIR}
    if vcs != wanted:
        raise Refused(f".cargo_vcs_info.json is {vcs}, not {wanted}")
    document = tomllib.loads(files["Cargo.toml"].decode("utf-8"))
    manifest = document["package"]
    if (manifest.get("name"), manifest.get("version")) != (PACKAGE, version):
        raise Refused(f"the packaged manifest names {manifest.get('name')} {manifest.get('version')}")
    # Packages built after the self-contained package layout carry their own
    # changelog in the crate tree. Older published tags predate it, and their
    # packages remain valid release evidence for a rehearsal.
    new_layout = git_has_path(root, commit, f"{PACKAGE_DIR}/CHANGELOG.md")
    if new_layout:
        dependencies = document.get("dependencies", {})
        for dependency in ("indexmap", "serde"):
            requirement = dependencies.get(dependency, {}).get("version")
            if not isinstance(requirement, str) or requirement.startswith("="):
                raise Refused(f"the packaged manifest doesn't give {dependency} a caret requirement")
        docs = manifest.get("metadata", {}).get("docs", {}).get("rs", {})
        if "libyaml-compat" not in docs.get("features", []):
            raise Refused("the packaged manifest doesn't document libyaml-compat on docs.rs")
    expected: dict[str, bytes] = {}
    listing = git(root, "ls-tree", "-r", "--full-tree", commit, "--", PACKAGE_DIR)
    for line in listing.splitlines():
        meta, path = line.split("\t", 1)
        mode = meta.split()[0]
        if mode not in ("100644", "100755"):
            raise Refused(f"{path} in {commit} has mode {mode}, which cargo wouldn't package as written")
        inner = path[len(PACKAGE_DIR) + 1 :]
        expected["Cargo.toml.orig" if inner == "Cargo.toml" else inner] = git_blob(root, commit, path)
    for packaged, source in FROM_WORKSPACE.items():
        expected[packaged] = git_blob(root, commit, source)
    if new_layout:
        crate_changelog = git_blob(root, commit, f"{PACKAGE_DIR}/CHANGELOG.md")
        workspace_changelog = git_blob(root, commit, "CHANGELOG.md")
        if crate_changelog != workspace_changelog:
            raise Refused(
                f"{PACKAGE_DIR}/CHANGELOG.md and CHANGELOG.md differ; "
                "the workspace and packaged changelogs must match"
            )
    copied = set(files) - GENERATED
    problems = [f"missing {path}" for path in sorted(expected.keys() - copied)]
    problems += [f"not in {commit}: {path}" for path in sorted(copied - expected.keys())]
    problems += [f"differs from {commit}: {path}" for path in sorted(copied & expected.keys()) if files[path] != expected[path]]
    if problems:
        raise Refused("the package doesn't match the commit:\n  " + "\n  ".join(problems))
    digest = hashlib.sha256(crate).hexdigest()
    print(f"{crate_path.name}: {len(copied)} files as in {commit}, {len(GENERATED)} generated, sha256 {digest}")
    return digest


Fetch = Callable[[str], tuple[int, bytes]]


def fetch(url: str) -> tuple[int, bytes]:
    """The status and body of a GET, with status 0 when no answer came, such as a timeout, a
    refused connection, or a reply cut short. A caller never takes that for a 404."""
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, b""
    except (OSError, http.client.HTTPException) as error:
        print(f"no answer from {url}: {error}", file=sys.stderr)
        return 0, b""


def published(version: str, get: Fetch = fetch) -> bool | None:
    status, _ = get(VERSION_API.format(name=PACKAGE, version=version))
    return {200: True, 404: False}.get(status)


def index_entry(version: str, get: Fetch) -> dict | None:
    """VERSION's line in the sparse index, or nothing while the index doesn't list it or gives
    something unreadable."""
    status, body = get(INDEX)
    if status != 200:
        return None
    try:
        for line in body.decode("utf-8").splitlines():
            entry = json.loads(line)
            if entry.get("name") == PACKAGE and entry.get("vers") == version:
                return entry
    except (ValueError, AttributeError):
        return None
    return None


def check_registry(
    crate_path: Path,
    version: str,
    seconds: float,
    get: Fetch = fetch,
    sleep: Callable[[float], None] = time.sleep,
) -> str:
    digest = hashlib.sha256(crate_path.read_bytes()).hexdigest()
    deadline = time.monotonic() + seconds
    while (entry := index_entry(version, get)) is None:
        if time.monotonic() >= deadline:
            raise Refused(f"the sparse index didn't list {PACKAGE} {version} within {seconds:.0f} s")
        sleep(5)
    if entry.get("yanked"):
        raise Refused(f"{PACKAGE} {version} is yanked")
    if entry.get("cksum") != digest:
        raise Refused(f"the sparse index's checksum is {entry.get('cksum')}, and the package's is {digest}")
    status, served = get(DOWNLOAD.format(name=PACKAGE, version=version))
    if status != 200:
        raise Refused(f"crates.io answered {status} for the download of {PACKAGE} {version}")
    if hashlib.sha256(served).hexdigest() != digest:
        raise Refused(f"crates.io serves other bytes for {PACKAGE} {version} than the package's")
    print(f"crates.io: {PACKAGE} {version} has sha256 {digest} in the sparse index and in the download")
    return digest


def record(tag: str, commit: str, digest: str, attestation_url: str | None = None) -> str:
    """The release notes' verification section."""
    if TAG.fullmatch(tag) is None:
        raise Refused(f"{tag!r} isn't a release tag, which is vMAJOR.MINOR.PATCH")
    if re.fullmatch(r"[0-9a-f]{40}", commit) is None:
        raise Refused(f"{commit!r} isn't a full commit id")
    if re.fullmatch(r"[0-9a-f]{64}", digest) is None:
        raise Refused(f"{digest!r} isn't a sha256 digest")
    crate = f"{PACKAGE}-{tag[1:]}.crate"
    ref = f"refs/tags/{tag}"
    identity = f"https://github.com/{REPOSITORY}/{WORKFLOW}@{ref}"
    lines = [
        "## Verification",
        "",
        f"`{crate}` is the file crates.io serves for {PACKAGE} {tag[1:]}, and this release's asset.",
        "",
        f"- sha256: `{digest}`",
        f"- source: `{REPOSITORY}`, `{ref}`, commit `{commit}`",
        f"- built and signed by: `{REPOSITORY}/{WORKFLOW}` on a GitHub-hosted runner",
        f"- certificate identity: `{identity}`",
        f"- OIDC issuer: `{ISSUER}`",
        "- attestation: SLSA build provenance v1, `https://slsa.dev/provenance/v1`",
    ]
    if attestation_url:
        lines.append(f"- attestation record: {attestation_url}")
    lines += [
        "",
        "To check a copy, from this release or from crates.io:",
        "",
        "```sh",
        f"echo '{digest}  {crate}' | sha256sum --check",
        f"gh attestation verify {crate} --repo {REPOSITORY} \\",
        f"  --cert-identity {identity} \\",
        f"  --cert-oidc-issuer {ISSUER} \\",
        f"  --source-ref {ref} --source-digest {commit} \\",
        "  --deny-self-hosted-runners",
        "```",
    ]
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    root = Path.cwd()
    try:
        match argv:
            case ["tag", tag]:
                check_tag(root, tag)
            case ["package", crate, commit]:
                check_package(root, Path(crate), commit)
            case ["published", version]:
                # 1 sends the workflow on to publish, so nothing but a clear "no" may give it.
                try:
                    answer = published(version)
                except Exception as error:  # noqa: BLE001
                    print(f"{PACKAGE} {version} on crates.io: unknown ({error!r})", file=sys.stderr)
                    return 2
                print(f"{PACKAGE} {version} on crates.io: {'unknown' if answer is None else answer}")
                return 2 if answer is None else 0 if answer else 1
            case ["registry", crate, version, *rest] if len(rest) <= 1:
                try:
                    seconds = float(rest[0]) if rest else 300
                except ValueError:
                    print(__doc__, file=sys.stderr)
                    return 2
                check_registry(Path(crate), version, seconds)
            case ["notes", version]:
                print(changelog_section((root / "CHANGELOG.md").read_text(encoding="utf-8"), version))
            case ["record", tag, commit, digest, *rest] if len(rest) <= 1:
                print(record(tag, commit, digest, rest[0] if rest else None))
            case _:
                print(__doc__, file=sys.stderr)
                return 2
    except Refused as refused:
        print(f"refused: {refused}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
