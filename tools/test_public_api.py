#!/usr/bin/env python3
"""Tests for tools/check_public_api.py and tools/install-public-api.sh."""

from __future__ import annotations

import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest

sys.dont_write_bytecode = True
TOOLS_DIR = Path(__file__).resolve().parent
REPO_ROOT = TOOLS_DIR.parent
sys.path.insert(0, str(TOOLS_DIR))

import check_public_api as cpa


def git(root: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(root), *args],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()


class PublicApiDiffing(unittest.TestCase):
    def test_purely_additive_changes_produce_no_breaks(self) -> None:
        base = {
            "pub struct ayaml::Span",
            "pub struct ayaml::span::Span",
            "pub const fn ayaml::span::Span::start(self) -> usize",
        }
        head = base | {
            "pub const fn ayaml::span::Span::range(self) -> core::ops::range::Range<usize>",
            "pub struct ayaml::ParseOptions",
        }
        diff = cpa.diff_listings(base, head)
        self.assertEqual(diff.removed, ())
        self.assertEqual(diff.changed, ())
        self.assertEqual(len(diff.added), 2)

    def test_removed_item_changed_signature_trait_and_assoc_type_are_detected(self) -> None:
        base = {
            "pub struct ayaml::Span",
            "pub struct ayaml::span::Span",
            "pub const fn ayaml::span::Span::new(ayaml::span::Marker, ayaml::span::Marker) -> Self",
            "pub const fn ayaml::span::Span::start(self) -> ayaml::span::Marker",
            "pub fn ayaml::old_fn(&str) -> usize",
            "impl core::marker::Send for ayaml::span::Span",
            "impl core::fmt::Display for ayaml::span::Span",
            "pub type ayaml::mapping::Mapping::IntoIter = indexmap::map::iter::IntoIter<ayaml::value::Value, ayaml::value::Value>",
        }
        head = {
            "pub struct ayaml::Span",
            "pub struct ayaml::span::Span",
            "pub const fn ayaml::span::Span::new(usize, usize) -> Self",
            "pub const fn ayaml::span::Span::start(self) -> usize",
            "impl !core::marker::Send for ayaml::span::Span",
            "pub type ayaml::mapping::Mapping::IntoIter = ayaml::mapping::IntoIter",
        }
        diff = cpa.diff_listings(base, head)
        self.assertEqual(
            set(diff.removed),
            {
                "pub fn ayaml::old_fn(&str) -> usize",
                "impl core::fmt::Display for ayaml::span::Span",
            },
        )
        changed_old = {old for old, _ in diff.changed}
        self.assertEqual(
            changed_old,
            {
                "pub const fn ayaml::span::Span::new(ayaml::span::Marker, ayaml::span::Marker) -> Self",
                "pub const fn ayaml::span::Span::start(self) -> ayaml::span::Marker",
                "impl core::marker::Send for ayaml::span::Span",
                "pub type ayaml::mapping::Mapping::IntoIter = indexmap::map::iter::IntoIter<ayaml::value::Value, ayaml::value::Value>",
            },
        )

    def test_unstable_compiler_marker_traits_are_filtered_out(self) -> None:
        lines = [
            "impl core::marker::Freeze for ayaml::mapping::Mapping",
            "impl !core::marker::Freeze for ayaml::mapping::Mapping",
            "impl core::marker::StructuralPartialEq for ayaml::mapping::Mapping",
            "impl core::marker::StructuralEq for ayaml::mapping::Mapping",
            "impl core::marker::Send for ayaml::mapping::Mapping",
            "impl core::marker::Sync for ayaml::mapping::Mapping",
            "impl core::panic::unwind_safe::RefUnwindSafe for ayaml::mapping::Mapping",
        ]
        filtered = cpa.filter_listing(lines)
        self.assertEqual(
            filtered,
            {
                "impl core::marker::Send for ayaml::mapping::Mapping",
                "impl core::marker::Sync for ayaml::mapping::Mapping",
                "impl core::panic::unwind_safe::RefUnwindSafe for ayaml::mapping::Mapping",
            },
        )

    def test_self_vs_concrete_owner_method_signature_is_equivalent(self) -> None:
        base = {
            "pub struct ayaml::mapping::Mapping",
            "pub fn ayaml::mapping::Mapping::eq(&self, &ayaml::mapping::Mapping) -> bool",
        }
        head = {
            "pub struct ayaml::mapping::Mapping",
            "pub fn ayaml::mapping::Mapping::eq(&self, &Self) -> bool",
        }
        diff = cpa.diff_listings(base, head)
        self.assertEqual(diff.removed, ())
        self.assertEqual(diff.changed, ())

    def test_self_equivalence_does_not_ignore_trait_method_changes(self) -> None:
        base = {
            "pub trait ayaml::MyTrait",
            "pub fn ayaml::MyTrait::build(&self) -> ayaml::MyTrait",
        }
        head = {
            "pub trait ayaml::MyTrait",
            "pub fn ayaml::MyTrait::build(&self) -> Self",
        }
        diff = cpa.diff_listings(base, head)
        self.assertEqual(len(diff.changed), 1)

    def test_non_exhaustive_change_and_removed_reexport_are_detected(self) -> None:
        base = {
            "pub enum ayaml::Value",
            "pub struct ayaml::Span",
            "pub struct ayaml::span::Span",
        }
        head = {
            "#[non_exhaustive] pub enum ayaml::Value",
            "pub struct ayaml::span::Span",
        }
        diff = cpa.diff_listings(base, head)
        self.assertEqual(diff.removed, ("pub struct ayaml::Span",))
        self.assertEqual(
            diff.changed,
            (("pub enum ayaml::Value", "#[non_exhaustive] pub enum ayaml::Value"),),
        )

    def test_parse_impl_header_handles_nested_generics_and_where_clauses(self) -> None:
        line = "impl<'a, T: core::convert::Into<core::option::Option<u8>>> core::convert::From<T> for &'a mut ayaml::mapping::Mapping where T: core::marker::Send"
        self.assertEqual(
            cpa.parse_impl_header(line),
            ("core::convert::From<T>", "&'a mut ayaml::mapping::Mapping"),
        )
        self.assertEqual(
            cpa.parse_impl_header("impl<'a> ayaml::Emitter<'a>"),
            (None, "ayaml::Emitter<'a>"),
        )

    def test_item_key_handles_generic_owners_traits_consts_and_fields(self) -> None:
        self.assertEqual(
            cpa.item_key("pub const fn ayaml::Emitter<'a>::new(&'a ayaml::limits::Limits) -> Self"),
            ("fn", "ayaml::Emitter::new"),
        )
        self.assertEqual(
            cpa.item_key("pub trait ayaml::mapping::Index: ayaml::mapping::private::Sealed"),
            ("trait", "ayaml::mapping::Index"),
        )
        self.assertEqual(
            cpa.item_key("pub const ayaml::MAX_DEPTH: usize"),
            ("const", "ayaml::MAX_DEPTH"),
        )
        self.assertEqual(
            cpa.item_key("pub ayaml::Event::MappingStart::span: ayaml::span::Span"),
            ("member", "ayaml::Event::MappingStart::span"),
        )
        self.assertEqual(
            cpa.find_root_reexports(
                {
                    "pub struct ayaml::Span",
                    "pub struct ayaml::Anchor<'a>(_)",
                    "#[non_exhaustive] pub enum ayaml::Event<'a>",
                    "pub const ayaml::MAX_DEPTH: usize",
                    "pub struct ayaml::mapping::Iter<'a>",
                }
            ),
            {"Span", "Anchor", "Event", "MAX_DEPTH"},
        )


class ChangelogMatching(unittest.TestCase):
    BASE_ITEMS = {
        "pub struct ayaml::Span",
        "pub struct ayaml::span::Span",
        "pub struct ayaml::Mapping",
        "pub struct ayaml::mapping::Mapping",
        "pub struct ayaml::mapping::Iter<'a>",
        "pub mod ayaml::compat",
        "pub fn ayaml::compat::parse_events_with_limits<'a>(&'a str, &ayaml::limits::Limits) -> core::result::Result<alloc::vec::Vec<ayaml::event::Event<'a>>, ayaml::error::Error>",
        "pub fn ayaml::parse_events_with_limits<'a>(&'a str, &ayaml::limits::Limits) -> core::result::Result<ayaml::EventStream<'a>, ayaml::error::Error>",
    }

    def test_active_changed_section_respects_unreleased_and_version_ordering(self) -> None:
        with_unreleased = (
            "# Changelog\n\n"
            "## [Unreleased]\n\n"
            "### Added\n\n- `Span::start` mentioned only in Added.\n\n"
            "### Changed\n\n- `Span::new` takes `usize`.\n\n"
            "### Fixed\n\n- `Span::end` mentioned only in Fixed.\n\n"
            "## [0.2.2] — 2026-11-20\n\n"
            "### Changed\n\n- `Mapping::clear` changed in older release.\n"
        )
        active = cpa.active_changed_section(with_unreleased, "0.2.2", "0.2.2")
        self.assertIn("Span::new", active)
        self.assertNotIn("Span::start", active)
        self.assertNotIn("Span::end", active)
        self.assertNotIn("Mapping::clear", active)

        released_only = (
            "# Changelog\n\n"
            "## [0.2.2] — 2026-11-20\n\n"
            "### Changed\n\n- `Span::new` in 0.2.2.\n"
        )
        # Same version as base -> no unreleased section -> empty!
        self.assertEqual(cpa.active_changed_section(released_only, "0.2.2", "0.2.2"), "")
        # Newer version than base -> 0.2.2 Changed is active!
        self.assertIn("Span::new", cpa.active_changed_section(released_only, "0.2.2", "0.2.1"))

    def test_method_change_requires_method_name_not_just_type_name(self) -> None:
        reexports = cpa.find_root_reexports(self.BASE_ITEMS)
        entry = cpa.ApiBreak(
            kind="changed",
            feature_set="default",
            old="pub const fn ayaml::span::Span::start(self) -> ayaml::span::Marker",
            new="pub const fn ayaml::span::Span::start(self) -> usize",
        )
        self.assertFalse(cpa.is_documented(entry, ["- `Span` layout shrunk."], reexports, set()))
        self.assertFalse(cpa.is_documented(entry, ["- `Span::end()` changed."], reexports, set()))
        self.assertTrue(cpa.is_documented(entry, ["- `Span::start()` returns `usize`."], reexports, set()))
        self.assertTrue(cpa.is_documented(entry, ["- `span::Span::start` returns `usize`."], reexports, set()))

    def test_removing_whole_type_covers_its_methods_and_impls(self) -> None:
        reexports = cpa.find_root_reexports(self.BASE_ITEMS)
        removed_types = {"ayaml::span::Span"}
        method_break = cpa.ApiBreak(
            kind="removed",
            feature_set="default",
            old="pub const fn ayaml::span::Span::start(self) -> usize",
        )
        impl_break = cpa.ApiBreak(
            kind="removed",
            feature_set="default",
            old="impl core::fmt::Debug for ayaml::span::Span",
        )
        bullets = ["- Removed `Span`."]
        self.assertTrue(cpa.is_documented(method_break, bullets, reexports, removed_types))
        self.assertTrue(cpa.is_documented(impl_break, bullets, reexports, removed_types))
        # Naming `Span::start` does NOT cover removing `Span` itself.
        self.assertFalse(cpa.is_documented(impl_break, ["- Changed `Span::start`."], reexports, removed_types))

    def test_compat_and_non_root_submodule_items_require_module_prefix(self) -> None:
        reexports = cpa.find_root_reexports(self.BASE_ITEMS)
        compat_fn = cpa.ApiBreak(
            kind="changed",
            feature_set="libyaml-compat",
            old="pub fn ayaml::compat::parse_events_with_limits<'a>(&'a str, &ayaml::limits::Limits) -> core::result::Result<alloc::vec::Vec<ayaml::event::Event<'a>>, ayaml::error::Error>",
            new="pub fn ayaml::compat::parse_events_with_limits<'a>(&'a str, &ayaml::limits::Limits) -> core::result::Result<ayaml::EventStream<'a>, ayaml::error::Error>",
        )
        # Bare `parse_events_with_limits` must NOT satisfy `compat::parse_events_with_limits`.
        self.assertFalse(
            cpa.is_documented(compat_fn, ["- `parse_events_with_limits` returns a stream."], reexports, set())
        )
        self.assertTrue(
            cpa.is_documented(
                compat_fn, ["- `compat::parse_events_with_limits` returns `EventStream`."], reexports, set()
            )
        )

        non_root_type = cpa.ApiBreak(
            kind="removed",
            feature_set="default",
            old="pub struct ayaml::mapping::Iter<'a>",
        )
        self.assertFalse(cpa.is_documented(non_root_type, ["- Removed `Iter`."], reexports, set()))
        self.assertTrue(cpa.is_documented(non_root_type, ["- Removed `mapping::Iter`."], reexports, set()))

    def test_associated_type_and_trait_impl_distinguish_ref_targets_and_same_bullet(self) -> None:
        reexports = cpa.find_root_reexports(self.BASE_ITEMS)
        owned = cpa.ApiBreak(
            kind="changed",
            feature_set="default",
            old="pub type ayaml::mapping::Mapping::IntoIter = indexmap::map::iter::IntoIter<ayaml::value::Value, ayaml::value::Value>",
            new="pub type ayaml::mapping::Mapping::IntoIter = ayaml::mapping::IntoIter",
        )
        shared = cpa.ApiBreak(
            kind="changed",
            feature_set="default",
            old="pub type &'a ayaml::mapping::Mapping::IntoIter = indexmap::map::iter::Iter<'a, ayaml::value::Value, ayaml::value::Value>",
            new="pub type &'a ayaml::mapping::Mapping::IntoIter = ayaml::mapping::Iter<'a>",
        )
        mutable = cpa.ApiBreak(
            kind="changed",
            feature_set="default",
            old="pub type &'a mut ayaml::mapping::Mapping::IntoIter = indexmap::map::iter::IterMut<'a, ayaml::value::Value, ayaml::value::Value>",
            new="pub type &'a mut ayaml::mapping::Mapping::IntoIter = ayaml::mapping::IterMut<'a>",
        )
        # Separate bullets do NOT combine across bullets.
        split = ["- `Mapping` shrinks spare capacity.", "- `IntoIterator` docs updated."]
        self.assertFalse(cpa.is_documented(owned, split, reexports, set()))
        # Unrelated trait in same bullet does NOT satisfy `IntoIter`.
        wrong_trait = ["- `Mapping` implements `Debug` differently."]
        self.assertFalse(cpa.is_documented(owned, wrong_trait, reexports, set()))
        # Naming only `&Mapping` does NOT satisfy owned `Mapping` or `&mut Mapping`.
        only_shared = ["- `&Mapping` `IntoIterator` changed."]
        self.assertFalse(cpa.is_documented(owned, only_shared, reexports, set()))
        self.assertTrue(cpa.is_documented(shared, only_shared, reexports, set()))
        self.assertFalse(cpa.is_documented(mutable, only_shared, reexports, set()))
        # Naming all three targets with `IntoIterator` satisfies all three.
        all_three = [
            "- `Mapping`, `&Mapping`, and `&mut Mapping` give `mapping::IntoIter`, `mapping::Iter`, "
            "and `mapping::IterMut` for their `IntoIterator` implementations."
        ]
        self.assertTrue(cpa.is_documented(owned, all_three, reexports, set()))
        self.assertTrue(cpa.is_documented(shared, all_three, reexports, set()))
        self.assertTrue(cpa.is_documented(mutable, all_three, reexports, set()))

        trait_break = cpa.ApiBreak(
            kind="removed",
            feature_set="default",
            old="impl core::marker::Send for ayaml::mapping::Mapping",
        )
        self.assertFalse(cpa.is_documented(trait_break, all_three, reexports, set()))
        self.assertTrue(
            cpa.is_documented(trait_break, ["- `Mapping` is no longer `Send`."], reexports, set())
        )


class ManifestAndGitSelection(unittest.TestCase):
    def test_validate_manifest_features_rejects_unclassified_feature(self) -> None:
        ok = '[package]\nname = "ayaml"\n\n[features]\nbench-internals = []\nlibyaml-compat = []\n'
        cpa.validate_manifest_features(ok)
        extra = ok + "yaml-1-1 = []\n"
        with self.assertRaises(RuntimeError) as caught:
            cpa.validate_manifest_features(extra)
        self.assertIn("yaml-1-1", str(caught.exception))

    def test_last_release_tag_picks_highest_strictly_before_head(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch) / "repo"
            git(Path(scratch), "init", "-q", "-b", "main", str(root))
            for key, value in [
                ("user.name", "t"),
                ("user.email", "t@example.com"),
                ("commit.gpgsign", "false"),
                ("tag.gpgsign", "false"),
            ]:
                git(root, "config", key, value)
            (root / "f").write_text("1\n", encoding="utf-8")
            git(root, "add", "f")
            git(root, "commit", "-q", "-m", "c1")
            git(root, "tag", "-a", "-m", "v0.1.3", "v0.1.3")
            git(root, "tag", "not-a-release")

            (root / "f").write_text("2\n", encoding="utf-8")
            git(root, "commit", "-q", "-am", "c2")
            git(root, "tag", "-a", "-m", "v0.2.0", "v0.2.0")
            # At v0.2.0 itself, the last release strictly before HEAD is v0.1.3.
            self.assertEqual(cpa.last_release_tag(root, "HEAD"), "v0.1.3")

            (root / "f").write_text("3\n", encoding="utf-8")
            git(root, "commit", "-q", "-am", "c3")
            # Past v0.2.0, the last release tag is v0.2.0.
            self.assertEqual(cpa.last_release_tag(root, "HEAD"), "v0.2.0")


class PinConsistencyAndInstaller(unittest.TestCase):
    def test_pinned_toolchain_matches_across_workflow_and_scripts(self) -> None:
        installer = (TOOLS_DIR / "install-public-api.sh").read_text(encoding="utf-8")
        workflow = (REPO_ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        local = (TOOLS_DIR / "ci-local.sh").read_text(encoding="utf-8")

        m_inst = re.search(r'^TOOLCHAIN="([^"]+)"', installer, re.M)
        self.assertIsNotNone(m_inst)
        pinned = m_inst.group(1)
        self.assertEqual(cpa.TOOLCHAIN, pinned)
        self.assertRegex(installer, r'(?m)^VERSION="[0-9]+\.[0-9]+\.[0-9]+"', "pinned version")
        self.assertRegex(installer, r'(?m)^CRATE_SHA256="[0-9a-f]{64}"', "pinned sha256")
        self.assertIn(f"toolchain: {pinned}", workflow)
        self.assertIn(f'tc="{pinned}"', local)

    def test_installer_verifies_digest_caches_and_rebuilds_on_cli_change(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            tools = root / "tools"
            tools.mkdir()
            # Build a fake .crate tarball and patch a test copy of install-public-api.sh with its digest.
            crate_buf = io.BytesIO()
            with tarfile.open(fileobj=crate_buf, mode="w:gz") as tf:
                manifest = b'[package]\nname = "public-api"\nversion = "0.52.0"\n'
                info = tarfile.TarInfo("public-api-0.52.0/Cargo.toml")
                info.size = len(manifest)
                tf.addfile(info, io.BytesIO(manifest))
                src_info = tarfile.TarInfo("public-api-0.52.0/src/lib.rs")
                src_info.size = 0
                tf.addfile(src_info, io.BytesIO(b""))
            crate_bytes = crate_buf.getvalue()
            crate_sha = hashlib.sha256(crate_bytes).hexdigest()

            script_src = (TOOLS_DIR / "install-public-api.sh").read_text(encoding="utf-8")
            script_src = re.sub(
                r'^CRATE_SHA256="[0-9a-f]{64}"',
                f'CRATE_SHA256="{crate_sha}"',
                script_src,
                flags=re.M,
            )
            script = tools / "install-public-api.sh"
            script.write_text(script_src, encoding="utf-8")
            script.chmod(script.stat().st_mode | stat.S_IXUSR)
            cli_rs = tools / "public-api-cli.rs"
            cli_rs.write_text("fn main() {}\n", encoding="utf-8")

            served = root / "served.crate"
            served.write_bytes(crate_bytes)
            calls = root / "calls"
            calls.write_text("", encoding="utf-8")

            fake_bin = root / "bin"
            fake_bin.mkdir()
            curl = fake_bin / "curl"
            curl.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                f'echo "curl" >>"{calls}"\n'
                'out=""\n'
                'while [[ $# -gt 0 ]]; do\n'
                '  if [[ "$1" == "--output" ]]; then out="$2"; shift 2; else shift; fi\n'
                "done\n"
                f'cp "$SERVED_CRATE" "$out"\n',
                encoding="utf-8",
            )
            curl.chmod(0o755)
            cargo = fake_bin / "cargo"
            cargo.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                'if [[ "${2:-}" == "--version" ]]; then echo "cargo 1.0.0"; exit 0; fi\n'
                f'echo "cargo-build" >>"{calls}"\n'
                'mkdir -p "$CARGO_TARGET_DIR/release"\n'
                'cat >"$CARGO_TARGET_DIR/release/ayaml-public-api" <<\'BIN\'\n'
                "#!/usr/bin/env bash\n"
                'echo "ayaml-public-api ${FAKE_VER:-0.52.0}"\n'
                "BIN\n"
                'chmod +x "$CARGO_TARGET_DIR/release/ayaml-public-api"\n',
                encoding="utf-8",
            )
            cargo.chmod(0o755)

            install_dir = root / "installed"
            env = {
                **os.environ,
                "PATH": f"{fake_bin}{os.pathsep}{os.environ['PATH']}",
                "SERVED_CRATE": str(served),
            }

            # 1. Tampered crate bytes must be rejected by sha256 check.
            bad = root / "bad.crate"
            bad.write_bytes(b"corrupted")
            res = subprocess.run(
                [str(script), str(install_dir)],
                capture_output=True,
                text=True,
                env={**env, "SERVED_CRATE": str(bad)},
            )
            self.assertNotEqual(res.returncode, 0)
            self.assertFalse((install_dir / "ayaml-public-api").exists())

            # 2. Good crate builds and installs.
            calls.write_text("", encoding="utf-8")
            res = subprocess.run([str(script), str(install_dir)], capture_output=True, text=True, env=env)
            self.assertEqual(res.returncode, 0, res.stderr)
            self.assertEqual(res.stdout.strip(), str(install_dir))
            self.assertEqual(calls.read_text().splitlines(), ["curl", "cargo-build"])

            # 3. Second call reuses cache with zero curl or cargo builds.
            calls.write_text("", encoding="utf-8")
            res = subprocess.run([str(script), str(install_dir)], capture_output=True, text=True, env=env)
            self.assertEqual(res.returncode, 0, res.stderr)
            self.assertEqual(calls.read_text().splitlines(), [])

            # 4. Editing public-api-cli.rs triggers a rebuild (reusing kept crate without curl).
            cli_rs.write_text("fn main() { /* changed */ }\n", encoding="utf-8")
            calls.write_text("", encoding="utf-8")
            res = subprocess.run([str(script), str(install_dir)], capture_output=True, text=True, env=env)
            self.assertEqual(res.returncode, 0, res.stderr)
            self.assertEqual(calls.read_text().splitlines(), ["cargo-build"])


@unittest.skipUnless(
    os.environ.get("AYAML_TEST_PUBLIC_API_LIVE") == "1",
    "set AYAML_TEST_PUBLIC_API_LIVE=1 to run live rustdoc comparison against v0.1.3 and v0.2.0",
)
class LiveReleaseRegression(unittest.TestCase):
    """Verify criterion 3: comparing v0.2.0 against v0.1.3 reports all four documented breaks."""

    def test_v0_1_3_to_v0_2_0_reports_all_four_breaks_and_matches_changelog(self) -> None:
        self.assertIsNotNone(shutil.which(cpa.TOOL_BIN), f"{cpa.TOOL_BIN} must be on PATH")

        # 1. With no changelog, exit 1 and report all four breaks of 0.2.0.
        raw = subprocess.run(
            [
                sys.executable,
                str(TOOLS_DIR / "check_public_api.py"),
                "--base",
                "v0.1.3",
                "--head",
                "v0.2.0",
                "--no-changelog",
                "--json",
            ],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
        )
        self.assertEqual(raw.returncode, 1, raw.stderr)
        data = json.loads(raw.stdout)
        undoc = data["undocumented"]
        by_feat: dict[str, set[str]] = {}
        for entry in undoc:
            by_feat.setdefault(entry["feature_set"], set()).add(entry["old"])

        # Break 1: Span::start and Span::end return Marker -> usize (both feature sets).
        for feat in ("default", "libyaml-compat"):
            self.assertIn("pub const fn ayaml::span::Span::start(self) -> ayaml::span::Marker", by_feat[feat])
            self.assertIn("pub const fn ayaml::span::Span::end(self) -> ayaml::span::Marker", by_feat[feat])
            # Break 2: Span::new and Span::empty take Marker -> usize.
            self.assertIn(
                "pub const fn ayaml::span::Span::new(ayaml::span::Marker, ayaml::span::Marker) -> Self",
                by_feat[feat],
            )
            self.assertIn("pub const fn ayaml::span::Span::empty(ayaml::span::Marker) -> Self", by_feat[feat])
            # Break 4: Mapping's three IntoIterator associated types.
            self.assertIn(
                "pub type ayaml::mapping::Mapping::IntoIter = indexmap::map::iter::IntoIter<ayaml::value::Value, ayaml::value::Value>",
                by_feat[feat],
            )
            self.assertIn(
                "pub type &'a ayaml::mapping::Mapping::IntoIter = indexmap::map::iter::Iter<'a, ayaml::value::Value, ayaml::value::Value>",
                by_feat[feat],
            )
            self.assertIn(
                "pub type &'a mut ayaml::mapping::Mapping::IntoIter = indexmap::map::iter::IterMut<'a, ayaml::value::Value, ayaml::value::Value>",
                by_feat[feat],
            )

        # Break 3: compat::parse_events_with_limits returns Vec<Event> -> EventStream (compat only).
        compat_break = (
            "pub fn ayaml::compat::parse_events_with_limits<'a>(&'a str, &ayaml::limits::Limits) -> "
            "core::result::Result<alloc::vec::Vec<ayaml::event::Event<'a>>, ayaml::error::Error>"
        )
        self.assertNotIn(compat_break, by_feat["default"])
        self.assertIn(compat_break, by_feat["libyaml-compat"])

        # 2. With v0.2.0's actual CHANGELOG.md, every break is named in Changed -> exit 0.
        ok = subprocess.run(
            [sys.executable, str(TOOLS_DIR / "check_public_api.py"), "--base", "v0.1.3", "--head", "v0.2.0"],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
        )
        self.assertEqual(ok.returncode, 0, ok.stderr)

        # 3. Removing any one of the four breaks from v0.2.0's CHANGELOG.md makes it fail.
        v020_changelog = git(REPO_ROOT, "show", "v0.2.0:CHANGELOG.md")
        for label, needle in (
            ("Span::start", "`Span::start()`"),
            ("Span::new", "`Span::new`"),
            ("compat::parse_events_with_limits", "`compat::parse_events_with_limits`"),
            ("&mut Mapping IntoIterator", "`&mut Mapping`"),
        ):
            with self.subTest(omitted=label), tempfile.NamedTemporaryFile(
                "w", suffix=".md", encoding="utf-8", delete=False
            ) as tmp:
                tmp_path = Path(tmp.name)
                try:
                    tmp.write(v020_changelog.replace(needle, "omitted"))
                    tmp.flush()
                    res = subprocess.run(
                        [
                            sys.executable,
                            str(TOOLS_DIR / "check_public_api.py"),
                            "--base",
                            "v0.1.3",
                            "--head",
                            "v0.2.0",
                            "--changelog",
                            str(tmp_path),
                        ],
                        cwd=REPO_ROOT,
                        capture_output=True,
                        text=True,
                    )
                    self.assertEqual(res.returncode, 1, f"expected failure when {label} omitted")
                finally:
                    tmp_path.unlink(missing_ok=True)


if __name__ == "__main__":
    unittest.main()
