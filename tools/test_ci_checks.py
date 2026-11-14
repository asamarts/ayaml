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


FAKE_GH = r'''#!/usr/bin/env python3
"""gh release, for the commands tools/github_release.sh runs, with releases kept in $GH_STATE.

The calls whose arguments start with GH_FAIL fail as an API error does, once GH_FAIL_AFTER of them
have passed. GH_BROKEN makes one command misbehave: create-published, create-draft, and
create-tampered make another release than the one asked for, and edit and delete change nothing."""
import os, shutil, sys
from pathlib import Path

state = Path(os.environ["GH_STATE"])
args = sys.argv[1:]
with open(state / "calls", "a") as log:
    log.write(" ".join(args) + "\n")
failing = os.environ.get("GH_FAIL")
if failing and " ".join(args).startswith(failing):
    counter = state / "matched"
    matched = int(counter.read_text()) if counter.exists() else 0
    counter.write_text(str(matched + 1))
    if matched >= int(os.environ.get("GH_FAIL_AFTER", "0")):
        sys.exit("HTTP 502: Bad Gateway (https://api.github.com/graphql)")
broken = os.environ.get("GH_BROKEN", "")
group, verb, *rest = args
assert group == "release", args
releases = state / "releases"


def option(flag):
    return rest[rest.index(flag) + 1] if flag in rest else None


if verb == "list":
    assert option("--jq") == ".[] | select(.isDraft) | .tagName", rest
    for release in sorted(releases.iterdir()):
        if (release / "draft").read_text() == "true":
            print(release.name)
    sys.exit()
name, *rest = rest
release = releases / name
# gh prints this, and exits 1, for a name that is neither a release's tag nor a draft's.
if verb != "create" and not release.is_dir():
    sys.exit("release not found")
if verb == "view":
    field = option("--json")
    assert option("--jq") is not None, rest
    if field == "isDraft":
        print((release / "draft").read_text())
    elif field == "assets":
        print(" ".join(sorted(path.name for path in (release / "assets").iterdir())))
    elif field == "body":
        print(os.environ.get("GH_BODY", (release / "body").read_text()))
elif verb == "download":
    for at, flag in enumerate(rest):
        if flag == "--pattern":
            shutil.copy(release / "assets" / rest[at + 1], option("--dir"))
elif verb == "edit":
    assert rest == ["--draft=false"], rest
    if broken != "edit":
        (release / "draft").write_text("false")
elif verb == "delete":
    assert rest == ["--yes"], rest
    if broken != "delete":
        shutil.rmtree(release)
elif verb == "create":
    if release.exists():
        sys.exit("already exists")
    valued = {"--target", "--title", "--notes-file"}
    files = [arg for at, arg in enumerate(rest) if not arg.startswith("--") and rest[at - 1] not in valued]
    (release / "assets").mkdir(parents=True)
    for path in files:
        shutil.copy(path, release / "assets")
    if broken == "create-tampered":
        [crate] = (release / "assets").glob("*.crate")
        crate.write_bytes(b"other bytes")
    # GitHub keeps no trailing line break.
    (release / "body").write_text(Path(option("--notes-file")).read_text().rstrip("\n"))
    draft = {"create-published": False, "create-draft": True}.get(broken, "--draft" in rest)
    (release / "draft").write_text("true" if draft else "false")
else:
    sys.exit(f"unexpected gh release {verb}")
'''

FAKE_CURL = r'''#!/usr/bin/env python3
"""curl, for fetch's download: it writes $FAKE_CRATE to --output, or fails if FAKE_CURL_FAILS is set."""
import os, shutil, sys
from pathlib import Path

args = sys.argv[1:]
with open(Path(os.environ["GH_STATE"]) / "calls", "a") as log:
    log.write("curl " + args[-1] + "\n")
if os.environ.get("FAKE_CURL_FAILS"):
    sys.exit("curl: (22) The requested URL returned error: 403")
shutil.copy(os.environ["FAKE_CRATE"], args[args.index("--output") + 1])
'''

FAKE_CHECK_RELEASE = r'''"""check_release.py, which exits with $FAKE_REGISTRY_EXIT."""
import os, sys
from pathlib import Path

with open(Path(os.environ["GH_STATE"]) / "calls", "a") as log:
    log.write("check_release.py " + " ".join(sys.argv[1:]) + "\n")
sys.exit(int(os.environ.get("FAKE_REGISTRY_EXIT", "0")))
'''

API_ERROR = "HTTP 502: Bad Gateway"


class GitHubRelease(unittest.TestCase):
    """tools/github_release.sh's release and rehearsal, with gh faked by releases kept in a directory."""

    CRATE = "ayaml-0.2.0.crate"

    def setUp(self) -> None:
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        (self.root / "tools").mkdir()
        shutil.copy(Path(__file__).resolve().parent / "github_release.sh", self.root / "tools")
        (self.root / "tools/check_release.py").write_text(FAKE_CHECK_RELEASE)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name, text in (("gh", FAKE_GH), ("curl", FAKE_CURL)):
            tool = self.bin / name
            tool.write_text(text)
            tool.chmod(tool.stat().st_mode | stat.S_IXUSR)
        self.state = self.root / "state"
        (self.state / "releases").mkdir(parents=True)
        self.dist = self.root / "dist"
        self.dist.mkdir()
        (self.dist / self.CRATE).write_bytes(b"the crate")
        self.digest = hashlib.sha256(b"the crate").hexdigest()
        (self.dist / "SHA256SUMS").write_text(f"{self.digest}  {self.CRATE}\n")
        self.notes = self.root / "notes.md"
        self.notes.write_text("## Verification\n\nnotes\n")

    def existing(self, name: str, draft: bool, assets: dict[str, bytes] | None = None) -> None:
        release = self.state / "releases" / name
        (release / "assets").mkdir(parents=True)
        if assets is None:
            assets = {path.name: path.read_bytes() for path in self.dist.iterdir()}
        for asset, data in assets.items():
            (release / "assets" / asset).write_bytes(data)
        (release / "body").write_text("notes")
        (release / "draft").write_text("true" if draft else "false")

    def reset(self) -> None:
        shutil.rmtree(self.state)
        (self.state / "releases").mkdir(parents=True)

    def run_script(self, *args: str, **env: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            ["bash", str(self.root / "tools/github_release.sh"), *args],
            capture_output=True,
            text=True,
            cwd=self.root,
            env={**os.environ, "PATH": f"{self.bin}{os.pathsep}{os.environ['PATH']}", "GH_STATE": str(self.state), "TMPDIR": str(self.root), **env},
            check=False,
        )

    def fetch(self, digest: str, **env: str) -> subprocess.CompletedProcess:
        served = self.root / "served.crate"
        served.write_bytes(b"the crate")
        return self.run_script("fetch", "0.2.0", digest, str(self.root / "fetched"), FAKE_CRATE=str(served), **env)

    def release(self, **env: str) -> subprocess.CompletedProcess:
        return self.run_script("release", "v0.2.0", "0.2.0", str(self.dist), str(self.notes), **env)

    def rehearse(self, name: str = "rehearsal-7-1", **env: str) -> subprocess.CompletedProcess:
        return self.run_script("rehearse", name, "main", "0.2.0", str(self.dist), str(self.notes), **env)

    def releases(self) -> dict[str, str]:
        return {path.name: (path / "draft").read_text() for path in (self.state / "releases").iterdir()}

    def calls(self) -> list[str]:
        path = self.state / "calls"
        return path.read_text().splitlines() if path.exists() else []

    def changes(self) -> list[str]:
        return [call for call in self.calls() if call.split()[:2] in (["release", v] for v in ("create", "edit", "delete"))]

    def test_fetch_downloads_the_crate_checks_it_and_writes_its_sums(self) -> None:
        result = self.fetch(self.digest)
        self.assertEqual(result.returncode, 0, result.stderr)
        fetched = self.root / "fetched"
        self.assertEqual((fetched / self.CRATE).read_bytes(), b"the crate")
        # The name alone, so that `sha256sum --check` works wherever SHA256SUMS is downloaded to.
        self.assertEqual((fetched / "SHA256SUMS").read_text(), f"{self.digest}  {self.CRATE}\n")
        self.assertEqual(
            self.calls(),
            [
                f"curl https://static.crates.io/crates/ayaml/{self.CRATE}",
                f"check_release.py registry {fetched / self.CRATE} 0.2.0 600",
            ],
        )

    def test_fetch_refuses_bytes_the_package_job_did_not_build_and_a_failed_check(self) -> None:
        for name, digest, env in (
            ("other bytes", hashlib.sha256(b"other").hexdigest(), {}),
            ("registry check fails", self.digest, {"FAKE_REGISTRY_EXIT": "1"}),
            ("download fails", self.digest, {"FAKE_CURL_FAILS": "1"}),
        ):
            with self.subTest(name):
                self.reset()
                shutil.rmtree(self.root / "fetched", ignore_errors=True)
                result = self.fetch(digest, **env)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.root / "fetched/SHA256SUMS").exists())
        self.assertIn("serves other bytes", self.fetch(hashlib.sha256(b"other").hexdigest()).stderr)

    def test_a_release_is_created_from_its_tag_and_checked(self) -> None:
        result = self.release()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("created the release v0.2.0", result.stdout)
        self.assertEqual(self.releases(), {"v0.2.0": "false"})
        assets = self.state / "releases/v0.2.0/assets"
        self.assertEqual(sorted(path.name for path in assets.iterdir()), ["SHA256SUMS", self.CRATE])
        self.assertEqual((assets / self.CRATE).read_bytes(), b"the crate")
        [create] = [call for call in self.calls() if call.startswith("release create")]
        self.assertIn("--verify-tag", create)
        self.assertNotIn("--draft", create)
        self.assertNotIn("--target", create)
        self.assertIn("release download v0.2.0", self.calls()[-1])

    def test_a_release_that_is_not_what_was_asked_for_fails_the_job(self) -> None:
        for broken, message in (
            ("create-tampered", None),
            ("create-draft", "but it is draft"),
        ):
            with self.subTest(broken):
                self.reset()
                result = self.release(GH_BROKEN=broken)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertNotIn("::notice::", result.stdout)
                if message:
                    self.assertIn(message, result.stderr)

    def test_a_rerun_publishes_a_draft_that_holds_the_same_bytes(self) -> None:
        self.existing("v0.2.0", draft=True)
        result = self.release()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("published the draft release v0.2.0", result.stdout)
        self.assertEqual(self.releases(), {"v0.2.0": "false"})
        self.assertEqual(self.changes(), ["release edit v0.2.0 --draft=false"])

    def test_a_draft_that_stays_a_draft_fails_the_job(self) -> None:
        self.existing("v0.2.0", draft=True)
        result = self.release(GH_BROKEN="edit")
        self.assertEqual(result.returncode, 1)
        self.assertIn("still draft", result.stderr)

    def test_a_rerun_leaves_a_published_release_as_it_is(self) -> None:
        self.existing("v0.2.0", draft=False)
        result = self.release()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("already holds", result.stdout)
        self.assertIn("release download v0.2.0", self.calls()[-1])
        self.assertEqual(self.changes(), [])

    def test_a_rerun_refuses_a_release_with_other_assets_or_bytes(self) -> None:
        sums = (self.dist / "SHA256SUMS").read_bytes()
        for draft in (True, False):
            for assets in (
                {self.CRATE: b"the crate", "SHA256SUMS": sums, "extra": b""},
                {self.CRATE: b"the crate"},
                {self.CRATE: b"other bytes", "SHA256SUMS": sums},
                {self.CRATE: b"the crate", "SHA256SUMS": sums.replace(self.CRATE.encode(), b"/dist/" + self.CRATE.encode())},
            ):
                with self.subTest(draft=draft, assets=sorted(assets)):
                    self.reset()
                    self.existing("v0.2.0", draft=draft, assets=assets)
                    result = self.release()
                    self.assertEqual(result.returncode, 1, result.stderr)
                    self.assertEqual(self.releases(), {"v0.2.0": "true" if draft else "false"})
                    self.assertEqual(self.changes(), [])

    def test_a_lookup_that_fails_changes_nothing_and_fails_the_job(self) -> None:
        for state in ("absent", "draft", "published"):
            with self.subTest(state):
                self.reset()
                if state != "absent":
                    self.existing("v0.2.0", draft=state == "draft")
                before = self.releases()
                result = self.release(GH_FAIL="release view v0.2.0 --json isDraft")
                self.assertEqual(result.returncode, 1)
                self.assertIn(API_ERROR, result.stderr)
                self.assertIn("couldn't say whether release v0.2.0 is there", result.stderr)
                self.assertEqual(self.releases(), before)
                self.assertEqual(self.changes(), [])

    def test_a_rehearsal_makes_a_draft_checks_it_and_leaves_nothing(self) -> None:
        self.existing("v0.1.1", draft=False)
        result = self.rehearse()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertLess(result.stdout.index("deleted the draft release rehearsal-7-1"), result.stdout.index("is deleted"))
        self.assertEqual(self.releases(), {"v0.1.1": "false"})
        [create] = [call for call in self.calls() if call.startswith("release create")]
        self.assertTrue(create.startswith("release create rehearsal-7-1 --draft --target main "), create)
        self.assertNotIn("--verify-tag", create)
        self.assertEqual(self.changes(), [create, "release delete rehearsal-7-1 --yes"])
        self.assertIn("release download rehearsal-7-1", " ".join(self.calls()))
        self.assertEqual(self.calls()[-1], "release view rehearsal-7-1 --json isDraft --jq .isDraft")
        self.assertFalse([call for call in self.calls() if "v0.1.1" in call])

    def test_a_rehearsal_that_fails_a_check_deletes_its_draft(self) -> None:
        for name, env, message in (
            ("other notes", {"GH_BODY": "other notes"}, "notes aren't the notes"),
            ("other bytes", {"GH_BROKEN": "create-tampered"}, None),
        ):
            with self.subTest(name):
                self.reset()
                result = self.rehearse(**env)
                self.assertEqual(result.returncode, 1)
                if message:
                    self.assertIn(message, result.stderr)
                self.assertIn("deleted the draft release rehearsal-7-1", result.stdout)
                self.assertEqual(self.releases(), {})

    def test_a_rehearsal_whose_release_is_not_a_draft_fails_and_leaves_it(self) -> None:
        result = self.rehearse(GH_BROKEN="create-published")
        self.assertEqual(result.returncode, 1)
        self.assertIn("it is published, not a draft", result.stderr)
        self.assertIn("::warning::release rehearsal-7-1 is published", result.stdout)
        self.assertEqual(self.releases(), {"rehearsal-7-1": "false"})

    def test_a_rehearsal_whose_draft_outlives_its_deletion_fails(self) -> None:
        result = self.rehearse(GH_BROKEN="delete")
        self.assertEqual(result.returncode, 1)
        self.assertIn("still there after its deletion", result.stderr)

    def test_a_rehearsal_whose_cleanup_cannot_look_fails(self) -> None:
        # The lookups before the create and after it pass, the notes differ, and the trap's fails.
        result = self.rehearse(GH_BODY="other notes", GH_FAIL="release view rehearsal-7-1 --json isDraft", GH_FAIL_AFTER="2")
        self.assertEqual(result.returncode, 1)
        self.assertIn("couldn't say whether release rehearsal-7-1 is there", result.stderr)
        self.assertEqual(self.releases(), {"rehearsal-7-1": "true"})
        result = self.run_script("discard", "rehearsal-7-1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.releases(), {})

    def test_a_rehearsal_takes_only_a_new_rehearsal_name(self) -> None:
        self.existing("v0.2.0", draft=True)
        self.existing("rehearsal-7-1", draft=False)
        for name in ("v0.2.0", "rehearsal-7-1", "rehearsal-7", "rehearsal-7-1-x"):
            with self.subTest(name=name):
                self.assertEqual(self.rehearse(name).returncode, 1)
        self.assertEqual(self.rehearse("rehearsal-8-1", GH_FAIL="release view rehearsal-8-1").returncode, 1)
        self.assertEqual(self.releases(), {"v0.2.0": "true", "rehearsal-7-1": "false"})
        self.assertEqual(self.changes(), [])

    def test_discard_deletes_a_rehearsals_draft_and_nothing_else(self) -> None:
        self.existing("rehearsal-7-1", draft=True)
        self.existing("rehearsal-8-1", draft=False)
        self.existing("v0.2.0", draft=True)
        for name, said in (
            ("rehearsal-7-1", "::notice::deleted the draft release rehearsal-7-1"),
            ("rehearsal-8-1", "::warning::release rehearsal-8-1 is published"),
            ("rehearsal-9-1", "::notice::there is no draft release rehearsal-9-1"),
        ):
            with self.subTest(name):
                result = self.run_script("discard", name)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(said, result.stdout)
        self.assertEqual(self.run_script("discard", "v0.2.0").returncode, 1)
        self.assertEqual(self.releases(), {"rehearsal-8-1": "false", "v0.2.0": "true"})
        self.assertEqual(self.changes(), ["release delete rehearsal-7-1 --yes"])

    def test_discard_warns_of_another_rehearsals_draft(self) -> None:
        self.existing("rehearsal-6-1", draft=True)
        self.existing("rehearsal-7-1", draft=True)
        self.existing("v0.2.0", draft=True)
        result = self.run_script("discard", "rehearsal-7-1")
        self.assertEqual(result.returncode, 0, result.stderr)
        warnings = [line for line in result.stdout.splitlines() if line.startswith("::warning::")]
        self.assertEqual(len(warnings), 1, result.stdout)
        self.assertIn("rehearsal-6-1 of another rehearsal", warnings[0])
        self.assertEqual(self.releases(), {"rehearsal-6-1": "true", "v0.2.0": "true"})

    def test_discard_that_cannot_look_fails(self) -> None:
        for failing in ("release view rehearsal-7-1", "release list"):
            with self.subTest(failing):
                self.reset()
                self.existing("rehearsal-7-1", draft=True)
                result = self.run_script("discard", "rehearsal-7-1", GH_FAIL=failing)
                self.assertEqual(result.returncode, 1)
                self.assertIn(API_ERROR, result.stderr)

    def test_a_command_with_the_wrong_arguments_prints_the_usage(self) -> None:
        for args in ((), ("release", "v0.2.0"), ("discard",), ("publish", "v0.2.0")):
            with self.subTest(args=args):
                result = self.run_script(*args)
                self.assertEqual(result.returncode, 2)
                self.assertIn("tools/github_release.sh rehearse NAME", result.stderr)
                self.assertIn("Each lookup tells", result.stderr)
                self.assertNotIn("set -euo", result.stderr)
        self.assertEqual(self.calls(), [])


class Release(unittest.TestCase):
    """tools/check_release.py, against a scratch repository laid out as this one is."""

    MANIFEST = b'[package]\nname = "ayaml"\nversion.workspace = true\n\n[dependencies.indexmap]\nversion = "2.0.0"\n\n[dependencies.serde]\nversion = "1.0.220"\n\n[package.metadata.docs.rs]\nfeatures = ["libyaml-compat"]\n'
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
        (self.root / "crates/ayaml/CHANGELOG.md").write_text(self.CHANGELOG, encoding="utf-8")
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
            "Cargo.toml": b'[package]\nname = "ayaml"\nversion = "0.2.0"\n\n[dependencies.indexmap]\nversion = "2.0.0"\n\n[dependencies.serde]\nversion = "1.0.220"\n\n[package.metadata.docs.rs]\nfeatures = ["libyaml-compat"]\n',
            "Cargo.toml.orig": self.MANIFEST,
            "Cargo.lock": b"# lock\n",
            "README.md": b"# ayaml\n",
            "CHANGELOG.md": self.CHANGELOG.encode(),
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

    def test_a_package_from_before_the_self_contained_layout_still_passes(self) -> None:
        (self.root / "crates/ayaml/CHANGELOG.md").unlink()
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "-m", "old layout")
        self.commit = git(self.root, "rev-parse", "HEAD")
        files = self.packaged()
        files.pop("CHANGELOG.md")
        self.check_package(files)

    def test_a_file_the_commit_does_not_have_is_refused(self) -> None:
        self.refused(lambda: self.check_package(self.packaged({"src/extra.rs": b"x"})), "not in")

    def test_a_workspace_and_crate_changelog_difference_is_refused(self) -> None:
        (self.root / "crates/ayaml/CHANGELOG.md").write_text(self.CHANGELOG + "\n", encoding="utf-8")
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "-m", "crate changelog differs")
        self.commit = git(self.root, "rev-parse", "HEAD")
        files = self.packaged({"CHANGELOG.md": (self.CHANGELOG + "\n").encode()})
        self.refused(lambda: self.check_package(files), "workspace and packaged changelogs")

    def test_a_missing_or_changed_file_is_refused(self) -> None:
        self.refused(lambda: self.check_package(self.packaged({"src/lib.rs": None})), "missing src/lib.rs")
        self.refused(lambda: self.check_package(self.packaged({"src/lib.rs": b"changed"})), "differs from")
        self.refused(lambda: self.check_package(self.packaged({"README.md": b"other"})), "differs from")
        self.refused(lambda: self.check_package(self.packaged({"CHANGELOG.md": b"other"})), "differs from")
        self.refused(lambda: self.check_package(self.packaged({"CHANGELOG.md": None})), "missing CHANGELOG.md")
        self.refused(lambda: self.check_package(self.packaged({"Cargo.toml.orig": b"x"})), "differs from")
        no_docs = self.packaged({"Cargo.toml": b'[package]\nname = "ayaml"\nversion = "0.2.0"\n\n[dependencies.indexmap]\nversion = "2.0.0"\n\n[dependencies.serde]\nversion = "1.0.220"\n'})
        self.refused(lambda: self.check_package(no_docs), "doesn't document libyaml-compat")
        exact = self.packaged({"Cargo.toml": b'[package]\nname = "ayaml"\nversion = "0.2.0"\n\n[dependencies.indexmap]\nversion = "=2.0.0"\n\n[dependencies.serde]\nversion = "1.0.220"\n\n[package.metadata.docs.rs]\nfeatures = ["libyaml-compat"]\n'})
        self.refused(lambda: self.check_package(exact), "doesn't give indexmap a caret requirement")

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

    def test_an_unexpected_failure_is_no_answer_either(self) -> None:
        # Exit 1 sends the workflow on to publish, so only crates.io's own "no" may give it.
        for failure in [ValueError("bad"), UnicodeEncodeError("ascii", "é", 0, 1, "x"), check_release.Refused("x")]:
            with mock.patch.object(check_release, "published", side_effect=failure), mock.patch("sys.stderr", io.StringIO()):
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


GITHUB = Path(__file__).resolve().parent.parent / ".github"
WORKFLOWS = GITHUB / "workflows"
# A `uses` key, quoted or not, in a block step or a flow mapping, and what it names.
USES = re.compile(r"""(?:^|[\s{,])(?:-\s+)?["']?uses["']?\s*:\s*["']?([^\s"',}#]+)["']?""")
COMMIT_REF = re.compile(r"[\w.-]+/[\w./-]+@[0-9a-f]{40}")
# The version a commit pin is said to be: a release, not a branch or a word such as `latest`.
VERSION_COMMENT = re.compile(r"#\s*v\d+(?:\.\d+){0,2}\s*$")
SCANNER_ACTION = re.compile(r"\s+(?:-\s+)?uses: trufflesecurity/trufflehog@[0-9a-f]{40} # v(\S+)")
SCANNER_TAG = re.compile(r'\s+SCANNER_TAG: "([^"]+)"')
SCANNER_DIGEST = re.compile(r"\s+SCANNER_DIGEST: sha256:[0-9a-f]{64}")
SCANNER_VERSION = "version: ${{ env.SCANNER_TAG }}@${{ env.SCANNER_DIGEST }}"


def workflow_files() -> list[Path]:
    """The workflows, and the composite actions this repository holds, which pin actions too."""
    found = [*WORKFLOWS.glob("*.yml"), *WORKFLOWS.glob("*.yaml")]
    found += [*GITHUB.glob("actions/**/action.yml"), *GITHUB.glob("actions/**/action.yaml")]
    return sorted(found)


def unpinned_actions(text: str) -> list[str]:
    """Each action used by anything but a full commit with its release beside it, and each image
    used by anything but a digest."""
    found = []
    for line in text.splitlines():
        # A comment starts at a `#` that begins the line or follows a blank.
        comment = re.search(r"(?:^|\s)#", line)
        code, note = (line, "") if comment is None else (line[: comment.start()], line[comment.start() :])
        for used in USES.finditer(code):
            name = used[1]
            if name.startswith("./"):
                continue
            if name.startswith("docker://"):
                pinned = "@sha256:" in name
            else:
                pinned = COMMIT_REF.fullmatch(name) is not None and VERSION_COMMENT.search(note) is not None
            if not pinned:
                found.append(line.strip())
    return found


def scanner_versions(text: str) -> tuple[str, str]:
    """The secret scan action's version, and the tag of the image its job pins, or "" where the
    job doesn't pin one by tag and digest or the step doesn't run it."""
    lines = text.splitlines()
    tag = next((match[1] for line in lines if (match := SCANNER_TAG.fullmatch(line))), "")
    digest = any(SCANNER_DIGEST.fullmatch(line) for line in lines)
    for at, line in enumerate(lines):
        action = SCANNER_ACTION.fullmatch(line)
        if action:
            step = []
            for later in lines[at + 1 :]:
                if re.match(r"\s+- ", later):
                    break
                step.append(later.strip())
            runs_pinned = SCANNER_VERSION in step
            return action[1], tag if digest and runs_pinned else ""
    raise AssertionError("no secret scan step")


class WorkflowPins(unittest.TestCase):
    def test_every_action_is_pinned_to_a_commit_with_its_version(self) -> None:
        files = workflow_files()
        self.assertGreaterEqual(len(files), 4)
        for path in files:
            self.assertEqual(unpinned_actions(path.read_text()), [], path.name)

    def test_an_action_by_tag_or_branch_or_without_its_version_is_found(self) -> None:
        commit = "3d3c42e5aac5ba805825da76410c181273ba90b1"
        unpinned = [
            "- uses: actions/checkout@v7",
            "- uses: dtolnay/rust-toolchain@master",
            f"uses: actions/cache@{commit}",
            f"- uses: actions/checkout@{commit[:12]} # v7.0.1",
            f"- uses: dtolnay/rust-toolchain@{commit} # master",
            f"- uses: actions/checkout@{commit} # latest",
            f"- uses: actions/checkout@{commit} # v7.0.1-beta",
            f'- "uses": actions/checkout@v7',
            f"- uses: 'actions/checkout@v7'",
            "- {uses: actions/checkout@v7, with: {fetch-depth: 0}}",
            "- name: x\n  uses: docker://alpine:3.20",
        ]
        pinned = [
            "- uses: ./.github/workflows/ci.yml",
            f"- uses: actions/checkout@{commit} # v7.0.1",
            f"- uses: dtolnay/rust-toolchain@{commit} # v1",
            f'- "uses": "actions/checkout@{commit}" # v7.0.1',
            f"- {{uses: actions/checkout@{commit}, with: {{fetch-depth: 0}}}} # v7.0.1",
            f"- uses: docker://alpine@sha256:{'0' * 64}",
            "# uses: actions/checkout@v7 in a comment is no step",
        ]
        for line in unpinned:
            self.assertEqual(unpinned_actions("      " + line), [line.splitlines()[-1].strip()], line)
        for line in pinned:
            self.assertEqual(unpinned_actions("      " + line), [], line)

    def test_composite_actions_and_yaml_files_are_read(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            (root / "workflows").mkdir()
            (root / "actions" / "setup").mkdir(parents=True)
            for path in ["workflows/a.yml", "workflows/b.yaml", "actions/setup/action.yml", "actions/setup/x.yml"]:
                (root / path).write_text("")
            with mock.patch.object(sys.modules[__name__], "GITHUB", root), mock.patch.object(
                sys.modules[__name__], "WORKFLOWS", root / "workflows"
            ):
                names = [str(path.relative_to(root)) for path in workflow_files()]
        self.assertEqual(names, ["actions/setup/action.yml", "workflows/a.yml", "workflows/b.yaml"])

    def test_the_secret_scanner_runs_the_version_of_its_action(self) -> None:
        action, image = scanner_versions((WORKFLOWS / "ci.yml").read_text())
        self.assertEqual(image, action, "pin ghcr.io/trufflesecurity/trufflehog at the action's version, by its digest")

    def test_an_image_an_update_left_behind_or_without_a_digest_is_found(self) -> None:
        job = (
            "    env:\n"
            "{env}"
            "    steps:\n"
            "      - name: TruffleHog\n"
            f"        uses: trufflesecurity/trufflehog@{'e' * 40} # v3.99.8\n"
            "        with:\n"
            "          path: ./\n"
            "          {version}\n"
            "      - name: Next\n"
            f"          {SCANNER_VERSION}\n"
        )
        digest = f"      SCANNER_DIGEST: sha256:{'0' * 64}\n"

        def versions(tag: str, with_digest: bool = True, version: str = SCANNER_VERSION) -> tuple[str, str]:
            env = f'      SCANNER_TAG: "{tag}"\n' + (digest if with_digest else "")
            return scanner_versions(job.format(env=env, version=version))

        self.assertEqual(versions("3.99.7"), ("3.99.8", "3.99.7"))
        self.assertEqual(versions("3.99.8"), ("3.99.8", "3.99.8"))
        self.assertEqual(versions("3.99.8", with_digest=False), ("3.99.8", ""))
        self.assertEqual(versions("3.99.8", version="version: 3.99.8"), ("3.99.8", ""))


class InstructionsBase(unittest.TestCase):
    def test_a_missing_or_zero_base_falls_back_to_the_parent(self) -> None:
        for given in ["", "0" * 40, "  "]:
            self.assertEqual(check_instructions.base_revision(given), "HEAD~1")
        self.assertEqual(check_instructions.base_revision("abc123"), "abc123")


if __name__ == "__main__":
    unittest.main()
