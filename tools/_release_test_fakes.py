"""Shared fake `gh`, `curl`, and `check_release.py` stubs for release script tests."""

from __future__ import annotations

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
