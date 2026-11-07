#!/usr/bin/env python3
"""Notice a scheduled workflow that has failed or stopped running.

usage: tools/check_scheduled_runs.py [--open-issue]

Asks `gh` about each workflow in WATCHED. Each one needs at least one run on main within
WINDOW_HOURS, and every scheduled run in that window has to have succeeded, with no sealed file left
behind. A run by hand counts as a run, so running a workflow is enough to show that it works before
its first scheduled run, but its own result is not reported.

With --open-issue, as in the scheduled-watch workflow, a problem opens one issue, and each new
problem adds to the one that's open. A problem any earlier issue reported isn't reported again, so
closing an issue doesn't bring back what it held. Only a person closes it: a later green run says
nothing about what a failed run found, which is sealed in that run's artifact until someone reads
it. The issue names the workflow and links the run, and holds nothing from its log.

GitHub turns off every schedule in a repository after 60 days with no activity, this one included,
so `tools/ci-local.sh` runs this too.

Exit status: 0 nothing is wrong, 1 a problem, 2 the check could not run.
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
import json
import subprocess
import sys

WATCHED = ("fuzz.yml", "advisories.yml")
# A daily schedule has run up to 8 hours late, so a shorter window would report a late run, and
# this one still reports a single missed run.
WINDOW_HOURS = 36
ISSUE_TITLE = "A scheduled workflow failed or stopped running"
RUN_FIELDS = "conclusion,createdAt,databaseId,event,status,url"


def problems(workflow: str, runs: list[dict], now: datetime) -> list[str]:
    """What's wrong with one workflow, given its recent runs."""
    window = timedelta(hours=WINDOW_HOURS)
    recent = [
        run for run in runs if now - datetime.fromisoformat(run["createdAt"]) <= window
    ]
    if not recent:
        # The last run's time keeps one outage's text the same from day to day, and tells it from
        # a later one.
        last = max((run["createdAt"] for run in runs), default=None)
        since = f", and the last was at {last}" if last else ""
        return [f"{workflow} has had no run in the last {WINDOW_HOURS} hours{since}"]
    found = []
    for run in recent:
        # A run started by hand shows that the workflow still runs, but its result says
        # only what that run was asked to do, so it opens no issue.
        if run["event"] != "schedule" or run["status"] != "completed":
            continue
        if run["conclusion"] != "success":
            found.append(f"{workflow} ended with {run['conclusion']}: {run['url']}")
        elif run.get("sealed"):
            found.append(f"{workflow} passed and left sealed files: {run['url']}")
    return found


def gh(*arguments: str) -> str:
    return subprocess.run(
        ["gh", *arguments], check=True, capture_output=True, text=True
    ).stdout


def recent_runs(workflow: str, now: datetime) -> list[dict]:
    runs = json.loads(
        gh(
            "run",
            "list",
            "--workflow",
            workflow,
            "--branch",
            "main",
            "--limit",
            "20",
            "--json",
            RUN_FIELDS,
        )
    )
    # A green run says nothing of what it left, such as a slow unit, so ask for its artifacts.
    window = timedelta(hours=WINDOW_HOURS)
    for run in runs:
        if (
            run["event"] == "schedule"
            and run["conclusion"] == "success"
            and now - datetime.fromisoformat(run["createdAt"]) <= window
        ):
            artifacts = gh(
                "api", f"repos/{{owner}}/{{repo}}/actions/runs/{run['databaseId']}/artifacts"
            )
            run["sealed"] = json.loads(artifacts)["total_count"]
    return runs


def as_list(found: list[str]) -> str:
    return "\n".join(f"- {problem}" for problem in found)


def update_issue(found: list[str]) -> None:
    """Report what no issue has yet, in the open one, or in a new one if there's none."""
    if not found:
        return
    issues = [
        issue
        for issue in json.loads(
            gh(
                "issue",
                "list",
                "--state",
                "all",
                "--limit",
                "100",
                "--json",
                "body,comments,number,state,title",
            )
        )
        if issue["title"] == ISSUE_TITLE
    ]
    reported = "\n".join(
        text
        for issue in issues
        for text in (issue["body"], *(comment["body"] for comment in issue["comments"]))
    )
    new = [problem for problem in found if problem not in reported]
    if not new:
        return
    still_open = [issue["number"] for issue in issues if issue["state"] == "OPEN"]
    if still_open:
        gh("issue", "comment", str(still_open[0]), "--body", as_list(new))
    else:
        gh("issue", "create", "--title", ISSUE_TITLE, "--body", as_list(new))


def main(arguments: list[str]) -> int:
    if arguments not in ([], ["--open-issue"]):
        print(__doc__, file=sys.stderr)
        return 2
    now = datetime.now(timezone.utc)
    try:
        found = [
            problem
            for workflow in WATCHED
            for problem in problems(workflow, recent_runs(workflow, now), now)
        ]
        if arguments:
            update_issue(found)
    except (OSError, subprocess.CalledProcessError, ValueError, KeyError) as error:
        detail = getattr(error, "stderr", "") or error
        print(f"FAIL: the check could not run: {detail}")
        return 2
    for problem in found:
        print(f"FAIL: {problem}")
    if found:
        return 1
    print(f"scheduled runs ok: {', '.join(WATCHED)}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
