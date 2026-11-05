#!/usr/bin/env python3
"""Prove that each kind of content is rejected and a clean change passes.

Every name here is made up. The real pattern list is private, so these tests
exercise the gate's behavior, not the list's contents.
"""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest

GATE_DIR = Path(__file__).resolve().parent
GATE = GATE_DIR / "gate.py"
HOOK = GATE_DIR.parent / "hooks" / "pre-push"
INSTALL_HOOK = GATE_DIR / "install-hook.sh"
REPO_ROOT = GATE_DIR.parent.parent

PATTERNS = """\
# id class scope regex
h1 host tree \\bfake-host-alias\\b
r1 runner tree \\bfake-runner-7\\b
p1 repo tree \\bfake-org/fake-private\\b
i1 identity tree fake-person
c1 comparison added \\bother_yaml_crate\\b
z1 end tree (?!)
"""

# Split so that this file holds no complete personal path.
HOME_PATH = "/ho" + "me/someone/projects/x"
MACOS_PATH = "/Us" + "ers/someone/x"
WINDOWS_PATH = "C:" + "\\Users\\someone"
TILDE_PATH = "~/" + "projects/x"
CLAUDE_PATH = "~/" + ".claude/x"
HOME_VAR_PATH = "$HOME" + "/.tmp/x"
ROOT_PATH = "/ro" + "ot/x"
SCANNER_OPT_OUT = "gitleaks:" + "allow"

# Assembled at run time so that this file is not itself a finding.
FAKE_CREDENTIAL = "AKIA" + "QYLPMN5HHHFPZAM2"

LEAKS = {
    "host": "reach fake-host-alias over ssh\n",
    "runner": "runs on fake-runner-7\n",
    "repo": "see fake-org/fake-private\n",
    "identity": "by fake-person\n",
    "comparison": "compare with other_yaml_crate\n",
    "path-home": f"open {HOME_PATH}\n",
    "path-macos": f"open {MACOS_PATH}\n",
    "path-windows": f"open {WINDOWS_PATH}\n",
    "path-tilde": f"open {TILDE_PATH}\n",
    "path-tilde-claude": f"open {CLAUDE_PATH}\n",
    "path-home-var": f"open {HOME_VAR_PATH}\n",
    "path-root": f"open {ROOT_PATH}\n",
    "scanner-opt-out": f"key = 1 # {SCANNER_OPT_OUT}\n",
}

SENSITIVE_OUTPUT = ("fake-host-alias", "fake-runner-7", "fake-org", "fake-person", "other_yaml_crate", "someone")


def stub_scanner(directory: Path, version: str = "", failing: bool = False) -> str:
    """Write a stand-in for gitleaks, so most tests need no download."""
    pins = (GATE_DIR / "gitleaks.env").read_text(encoding="utf-8")
    pinned = next(line.split("=", 1)[1] for line in pins.splitlines() if line.startswith("GITLEAKS_VERSION="))
    path = directory / "gitleaks-stub"
    path.write_text(
        "#!/bin/sh\n"
        f'if [ "$1" = version ]; then echo {version or pinned}; exit 0; fi\n'
        f"exit {1 if failing else 0}\n",
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return str(path)


class GateCase(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="leak-gate-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith(("GIT_", "AYAML_"))
        }
        self.env.update(
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CONFIG_NOSYSTEM="1",
            GIT_AUTHOR_NAME="Test",
            GIT_AUTHOR_EMAIL="test@example.invalid",
            GIT_COMMITTER_NAME="Test",
            GIT_COMMITTER_EMAIL="test@example.invalid",
            AYAML_LEAK_PATTERNS=PATTERNS,
            AYAML_GITLEAKS=stub_scanner(self.root),
        )
        self.git("init", "-q", "-b", "main")
        self.write("README.md", "clean\n")
        self.base = self.commit("Start")

    def git(self, *args: str, cwd: Path | None = None, check: bool = True) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["git", *args],
            cwd=cwd or self.repo,
            env=self.env,
            text=True,
            capture_output=True,
            check=check,
        )

    def write(self, name: str, content: str) -> None:
        path = self.repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def commit(self, message: str = "Change") -> str:
        self.git("add", "-A")
        self.git("commit", "-q", "--allow-empty", "-m", message)
        return self.git("rev-parse", "HEAD").stdout.strip()

    def gate(self, *args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(GATE), *args],
            cwd=self.repo,
            env={**self.env, **(env or {})},
            text=True,
            capture_output=True,
        )

    def scan(self, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
        return self.gate("scan", "--head", "HEAD", "--not", self.base, env=env)

    def assert_rejected(self, result: subprocess.CompletedProcess[str], where: str) -> None:
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(where, result.stderr)
        self.assert_no_disclosure(result)

    def assert_no_disclosure(self, result: subprocess.CompletedProcess[str]) -> None:
        output = result.stdout + result.stderr
        for text in SENSITIVE_OUTPUT:
            self.assertNotIn(text, output)


class Classes(GateCase):
    def test_clean_change_passes(self) -> None:
        self.write("docs.md", "A plain description of libyaml behavior.\n")
        self.commit("Describe behavior")
        result = self.scan()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_each_class_is_rejected_in_an_added_line(self) -> None:
        for name, content in LEAKS.items():
            with self.subTest(name):
                self.git("checkout", "-q", "-B", name, self.base)
                self.write("new.txt", content)
                self.commit()
                self.assert_rejected(self.scan(), "new.txt")

    def test_each_class_is_rejected_in_a_commit_message(self) -> None:
        for name, content in LEAKS.items():
            with self.subTest(name):
                self.git("checkout", "-q", "-B", name, self.base)
                self.commit(f"Change\n\n{content}")
                self.assert_rejected(self.scan(), "message or metadata")

    def test_identity_in_commit_metadata_is_rejected(self) -> None:
        self.env["GIT_AUTHOR_EMAIL"] = "fake-person@example.invalid"
        self.commit()
        self.assert_rejected(self.scan(), "message or metadata")

    def test_file_name_is_rejected(self) -> None:
        self.write("notes/fake-host-alias.md", "clean\n")
        self.commit()
        self.assert_rejected(self.scan(), "path notes/")

    def test_scanner_report_is_redacted(self) -> None:
        directory = self.root / "noisy"
        directory.mkdir()
        scanner = Path(stub_scanner(directory))
        script = scanner.read_text(encoding="utf-8").replace("exit 0\n", "exit 0\n", 1)
        script = script.rsplit("exit", 1)[0] + "echo File: notes/fake-host-alias.md\nexit 1\n"
        scanner.write_text(script, encoding="utf-8")
        self.commit()
        result = self.scan({"AYAML_GITLEAKS": str(scanner)})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("notes/[redacted].md", result.stderr)
        self.assert_no_disclosure(result)

    def test_match_is_case_insensitive(self) -> None:
        self.write("new.txt", "FAKE-HOST-ALIAS\n")
        self.commit()
        self.assert_rejected(self.scan(), "new.txt")


class Scopes(GateCase):
    def test_tree_scope_sees_content_older_than_the_range(self) -> None:
        self.write("old.txt", LEAKS["host"])
        older = self.commit()
        self.write("other.txt", "clean\n")
        self.commit()
        result = self.gate("scan", "--head", "HEAD", "--not", older)
        self.assert_rejected(result, "tree old.txt")

    def test_added_scope_allows_old_content_and_rejects_new(self) -> None:
        self.write("old.txt", LEAKS["comparison"])
        older = self.commit()
        self.write("other.txt", "clean\n")
        self.commit()
        result = self.gate("scan", "--head", "HEAD", "--not", older)
        self.assertEqual(result.returncode, 0, result.stderr)

        self.write("other.txt", LEAKS["comparison"])
        self.commit()
        result = self.gate("scan", "--head", "HEAD", "--not", older)
        self.assert_rejected(result, "other.txt")

    def test_added_scope_allows_removing_a_line(self) -> None:
        self.write("old.txt", LEAKS["comparison"] + "kept\n")
        older = self.commit()
        self.write("old.txt", "kept\n")
        self.commit()
        result = self.gate("scan", "--head", "HEAD", "--not", older)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_content_removed_by_a_later_commit_is_still_rejected(self) -> None:
        self.write("new.txt", LEAKS["host"])
        self.commit()
        self.write("new.txt", "clean\n")
        self.commit()
        self.assert_rejected(self.scan(), "new.txt")

    def test_unresolvable_exclusion_widens_the_scan(self) -> None:
        self.write("new.txt", LEAKS["comparison"])
        self.commit()
        result = self.gate("scan", "--head", "HEAD", "--not", "0123456789" * 4)
        self.assert_rejected(result, "new.txt")

    def test_line_numbers_follow_the_new_file(self) -> None:
        self.write("new.txt", "one\ntwo\n")
        older = self.commit()
        self.write("new.txt", f"one\ntwo\nthree\n{LEAKS['comparison']}")
        self.commit()
        result = self.gate("scan", "--head", "HEAD", "--not", older)
        self.assert_rejected(result, "new.txt:4:")

    def test_binary_content_is_scanned(self) -> None:
        (self.repo / "blob.bin").write_bytes(b"\0\xff" + LEAKS["host"].encode() + b"\0")
        self.commit()
        self.assert_rejected(self.scan(), "blob.bin")

    def test_utf16_content_is_scanned(self) -> None:
        for encoding in ("utf-16", "utf-16-le", "utf-16-be", "utf-32"):
            with self.subTest(encoding):
                (self.repo / "wide.txt").write_bytes(LEAKS["host"].encode(encoding))
                self.commit()
                self.assert_rejected(self.scan(), "wide.txt")

    def test_nested_annotated_tags_are_scanned_with_no_new_commits(self) -> None:
        self.git("tag", "-a", "inner", "-m", LEAKS["host"])
        self.git("tag", "-a", "outer", "inner", "-m", "clean")
        result = self.gate("scan", "--head", "outer", "--not", "HEAD")
        self.assert_rejected(result, "annotated tag")

    def test_branch_name_is_scanned(self) -> None:
        result = self.gate("scan", "--head", "HEAD", "--not", "HEAD", "--ref", "refs/heads/fake-host-alias")
        self.assert_rejected(result, "branch or tag name")

    def test_merge_commit_message_is_scanned(self) -> None:
        self.git("checkout", "-q", "-b", "side")
        self.write("side.txt", "clean\n")
        self.commit()
        self.git("checkout", "-q", "main")
        self.write("main.txt", "clean\n")
        self.commit()
        self.git("merge", "-q", "--no-ff", "-m", f"Merge\n\n{LEAKS['host']}", "side")
        self.assert_rejected(self.scan(), "message or metadata")


class FailsClosed(GateCase):
    def assert_cannot_run(self, result: subprocess.CompletedProcess[str]) -> None:
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("could not run", result.stderr)
        self.assert_no_disclosure(result)

    def test_missing_list(self) -> None:
        self.assert_cannot_run(self.scan({"AYAML_LEAK_PATTERNS": ""}))
        self.assert_cannot_run(self.gate("text", "X", env={"AYAML_LEAK_PATTERNS": ""}))

    def test_unreadable_list_file(self) -> None:
        result = self.scan({"AYAML_LEAK_PATTERNS": "", "AYAML_LEAK_PATTERNS_FILE": str(self.root / "absent")})
        self.assert_cannot_run(result)

    def test_list_from_git_config(self) -> None:
        path = self.root / "list.txt"
        path.write_text(PATTERNS, encoding="utf-8")
        self.git("config", "ayaml.leakPatterns", str(path))
        self.write("new.txt", LEAKS["host"])
        self.commit()
        self.assert_rejected(self.scan({"AYAML_LEAK_PATTERNS": ""}), "new.txt")

    def test_truncated_list(self) -> None:
        for removed in ("h1 ", "r1 ", "p1 ", "c1 "):
            with self.subTest(removed):
                partial = "\n".join(line for line in PATTERNS.splitlines() if not line.startswith(removed))
                self.assert_cannot_run(self.scan({"AYAML_LEAK_PATTERNS": partial}))

    def test_list_cut_short_looks_truncated(self) -> None:
        partial = "\n".join(line for line in PATTERNS.splitlines() if not line.startswith("z1 "))
        self.assert_cannot_run(self.scan({"AYAML_LEAK_PATTERNS": partial}))

    def test_path_leaving_the_tree_fails_closed_without_naming_it(self) -> None:
        self.write("new.txt", "clean\n")
        blob = self.git("hash-object", "-w", "new.txt").stdout.strip()
        entry = b"100644 ../fake-host-alias\0" + bytes.fromhex(blob)
        tree = subprocess.run(
            ["git", "hash-object", "-t", "tree", "-w", "--stdin", "--literally"],
            cwd=self.repo,
            env=self.env,
            input=entry,
            capture_output=True,
            check=True,
        ).stdout.decode().strip()
        commit = self.git("commit-tree", tree, "-m", "Change").stdout.strip()
        self.assert_cannot_run(self.gate("scan", "--head", commit, "--not", "HEAD"))

    def test_malformed_list(self) -> None:
        for bad in ("x host tree", "x host nowhere fake-person", "x host tree fake-person(", "h1 host tree fake-person"):
            with self.subTest(bad):
                self.assert_cannot_run(self.scan({"AYAML_LEAK_PATTERNS": f"{PATTERNS}\n{bad}\n"}))

    def test_missing_scanner(self) -> None:
        path = str(self.root / "bin-empty")
        os.mkdir(path)
        git_dir = str(Path(shutil.which("git") or "git").parent)
        result = self.scan({"AYAML_GITLEAKS": "", "PATH": f"{path}:{git_dir}"})
        self.assert_cannot_run(result)

    def test_unexecutable_scanner(self) -> None:
        self.assert_cannot_run(self.scan({"AYAML_GITLEAKS": str(self.root / "absent")}))

    def test_wrong_scanner_version(self) -> None:
        directory = self.root / "wrong"
        directory.mkdir()
        self.assert_cannot_run(self.scan({"AYAML_GITLEAKS": stub_scanner(directory, version="0.0.1")}))

    def test_scanner_finding_rejects(self) -> None:
        directory = self.root / "failing"
        directory.mkdir()
        self.commit()
        result = self.scan({"AYAML_GITLEAKS": stub_scanner(directory, failing=True)})
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_unknown_revision(self) -> None:
        result = self.gate("scan", "--head", "no-such-revision")
        self.assert_cannot_run(result)


class Text(GateCase):
    def test_title_and_body(self) -> None:
        for name, content in LEAKS.items():
            with self.subTest(name):
                result = self.gate("text", "PR_TITLE", "PR_BODY", env={"PR_TITLE": "Fine", "PR_BODY": content})
                self.assert_rejected(result, "PR_BODY")
                result = self.gate("text", "PR_TITLE", "PR_BODY", env={"PR_TITLE": content, "PR_BODY": "Fine"})
                self.assert_rejected(result, "PR_TITLE")

    def test_clean_and_unset_text_passes(self) -> None:
        result = self.gate("text", "PR_TITLE", "PR_BODY", env={"PR_TITLE": "A plain title"})
        self.assertEqual(result.returncode, 0, result.stderr)


class Hook(GateCase):
    def setUp(self) -> None:
        super().setUp()
        self.bare = self.root / "remote.git"
        self.git("init", "-q", "--bare", "-b", "main", str(self.bare))
        self.git("remote", "add", "origin", str(self.bare))
        self.git("push", "-q", "origin", "main")
        self.git("config", "core.hooksPath", str(HOOK.parent))

    def remote_refs(self) -> str:
        return self.git("ls-remote", str(self.bare)).stdout

    def test_clean_push_lands(self) -> None:
        self.write("new.txt", "clean\n")
        head = self.commit()
        result = self.git("push", "origin", "main", check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(head, self.remote_refs())

    def test_leak_stays_local(self) -> None:
        self.write("new.txt", LEAKS["host"])
        head = self.commit()
        result = self.git("push", "origin", "main", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("leak gate", result.stderr)
        self.assertNotIn(head, self.remote_refs())
        self.assert_no_disclosure(result)

    def test_new_branch_with_leak_stays_local(self) -> None:
        self.git("checkout", "-q", "-b", "feature")
        self.write("new.txt", LEAKS["comparison"])
        head = self.commit()
        result = self.git("push", "origin", "feature", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(head, self.remote_refs())

    def test_missing_list_fails_closed(self) -> None:
        self.env["AYAML_LEAK_PATTERNS"] = ""
        head = self.commit()
        result = self.git("push", "origin", "main", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(head, self.remote_refs())

    def test_notes_and_other_refs_stay_local(self) -> None:
        self.git("notes", "add", "-m", "local record", "HEAD")
        result = self.git("push", "origin", "refs/notes/commits", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("refusing to publish", result.stderr)
        self.assertNotIn("refs/notes", self.remote_refs())

        self.git("tag", "not-a-release")
        result = self.git("push", "origin", "refs/tags/not-a-release", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("not-a-release", self.remote_refs())

    def test_release_tag_and_its_message_are_checked(self) -> None:
        self.git("tag", "v1.0.0")
        result = self.git("push", "origin", "refs/tags/v1.0.0", check=False)
        self.assertEqual(result.returncode, 0, result.stderr)

        self.git("tag", "-a", "v1.0.1", "-m", LEAKS["host"])
        result = self.git("push", "origin", "refs/tags/v1.0.1", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("v1.0.1", self.remote_refs())

    def test_deletion_needs_no_scan(self) -> None:
        self.git("push", "-q", "origin", "main:refs/heads/to-delete")
        self.env["AYAML_LEAK_PATTERNS"] = ""
        result = self.git("push", "origin", "--delete", "to-delete", check=False)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_linked_worktree_runs_the_hook(self) -> None:
        worktree = self.root / "linked"
        self.git("worktree", "add", "-q", "-b", "linked", str(worktree))
        (worktree / "new.txt").write_text(LEAKS["host"], encoding="utf-8")
        self.git("add", "-A", cwd=worktree)
        self.git("commit", "-q", "-m", "Change", cwd=worktree)
        result = self.git("push", "origin", "linked", cwd=worktree, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("leak gate", result.stderr)


class InstalledHook(GateCase):
    """The installed hook runs the gate from origin/main, whatever is checked out."""

    def setUp(self) -> None:
        super().setUp()
        self.before_gate = self.base
        for name in ("gate.py", "gitleaks.env", "install-hook.sh"):
            self.copy(GATE_DIR / name, f"tools/leak-scan/{name}")
        self.copy(HOOK, "tools/hooks/pre-push")
        self.copy(REPO_ROOT / ".gitleaks.toml", ".gitleaks.toml")
        self.commit("Add the gate")
        self.bare = self.root / "remote.git"
        self.git("init", "-q", "--bare", "-b", "main", str(self.bare))
        self.git("remote", "add", "origin", str(self.bare))
        self.git("push", "-q", "origin", "main")
        self.git("config", "core.hooksPath", "tools/hooks")
        self.install()

    def copy(self, source: Path, name: str) -> None:
        target = self.repo / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)

    def install(self) -> None:
        subprocess.run(
            [str(self.repo / "tools/leak-scan/install-hook.sh")],
            cwd=self.repo,
            env=self.env,
            check=True,
            capture_output=True,
        )

    def remote_refs(self) -> str:
        return self.git("ls-remote", str(self.bare)).stdout

    def test_install_replaces_a_relative_hooks_path(self) -> None:
        self.assertEqual(self.git("config", "core.hooksPath", check=False).stdout, "")

    def test_branch_from_before_the_gate_is_still_scanned(self) -> None:
        self.git("checkout", "-q", "-b", "old", self.before_gate)
        self.write("new.txt", LEAKS["host"])
        head = self.commit()
        result = self.git("push", "origin", "old", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("leak gate", result.stderr)
        self.assertNotIn(head, self.remote_refs())
        self.assert_no_disclosure(result)

    def test_clean_branch_from_before_the_gate_lands(self) -> None:
        self.git("checkout", "-q", "-b", "old", self.before_gate)
        self.write("new.txt", "clean\n")
        head = self.commit()
        result = self.git("push", "origin", "old", check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(head, self.remote_refs())

    def test_branch_that_disables_the_gate_is_still_scanned(self) -> None:
        self.git("checkout", "-q", "-b", "weakened")
        self.write("tools/leak-scan/gate.py", "raise SystemExit(0)\n")
        self.write("new.txt", LEAKS["host"])
        head = self.commit()
        result = self.git("push", "origin", "weakened", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(head, self.remote_refs())

    def test_missing_main_fails_closed(self) -> None:
        self.git("update-ref", "-d", "refs/remotes/origin/main")
        self.git("checkout", "-q", "-b", "feature")
        head = self.commit()
        result = self.git("push", "origin", "feature", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("did not run", result.stderr)
        self.assertNotIn(head, self.remote_refs())

    def test_branch_name_is_scanned(self) -> None:
        head = self.commit()
        result = self.git("push", "origin", "HEAD:refs/heads/fake-host-alias", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(head, self.remote_refs())
        self.assert_no_disclosure(result)

    def test_linked_worktree_runs_the_hook(self) -> None:
        worktree = self.root / "linked"
        self.git("worktree", "add", "-q", "-b", "linked", str(worktree))
        (worktree / "new.txt").write_text(LEAKS["host"], encoding="utf-8")
        self.git("add", "-A", cwd=worktree)
        self.git("commit", "-q", "-m", "Change", cwd=worktree)
        result = self.git("push", "origin", "linked", cwd=worktree, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("leak gate", result.stderr)

    def test_hook_leaves_no_scratch_behind(self) -> None:
        self.write("new.txt", "clean\n")
        self.commit()
        self.git("push", "-q", "origin", "HEAD:refs/heads/feature")
        self.assertEqual(list((self.repo / ".git").glob("leak-gate.*")), [])


@unittest.skipUnless(os.environ.get("AYAML_GITLEAKS"), "AYAML_GITLEAKS is not set")
class RealScanner(GateCase):
    def setUp(self) -> None:
        super().setUp()
        self.env["AYAML_GITLEAKS"] = os.environ["AYAML_GITLEAKS"]

    def test_clean_change_passes(self) -> None:
        self.write("new.txt", "clean\n")
        self.commit()
        result = self.scan()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_credential_in_a_file_is_rejected(self) -> None:
        self.write("new.txt", f"key = {FAKE_CREDENTIAL}\n")
        self.commit()
        result = self.scan()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertNotIn(FAKE_CREDENTIAL, result.stdout + result.stderr)

    def test_credential_in_a_later_removed_commit_is_rejected(self) -> None:
        self.write("new.txt", f"key = {FAKE_CREDENTIAL}\n")
        self.commit()
        self.write("new.txt", "clean\n")
        self.commit()
        result = self.scan()
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_credential_in_a_commit_message_is_rejected(self) -> None:
        self.commit(f"Change\n\nkey = {FAKE_CREDENTIAL}")
        result = self.scan()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertNotIn(FAKE_CREDENTIAL, result.stdout + result.stderr)

    def test_credential_in_a_skipped_file_type_is_rejected(self) -> None:
        self.write("notes.pdf", f"key = {FAKE_CREDENTIAL}\n")
        self.commit()
        self.git("checkout", "-q", "-b", "later")
        self.write("other.txt", "clean\n")
        self.commit()
        result = self.gate("scan", "--head", "HEAD", "--not", "HEAD~1")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertNotIn(FAKE_CREDENTIAL, result.stdout + result.stderr)

    def test_utf16_credential_is_rejected(self) -> None:
        (self.repo / "wide.txt").write_bytes(f"key = {FAKE_CREDENTIAL}\n".encode("utf-16"))
        self.commit()
        result = self.scan()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertNotIn(FAKE_CREDENTIAL, result.stdout + result.stderr)

    def test_credential_only_in_a_merge_is_rejected(self) -> None:
        self.git("checkout", "-q", "-b", "side")
        self.write("side.txt", "clean\n")
        self.commit()
        self.git("checkout", "-q", "main")
        self.git("merge", "-q", "--no-ff", "--no-commit", "side")
        self.write("merged.txt", f"key = {FAKE_CREDENTIAL}\n")
        self.commit("Merge")
        result = self.scan()
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_credential_in_a_tag_with_no_new_commits_is_rejected(self) -> None:
        self.git("tag", "-a", "v1.0.0", "-m", f"key = {FAKE_CREDENTIAL}")
        result = self.gate("scan", "--head", "v1.0.0", "--not", "HEAD")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertNotIn(FAKE_CREDENTIAL, result.stdout + result.stderr)

    def test_scan_does_not_depend_on_git_output_settings(self) -> None:
        self.git("config", "color.ui", "always")
        self.write("new.txt", f"key = {FAKE_CREDENTIAL}\n")
        self.commit()
        self.write("new.txt", "clean\n")
        self.commit()
        result = self.scan()
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_credential_in_pull_request_text_is_rejected(self) -> None:
        result = self.gate("text", "PR_BODY", env={"PR_BODY": f"key = {FAKE_CREDENTIAL}"})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertNotIn(FAKE_CREDENTIAL, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
