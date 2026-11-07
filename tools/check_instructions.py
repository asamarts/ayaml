#!/usr/bin/env python3
"""Fail when a change raises the instructions ayaml executes on fixed inputs.

usage: tools/check_instructions.py BASE_REV

An empty BASE_REV, or the zero id that a tag push gives as its "before", means the
parent of HEAD: a release run, from a tag or started by hand, has no base of its own.

Builds tools/instruction-probe.rs against BASE_REV and against the working tree, and
counts each operation on each input under callgrind. Counts don't depend on load, so
this works on a shared runner where timing doesn't. They also miss cache and branch
effects, so they catch a slower algorithm or an added pass, not every slowdown.

A rise above LIMIT_PERCENT fails. If a change needs more, add a line to
tools/instruction-rises.tsv in that change, saying why:

    OPERATION<TAB>WORKLOAD<TAB>PERCENT<TAB>REASON

Only lines the change itself adds count, so an old allowance can't hide a new rise.
Scratch goes under target/instructions. Exit status: 0 within limits, 1 over a limit,
2 the check could not run.
"""

from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

LIMIT_PERCENT = 5.0
ITERATIONS = 10
ROOT = Path(__file__).resolve().parent.parent
WORK = ROOT / "target" / "instructions"
RISES = "tools/instruction-rises.tsv"
PROBE = "tools/instruction-probe.rs"
OPERATIONS = ("events", "value", "typed", "emit")
COLLECTED = re.compile(r"Collected\s*:\s*(\d+)")


def workloads() -> dict[str, str]:
    fixtures = ROOT / "tests" / "fixtures" / "compat"
    return {
        "toolchain-pins": (fixtures / "toolchain-pins.yml").read_text(encoding="utf-8"),
        "merges": (fixtures / "scalar-tags-merges-aliases.yml").read_text(encoding="utf-8"),
        "tiny": "a\n",
        "large-scalar": "x" * 100_000 + "\n",
        "flow-depth-64": "[" * 64 + "0" + "]" * 64 + "\n",
        "aliases-1000": "base: &base {x: 1}\nitems:\n" + "  - *base\n" * 1000,
        "mapping-1000": "".join(f"key{i}: value{i}\n" for i in range(1000)),
        "short-sequence": "- a\n" * 5000,
    }


def run(*command: str | Path, cwd: Path | None = None, env: dict[str, str] | None = None) -> str:
    completed = subprocess.run(
        [str(part) for part in command],
        cwd=cwd,
        env={**os.environ, **(env or {})},
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        sys.stderr.write(completed.stderr)
        raise SystemExit(f"could not run: {' '.join(str(part) for part in command[:3])}") from None
    return completed.stdout


def extract_base(rev: str) -> Path:
    tree = WORK / "base-tree"
    tree.mkdir(parents=True)
    archive = subprocess.Popen(["git", "archive", rev], cwd=ROOT, stdout=subprocess.PIPE)
    subprocess.run(["tar", "-x", "-C", str(tree)], stdin=archive.stdout, check=True)
    assert archive.stdout is not None
    archive.stdout.close()
    if archive.wait() != 0:
        raise SystemExit(f"cannot read {rev}")
    return tree


def build_probe(side: str, tree: Path) -> Path:
    """Build a probe against `tree`, using that tree's probe if it has one."""
    crate = WORK / f"probe-{side}"
    (crate / "src").mkdir(parents=True)
    source = tree / PROBE if (tree / PROBE).exists() else ROOT / PROBE
    shutil.copy(source, crate / "src" / "main.rs")
    shutil.copy(tree / "Cargo.lock", crate / "Cargo.lock")
    (crate / "Cargo.toml").write_text(
        "[package]\n"
        'name = "ayaml-instruction-probe"\n'
        'version = "0.0.0"\n'
        'edition = "2024"\n'
        "publish = false\n\n"
        "[dependencies]\n"
        f'ayaml = {{ path = "{tree}/crates/ayaml" }}\n\n'
        "[workspace]\n",
        encoding="utf-8",
    )
    run("cargo", "build", "--quiet", "--release", cwd=crate, env={"CARGO_TARGET_DIR": str(WORK / "target")})
    binary = WORK / f"probe-{side}.bin"
    shutil.copy(WORK / "target" / "release" / "ayaml-instruction-probe", binary)
    return binary


def collected(binary: Path, operation: str, path: Path, iterations: int) -> int:
    completed = subprocess.run(
        [
            "valgrind",
            "--tool=callgrind",
            "--callgrind-out-file=/dev/null",
            str(binary),
            operation,
            str(path),
            str(iterations),
        ],
        capture_output=True,
        text=True,
    )
    found = COLLECTED.search(completed.stderr)
    if completed.returncode != 0 or found is None:
        sys.stderr.write(completed.stderr[-2000:])
        raise SystemExit(f"callgrind failed on {operation} {path.name}")
    return int(found.group(1))


def per_iteration(binary: Path, operation: str, path: Path) -> int:
    """Count one iteration, leaving out process start and any one-time parse."""
    total = collected(binary, operation, path, ITERATIONS)
    setup = collected(binary, operation, path, 0)
    return (total - setup) // ITERATIONS


def accepted_rises(base_rev: str) -> dict[tuple[str, str], float]:
    """Return the allowances this change adds to the rises file."""
    head_file = ROOT / RISES
    head_lines = head_file.read_text(encoding="utf-8").splitlines() if head_file.exists() else []
    shown = subprocess.run(["git", "show", f"{base_rev}:{RISES}"], cwd=ROOT, capture_output=True, text=True)
    base_lines = set(shown.stdout.splitlines()) if shown.returncode == 0 else set()
    accepted: dict[tuple[str, str], float] = {}
    for line in head_lines:
        if not line.strip() or line.startswith("#") or line in base_lines:
            continue
        fields = line.split("\t")
        if len(fields) != 4 or not fields[3].strip():
            raise SystemExit(f"{RISES}: expected OPERATION, WORKLOAD, PERCENT and REASON in: {line}")
        accepted[(fields[0], fields[1])] = float(fields[2])
    return accepted


def base_revision(given: str) -> str:
    """The revision to compare with: GIVEN, or HEAD's parent when there is none."""
    given = given.strip()
    if not given or set(given) == {"0"}:
        return "HEAD~1"
    return given


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    base_rev = base_revision(sys.argv[1])
    if shutil.which("valgrind") is None:
        print("could not run: valgrind is required", file=sys.stderr)
        return 2

    if WORK.exists():
        shutil.rmtree(WORK)
    (WORK / "inputs").mkdir(parents=True)
    accepted = accepted_rises(base_rev)

    inputs: dict[str, Path] = {}
    for name, text in workloads().items():
        inputs[name] = WORK / "inputs" / f"{name}.yml"
        inputs[name].write_text(text, encoding="utf-8")

    base_probe = build_probe("base", extract_base(base_rev))
    head_probe = build_probe("head", ROOT)

    cells = [(operation, name) for operation in OPERATIONS for name in inputs]
    with ThreadPoolExecutor(max_workers=min(4, os.cpu_count() or 1)) as pool:
        base = list(pool.map(lambda cell: per_iteration(base_probe, cell[0], inputs[cell[1]]), cells))
        head = list(pool.map(lambda cell: per_iteration(head_probe, cell[0], inputs[cell[1]]), cells))

    failed = []
    print(f"{'operation':<10}{'workload':<18}{'base':>14}{'head':>14}{'change':>10}")
    for (operation, name), before, after in zip(cells, base, head):
        change = (after - before) * 100.0 / max(before, 1)
        limit = accepted.get((operation, name), LIMIT_PERCENT)
        mark = "" if change <= limit else f"  over {limit:g}%"
        if mark:
            failed.append(f"{operation} {name}")
        print(f"{operation:<10}{name:<18}{before:>14}{after:>14}{change:>9.2f}%{mark}")

    if failed:
        print(
            f"\n{len(failed)} over the limit. Fix the rise, or add a line to {RISES} in this "
            "change saying why it's accepted.",
            file=sys.stderr,
        )
        return 1
    print(f"\nchecked {len(cells)} counts against {base_rev}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
