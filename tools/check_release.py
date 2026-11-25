#!/usr/bin/env python3
"""Checks that bind a release together: its tag, version, commit, package, registry bytes, and asset.

usage:
  tools/check_release.py tag TAG
      HEAD is the commit of TAG, an annotated tag; the checkout is clean; the commit is on
      origin/main; each published workspace crate's version is TAG's (and ayaml-edit pins
      ayaml = "=VERSION"); and CHANGELOG.md has a section for it.
  tools/check_release.py package [--package PKG] CRATE COMMIT
      CRATE, as `cargo package` wrote it, holds exactly COMMIT's files of PKG's directory,
      any workspace files it packages, and what cargo generates, and it names COMMIT.
  tools/check_release.py published [--package PKG] [VERSION]
      Exits 0 if crates.io has PKG (at VERSION, if given), 1 if it doesn't, and 2 for any
      other answer, an error in this script included.
  tools/check_release.py registry [--package PKG] CRATE VERSION [SECONDS]
      Waits up to SECONDS, 300 by default, for the sparse index to list PKG VERSION, then checks
      that its checksum and the bytes crates.io serves are CRATE's.
  tools/check_release.py notes VERSION
      Prints the CHANGELOG.md section for VERSION.
  tools/check_release.py record TAG COMMIT DIGEST [ATTESTATION_URL]
                                [--extra PKG DIGEST [ATTESTATION_URL]]...
      Prints what a consumer needs to verify the release's attestation(s): the workflow, issuer,
      repository, ref, commit, artifact name(s), and digest(s), and the command(s) that check them.

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
from typing import Callable, Sequence
import urllib.error
import urllib.request

PACKAGE = "ayaml"
PACKAGE_DIR = "crates/ayaml"
PACKAGES: dict[str, dict[str, object]] = {
    "ayaml": {
        "dir": "crates/ayaml",
        "from_workspace": {"README.md": "README.md"},
    },
    "ayaml-edit": {
        "dir": "crates/ayaml-edit",
        "from_workspace": {},
    },
}
REPOSITORY = "asamarts/ayaml"
WORKFLOW = ".github/workflows/release.yml"
ISSUER = "https://token.actions.githubusercontent.com"
# What the primary package holds from outside its directory, by packaged name.
FROM_WORKSPACE = {"README.md": "README.md"}
# What cargo writes rather than copies. The manifest it copies is packaged as Cargo.toml.orig.
GENERATED = {".cargo_vcs_info.json", "Cargo.toml", "Cargo.lock"}
TAG = re.compile(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
USER_AGENT = "ayaml-release-check (https://github.com/asamarts/ayaml)"
DOWNLOAD = "https://static.crates.io/crates/{name}/{name}-{version}.crate"
VERSION_API = "https://crates.io/api/v1/crates/{name}/{version}"
CRATE_API = "https://crates.io/api/v1/crates/{name}"


def index_url(package: str = PACKAGE) -> str:
    """crates.io sparse index URL for a crate name (at least 4 characters)."""
    lower = package.lower()
    if len(lower) < 4:
        raise Refused(f"unsupported short crate name {package!r}")
    return f"https://index.crates.io/{lower[:2]}/{lower[2:4]}/{lower}"


INDEX = index_url(PACKAGE)


class Refused(Exception):
    """A check found a difference."""


def package_spec(package: str) -> tuple[str, dict[str, str]]:
    if package not in PACKAGES:
        raise Refused(f"unknown package {package!r}; expected one of {', '.join(sorted(PACKAGES))}")
    spec = PACKAGES[package]
    return str(spec["dir"]), dict(spec["from_workspace"])  # type: ignore[arg-type]


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


def package_version(root: Path, package: str = PACKAGE) -> str:
    pkg_dir, _ = package_spec(package)
    manifest = tomllib.loads((root / pkg_dir / "Cargo.toml").read_text(encoding="utf-8"))
    version = manifest["package"]["version"]
    if isinstance(version, dict) and version.get("workspace") is True:
        workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
        version = workspace["workspace"]["package"]["version"]
    if not isinstance(version, str):
        raise Refused(f"{pkg_dir}/Cargo.toml has no version")
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
    declared = package_version(root, "ayaml")
    if declared != version:
        raise Refused(f"{tag} names {version}, and {PACKAGE_DIR} declares {declared}")
    edit_manifest_path = root / "crates/ayaml-edit/Cargo.toml"
    if edit_manifest_path.is_file():
        edit_declared = package_version(root, "ayaml-edit")
        if edit_declared != version:
            raise Refused(f"{tag} names {version}, and crates/ayaml-edit declares {edit_declared}")
        edit_manifest = tomllib.loads(edit_manifest_path.read_text(encoding="utf-8"))
        ayaml_dep = edit_manifest.get("dependencies", {}).get("ayaml", {})
        dep_ver = ayaml_dep.get("version") if isinstance(ayaml_dep, dict) else ayaml_dep
        if dep_ver != f"={version}":
            raise Refused(
                f"crates/ayaml-edit/Cargo.toml pins ayaml to {dep_ver!r}, not '={version}'"
            )
    changelog_section((root / "CHANGELOG.md").read_text(encoding="utf-8"), version)
    print(f"{tag}: annotated, commit {commit} on origin/main, clean, version {version}, in CHANGELOG.md")
    return commit


def crate_files(crate: bytes, package: str = PACKAGE) -> tuple[str, dict[str, bytes]]:
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
    prefix = f"{package}-"
    if top is None or not top.startswith(prefix):
        raise Refused(f"the package's directory is {top!r}, not {prefix}VERSION")
    return top[len(prefix) :], files


def check_package(root: Path, crate_path: Path, commit: str, package: str = PACKAGE) -> str:
    pkg_dir, from_workspace = package_spec(package)
    crate = crate_path.read_bytes()
    version, files = crate_files(crate, package=package)
    if crate_path.name != f"{package}-{version}.crate":
        raise Refused(f"{crate_path.name} holds {package}-{version}")
    vcs = json.loads(files.get(".cargo_vcs_info.json", b"null"))
    wanted = {"git": {"sha1": commit}, "path_in_vcs": pkg_dir}
    if vcs != wanted:
        raise Refused(f".cargo_vcs_info.json is {vcs}, not {wanted}")
    document = tomllib.loads(files["Cargo.toml"].decode("utf-8"))
    manifest = document["package"]
    if (manifest.get("name"), manifest.get("version")) != (package, version):
        raise Refused(f"the packaged manifest names {manifest.get('name')} {manifest.get('version')}")
    if package == "ayaml":
        # Packages built after the self-contained package layout carry their own
        # changelog in the crate tree. Older published tags predate it, and their
        # packages remain valid release evidence for a rehearsal.
        new_layout = git_has_path(root, commit, f"{pkg_dir}/CHANGELOG.md")
        if new_layout:
            dependencies = document.get("dependencies", {})
            for dependency in ("indexmap", "serde"):
                requirement = dependencies.get(dependency, {}).get("version")
                if not isinstance(requirement, str) or requirement.startswith("="):
                    raise Refused(f"the packaged manifest doesn't give {dependency} a caret requirement")
            docs = manifest.get("metadata", {}).get("docs", {}).get("rs", {})
            if "libyaml-compat" not in docs.get("features", []):
                raise Refused("the packaged manifest doesn't document libyaml-compat on docs.rs")
    else:
        new_layout = False
        dependencies = document.get("dependencies", {})
        ayaml_spec = dependencies.get("ayaml", {})
        requirement = ayaml_spec.get("version") if isinstance(ayaml_spec, dict) else ayaml_spec
        if requirement != f"={version}":
            raise Refused(
                f"the packaged {package} manifest pins ayaml to {requirement!r}, not '={version}'"
            )
    expected: dict[str, bytes] = {}
    listing = git(root, "ls-tree", "-r", "--full-tree", commit, "--", pkg_dir)
    for line in listing.splitlines():
        meta, path = line.split("\t", 1)
        mode = meta.split()[0]
        if mode not in ("100644", "100755"):
            raise Refused(f"{path} in {commit} has mode {mode}, which cargo wouldn't package as written")
        inner = path[len(pkg_dir) + 1 :]
        expected["Cargo.toml.orig" if inner == "Cargo.toml" else inner] = git_blob(root, commit, path)
    for packaged, source in from_workspace.items():
        expected[packaged] = git_blob(root, commit, source)
    if new_layout:
        crate_changelog = git_blob(root, commit, f"{pkg_dir}/CHANGELOG.md")
        workspace_changelog = git_blob(root, commit, "CHANGELOG.md")
        if crate_changelog != workspace_changelog:
            raise Refused(
                f"{pkg_dir}/CHANGELOG.md and CHANGELOG.md differ; "
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


def published(version: str | None = None, get: Fetch = fetch, package: str = PACKAGE) -> bool | None:
    package_spec(package)
    url = (
        CRATE_API.format(name=package)
        if version is None
        else VERSION_API.format(name=package, version=version)
    )
    status, _ = get(url)
    return {200: True, 404: False}.get(status)


def index_entry(version: str, get: Fetch, package: str = PACKAGE) -> dict | None:
    """VERSION's line in the sparse index, or nothing while the index doesn't list it or gives
    something unreadable."""
    status, body = get(index_url(package))
    if status != 200:
        return None
    try:
        for line in body.decode("utf-8").splitlines():
            entry = json.loads(line)
            if entry.get("name") == package and entry.get("vers") == version:
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
    package: str = PACKAGE,
) -> str:
    package_spec(package)
    digest = hashlib.sha256(crate_path.read_bytes()).hexdigest()
    deadline = time.monotonic() + seconds
    while (entry := index_entry(version, get, package=package)) is None:
        if time.monotonic() >= deadline:
            raise Refused(f"the sparse index didn't list {package} {version} within {seconds:.0f} s")
        sleep(5)
    if entry.get("yanked"):
        raise Refused(f"{package} {version} is yanked")
    if entry.get("cksum") != digest:
        raise Refused(f"the sparse index's checksum is {entry.get('cksum')}, and the package's is {digest}")
    status, served = get(DOWNLOAD.format(name=package, version=version))
    if status != 200:
        raise Refused(f"crates.io answered {status} for the download of {package} {version}")
    if hashlib.sha256(served).hexdigest() != digest:
        raise Refused(f"crates.io serves other bytes for {package} {version} than the package's")
    print(f"crates.io: {package} {version} has sha256 {digest} in the sparse index and in the download")
    return digest


def record(
    tag: str,
    commit: str,
    digest: str,
    attestation_url: str | None = None,
    extra_artifacts: Sequence[tuple[str, str, str | None]] = (),
) -> str:
    """The release notes' verification section."""
    if TAG.fullmatch(tag) is None:
        raise Refused(f"{tag!r} isn't a release tag, which is vMAJOR.MINOR.PATCH")
    if re.fullmatch(r"[0-9a-f]{40}", commit) is None:
        raise Refused(f"{commit!r} isn't a full commit id")
    artifacts: list[tuple[str, str, str | None]] = [(PACKAGE, digest, attestation_url)]
    for extra_pkg, extra_digest, extra_url in extra_artifacts:
        package_spec(extra_pkg)
        artifacts.append((extra_pkg, extra_digest, extra_url))
    for _, art_digest, _ in artifacts:
        if re.fullmatch(r"[0-9a-f]{64}", art_digest) is None:
            raise Refused(f"{art_digest!r} isn't a sha256 digest")

    version = tag[1:]
    ref = f"refs/tags/{tag}"
    identity = f"https://github.com/{REPOSITORY}/{WORKFLOW}@{ref}"

    if len(artifacts) == 1:
        crate = f"{PACKAGE}-{version}.crate"
        lines = [
            "## Verification",
            "",
            f"`{crate}` is the file crates.io serves for {PACKAGE} {version}, and this release's asset.",
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

    crate_names = [f"`{pkg}-{version}.crate`" for pkg, _, _ in artifacts]
    lines = [
        "## Verification",
        "",
        f"{' and '.join(crate_names)} are the files crates.io serves for version {version}, and this release's assets.",
        "",
    ]
    for pkg, art_digest, art_url in artifacts:
        crate = f"{pkg}-{version}.crate"
        lines.append(f"- `{crate}` sha256: `{art_digest}`")
        if art_url:
            lines.append(f"- `{crate}` attestation record: {art_url}")
    lines += [
        f"- source: `{REPOSITORY}`, `{ref}`, commit `{commit}`",
        f"- built and signed by: `{REPOSITORY}/{WORKFLOW}` on a GitHub-hosted runner",
        f"- certificate identity: `{identity}`",
        f"- OIDC issuer: `{ISSUER}`",
        "- attestation: SLSA build provenance v1, `https://slsa.dev/provenance/v1`",
        "",
        "To check copies, from this release or from crates.io:",
        "",
        "```sh",
    ]
    for pkg, art_digest, _ in artifacts:
        crate = f"{pkg}-{version}.crate"
        lines.append(f"echo '{art_digest}  {crate}' | sha256sum --check")
    for pkg, _, _ in artifacts:
        crate = f"{pkg}-{version}.crate"
        lines += [
            f"gh attestation verify {crate} --repo {REPOSITORY} \\",
            f"  --cert-identity {identity} \\",
            f"  --cert-oidc-issuer {ISSUER} \\",
            f"  --source-ref {ref} --source-digest {commit} \\",
            "  --deny-self-hosted-runners",
        ]
    lines.append("```")
    return "\n".join(lines)


def _pop_package_flag(args: list[str]) -> tuple[str, list[str]]:
    if len(args) >= 2 and args[0] == "--package":
        return args[1], args[2:]
    return PACKAGE, args


def _parse_record_args(rest: list[str]) -> tuple[str | None, list[tuple[str, str, str | None]]]:
    attestation_url: str | None = None
    extras: list[tuple[str, str, str | None]] = []
    idx = 0
    if idx < len(rest) and rest[idx] != "--extra":
        attestation_url = rest[idx]
        idx += 1
    while idx < len(rest):
        if rest[idx] != "--extra":
            raise ValueError(f"unexpected argument {rest[idx]!r}")
        idx += 1
        if idx + 2 > len(rest):
            raise ValueError("--extra requires PKG DIGEST [ATTESTATION_URL]")
        pkg = rest[idx]
        art_digest = rest[idx + 1]
        idx += 2
        art_url: str | None = None
        if idx < len(rest) and rest[idx] != "--extra":
            art_url = rest[idx]
            idx += 1
        extras.append((pkg, art_digest, art_url))
    return attestation_url, extras


def main(argv: list[str]) -> int:
    root = Path.cwd()
    try:
        match argv:
            case ["tag", tag]:
                check_tag(root, tag)
            case ["package", *rest]:
                pkg, pos = _pop_package_flag(rest)
                if len(pos) != 2:
                    print(__doc__, file=sys.stderr)
                    return 2
                check_package(root, Path(pos[0]), pos[1], package=pkg)
            case ["published", *rest]:
                pkg, pos = _pop_package_flag(rest)
                if len(pos) > 1:
                    print(__doc__, file=sys.stderr)
                    return 2
                version = pos[0] if pos else None
                label = f"{pkg} {version}" if version is not None else pkg
                # 1 sends the workflow on to publish, so nothing but a clear "no" may give it.
                try:
                    answer = published(version, package=pkg)
                except Exception as error:  # noqa: BLE001
                    print(f"{label} on crates.io: unknown ({error!r})", file=sys.stderr)
                    return 2
                print(f"{label} on crates.io: {'unknown' if answer is None else answer}")
                return 2 if answer is None else 0 if answer else 1
            case ["registry", *rest]:
                pkg, pos = _pop_package_flag(rest)
                if len(pos) not in (2, 3):
                    print(__doc__, file=sys.stderr)
                    return 2
                try:
                    seconds = float(pos[2]) if len(pos) == 3 else 300
                except ValueError:
                    print(__doc__, file=sys.stderr)
                    return 2
                check_registry(Path(pos[0]), pos[1], seconds, package=pkg)
            case ["notes", version]:
                print(changelog_section((root / "CHANGELOG.md").read_text(encoding="utf-8"), version))
            case ["record", tag, commit, digest, *rest]:
                try:
                    attestation_url, extras = _parse_record_args(rest)
                except ValueError:
                    print(__doc__, file=sys.stderr)
                    return 2
                print(record(tag, commit, digest, attestation_url, extras))
            case _:
                print(__doc__, file=sys.stderr)
                return 2
    except Refused as refused:
        print(f"refused: {refused}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
