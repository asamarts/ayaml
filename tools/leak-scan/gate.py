#!/usr/bin/env python3
"""Reject content that must not reach this public repository.

Commands:

    gate.py scan --head REV [--not REV ...] [--exclude-remote NAME] [--ref NAME ...]
        Scan every commit reachable from REV and not from the excluded
        revisions (raw metadata and message, changed paths, added lines), any
        annotated tags REV names, the whole tree at REV, and the given branch
        or tag names.

    gate.py text NAME [NAME ...]
        Scan the text held in the named environment variables, such as a pull
        request title and body. An unset variable counts as empty.

    gate.py check-list
        Load the pattern list and report how many patterns each class has.

Exit status: 0 clean, 1 something was found, 2 the gate could not run. Anything
other than 0 must stop the change, so a missing list or scanner fails closed.

Findings name a location and a pattern id. They never print the matching text or
the pattern, because CI logs are public and the list is private.

See README.md in this directory for the list format and setup.
"""

from __future__ import annotations

import argparse
import dataclasses
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tempfile

GATE_DIR = Path(__file__).resolve().parent
REPO_CONFIG = GATE_DIR.parent.parent / ".gitleaks.toml"
PINS = GATE_DIR / "gitleaks.env"

LIST_ENV = "AYAML_LEAK_PATTERNS"
LIST_FILE_ENV = "AYAML_LEAK_PATTERNS_FILE"
LIST_CONFIG = "ayaml.leakPatterns"
SCANNER_ENV = "AYAML_GITLEAKS"
SCANNER_CONFIG = "ayaml.gitleaks"

# A list without one of these classes, or whose last pattern is not of the end
# class, is treated as truncated.
REQUIRED_CLASSES = ("host", "runner", "repo", "comparison")
END_CLASS = "end"
SCOPES = ("tree", "added")


class GateError(Exception):
    """The gate could not run. The message must not contain list content."""


@dataclasses.dataclass(frozen=True)
class Pattern:
    id: str
    cls: str
    scope: str
    regex: re.Pattern[bytes]


def builtin(id_: str, regex: bytes) -> Pattern:
    return Pattern(id_, "path", "tree", re.compile(regex, re.IGNORECASE))


# Personal paths reveal nothing by being listed here, so they need no private
# list. The trailing separators keep prose such as "the home directory" clean.
# The last entry is the scanner's own opt-out comment, split so that this file
# doesn't hold it.
BUILTIN_PATTERNS = (
    builtin("path-home", rb"/home/[a-z_][a-z0-9_-]*/"),
    builtin("path-macos", rb"/Users/[a-z][a-z0-9._-]*/"),
    builtin("path-windows", rb"\b[a-z]:\\Users\\"),
    builtin("path-tilde", rb"~/(?:projects|\.tmp|\.claude)\b"),
    builtin("path-home-var", rb"\$\{?HOME\}?/(?:projects|\.tmp|\.claude)\b"),
    builtin("path-root", rb"(?<![\w./-])/root/[\w.-]"),
    builtin("scanner-opt-out", rb"gitleaks:" + rb"allow"),
)


def variants(data: bytes) -> list[bytes]:
    """Return the forms of `data` to scan. UTF-16 and UTF-32 hide text behind NULs."""
    return [data, data.replace(b"\0", b"")] if b"\0" in data else [data]


@dataclasses.dataclass
class Findings:
    # (location, pattern id, class) -> (first line, occurrences)
    hits: dict[tuple[str, str, str], tuple[int, int]] = dataclasses.field(default_factory=dict)

    def scan(self, location: str, data: bytes, patterns: list[Pattern]) -> None:
        for form in variants(data):
            for pattern in patterns:
                for match in pattern.regex.finditer(form):
                    key = (location, pattern.id, pattern.cls)
                    line = form.count(b"\n", 0, match.start()) + 1
                    first, count = self.hits.get(key, (line, 0))
                    self.hits[key] = (first, count + 1)

    def report(self, patterns: list[Pattern]) -> None:
        for (location, pattern_id, cls), (line, count) in sorted(self.hits.items()):
            extra = f" ({count} occurrences)" if count > 1 else ""
            # A path can itself hold the private text, and CI logs are public.
            shown = redact(location.encode("utf-8", "surrogateescape"), patterns).decode("utf-8", "replace")
            print(f"leak gate: {shown}:{line}: {cls} pattern {pattern_id}{extra}", file=sys.stderr)


def git(*args: str, input_: bytes | None = None, check: bool = True) -> bytes:
    completed = subprocess.run(
        ["git", "-c", "core.quotePath=false", *args],
        input=input_,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if check and completed.returncode != 0:
        raise GateError(f"git {args[0]} failed with status {completed.returncode}")
    return completed.stdout


def git_config_path(name: str) -> str:
    return git("config", "--type=path", "--get", name, check=False).decode("utf-8", "replace").strip()


def read_list_text() -> str:
    text = os.environ.get(LIST_ENV, "")
    if text.strip():
        return text
    path = os.environ.get(LIST_FILE_ENV, "") or git_config_path(LIST_CONFIG)
    if not path:
        raise GateError(
            f"no pattern list: set {LIST_ENV}, {LIST_FILE_ENV}, or `git config {LIST_CONFIG}`. "
            "On a pull request from a fork the list is unavailable; a maintainer has to "
            "bring the change onto a branch in this repository"
        )
    try:
        return Path(path).read_text(encoding="utf-8")
    except OSError as error:
        raise GateError(f"cannot read the pattern list ({error.strerror})") from None


def parse_list(text: str) -> list[Pattern]:
    patterns: list[Pattern] = []
    seen: set[str] = set()
    for number, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        fields = line.split(None, 3)
        if len(fields) != 4:
            raise GateError(f"pattern list line {number}: expected `id class scope regex`")
        id_, cls, scope, regex = fields
        if scope not in SCOPES:
            raise GateError(f"pattern list line {number}: scope must be one of {', '.join(SCOPES)}")
        if id_ in seen:
            raise GateError(f"pattern list line {number}: repeated id")
        seen.add(id_)
        try:
            compiled = re.compile(regex.encode("utf-8"), re.IGNORECASE)
        except re.error:
            raise GateError(f"pattern list line {number}: invalid regular expression") from None
        patterns.append(Pattern(id_, cls, scope, compiled))
    missing = [cls for cls in REQUIRED_CLASSES if not any(p.cls == cls for p in patterns)]
    if missing:
        raise GateError(f"pattern list has no {', '.join(missing)} pattern, so it looks truncated")
    if patterns[-1].cls != END_CLASS:
        raise GateError(f"pattern list does not end with an {END_CLASS} pattern, so it looks truncated")
    return patterns


def load_patterns() -> list[Pattern]:
    return [*BUILTIN_PATTERNS, *parse_list(read_list_text())]


def expected_scanner_version() -> str:
    for line in PINS.read_text(encoding="utf-8").splitlines():
        if line.startswith("GITLEAKS_VERSION="):
            return line.split("=", 1)[1].strip()
    raise GateError("gitleaks.env has no pinned version")


def find_scanner() -> str:
    candidate = os.environ.get(SCANNER_ENV, "") or git_config_path(SCANNER_CONFIG) or shutil.which("gitleaks") or ""
    if not candidate:
        raise GateError(
            f"gitleaks not found: run tools/leak-scan/install-gitleaks.sh, then set {SCANNER_ENV} "
            f"or `git config {SCANNER_CONFIG}`"
        )
    expected = expected_scanner_version()
    try:
        observed = subprocess.run([candidate, "version"], capture_output=True, text=True).stdout.strip()
    except OSError:
        raise GateError("gitleaks could not be executed") from None
    if observed != expected:
        raise GateError(f"gitleaks {expected} is pinned; found {observed or 'an unknown version'}")
    return candidate


def redact(data: bytes, patterns: list[Pattern]) -> bytes:
    """Remove pattern matches from text the gate is about to print."""
    for pattern in patterns:
        data = pattern.regex.sub(b"[redacted]", data)
    return data


def run_scanner(scanner: str, patterns: list[Pattern], mode: list[str], input_: bytes | None = None) -> bool:
    """Return True when gitleaks reports nothing."""
    # gitleaks reads an ignore file from its working directory. Running it here
    # keeps the change being scanned from supplying one.
    completed = subprocess.run(
        [scanner, *mode, "--no-banner", "--redact", "--log-level=warn", "--config", str(REPO_CONFIG)],
        input=input_ if input_ is not None else b"",
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        cwd=GATE_DIR,
    )
    if completed.returncode != 0:
        # gitleaks redacts the secret value. Its report still names files.
        sys.stderr.write(redact(completed.stdout, patterns).decode("utf-8", "replace"))
        print(f"leak gate: gitleaks {mode[0]} failed the scan", file=sys.stderr)
        return False
    return True


def resolves(rev: str) -> bool:
    return (
        subprocess.run(
            ["git", "rev-parse", "--verify", "--quiet", f"{rev}^{{commit}}"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        ).returncode
        == 0
    )


# A signature's armor, and the base64 between. Anything else in a signature header is read as text.
ARMOR_BEGIN = re.compile(rb"-----BEGIN (?:PGP|SSH) SIGNATURE-----")
ARMOR_END = re.compile(rb"-----END (?:PGP|SSH) SIGNATURE-----")
BASE64_LINE = re.compile(rb"[A-Za-z0-9+/=]*")
HUNK = re.compile(rb"^@@ -\d+(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")


def added_lines(sha: str) -> list[tuple[str, int, bytes]]:
    """Return (path, new line number, text) for every line a commit adds."""
    diff = git(
        "-c",
        "diff.noprefix=false",
        "-c",
        "diff.mnemonicPrefix=false",
        "diff-tree",
        "-r",
        "--root",
        "-m",
        "--first-parent",
        "--no-commit-id",
        "--no-renames",
        "--no-ext-diff",
        "--no-color",
        "--text",
        "--unified=0",
        "-p",
        sha,
    )
    added: list[tuple[str, int, bytes]] = []
    path = "?"
    old_left = new_left = 0
    new_line = 0
    for line in diff.split(b"\n"):
        if old_left or new_left:
            if line.startswith(b"-"):
                old_left -= 1
            elif line.startswith(b"+"):
                added.append((path, new_line, line[1:]))
                new_line += 1
                new_left -= 1
            continue
        if line.startswith(b"+++ "):
            name = line[4:]
            if name.startswith(b"b/"):
                name = name[2:]
            path = name.decode("utf-8", "replace")
            continue
        hunk = HUNK.match(line)
        if hunk:
            old_left = int(hunk.group(1)) if hunk.group(1) is not None else 1
            new_line = int(hunk.group(2))
            new_left = int(hunk.group(3)) if hunk.group(3) is not None else 1
    return added


def changed_paths(sha: str) -> list[str]:
    names = git(
        "diff-tree",
        "-r",
        "--root",
        "-m",
        "--first-parent",
        "--no-commit-id",
        "--no-renames",
        "--name-only",
        "-z",
        sha,
    )
    return [name.decode("utf-8", "replace") for name in names.split(b"\0") if name]


def without_signature(raw: bytes) -> bytes:
    """Return a commit's raw text without its signature.

    GitHub signs the commits it writes, and base64 can look like anything, so a signature
    matched a short pattern by chance on `main`. It holds nothing anyone wrote. Only armor and the
    base64 inside it are left out, so any other text in that header is still read.
    """
    headers, separator, message = raw.partition(b"\n\n")
    kept = []
    in_signature = in_armor = False
    for line in headers.split(b"\n"):
        if line.startswith(b" "):
            text = line[1:]
        else:
            name, _, text = line.partition(b" ")
            in_signature = name in (b"gpgsig", b"gpgsig-sha256")
            in_armor = False
        if in_signature and not in_armor and ARMOR_BEGIN.fullmatch(text):
            in_armor = True
        elif in_armor and ARMOR_END.fullmatch(text):
            in_armor = False
        elif not (in_armor and BASE64_LINE.fullmatch(text)):
            kept.append(line)
    return b"\n".join(kept) + separator + message


def scan_commits(shas: list[str], patterns: list[Pattern], findings: Findings) -> bytes:
    """Scan each commit and return what it adds, as text for gitleaks.

    gitleaks reads this text rather than the repository, because its own history
    mode exits 0 when it can't read the history.
    """
    text = bytearray()
    for sha in shas:
        short = sha[:12]
        raw = without_signature(git("cat-file", "commit", sha))
        text += b"\n".join(variants(raw)) + b"\n"
        findings.scan(f"commit {short} message or metadata", raw, patterns)
        for path in changed_paths(sha):
            findings.scan(f"commit {short} path {path}", path.encode("utf-8", "surrogateescape"), patterns)
        for path, line, added in added_lines(sha):
            text += b"\n".join(variants(added)) + b"\n"
            findings.scan(f"commit {short} {path}:{line}", added, patterns)
    return bytes(text)


def peel_tags(rev: str) -> list[bytes]:
    """Return every annotated tag object between REV and the commit it names."""
    tags: list[bytes] = []
    oid = git("rev-parse", "--verify", rev).decode().strip()
    while git("cat-file", "-t", oid).strip() == b"tag":
        raw = git("cat-file", "tag", oid)
        tags.append(raw)
        oid = raw.split(b"\n", 1)[0].split()[1].decode()
    return tags


def tree_entries(rev: str) -> list[tuple[str, str]]:
    listing = git("ls-tree", "-r", "-z", "--full-tree", rev)
    entries: list[tuple[str, str]] = []
    for item in listing.split(b"\0"):
        if not item:
            continue
        meta, name = item.split(b"\t", 1)
        mode, kind, oid = meta.split()
        if kind == b"blob":
            entries.append((name.decode("utf-8", "surrogateescape"), oid.decode()))
    return entries


def scan_tree(rev: str, patterns: list[Pattern], findings: Findings, export: Path) -> None:
    """Scan tree-scoped patterns over paths and blobs, exporting for gitleaks.

    Exported names get a suffix because gitleaks skips files by extension, and
    errors name no path because a path can hold the private text.
    """
    tree_patterns = [pattern for pattern in patterns if pattern.scope == "tree"]
    batch = subprocess.Popen(["git", "cat-file", "--batch"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    assert batch.stdin is not None and batch.stdout is not None
    try:
        for path, oid in tree_entries(rev):
            findings.scan(f"tree path {path}", path.encode("utf-8", "surrogateescape"), tree_patterns)
            if ".." in PurePosixPath(path).parts or path.startswith("/"):
                raise GateError("a path in the tree leaves the tree")
            batch.stdin.write(oid.encode() + b"\n")
            batch.stdin.flush()
            header = batch.stdout.readline().split()
            if len(header) != 3:
                raise GateError("cannot read a blob")
            data = batch.stdout.read(int(header[2]))
            batch.stdout.read(1)
            findings.scan(f"tree {path}", data, tree_patterns)
            target = export / f"{path}.scan"
            try:
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
            except OSError as error:
                raise GateError(f"cannot export a blob ({error.strerror})") from None
    finally:
        batch.stdin.close()
        batch.stdout.close()
        batch.wait()


def command_scan(args: argparse.Namespace) -> int:
    if not resolves(args.head):
        raise GateError("the revision to scan does not resolve")
    patterns = load_patterns()
    scanner = find_scanner()

    # An exclusion that no longer resolves (a force push) must not shrink the
    # scan, so it is dropped and more commits are read.
    excludes = [rev for rev in args.excludes if resolves(rev)]
    excludes += [f"--remotes={name}" for name in args.exclude_remotes]
    rev_args = [args.head, "--not", *excludes]
    shas = git("rev-list", *rev_args).decode().split()

    findings = Findings()
    text = scan_commits(shas, patterns, findings)
    for tag in peel_tags(args.head):
        findings.scan("annotated tag", tag, patterns)
        text += b"\n".join(variants(tag)) + b"\n"
    for ref in args.refs:
        name = ref.encode("utf-8", "surrogateescape")
        findings.scan("branch or tag name", name, patterns)
        text += name + b"\n"

    clean = True
    scratch = git("rev-parse", "--absolute-git-dir").decode().strip()
    with tempfile.TemporaryDirectory(prefix="leak-scan-", dir=scratch) as temporary:
        export = Path(temporary) / "tree"
        export.mkdir()
        scan_tree(args.head, patterns, findings, export)
        clean &= run_scanner(scanner, patterns, ["dir", str(export)])
    clean &= run_scanner(scanner, patterns, ["stdin"], text)

    findings.report(patterns)
    print(f"leak gate: scanned {len(shas)} commit(s) and the tree at {args.head}")
    if findings.hits or not clean:
        print("leak gate: rejected. Remove the content from every commit, not only the tip.", file=sys.stderr)
        return 1
    return 0


def command_text(args: argparse.Namespace) -> int:
    patterns = load_patterns()
    scanner = find_scanner()
    findings = Findings()
    clean = True
    for name in args.names:
        data = os.environ.get(name, "").encode("utf-8", "surrogateescape")
        findings.scan(name, data, patterns)
        clean &= run_scanner(scanner, patterns, ["stdin"], b"\n".join(variants(data)))
    findings.report(patterns)
    print(f"leak gate: scanned {len(args.names)} text field(s)")
    if findings.hits or not clean:
        print("leak gate: rejected. Edit the text; the change re-runs this check.", file=sys.stderr)
        return 1
    return 0


def command_check_list(_: argparse.Namespace) -> int:
    counts: dict[str, int] = {}
    for pattern in parse_list(read_list_text()):
        counts[pattern.cls] = counts.get(pattern.cls, 0) + 1
    for cls, count in sorted(counts.items()):
        print(f"{cls}: {count}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)

    scan = commands.add_parser("scan")
    scan.add_argument("--head", required=True)
    scan.add_argument("--not", dest="excludes", action="append", default=[])
    scan.add_argument("--exclude-remote", dest="exclude_remotes", action="append", default=[])
    scan.add_argument("--ref", dest="refs", action="append", default=[])
    scan.set_defaults(run=command_scan)

    text = commands.add_parser("text")
    text.add_argument("names", nargs="+")
    text.set_defaults(run=command_text)

    check_list = commands.add_parser("check-list")
    check_list.set_defaults(run=command_check_list)

    args = parser.parse_args()
    try:
        return args.run(args)
    except GateError as error:
        print(f"leak gate: could not run: {error}", file=sys.stderr)
        return 2
    except Exception as error:
        # A traceback can print a path, and a path can hold the private text.
        print(f"leak gate: could not run: {type(error).__name__}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
