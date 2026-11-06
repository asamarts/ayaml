#!/usr/bin/env python3
"""Tests for the checks that decide what CI's collected data means."""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
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
sys.path.insert(0, str(Path(__file__).resolve().parent))

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

    def test_an_open_issue_is_not_repeated(self) -> None:
        issues = json.dumps([{"number": 7, "title": check_scheduled_runs.ISSUE_TITLE}])
        status, calls = self.with_gh("[]", "[]", issues, arguments=["--open-issue"])
        self.assertEqual(status, 1)
        self.assertEqual(calls[-1][:2], ["issue", "list"])

    def test_an_issue_is_closed_once_nothing_is_wrong(self) -> None:
        good = json.dumps([run(3)])
        issues = json.dumps(
            [
                {"number": 7, "title": check_scheduled_runs.ISSUE_TITLE},
                {"number": 8, "title": "Something else"},
            ]
        )
        status, calls = self.with_gh(good, good, issues, "", arguments=["--open-issue"])
        self.assertEqual(status, 0)
        self.assertEqual(calls[-1][:3], ["issue", "close", "7"])
        self.assertEqual(len(calls), 4)

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


if __name__ == "__main__":
    unittest.main()
