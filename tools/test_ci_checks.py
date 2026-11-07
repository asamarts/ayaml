#!/usr/bin/env python3
"""Tests for the checks that decide what CI's collected data means."""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
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
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_instructions
import check_release
import check_scheduled_runs
import check_unsafe_census

NO_UNSAFE = {
    kind: {"safe": 1, "unsafe_": 0}
    for kind in ("functions", "exprs", "item_impls", "item_traits", "methods")
}


def package(name: str, unsafe_exprs: int = 0, forbids: bool = False) -> dict:
    used = dict(NO_UNSAFE)
    used["exprs"] = {"safe": 1, "unsafe_": unsafe_exprs}
    return {
        "package": {"id": {"name": name}},
        "unsafety": {"forbids_unsafe": forbids, "used": used},
    }


def census(*packages: dict, unscanned: tuple[str, ...] = (), no_metrics: tuple[str, ...] = ()) -> dict:
    return {
        "packages": list(packages),
        "packages_without_metrics": list(no_metrics),
        "used_but_not_scanned_files": list(unscanned),
    }


AYAML = package("ayaml", forbids=True)
SERDE_OUTPUT = "/w/target/debug/build/serde_core-c4163549dd34a516/out/private.rs"


class Census(unittest.TestCase):
    def assert_problem(self, data: dict, text: str) -> None:
        found = check_unsafe_census.problems(data)
        self.assertEqual(len(found), 1, found)
        self.assertIn(text, found[0])

    def test_a_good_census_passes(self) -> None:
        data = census(AYAML, package("indexmap", 80), package("quote"), unscanned=(SERDE_OUTPUT,))
        self.assertEqual(check_unsafe_census.problems(data), [])

    def test_unsafe_code_in_ayaml_fails(self) -> None:
        self.assert_problem(census(package("ayaml", 1, forbids=True)), "ayaml has 1 unsafe")

    def test_ayaml_has_to_forbid_unsafe_code(self) -> None:
        self.assert_problem(census(package("ayaml")), "doesn't forbid")

    def test_ayaml_has_to_be_there_once(self) -> None:
        self.assert_problem(census(package("quote")), "0 entries for ayaml")
        self.assert_problem(census(AYAML, AYAML), "2 entries for ayaml")

    def test_a_new_crate_with_unsafe_code_fails(self) -> None:
        self.assert_problem(census(AYAML, package("smallvec", 3)), "smallvec has 3 unsafe")

    def test_any_kind_of_unsafe_item_counts(self) -> None:
        for kind in NO_UNSAFE:
            crate = package("smallvec")
            crate["unsafety"]["used"] = {**NO_UNSAFE, kind: {"safe": 0, "unsafe_": 1}}
            self.assert_problem(census(AYAML, crate), "smallvec has 1 unsafe")

    def test_a_crate_without_numbers_fails(self) -> None:
        self.assert_problem(census(AYAML, no_metrics=("smallvec",)), "no numbers for smallvec")

    def test_an_unexpected_unscanned_file_fails(self) -> None:
        for path in (
            "/w/crates/ayaml/src/lib.rs",
            "/w/target/debug/build/smallvec-0a1b/out/private.rs",
            "/w/target/debug/build/serde-0a1b/out/other.rs",
        ):
            self.assert_problem(census(AYAML, unscanned=(path,)), "didn't scan")

    def run_main(self, content: str | None) -> int:
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "geiger.json")
            if content is not None:
                Path(path).write_text(content, encoding="utf-8")
            with mock.patch("builtins.print"):
                return check_unsafe_census.main([path])

    def test_main_reports_each_outcome(self) -> None:
        self.assertEqual(self.run_main(json.dumps(census(AYAML))), 0)
        self.assertEqual(self.run_main(json.dumps(census(package("ayaml")))), 1)

    def test_what_cannot_be_read_as_a_census_fails(self) -> None:
        for content in (None, "", "not json", "{}", "[]", '{"packages": [{}]}', '{"packages": 1}'):
            self.assertEqual(self.run_main(content), 2, content)


NOW = datetime(2026, 11, 6, 15, 0, tzinfo=timezone.utc)


def run(hours_ago: float, conclusion: str = "success", status: str = "completed") -> dict:
    created = NOW - timedelta(hours=hours_ago)
    return {
        "conclusion": conclusion,
        "createdAt": created.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "databaseId": 1,
        "event": "schedule",
        "status": status,
        "url": "https://example.invalid/run",
    }


class ScheduledRuns(unittest.TestCase):
    def problems(self, *runs: dict) -> list[str]:
        return check_scheduled_runs.problems("fuzz.yml", list(runs), NOW)

    def test_a_recent_success_passes(self) -> None:
        self.assertEqual(self.problems(run(3)), [])
        self.assertEqual(self.problems(run(3), run(27)), [])

    def test_no_runs_at_all_is_a_problem(self) -> None:
        self.assertIn("no run in the last 36 hours", self.problems()[0])

    def test_a_missed_run_is_a_problem(self) -> None:
        self.assertEqual(self.problems(run(35.9)), [])
        self.assertIn("no run", self.problems(run(36.1))[0])

    def test_a_failed_run_is_a_problem(self) -> None:
        for conclusion in ("failure", "cancelled", "timed_out", "startup_failure"):
            found = self.problems(run(3, conclusion))
            self.assertEqual(len(found), 1)
            self.assertIn(f"ended with {conclusion}", found[0])

    def test_a_failure_behind_a_later_success_is_still_reported(self) -> None:
        self.assertEqual(len(self.problems(run(3), run(27, "failure"))), 1)

    def test_an_old_failure_is_forgotten(self) -> None:
        self.assertEqual(self.problems(run(3), run(37, "failure")), [])

    def test_a_failed_run_started_by_hand_is_not_a_problem(self) -> None:
        by_hand = run(3, "failure") | {"event": "workflow_dispatch"}
        self.assertEqual(self.problems(by_hand), [])

    def test_a_run_started_by_hand_still_counts_as_a_run(self) -> None:
        self.assertEqual(self.problems(run(3) | {"event": "workflow_dispatch"}), [])

    def test_a_run_still_going_counts_as_a_run_and_not_as_a_failure(self) -> None:
        self.assertEqual(self.problems(run(1, "", "in_progress")), [])

    def with_gh(self, *replies: str, arguments: list[str] | None = None) -> tuple[int, list[list[str]]]:
        calls: list[list[str]] = []
        outputs = iter(replies)

        def fake(command: list[str], **_: object) -> mock.Mock:
            calls.append(command[1:])
            return mock.Mock(stdout=next(outputs))

        with (
            mock.patch("subprocess.run", fake),
            mock.patch.object(check_scheduled_runs, "datetime", wraps=datetime) as clock,
            mock.patch("builtins.print"),
        ):
            clock.now.return_value = NOW
            status = check_scheduled_runs.main(arguments if arguments is not None else [])
        return status, calls

    def test_no_run_says_when_the_last_was_so_that_each_outage_reads_differently(self) -> None:
        found = self.problems(run(40))
        self.assertEqual(len(found), 1)
        self.assertIn("no run in the last 36 hours", found[0])
        self.assertIn(run(40)["createdAt"], found[0])
        self.assertNotEqual(found, self.problems(run(50)))

    def test_a_green_run_that_left_a_sealed_file_is_a_problem(self) -> None:
        found = self.problems(run(3) | {"sealed": 1})
        self.assertEqual(len(found), 1)
        self.assertIn("passed and left sealed files", found[0])
        self.assertEqual(self.problems(run(3) | {"sealed": 0}), [])

    def test_the_watch_asks_only_about_green_scheduled_runs_in_its_window(self) -> None:
        runs = [
            run(3) | {"databaseId": 11},
            run(4, "failure") | {"databaseId": 12},
            run(5) | {"databaseId": 13, "event": "workflow_dispatch"},
            run(37) | {"databaseId": 14},
        ]
        replies = [json.dumps(runs), json.dumps({"total_count": 2}), "[]", "[]", ""]
        status, calls = self.with_gh(*replies, arguments=["--open-issue"])
        self.assertEqual(status, 1)
        asked = [call[1] for call in calls if call[0] == "api"]
        self.assertEqual(asked, ["repos/{owner}/{repo}/actions/runs/11/artifacts"])
        self.assertEqual(calls[-1][:2], ["issue", "create"])
        self.assertIn("fuzz.yml passed and left sealed files", calls[-1][-1])

    def test_main_does_not_touch_issues_unless_asked(self) -> None:
        status, calls = self.with_gh("[]", "[]")
        self.assertEqual(status, 1)
        self.assertEqual([call[0] for call in calls], ["run", "run"])

    def test_a_problem_opens_one_issue(self) -> None:
        status, calls = self.with_gh("[]", "[]", "[]", "", arguments=["--open-issue"])
        self.assertEqual(status, 1)
        self.assertEqual(calls[-1][:2], ["issue", "create"])
        self.assertIn("fuzz.yml has had no run", calls[-1][-1])
        self.assertIn("advisories.yml has had no run", calls[-1][-1])

    @staticmethod
    def issues(body: str, comment: str = "", state: str = "OPEN") -> str:
        return json.dumps(
            [
                {
                    "number": 7,
                    "state": state,
                    "title": check_scheduled_runs.ISSUE_TITLE,
                    "body": body,
                    "comments": [{"body": comment}] if comment else [],
                },
                {
                    "number": 8,
                    "state": "OPEN",
                    "title": "Something else",
                    "body": "- advisories.yml has had no run in the last 36 hours",
                    "comments": [],
                },
            ]
        )

    def test_a_problem_already_in_the_open_issue_is_not_repeated(self) -> None:
        body = "- fuzz.yml has had no run in the last 36 hours"
        comment = "- advisories.yml has had no run in the last 36 hours"
        status, calls = self.with_gh(
            "[]", "[]", self.issues(body, comment), arguments=["--open-issue"]
        )
        self.assertEqual(status, 1)
        self.assertEqual(calls[-1][:2], ["issue", "list"])

    def test_a_new_problem_adds_to_the_open_issue(self) -> None:
        status, calls = self.with_gh(
            "[]",
            "[]",
            self.issues("- fuzz.yml has had no run in the last 36 hours"),
            "",
            arguments=["--open-issue"],
        )
        self.assertEqual(status, 1)
        self.assertEqual(calls[-1][:3], ["issue", "comment", "7"])
        self.assertIn("advisories.yml has had no run", calls[-1][-1])
        self.assertNotIn("fuzz.yml", calls[-1][-1])

    def test_a_problem_that_an_earlier_one_starts_with_is_still_filed(self) -> None:
        # The first text is a prefix of the second, and it's the shorter one that's new.
        earlier = "- fuzz.yml has had no run in the last 36 hours, and the last was at 2026-01-01T00:00:00+00:00"
        status, calls = self.with_gh(
            "[]",
            "[]",
            self.issues(earlier),
            "",
            arguments=["--open-issue"],
        )
        self.assertEqual(status, 1)
        self.assertEqual(calls[-1][:3], ["issue", "comment", "7"])
        self.assertIn("- fuzz.yml has had no run in the last 36 hours", calls[-1][-1].splitlines())

    def test_closed_issues_are_read_too(self) -> None:
        _, calls = self.with_gh("[]", "[]", self.issues("- x"), "", arguments=["--open-issue"])
        listed = next(call for call in calls if call[:2] == ["issue", "list"])
        self.assertEqual(listed[listed.index("--state") + 1], "all")

    def test_a_problem_in_a_closed_issue_is_not_filed_again(self) -> None:
        # Someone read it and closed the issue, and that run is still inside the window.
        body = "- fuzz.yml has had no run in the last 36 hours\n- advisories.yml has had no run in the last 36 hours"
        status, calls = self.with_gh(
            "[]", "[]", self.issues(body, state="CLOSED"), arguments=["--open-issue"]
        )
        self.assertEqual(status, 1)
        self.assertEqual(calls[-1][:2], ["issue", "list"])

    def test_a_new_problem_after_a_closed_issue_opens_another(self) -> None:
        status, calls = self.with_gh(
            "[]",
            "[]",
            self.issues("- fuzz.yml has had no run in the last 36 hours", state="CLOSED"),
            "",
            arguments=["--open-issue"],
        )
        self.assertEqual(status, 1)
        self.assertEqual(calls[-1][:2], ["issue", "create"])
        self.assertIn("advisories.yml has had no run", calls[-1][-1])
        self.assertNotIn("fuzz.yml", calls[-1][-1])

    def test_a_green_check_leaves_an_open_issue_for_a_person_to_close(self) -> None:
        # What the failed run found stays sealed until someone reads it.
        good = json.dumps([run(3)])
        none = json.dumps({"total_count": 0})
        status, calls = self.with_gh(good, none, good, none, arguments=["--open-issue"])
        self.assertEqual(status, 0)
        self.assertEqual([call[0] for call in calls], ["run", "api", "run", "api"])

    def test_a_check_that_cannot_run_says_so(self) -> None:
        with mock.patch("subprocess.run", side_effect=FileNotFoundError("gh")), mock.patch("builtins.print"):
            self.assertEqual(check_scheduled_runs.main([]), 2)
        with mock.patch("builtins.print"):
            self.assertEqual(check_scheduled_runs.main(["--wrong"]), 2)


SECRET = "input-only-the-maintainer-should-read"


def fake_tool(directory: Path, name: str, body: str) -> None:
    path = directory / name
    path.write_text("#!/bin/sh\n" + body)
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


class FuzzHostedTests(unittest.TestCase):
    """What `tools/fuzz-hosted.sh` lets out of a run, with `cargo` and sometimes `age` faked."""

    def setUp(self) -> None:
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.root / "tools").mkdir()
        here = Path(__file__).resolve().parent
        shutil.copy(here / "fuzz-hosted.sh", self.root / "tools")
        shutil.copy(here / "fuzz-crash-recipient.txt", self.root / "tools")
        self.output = self.root / "sealed"

    def fuzzer(self, status: int, crash: bool = True) -> None:
        artifact = (
            f"mkdir -p fuzz/artifacts/parse; printf %s {SECRET} > fuzz/artifacts/parse/crash-abc\n"
            if crash
            else ""
        )
        fake_tool(
            self.bin,
            "cargo",
            f"{artifact}echo 'Base64: {SECRET}'\necho 'stat::number_of_executed_units: 123'\n"
            f"exit {status}\n",
        )

    def run_script(self) -> subprocess.CompletedProcess:
        return subprocess.run(
            ["bash", str(self.root / "tools/fuzz-hosted.sh"), "parse", "60", str(self.output)],
            capture_output=True,
            text=True,
            env={**os.environ, "PATH": f"{self.bin}{os.pathsep}{os.environ['PATH']}"},
            check=False,
        )

    def assert_nothing_plain_left(self, result: subprocess.CompletedProcess) -> None:
        self.assertNotIn(SECRET, result.stdout + result.stderr)
        self.assertFalse((self.root / "fuzz/artifacts/parse").exists())
        for path in self.root.rglob("*"):
            if path.is_file() and path.parent not in (self.bin, self.root / "tools"):
                self.assertNotIn(SECRET.encode(), path.read_bytes(), path)

    def test_a_clean_run_reports_its_count_and_keeps_nothing(self) -> None:
        self.fuzzer(0, crash=False)
        result = self.run_script()
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "target=parse exit=0 runs=123\n")
        self.assertFalse(self.output.exists())

    def test_a_crash_keeps_its_exit_status_and_prints_no_input(self) -> None:
        self.fuzzer(77)
        fake_tool(self.bin, "age", 'while [ "$1" != -o ]; do shift; done; echo sealed > "$2"\n')
        result = self.run_script()
        self.assertEqual(result.returncode, 77)
        self.assertIn("target=parse exit=77 runs=123\n", result.stdout)
        self.assertIn("encrypted=fuzz-parse.tar.gz.age sha256=", result.stdout)
        self.assert_nothing_plain_left(result)

    def test_a_failed_encryption_keeps_nothing(self) -> None:
        self.fuzzer(77)
        fake_tool(self.bin, "age", 'while [ "$1" != -o ]; do shift; done; echo part > "$2"; exit 1\n')
        result = self.run_script()
        self.assertEqual(result.returncode, 77)
        self.assertIn("nothing was kept", result.stderr)
        self.assertEqual(list(self.output.iterdir()), [])
        self.assert_nothing_plain_left(result)

    def test_a_green_run_that_left_a_file_seals_it_and_stays_green(self) -> None:
        self.fuzzer(0)
        fake_tool(self.bin, "age", 'while [ "$1" != -o ]; do shift; done; echo sealed > "$2"\n')
        result = self.run_script()
        self.assertEqual(result.returncode, 0)
        self.assertIn("::warning::the parse fuzzer passed and left 1 files\n", result.stdout)
        self.assertTrue((self.output / "fuzz-parse.tar.gz.age").exists())
        self.assert_nothing_plain_left(result)

    def test_a_failed_encryption_turns_a_green_run_red(self) -> None:
        self.fuzzer(0)
        fake_tool(self.bin, "age", 'while [ "$1" != -o ]; do shift; done; echo part > "$2"; exit 1\n')
        result = self.run_script()
        self.assertEqual(result.returncode, 1)
        self.assertIn("nothing was kept", result.stderr)
        self.assert_nothing_plain_left(result)

    def test_what_a_run_left_stays_out_of_the_cached_corpus(self) -> None:
        corpus = self.root / "fuzz/corpus/parse"
        corpus.mkdir(parents=True)
        (corpus / hashlib.sha1(SECRET.encode()).hexdigest()).write_text(SECRET)
        (corpus / "other").write_text("kept")
        self.fuzzer(0)
        fake_tool(self.bin, "age", 'while [ "$1" != -o ]; do shift; done; echo sealed > "$2"\n')
        result = self.run_script()
        self.assertEqual(result.returncode, 0)
        self.assertEqual([path.name for path in corpus.iterdir()], ["other"])
        self.assert_nothing_plain_left(result)

    def test_a_failed_pack_does_not_show_a_file_name(self) -> None:
        self.fuzzer(77)
        fake_tool(self.bin, "tar", 'echo "tar: crash-abc: Cannot open" >&2; exit 2\n')
        result = self.run_script()
        self.assertEqual(result.returncode, 77)
        self.assertIn("nothing was kept", result.stderr)
        self.assertNotIn("crash-abc", result.stdout + result.stderr)
        self.assert_nothing_plain_left(result)

    def test_a_failure_with_no_crash_file_still_hides_the_log(self) -> None:
        self.fuzzer(1, crash=False)
        fake_tool(self.bin, "age", 'while [ "$1" != -o ]; do shift; done; echo sealed > "$2"\n')
        result = self.run_script()
        self.assertEqual(result.returncode, 1)
        self.assertTrue((self.output / "fuzz-parse.tar.gz.age").exists())
        self.assert_nothing_plain_left(result)

    def test_a_bad_target_name_is_refused(self) -> None:
        result = subprocess.run(
            ["bash", str(self.root / "tools/fuzz-hosted.sh"), "../..", "60", str(self.output)],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 2)

    def test_a_fuzzing_stat_line_cannot_carry_text(self) -> None:
        fake_tool(
            self.bin,
            "cargo",
            f"echo 'stat::number_of_executed_units: {SECRET}'\nexit 0\n",
        )
        result = self.run_script()
        self.assertEqual(result.stdout, "target=parse exit=0 runs=unknown\n")

    @unittest.skipUnless(shutil.which("age") and shutil.which("age-keygen"), "needs age")
    def test_the_maintainer_can_read_it_back_with_the_key(self) -> None:
        self.fuzzer(77)
        key = self.root / "key.txt"
        subprocess.run(["age-keygen", "-o", str(key)], capture_output=True, check=True)
        recipient = subprocess.run(
            ["age-keygen", "-y", str(key)], capture_output=True, text=True, check=True
        ).stdout
        (self.root / "tools/fuzz-crash-recipient.txt").write_text(recipient)
        result = self.run_script()
        self.assertEqual(result.returncode, 77)
        self.assertNotIn(SECRET, result.stdout + result.stderr)
        plain = subprocess.run(
            ["age", "-d", "-i", str(key), str(self.output / "fuzz-parse.tar.gz.age")],
            capture_output=True,
            check=True,
        ).stdout
        with tarfile.open(fileobj=io.BytesIO(plain)) as archive:
            names = archive.getnames()
            crash = archive.extractfile("parse/crash-abc").read().decode()
            log = archive.extractfile("fuzz.log").read().decode()
        self.assertIn("fuzz.log", names)
        self.assertEqual(crash, SECRET)
        self.assertIn(SECRET, log)


def git(root: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(root), *args], capture_output=True, text=True, check=True
    ).stdout.strip()


def crate(top: str, files: dict[str, bytes], extra: list[tarfile.TarInfo] = ()) -> bytes:
    """A gzipped tarball holding FILES under TOP, as cargo package writes one."""
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
        for name, data in files.items():
            info = tarfile.TarInfo(f"{top}/{name}")
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
        for info in extra:
            archive.addfile(info)
    return buffer.getvalue()


class Release(unittest.TestCase):
    """tools/check_release.py, against a scratch repository laid out as this one is."""

    MANIFEST = b'[package]\nname = "ayaml"\nversion.workspace = true\n'
    CHANGELOG = "# Changelog\n\n## [0.2.0] \u2014 2026-11-07\n\n- A change.\n\n## [0.1.0] - 2026-01-01\n\n- First.\n"

    def setUp(self) -> None:
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name) / "repo"
        (self.root / "crates/ayaml/src").mkdir(parents=True)
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "0.2.0"\n')
        (self.root / "crates/ayaml/Cargo.toml").write_bytes(self.MANIFEST)
        (self.root / "crates/ayaml/src/lib.rs").write_text("//! ayaml\n")
        (self.root / "README.md").write_text("# ayaml\n")
        (self.root / "CHANGELOG.md").write_text(self.CHANGELOG, encoding="utf-8")
        git(self.root.parent, "init", "-q", "-b", "main", str(self.root))
        for key, value in [("user.name", "t"), ("user.email", "t@example.com"), ("tag.gpgsign", "false"), ("commit.gpgsign", "false")]:
            git(self.root, "config", key, value)
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "-m", "release")
        self.commit = git(self.root, "rev-parse", "HEAD")
        git(self.root, "update-ref", "refs/remotes/origin/main", self.commit)
        git(self.root, "tag", "-a", "-m", "v0.2.0", "v0.2.0")

    def refused(self, call, text: str) -> None:
        with self.assertRaises(check_release.Refused) as caught:
            call()
        self.assertIn(text, str(caught.exception))

    # tag

    def test_an_annotated_tag_on_main_with_its_version_passes(self) -> None:
        self.assertEqual(check_release.check_tag(self.root, "v0.2.0"), self.commit)

    def test_a_tag_that_is_not_a_release_tag_is_refused(self) -> None:
        for tag in ["0.2.0", "v0.2", "v0.2.0-rc.1", "v01.2.0", "v0.2.0\n"]:
            self.refused(lambda: check_release.check_tag(self.root, tag), "isn't a release tag")

    def test_a_lightweight_tag_is_refused(self) -> None:
        git(self.root, "tag", "-d", "v0.2.0")
        git(self.root, "tag", "v0.2.0")
        self.refused(lambda: check_release.check_tag(self.root, "v0.2.0"), "lightweight")

    def test_a_checkout_away_from_the_tag_is_refused(self) -> None:
        (self.root / "README.md").write_text("# changed\n")
        git(self.root, "commit", "-q", "-am", "later")
        self.refused(lambda: check_release.check_tag(self.root, "v0.2.0"), "the checkout is at")

    def test_a_dirty_checkout_is_refused(self) -> None:
        (self.root / "stray.txt").write_text("x")
        self.refused(lambda: check_release.check_tag(self.root, "v0.2.0"), "isn't clean")

    def test_a_commit_off_main_is_refused(self) -> None:
        git(self.root, "checkout", "-q", "--orphan", "side")
        git(self.root, "commit", "-q", "-m", "side")
        git(self.root, "tag", "-a", "-m", "v0.3.0", "v0.3.0")
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "0.3.0"\n')
        git(self.root, "commit", "-q", "-am", "bump")
        git(self.root, "tag", "-f", "-a", "-m", "v0.3.0", "v0.3.0")
        self.refused(lambda: check_release.check_tag(self.root, "v0.3.0"), "isn't on origin/main")

    def test_a_tag_naming_another_version_is_refused(self) -> None:
        git(self.root, "tag", "-a", "-m", "v0.2.1", "v0.2.1")
        self.refused(lambda: check_release.check_tag(self.root, "v0.2.1"), "declares 0.2.0")

    def test_a_version_without_a_changelog_section_is_refused(self) -> None:
        text = self.CHANGELOG.replace("## [0.2.0] \u2014 2026-11-07\n\n- A change.\n\n", "")
        self.refused(lambda: check_release.changelog_section(text, "0.2.0"), "has no section")
        self.refused(lambda: check_release.changelog_section("## [0.2.0] - 2026-11-07\n\n## [0.1.0] - x\n", "0.2.0"), "is empty")
        # A heading for 0.2.0 isn't one for 0.2.00 or 0.2.0-rc.1.
        self.refused(lambda: check_release.changelog_section(self.CHANGELOG, "0.2"), "has no section")

    def test_a_section_runs_to_the_next_heading(self) -> None:
        self.assertEqual(check_release.changelog_section(self.CHANGELOG, "0.2.0"), "- A change.")
        self.assertEqual(check_release.changelog_section(self.CHANGELOG, "0.1.0"), "- First.")

    # package

    def packaged(self, changes: dict[str, bytes | None] | None = None) -> dict[str, bytes]:
        """The files cargo would package for the commit, with CHANGES: a path to None removes it."""
        files = {
            ".cargo_vcs_info.json": json.dumps({"git": {"sha1": self.commit}, "path_in_vcs": "crates/ayaml"}).encode(),
            "Cargo.toml": b'[package]\nname = "ayaml"\nversion = "0.2.0"\n',
            "Cargo.toml.orig": self.MANIFEST,
            "Cargo.lock": b"# lock\n",
            "README.md": b"# ayaml\n",
            "src/lib.rs": b"//! ayaml\n",
        }
        for name, data in (changes or {}).items():
            if data is None:
                files.pop(name)
            else:
                files[name] = data
        return files

    def check_package(self, files: dict[str, bytes], name: str = "ayaml-0.2.0.crate", top: str = "ayaml-0.2.0", extra=()) -> str:
        path = self.root.parent / name
        path.write_bytes(crate(top, files, list(extra)))
        return check_release.check_package(self.root, path, self.commit)

    def test_a_package_of_the_commit_passes(self) -> None:
        path = self.root.parent / "ayaml-0.2.0.crate"
        digest = self.check_package(self.packaged())
        self.assertEqual(digest, hashlib.sha256(path.read_bytes()).hexdigest())

    def test_a_file_the_commit_does_not_have_is_refused(self) -> None:
        self.refused(lambda: self.check_package(self.packaged({"src/extra.rs": b"x"})), "not in")

    def test_a_missing_or_changed_file_is_refused(self) -> None:
        self.refused(lambda: self.check_package(self.packaged({"src/lib.rs": None})), "missing src/lib.rs")
        self.refused(lambda: self.check_package(self.packaged({"src/lib.rs": b"changed"})), "differs from")
        self.refused(lambda: self.check_package(self.packaged({"README.md": b"other"})), "differs from")
        self.refused(lambda: self.check_package(self.packaged({"Cargo.toml.orig": b"x"})), "differs from")

    def test_a_package_naming_another_commit_or_path_is_refused(self) -> None:
        other = json.dumps({"git": {"sha1": "0" * 40}, "path_in_vcs": "crates/ayaml"}).encode()
        self.refused(lambda: self.check_package(self.packaged({".cargo_vcs_info.json": other})), ".cargo_vcs_info.json")
        dirty = json.dumps({"git": {"sha1": self.commit, "dirty": True}, "path_in_vcs": "crates/ayaml"}).encode()
        self.refused(lambda: self.check_package(self.packaged({".cargo_vcs_info.json": dirty})), ".cargo_vcs_info.json")

    def test_a_package_of_another_version_or_name_is_refused(self) -> None:
        self.refused(lambda: self.check_package(self.packaged(), name="ayaml-0.2.1.crate"), "holds ayaml-0.2.0")
        manifest = b'[package]\nname = "ayaml"\nversion = "0.2.1"\n'
        self.refused(lambda: self.check_package(self.packaged({"Cargo.toml": manifest})), "packaged manifest")
        self.refused(lambda: self.check_package(self.packaged(), top="other-0.2.0", name="ayaml-0.2.0.crate"), "not ayaml-VERSION")

    def test_anything_but_plain_files_in_one_directory_is_refused(self) -> None:
        link = tarfile.TarInfo("ayaml-0.2.0/src/link.rs")
        link.type = tarfile.SYMTYPE
        link.linkname = "/etc/passwd"
        self.refused(lambda: self.check_package(self.packaged(), extra=[link]), "isn't a plain file")
        outside = tarfile.TarInfo("ayaml-0.2.0/../evil")
        self.refused(lambda: self.check_package(self.packaged(), extra=[outside]), "outside its directory")
        other = tarfile.TarInfo("other/file")
        self.refused(lambda: self.check_package(self.packaged(), extra=[other]), "outside ayaml-0.2.0")
        again = tarfile.TarInfo("ayaml-0.2.0/README.md")
        self.refused(lambda: self.check_package(self.packaged(), extra=[again]), "twice")

    def test_a_file_mode_cargo_would_not_package_as_written_is_refused(self) -> None:
        os.symlink("lib.rs", self.root / "crates/ayaml/src/alias.rs")
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "-m", "link")
        self.commit = git(self.root, "rev-parse", "HEAD")
        self.refused(lambda: self.check_package(self.packaged()), "has mode 120000")

    # registry

    def registry(self, served: bytes, entries: list[dict], index_misses: int = 0, download: int = 200):
        calls = {"index": 0}

        def fetch(url: str) -> tuple[int, bytes]:
            if url == check_release.INDEX:
                calls["index"] += 1
                if calls["index"] <= index_misses:
                    return 404, b""
                return 200, "\n".join(json.dumps(entry) for entry in entries).encode()
            if url == check_release.DOWNLOAD.format(name="ayaml", version="0.2.0"):
                return download, served if download == 200 else b""
            if url == check_release.VERSION_API.format(name="ayaml", version="0.2.0"):
                return (200 if entries else 404), b""
            raise AssertionError(url)

        return fetch, calls

    def check_registry(self, fetch, seconds: float = 60) -> str:
        path = self.root.parent / "built.crate"
        path.write_bytes(b"crate bytes")
        return check_release.check_registry(path, "0.2.0", seconds, get=fetch, sleep=lambda _: None)

    def entry(self, **fields) -> dict:
        return {"name": "ayaml", "vers": "0.2.0", "cksum": hashlib.sha256(b"crate bytes").hexdigest(), "yanked": False, **fields}

    def test_the_same_bytes_in_the_index_and_the_download_pass(self) -> None:
        fetch, calls = self.registry(b"crate bytes", [self.entry(vers="0.1.0"), self.entry()], index_misses=3)
        self.assertEqual(self.check_registry(fetch), hashlib.sha256(b"crate bytes").hexdigest())
        self.assertEqual(calls["index"], 4)

    def test_a_version_the_index_never_lists_is_refused(self) -> None:
        fetch, _ = self.registry(b"crate bytes", [self.entry(vers="0.1.0")])
        self.refused(lambda: self.check_registry(fetch, seconds=0), "didn't list ayaml 0.2.0")

    def test_other_bytes_are_refused(self) -> None:
        fetch, _ = self.registry(b"crate bytes", [self.entry(cksum="0" * 64)])
        self.refused(lambda: self.check_registry(fetch), "the sparse index's checksum")
        fetch, _ = self.registry(b"other bytes", [self.entry()])
        self.refused(lambda: self.check_registry(fetch), "serves other bytes")
        fetch, _ = self.registry(b"crate bytes", [self.entry()], download=403)
        self.refused(lambda: self.check_registry(fetch), "answered 403")
        fetch, _ = self.registry(b"crate bytes", [self.entry(yanked=True)])
        self.refused(lambda: self.check_registry(fetch), "is yanked")

    def test_published_answers_yes_no_or_unknown(self) -> None:
        fetch, _ = self.registry(b"", [self.entry()])
        self.assertIs(check_release.published("0.2.0", get=fetch), True)
        fetch, _ = self.registry(b"", [])
        self.assertIs(check_release.published("0.2.0", get=fetch), False)
        self.assertIsNone(check_release.published("0.2.0", get=lambda url: (503, b"")))

    def test_no_answer_is_never_taken_for_a_missing_version(self) -> None:
        import http.client
        import urllib.error

        for failure in [
            urllib.error.URLError("refused"),
            TimeoutError("timed out"),
            ConnectionResetError("reset"),
            http.client.RemoteDisconnected("closed"),
            http.client.IncompleteRead(b"par"),
        ]:
            with mock.patch.object(check_release.urllib.request, "urlopen", side_effect=failure), mock.patch("sys.stderr", io.StringIO()):
                self.assertEqual(check_release.fetch("https://crates.io/x"), (0, b""), failure)
                self.assertIsNone(check_release.published("0.2.0"), failure)
                self.assertEqual(check_release.main(["published", "0.2.0"]), 2, failure)

    def test_no_answer_or_a_broken_index_is_waited_out(self) -> None:
        fetch, calls = self.registry(b"crate bytes", [self.entry()])
        answers = iter([(0, b""), (200, b"{not json"), (200, b"\xff"), (200, b"[1]")])

        def flaky(url: str) -> tuple[int, bytes]:
            if url == check_release.INDEX:
                answer = next(answers, None)
                if answer is not None:
                    return answer
            return fetch(url)

        self.assertEqual(self.check_registry(flaky), hashlib.sha256(b"crate bytes").hexdigest())
        self.assertEqual(calls["index"], 1)
        fetch, _ = self.registry(b"crate bytes", [self.entry()])
        self.refused(lambda: self.check_registry(lambda url: (0, b"") if url == check_release.INDEX else fetch(url), seconds=0), "didn't list")

    # record

    def test_the_record_names_what_a_consumer_verifies(self) -> None:
        digest = "a" * 64
        text = check_release.record("v0.2.0", self.commit, digest, "https://github.com/asamarts/ayaml/attestations/1")
        for part in [
            "ayaml-0.2.0.crate",
            digest,
            self.commit,
            "refs/tags/v0.2.0",
            "asamarts/ayaml/.github/workflows/release.yml",
            "https://github.com/asamarts/ayaml/.github/workflows/release.yml@refs/tags/v0.2.0",
            "https://token.actions.githubusercontent.com",
            "https://slsa.dev/provenance/v1",
            "https://github.com/asamarts/ayaml/attestations/1",
            "gh attestation verify ayaml-0.2.0.crate",
        ]:
            self.assertIn(part, text)
        self.refused(lambda: check_release.record("v0.2", self.commit, digest), "isn't a release tag")
        self.refused(lambda: check_release.record("v0.2.0", "abc", digest), "full commit id")
        self.refused(lambda: check_release.record("v0.2.0", self.commit, "abc"), "sha256 digest")


WORKFLOWS = Path(__file__).resolve().parent.parent / ".github" / "workflows"
USES = re.compile(r"\s*(?:-\s+)?uses:\s*(.+?)\s*")
PINNED = re.compile(r"[\w.-]+/[\w./-]+@[0-9a-f]{40} # \S+")
SCANNER_ACTION = re.compile(r"\s+(?:-\s+)?uses: trufflesecurity/trufflehog@[0-9a-f]{40} # v(\S+)")
SCANNER_IMAGE = re.compile(r"\s+version: (\S+)@sha256:[0-9a-f]{64}")


def unpinned_actions(text: str) -> list[str]:
    """Each action a workflow uses by anything but a full commit with its version beside it."""
    found = []
    for line in text.splitlines():
        used = USES.fullmatch(line)
        if used and not used[1].startswith("./") and not PINNED.fullmatch(used[1]):
            found.append(used[1])
    return found


def scanner_versions(text: str) -> tuple[str, str]:
    """The secret scan action's version, and the tag of the image its step pins, or "" for none."""
    lines = text.splitlines()
    for at, line in enumerate(lines):
        action = SCANNER_ACTION.fullmatch(line)
        if action:
            for later in lines[at + 1 :]:
                if re.match(r"\s+- ", later):
                    break
                image = SCANNER_IMAGE.fullmatch(later)
                if image:
                    return action[1], image[1]
            return action[1], ""
    raise AssertionError("no secret scan step")


class WorkflowPins(unittest.TestCase):
    def test_every_action_is_pinned_to_a_commit_with_its_version(self) -> None:
        files = sorted(WORKFLOWS.glob("*.yml"))
        self.assertGreaterEqual(len(files), 4)
        for path in files:
            self.assertEqual(unpinned_actions(path.read_text()), [], path.name)

    def test_an_action_by_tag_or_branch_or_without_its_version_is_found(self) -> None:
        commit = "3d3c42e5aac5ba805825da76410c181273ba90b1"
        text = (
            "      - uses: actions/checkout@v7\n"
            "      - uses: dtolnay/rust-toolchain@master\n"
            f"        uses: actions/cache@{commit}\n"
            f"      - uses: actions/checkout@{commit[:12]} # v7.0.1\n"
            "      - uses: ./.github/workflows/ci.yml\n"
            f"      - uses: actions/checkout@{commit} # v7.0.1\n"
        )
        self.assertEqual(
            unpinned_actions(text),
            [
                "actions/checkout@v7",
                "dtolnay/rust-toolchain@master",
                f"actions/cache@{commit}",
                f"actions/checkout@{commit[:12]} # v7.0.1",
            ],
        )

    def test_the_secret_scanner_runs_the_version_of_its_action(self) -> None:
        action, image = scanner_versions((WORKFLOWS / "ci.yml").read_text())
        self.assertEqual(image, action, "pin ghcr.io/trufflesecurity/trufflehog at the action's version, by its digest")

    def test_an_image_an_update_left_behind_or_without_a_digest_is_found(self) -> None:
        step = (
            "      - name: TruffleHog\n"
            f"        uses: trufflesecurity/trufflehog@{'e' * 40} # v3.99.8\n"
            "        with:\n"
            "          path: ./\n"
            "          version: {}\n"
            "      - name: Next\n"
            f"          version: 3.99.8@sha256:{'0' * 64}\n"
        )
        self.assertEqual(scanner_versions(step.format(f"3.99.7@sha256:{'0' * 64}")), ("3.99.8", "3.99.7"))
        self.assertEqual(scanner_versions(step.format("3.99.8")), ("3.99.8", ""))
        self.assertEqual(scanner_versions(step.format(f"3.99.8@sha256:{'0' * 64}")), ("3.99.8", "3.99.8"))


class InstructionsBase(unittest.TestCase):
    def test_a_missing_or_zero_base_falls_back_to_the_parent(self) -> None:
        for given in ["", "0" * 40, "  "]:
            self.assertEqual(check_instructions.base_revision(given), "HEAD~1")
        self.assertEqual(check_instructions.base_revision("abc123"), "abc123")


if __name__ == "__main__":
    unittest.main()
