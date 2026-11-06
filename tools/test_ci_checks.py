#!/usr/bin/env python3
"""Tests for the checks that decide what CI's collected data means."""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import sys
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


if __name__ == "__main__":
    unittest.main()
