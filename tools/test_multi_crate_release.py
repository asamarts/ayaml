#!/usr/bin/env python3
"""Unit tests for multi-crate (ayaml + ayaml-edit) CI, unsafe census, and release tooling (AY-163)."""

from __future__ import annotations

import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
TOOLS_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS_DIR))

import check_release
import check_unsafe_census
from _release_test_fakes import FAKE_CHECK_RELEASE, FAKE_GH

NO_UNSAFE = {
    kind: {"safe": 1, "unsafe_": 0}
    for kind in ("functions", "exprs", "item_impls", "item_traits", "methods")
}


def _pkg_entry(name: str, unsafe_exprs: int = 0, forbids: bool = False) -> dict:
    used = dict(NO_UNSAFE)
    used["exprs"] = {"safe": 1, "unsafe_": unsafe_exprs}
    return {
        "package": {"id": {"name": name}},
        "unsafety": {"forbids_unsafe": forbids, "used": used},
    }


def _git(cwd: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(cwd), *args], capture_output=True, text=True, check=True
    ).stdout.strip()


def _make_crate(top: str, files: dict[str, bytes]) -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as archive:
        for name, data in files.items():
            info = tarfile.TarInfo(f"{top}/{name}")
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
    return buf.getvalue()


class MultiCrateUnsafeCensusTests(unittest.TestCase):
    """tools/check_unsafe_census.py enforces forbid(unsafe_code) on both ayaml and ayaml-edit."""

    def test_ayaml_edit_census_passes_when_both_workspace_crates_forbid_unsafe(self) -> None:
        data = {
            "packages": [
                _pkg_entry("ayaml-edit", forbids=True),
                _pkg_entry("ayaml", forbids=True),
                _pkg_entry("indexmap", 40),
            ],
            "packages_without_metrics": [],
            "used_but_not_scanned_files": [],
        }
        self.assertEqual(check_unsafe_census.problems(data, target_crate="ayaml-edit"), [])

    def test_ayaml_edit_census_rejects_unsafe_missing_forbid_or_missing_crate(self) -> None:
        ayaml_ok = _pkg_entry("ayaml", forbids=True)
        cases = [
            (
                [_pkg_entry("ayaml-edit", 1, forbids=True), ayaml_ok],
                "ayaml-edit has 1 unsafe items",
            ),
            (
                [_pkg_entry("ayaml-edit", 0, forbids=False), ayaml_ok],
                "ayaml-edit doesn't forbid unsafe code",
            ),
            (
                [ayaml_ok],
                "0 entries for ayaml-edit",
            ),
            (
                [_pkg_entry("ayaml-edit", forbids=True)],
                "0 entries for ayaml",
            ),
        ]
        for pkgs, expected in cases:
            with self.subTest(expected=expected):
                found = check_unsafe_census.problems(
                    {
                        "packages": pkgs,
                        "packages_without_metrics": [],
                        "used_but_not_scanned_files": [],
                    },
                    target_crate="ayaml-edit",
                )
                self.assertEqual(len(found), 1, found)
                self.assertIn(expected, found[0])


class MultiCrateReleaseChecksTests(unittest.TestCase):
    """tools/check_release.py handles ayaml-edit tag, package, published, registry, and record paths."""

    AYAML_MANIFEST = (
        b'[package]\nname = "ayaml"\nversion.workspace = true\n\n'
        b'[dependencies.indexmap]\nversion = "2.0.0"\n\n'
        b'[dependencies.serde]\nversion = "1.0.220"\n\n'
        b'[package.metadata.docs.rs]\nfeatures = ["libyaml-compat"]\n'
    )
    EDIT_MANIFEST = (
        b'[package]\nname = "ayaml-edit"\nversion.workspace = true\n\n'
        b'[dependencies]\nayaml = { path = "../ayaml", version = "=0.2.0" }\n'
    )
    CHANGELOG = "# Changelog\n\n## [0.2.0] \u2014 2026-11-07\n\n- Added ayaml-edit.\n"

    def setUp(self) -> None:
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name) / "repo"
        (self.root / "crates/ayaml/src").mkdir(parents=True)
        (self.root / "crates/ayaml-edit/src").mkdir(parents=True)
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "0.2.0"\n', encoding="utf-8")
        (self.root / "crates/ayaml/Cargo.toml").write_bytes(self.AYAML_MANIFEST)
        (self.root / "crates/ayaml/src/lib.rs").write_text("//! ayaml\n", encoding="utf-8")
        (self.root / "crates/ayaml/CHANGELOG.md").write_text(self.CHANGELOG, encoding="utf-8")
        (self.root / "crates/ayaml-edit/Cargo.toml").write_bytes(self.EDIT_MANIFEST)
        (self.root / "crates/ayaml-edit/README.md").write_text("# ayaml-edit\n", encoding="utf-8")
        (self.root / "crates/ayaml-edit/src/lib.rs").write_text("//! ayaml-edit\n", encoding="utf-8")
        (self.root / "README.md").write_text("# ayaml\n", encoding="utf-8")
        (self.root / "CHANGELOG.md").write_text(self.CHANGELOG, encoding="utf-8")
        _git(self.root.parent, "init", "-q", "-b", "main", str(self.root))
        for k, v in (
            ("user.name", "t"),
            ("user.email", "t@example.com"),
            ("tag.gpgsign", "false"),
            ("commit.gpgsign", "false"),
        ):
            _git(self.root, "config", k, v)
        _git(self.root, "add", "-A")
        _git(self.root, "commit", "-q", "-m", "release")
        self.commit = _git(self.root, "rev-parse", "HEAD")
        _git(self.root, "update-ref", "refs/remotes/origin/main", self.commit)
        _git(self.root, "tag", "-a", "-m", "v0.2.0", "v0.2.0")

    def _edit_files(self, changes: dict[str, bytes | None] | None = None) -> dict[str, bytes]:
        files = {
            ".cargo_vcs_info.json": json.dumps(
                {"git": {"sha1": self.commit}, "path_in_vcs": "crates/ayaml-edit"}
            ).encode(),
            "Cargo.toml": (
                b'[package]\nname = "ayaml-edit"\nversion = "0.2.0"\n\n'
                b'[dependencies.ayaml]\nversion = "=0.2.0"\n'
            ),
            "Cargo.toml.orig": self.EDIT_MANIFEST,
            "Cargo.lock": b"# lock\n",
            "README.md": b"# ayaml-edit\n",
            "src/lib.rs": b"//! ayaml-edit\n",
        }
        for k, v in (changes or {}).items():
            if v is None:
                files.pop(k)
            else:
                files[k] = v
        return files

    def test_tag_checks_ayaml_edit_version_and_exact_ayaml_pin(self) -> None:
        self.assertEqual(check_release.check_tag(self.root, "v0.2.0"), self.commit)

        # Non-exact ayaml requirement in crates/ayaml-edit/Cargo.toml is refused.
        (self.root / "crates/ayaml-edit/Cargo.toml").write_text(
            '[package]\nname = "ayaml-edit"\nversion.workspace = true\n\n'
            '[dependencies]\nayaml = { path = "../ayaml", version = "0.2.0" }\n',
            encoding="utf-8",
        )
        _git(self.root, "commit", "-q", "-am", "loose pin")
        commit = _git(self.root, "rev-parse", "HEAD")
        _git(self.root, "update-ref", "refs/remotes/origin/main", commit)
        _git(self.root, "tag", "-f", "-a", "-m", "v0.2.0", "v0.2.0")
        with self.assertRaises(check_release.Refused) as caught:
            check_release.check_tag(self.root, "v0.2.0")
        self.assertIn("not '=0.2.0'", str(caught.exception))

    def test_package_verifies_ayaml_edit_files_vcs_and_pin(self) -> None:
        crate_path = self.root.parent / "ayaml-edit-0.2.0.crate"
        crate_path.write_bytes(_make_crate("ayaml-edit-0.2.0", self._edit_files()))
        digest = check_release.check_package(self.root, crate_path, self.commit, package="ayaml-edit")
        self.assertEqual(digest, hashlib.sha256(crate_path.read_bytes()).hexdigest())

        # Caret pin on ayaml is refused in packaged ayaml-edit.
        bad_pin = self._edit_files(
            {
                "Cargo.toml": (
                    b'[package]\nname = "ayaml-edit"\nversion = "0.2.0"\n\n'
                    b'[dependencies.ayaml]\nversion = "0.2.0"\n'
                )
            }
        )
        crate_path.write_bytes(_make_crate("ayaml-edit-0.2.0", bad_pin))
        with self.assertRaises(check_release.Refused) as caught:
            check_release.check_package(self.root, crate_path, self.commit, package="ayaml-edit")
        self.assertIn("pins ayaml", str(caught.exception))

    def test_published_and_registry_support_ayaml_edit(self) -> None:
        self.assertEqual(
            check_release.index_url("ayaml-edit"),
            "https://index.crates.io/ay/am/ayaml-edit",
        )
        crate_bytes = b"ayaml-edit archive"
        digest = hashlib.sha256(crate_bytes).hexdigest()
        crate_path = self.root.parent / "ayaml-edit-0.2.0.crate"
        crate_path.write_bytes(crate_bytes)

        def fake_get(url: str) -> tuple[int, bytes]:
            if url == "https://crates.io/api/v1/crates/ayaml-edit":
                return 404, b""
            if url == "https://crates.io/api/v1/crates/ayaml-edit/0.2.0":
                return 200, b"{}"
            if url == "https://index.crates.io/ay/am/ayaml-edit":
                return (
                    200,
                    json.dumps({"name": "ayaml-edit", "vers": "0.2.0", "cksum": digest, "yanked": False}).encode(),
                )
            if url == "https://static.crates.io/crates/ayaml-edit/ayaml-edit-0.2.0.crate":
                return 200, crate_bytes
            raise AssertionError(f"unexpected url: {url}")

        self.assertIs(check_release.published(None, get=fake_get, package="ayaml-edit"), False)
        self.assertIs(check_release.published("0.2.0", get=fake_get, package="ayaml-edit"), True)
        self.assertEqual(
            check_release.check_registry(
                crate_path, "0.2.0", 0, get=fake_get, sleep=lambda _: None, package="ayaml-edit"
            ),
            digest,
        )

    def test_record_supports_multi_crate_release_and_rehearsal(self) -> None:
        d1 = "a" * 64
        d2 = "b" * 64
        u1 = "https://github.com/asamarts/ayaml/attestations/1"
        u2 = "https://github.com/asamarts/ayaml/attestations/2"
        out = io.StringIO()
        with mock.patch("sys.stdout", out):
            rc = check_release.main(
                ["record", "v0.2.0", self.commit, d1, u1, "--extra", "ayaml-edit", d2, u2]
            )
        self.assertEqual(rc, 0)
        text = out.getvalue()
        for needle in (
            "ayaml-0.2.0.crate",
            "ayaml-edit-0.2.0.crate",
            d1,
            d2,
            u1,
            u2,
            "gh attestation verify ayaml-0.2.0.crate",
            "gh attestation verify ayaml-edit-0.2.0.crate",
        ):
            self.assertIn(needle, text)

        # Rehearsal omits attestation URLs for both crates.
        out_reh = io.StringIO()
        with mock.patch("sys.stdout", out_reh):
            rc_reh = check_release.main(
                ["record", "v0.2.0", self.commit, d1, "--extra", "ayaml-edit", d2]
            )
        self.assertEqual(rc_reh, 0)
        self.assertIn("ayaml-edit-0.2.0.crate", out_reh.getvalue())
        self.assertNotIn("attestation record:", out_reh.getvalue())

        # Duplicate package names in --extra are refused.
        for bad_args in (
            ["record", "v0.2.0", self.commit, d1, "--extra", "ayaml", d2],
            [
                "record",
                "v0.2.0",
                self.commit,
                d1,
                "--extra",
                "ayaml-edit",
                d2,
                "--extra",
                "ayaml-edit",
                d2,
            ],
        ):
            with self.subTest(bad_args=bad_args), mock.patch("sys.stderr", io.StringIO()):
                self.assertEqual(check_release.main(bad_args), 1)

    def test_published_cli_rejects_malformed_flags_and_versions_with_exit_2(self) -> None:
        # Exit 1 triggers a publish in release.yml, so malformed CLI invocations must exit 2.
        for bad_args in (
            ["published", "--package"],
            ["published", "--package="],
            ["published", "--package", "-v"],
            ["published", "--unknown", "0.2.0"],
            ["published", "v0.2.0"],
            ["published", "--package=ayaml-edit", "not-a-semver"],
        ):
            with (
                self.subTest(bad_args=bad_args),
                mock.patch("sys.stderr", io.StringIO()),
                mock.patch.object(check_release, "published", side_effect=AssertionError("should not query crates.io")),
            ):
                self.assertEqual(check_release.main(bad_args), 2)

        # `--package=ayaml-edit` with valid semver routes cleanly to published().
        with (
            mock.patch("sys.stdout", io.StringIO()),
            mock.patch.object(check_release, "published", return_value=True) as mocked,
        ):
            self.assertEqual(check_release.main(["published", "--package=ayaml-edit", "0.2.0"]), 0)
            mocked.assert_called_once_with("0.2.0", package="ayaml-edit")


class MultiCrateGitHubReleaseScriptTests(unittest.TestCase):
    """tools/github_release.sh fetches, checksums, uploads, and verifies both crates together."""

    def test_fetch_and_release_handle_ayaml_and_ayaml_edit(self) -> None:
        fake_multi_curl = r'''#!/usr/bin/env python3
import os, shutil, sys
from pathlib import Path
args = sys.argv[1:]
url = args[-1]
with open(Path(os.environ["GH_STATE"]) / "calls", "a") as log:
    log.write("curl " + url + "\n")
out = args[args.index("--output") + 1]
src = os.environ["FAKE_EDIT_CRATE"] if "ayaml-edit" in url else os.environ["FAKE_AYAML_CRATE"]
shutil.copy(src, out)
'''
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            (root / "tools").mkdir()
            shutil.copy(TOOLS_DIR / "github_release.sh", root / "tools/github_release.sh")
            (root / "tools/check_release.py").write_text(FAKE_CHECK_RELEASE, encoding="utf-8")
            bin_dir = root / "bin"
            bin_dir.mkdir()
            for name, body in (("gh", FAKE_GH), ("curl", fake_multi_curl)):
                p = bin_dir / name
                p.write_text(body, encoding="utf-8")
                p.chmod(p.stat().st_mode | stat.S_IXUSR)
            state = root / "state"
            (state / "releases").mkdir(parents=True)

            ayaml_src = root / "ayaml.crate"
            edit_src = root / "edit.crate"
            ayaml_src.write_bytes(b"ayaml bytes")
            edit_src.write_bytes(b"ayaml-edit bytes")
            d1 = hashlib.sha256(b"ayaml bytes").hexdigest()
            d2 = hashlib.sha256(b"ayaml-edit bytes").hexdigest()
            dist = root / "dist"
            notes = root / "notes.md"
            notes.write_text("## Verification\n\nboth crates\n", encoding="utf-8")

            env = {
                **os.environ,
                "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                "GH_STATE": str(state),
                "TMPDIR": str(root),
                "FAKE_AYAML_CRATE": str(ayaml_src),
                "FAKE_EDIT_CRATE": str(edit_src),
            }
            fetched = subprocess.run(
                [
                    "bash",
                    str(root / "tools/github_release.sh"),
                    "fetch",
                    "0.2.0",
                    d1,
                    str(dist),
                    f"ayaml-edit={d2}",
                ],
                cwd=root,
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertEqual(fetched.returncode, 0, fetched.stderr)
            self.assertEqual(
                (dist / "SHA256SUMS").read_text(encoding="utf-8"),
                f"{d1}  ayaml-0.2.0.crate\n{d2}  ayaml-edit-0.2.0.crate\n",
            )
            calls = (state / "calls").read_text(encoding="utf-8").splitlines()
            self.assertIn(
                f"check_release.py registry --package ayaml-edit {dist / 'ayaml-edit-0.2.0.crate'} 0.2.0 600",
                calls,
            )

            rel = subprocess.run(
                [
                    "bash",
                    str(root / "tools/github_release.sh"),
                    "release",
                    "v0.2.0",
                    "0.2.0",
                    str(dist),
                    str(notes),
                ],
                cwd=root,
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertEqual(rel.returncode, 0, rel.stderr)
            self.assertIn(f"ayaml-0.2.0.crate ({d1})", rel.stdout)
            self.assertIn(f"ayaml-edit-0.2.0.crate ({d2})", rel.stdout)
            uploaded = sorted(p.name for p in (state / "releases/v0.2.0/assets").iterdir())
            self.assertEqual(
                uploaded,
                ["SHA256SUMS", "ayaml-0.2.0.crate", "ayaml-edit-0.2.0.crate"],
            )

            # Duplicate package spec in fetch is refused.
            dup = subprocess.run(
                [
                    "bash",
                    str(root / "tools/github_release.sh"),
                    "fetch",
                    "0.2.0",
                    d1,
                    str(root / "dup_dist"),
                    f"ayaml-edit={d2}",
                    f"ayaml-edit={d2}",
                ],
                cwd=root,
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(dup.returncode, 0)
            self.assertIn("duplicate package", dup.stderr)

            # Missing crate files in dist fails release immediately instead of silently uploading empty list.
            empty_dist = root / "empty_dist"
            empty_dist.mkdir()
            (empty_dist / "SHA256SUMS").write_text("", encoding="utf-8")
            empty_rel = subprocess.run(
                [
                    "bash",
                    str(root / "tools/github_release.sh"),
                    "release",
                    "v0.2.1",
                    "0.2.1",
                    str(empty_dist),
                    str(notes),
                ],
                cwd=root,
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(empty_rel.returncode, 0)
            self.assertIn("no *-0.2.1.crate files found", empty_rel.stderr)

            # Corrupt, incomplete, or blank-line SHA256SUMS must fail BEFORE gh release create.
            for case_name, bad_sums, expected_err in (
                (
                    "corrupt-hash",
                    f"{'0' * 64}  ayaml-0.2.0.crate\n{d2}  ayaml-edit-0.2.0.crate\n",
                    "SHA256SUMS verification failed",
                ),
                (
                    "missing-crate-entry",
                    f"{d1}  ayaml-0.2.0.crate\n",
                    "does not cover exactly the *-0.2.0.crate files",
                ),
                (
                    "blank-line",
                    f"{d1}  ayaml-0.2.0.crate\n\n{d2}  ayaml-edit-0.2.0.crate\n",
                    "contains a blank line",
                ),
            ):
                with self.subTest(case=case_name):
                    bad_dist = root / f"bad_{case_name}"
                    bad_dist.mkdir()
                    shutil.copy(ayaml_src, bad_dist / "ayaml-0.2.0.crate")
                    shutil.copy(edit_src, bad_dist / "ayaml-edit-0.2.0.crate")
                    (bad_dist / "SHA256SUMS").write_text(bad_sums, encoding="utf-8")
                    bad_tag = f"v0.2.9-{case_name}"
                    res = subprocess.run(
                        [
                            "bash",
                            str(root / "tools/github_release.sh"),
                            "release",
                            bad_tag,
                            "0.2.0",
                            str(bad_dist),
                            str(notes),
                        ],
                        cwd=root,
                        env=env,
                        capture_output=True,
                        text=True,
                    )
                    self.assertNotEqual(res.returncode, 0)
                    self.assertIn(expected_err, res.stderr)
                    self.assertFalse(
                        (state / "releases" / bad_tag).exists(),
                        f"gh release create must not run when SHA256SUMS fails ({case_name})",
                    )


if __name__ == "__main__":
    unittest.main()
