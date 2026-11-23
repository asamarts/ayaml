#!/usr/bin/env python3
"""Fail when a change raises steady-state instructions (`Ir`) or `EstimatedCycles` on fixed inputs.

usage: tools/check_instructions.py [--jsonl-out PATH] BASE_REV

An empty BASE_REV, or the zero id that a tag push gives as its "before", means the
parent of HEAD: a release run, from a tag or started by hand, has no base of its own.

Builds tools/instruction-probe.rs against BASE_REV and against the working tree, and
runs Valgrind Callgrind with `--cache-sim=yes --branch-sim=yes` on shared
`ayaml-fixtures` workloads. Steady-state per-iteration counters are computed as
`(counts(K1 + N) - counts(K1)) // N` with `K1 = 1` warmup iteration excluded.

A rise above IR_LIMIT_PERCENT (+2.0% `Ir`) or CYCLES_LIMIT_PERCENT (+5.0%
`EstimatedCycles`) fails. If a change needs more, add a line to
tools/instruction-rises.tsv in that change, saying why:

    OPERATION<TAB>WORKLOAD<TAB>PERCENT<TAB>REASON

Only lines the change itself adds count, so an old allowance can't hide a new rise.
An operation that BASE_REV's probe doesn't have is counted and shown, and compared from
the next change on. Scratch and JSONL output go under target/instructions.
Exit status: 0 within limits, 1 over a limit, 2 the check could not run.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from typing import Any

IR_LIMIT_PERCENT = 2.0
CYCLES_LIMIT_PERCENT = 5.0
LIMIT_PERCENT = IR_LIMIT_PERCENT
WARMUP_ITERATIONS = 1
ITERATIONS = 4
EMIT_ITERATIONS = 12
WALK_ITERATIONS = 64
ROOT = Path(__file__).resolve().parent.parent
WORK = ROOT / "target" / "instructions"
RISES = "tools/instruction-rises.tsv"
PROBE = "tools/instruction-probe.rs"
OPERATIONS = (
    "events",
    "value",
    "typed",
    "typed_struct",
    "borrowed",
    "spanned",
    "emit",
    "serialize",
    "walk",
    "late_typed_err",
)

CORE_WORKLOADS = (
    "frontmatter-tiny",
    "boundary-4kb-below",
    "boundary-4kb-above",
    "ci-workflow",
    "escape-routes",
    "unicode-multibyte",
    "aliases-1000",
    "nested-keys",
)
CORE_OPS = ("events", "value", "typed", "spanned", "emit", "serialize", "walk")
STRUCT_WORKLOADS = ("frontmatter-tiny", "boundary-4kb-below", "boundary-4kb-above")
STRUCT_OPS = ("typed_struct", "borrowed")


def nested_keys(depth: int, width: int) -> str:
    """A mapping of `width` entries that is the key of a mapping, `depth` times over."""
    inner = "{" + ", ".join(f"k{index}: {index}" for index in range(width)) + "}"
    for _ in range(depth):
        inner = "{? " + inner + ": null}"
    return inner + "\n"


def workloads() -> dict[str, str]:
    """Fallback synthetic workload map (kept for unit-test compatibility)."""
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
        "nested-keys": nested_keys(100, 1000),
    }


def scheduled_cells(available_inputs: set[str]) -> list[tuple[str, str]]:
    """Return ordered `(operation, workload)` pairs covered within CI time budgets."""
    cells: list[tuple[str, str]] = []
    for op in CORE_OPS:
        for name in CORE_WORKLOADS:
            if name in available_inputs:
                cells.append((op, name))
    for op in STRUCT_OPS:
        for name in STRUCT_WORKLOADS:
            if name in available_inputs:
                cells.append((op, name))
    if "typed-mismatch-late" in available_inputs:
        cells.append(("late_typed_err", "typed-mismatch-late"))
    return cells


def parse_callgrind_out(text: str) -> dict[str, int]:
    """Parse `events:` and `summary:` lines from a Callgrind output file."""
    names: list[str] | None = None
    counts: list[int] | None = None
    for raw in text.splitlines():
        line = raw.strip()
        if line.startswith("events:"):
            names = line[len("events:") :].split()
        elif line.startswith("summary:"):
            try:
                counts = [int(tok) for tok in line[len("summary:") :].split()]
            except ValueError as exc:
                raise ValueError(f"invalid callgrind summary integer: {exc}") from exc
    if not names or counts is None:
        raise ValueError("callgrind output missing `events:` or `summary:` line")
    if len(counts) > len(names):
        raise ValueError(f"callgrind summary has {len(counts)} counts for {len(names)} events")
    result = {name: (counts[i] if i < len(counts) else 0) for i, name in enumerate(names)}
    if "Ir" not in result:
        raise ValueError("callgrind output missing `Ir` counter")
    return result


def estimated_cycles(counters: dict[str, int]) -> int:
    """Compute Valgrind/Callgrind EstimatedCycles: Ir + 10*L1_miss + 100*LL_miss + 20*branch_miss."""
    ir = counters.get("Ir", 0)
    l1_miss = counters.get("I1mr", 0) + counters.get("D1mr", 0) + counters.get("D1mw", 0)
    ll_miss = counters.get("ILmr", 0) + counters.get("DLmr", 0) + counters.get("DLmw", 0)
    br_miss = counters.get("Bcm", 0) + counters.get("Bim", 0)
    return ir + 10 * l1_miss + 100 * ll_miss + 20 * br_miss


def steady_state_delta(
    k1_counters: dict[str, int],
    k2_counters: dict[str, int],
    delta_iterations: int,
) -> dict[str, int]:
    """Subtract warmup snapshot `k1` from `k2` and divide by `delta_iterations`."""
    if delta_iterations <= 0:
        raise ValueError("delta_iterations must be >= 1")
    keys = set(k1_counters) | set(k2_counters)
    out = {
        k: max(0, k2_counters.get(k, 0) - k1_counters.get(k, 0)) // delta_iterations
        for k in keys
    }
    out["EstimatedCycles"] = estimated_cycles(out)
    return out


def evaluate_cells(
    cells: list[tuple[str, str]],
    base_counts: list[dict[str, int] | None],
    head_counts: list[dict[str, int]],
    accepted: dict[tuple[str, str], float],
    manifest: dict[str, dict[str, Any]],
    *,
    ir_limit_pct: float = IR_LIMIT_PERCENT,
    cycles_limit_pct: float = CYCLES_LIMIT_PERCENT,
) -> tuple[list[dict[str, Any]], list[str]]:
    """Evaluate base vs head steady-state counts and return `(jsonl_rows, failed_messages)`."""
    rows: list[dict[str, Any]] = []
    failed: list[str] = []
    for (op, name), before, after in zip(cells, base_counts, head_counts):
        meta = manifest.get(name, {})
        head_ir = after["Ir"]
        head_cyc = after["EstimatedCycles"]
        l1 = after.get("I1mr", 0) + after.get("D1mr", 0) + after.get("D1mw", 0)
        ll = after.get("ILmr", 0) + after.get("DLmr", 0) + after.get("DLmw", 0)
        br = after.get("Bcm", 0) + after.get("Bim", 0)
        row: dict[str, Any] = {
            "schema_version": 1,
            "operation": op,
            "workload": name,
            "bytes": int(meta.get("bytes", 0)),
            "fixture_sha256": str(meta.get("sha256", "unknown")),
            "base_ir": None if before is None else before["Ir"],
            "head_ir": head_ir,
            "ir_change_pct": None,
            "base_estimated_cycles": None if before is None else before["EstimatedCycles"],
            "head_estimated_cycles": head_cyc,
            "cycles_change_pct": None,
            "l1_misses": l1,
            "ll_misses": ll,
            "branch_mispredicts": br,
            "status": "new" if before is None else "ok",
        }
        if before is not None:
            b_ir = before["Ir"]
            b_cyc = before["EstimatedCycles"]
            ir_pct = (head_ir - b_ir) * 100.0 / max(b_ir, 1)
            cyc_pct = (head_cyc - b_cyc) * 100.0 / max(b_cyc, 1)
            row["ir_change_pct"] = round(ir_pct, 3)
            row["cycles_change_pct"] = round(cyc_pct, 3)
            override = accepted.get((op, name))
            eff_ir_lim = max(ir_limit_pct, override) if override is not None else ir_limit_pct
            eff_cyc_lim = (
                max(cycles_limit_pct, override) if override is not None else cycles_limit_pct
            )
            reasons = []
            if ir_pct > eff_ir_lim:
                reasons.append(f"Ir {ir_pct:+.2f}% > +{eff_ir_lim:g}%")
            if cyc_pct > eff_cyc_lim:
                reasons.append(f"EstCycles {cyc_pct:+.2f}% > +{eff_cyc_lim:g}%")
            if reasons:
                row["status"] = "fail"
                failed.append(f"{op} {name}: {', '.join(reasons)}")
        rows.append(row)
    return rows, failed


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


def probe_source(tree: Path) -> Path:
    """The probe that is built against `tree`: that tree's own, if it has one."""
    return tree / PROBE if (tree / PROBE).exists() else ROOT / PROBE


def known_operations(source: str) -> tuple[str, ...]:
    """The operations that a probe's source has an arm for."""
    return tuple(operation for operation in OPERATIONS if f'"{operation}" =>' in source)


def build_probe(side: str, tree: Path) -> Path:
    """Build a probe against `tree`, wiring `ayaml-fixtures` and `serde` when used."""
    crate = WORK / f"probe-{side}"
    (crate / "src").mkdir(parents=True)
    src_path = probe_source(tree)
    src_text = src_path.read_text(encoding="utf-8")
    shutil.copy(src_path, crate / "src" / "main.rs")
    shutil.copy(tree / "Cargo.lock", crate / "Cargo.lock")
    deps = [f'ayaml = {{ path = "{tree}/crates/ayaml" }}']
    if "ayaml_fixtures" in src_text:
        deps.append(f'ayaml-fixtures = {{ path = "{tree}/crates/ayaml-fixtures" }}')
    if "serde::" in src_text or "use serde" in src_text:
        deps.append('serde = { version = "1.0.220", features = ["derive"] }')
    deps_block = "\n".join(deps)
    (crate / "Cargo.toml").write_text(
        "[package]\n"
        'name = "ayaml-instruction-probe"\n'
        'version = "0.0.0"\n'
        'edition = "2024"\n'
        "publish = false\n\n"
        "[dependencies]\n"
        f"{deps_block}\n\n"
        "[workspace]\n",
        encoding="utf-8",
    )
    remap = (
        f"--remap-path-prefix={tree.resolve()}=/ayaml "
        f"--remap-path-prefix={crate.resolve()}=/probe"
    )
    existing_rustflags = os.environ.get("RUSTFLAGS", "").strip()
    rustflags = f"{existing_rustflags} {remap}".strip() if existing_rustflags else remap
    run(
        "cargo",
        "build",
        "--quiet",
        "--release",
        cwd=crate,
        env={"CARGO_TARGET_DIR": str(WORK / "target"), "RUSTFLAGS": rustflags},
    )
    binary = WORK / f"probe-{side}.bin"
    shutil.copy(WORK / "target" / "release" / "ayaml-instruction-probe", binary)
    return binary


def collected(binary: Path, operation: str, path: Path, iterations: int) -> dict[str, int]:
    """Run Callgrind with cache + branch simulation and return parsed summary counters."""
    with tempfile.NamedTemporaryFile(
        prefix="cg-", suffix=".out", dir=WORK, delete=False
    ) as handle:
        cg_out = Path(handle.name)
    try:
        completed = subprocess.run(
            [
                "valgrind",
                "--tool=callgrind",
                "--cache-sim=yes",
                "--branch-sim=yes",
                f"--callgrind-out-file={cg_out}",
                str(binary),
                operation,
                str(path),
                str(iterations),
            ],
            capture_output=True,
            text=True,
        )
        if completed.returncode != 0 or not cg_out.is_file():
            sys.stderr.write(completed.stderr[-2000:])
            raise SystemExit(f"callgrind failed on {operation} {path.name}")
        return parse_callgrind_out(cg_out.read_text(encoding="utf-8"))
    finally:
        cg_out.unlink(missing_ok=True)


def iterations_for(operation: str) -> int:
    """Steady-state delta iterations sized so one-time setup variance cannot contaminate cheap ops."""
    if operation == "walk":
        return WALK_ITERATIONS
    if operation in ("emit", "serialize"):
        return EMIT_ITERATIONS
    return ITERATIONS


def per_iteration(binary: Path, operation: str, path: Path) -> dict[str, int]:
    """Steady-state per-iteration counters excluding startup and 1 warmup iteration."""
    iters = iterations_for(operation)
    k1 = collected(binary, operation, path, WARMUP_ITERATIONS)
    k2 = collected(binary, operation, path, WARMUP_ITERATIONS + iters)
    return steady_state_delta(k1, k2, iters)


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


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base_rev", nargs="?", default=None)
    parser.add_argument("--jsonl-out", type=Path, default=None)
    args = parser.parse_args(argv)
    if args.base_rev is None:
        parser.print_help(sys.stderr)
        return 2
    base_rev = base_revision(args.base_rev)
    if shutil.which("valgrind") is None:
        print("could not run: valgrind is required", file=sys.stderr)
        return 2

    if WORK.exists():
        shutil.rmtree(WORK)
    inputs_dir = WORK / "inputs"
    inputs_dir.mkdir(parents=True)
    accepted = accepted_rises(base_rev)

    base_tree = extract_base(base_rev)
    in_base = known_operations(probe_source(base_tree).read_text(encoding="utf-8"))
    in_head = known_operations((ROOT / PROBE).read_text(encoding="utf-8"))
    if in_head != OPERATIONS:
        print(f"could not run: {PROBE} has no arm for {set(OPERATIONS) - set(in_head)}", file=sys.stderr)
        return 2

    base_probe = build_probe("base", base_tree)
    head_probe = build_probe("head", ROOT)

    run(head_probe, "--dump-fixtures", inputs_dir)
    manifest: dict[str, dict[str, Any]] = json.loads(
        (inputs_dir / "manifest.json").read_text(encoding="utf-8")
    )
    inputs = {name: inputs_dir / f"{name}.yml" for name in manifest}
    cells = scheduled_cells(set(inputs))

    def count_base(cell: tuple[str, str]) -> dict[str, int] | None:
        return per_iteration(base_probe, cell[0], inputs[cell[1]]) if cell[0] in in_base else None

    with ThreadPoolExecutor(max_workers=min(4, os.cpu_count() or 1)) as pool:
        base_counts = list(pool.map(count_base, cells))
        head_counts = list(pool.map(lambda cell: per_iteration(head_probe, cell[0], inputs[cell[1]]), cells))

    rows, failed = evaluate_cells(cells, base_counts, head_counts, accepted, manifest)

    jsonl_path = args.jsonl_out or (WORK / "callgrind.jsonl")
    jsonl_path.parent.mkdir(parents=True, exist_ok=True)
    jsonl_path.write_text(
        "".join(json.dumps(row, separators=(",", ":")) + "\n" for row in rows),
        encoding="utf-8",
    )

    print(
        f"{'operation':<15}{'workload':<20}{'base Ir':>12}{'head Ir':>12}{'Ir %':>8}"
        f"{'base Cyc':>13}{'head Cyc':>13}{'Cyc %':>8}{'L1m':>9}{'LLm':>8}{'Brm':>8}"
    )
    compared = 0
    for row in rows:
        if row["base_ir"] is None:
            print(
                f"{row['operation']:<15}{row['workload']:<20}{'':>12}{row['head_ir']:>12}{'new':>8}"
                f"{'':>13}{row['head_estimated_cycles']:>13}{'new':>8}"
                f"{row['l1_misses']:>9}{row['ll_misses']:>8}{row['branch_mispredicts']:>8}"
            )
            continue
        compared += 1
        mark = "  FAIL" if row["status"] == "fail" else ""
        print(
            f"{row['operation']:<15}{row['workload']:<20}{row['base_ir']:>12}{row['head_ir']:>12}"
            f"{row['ir_change_pct']:>7.2f}%{row['base_estimated_cycles']:>13}"
            f"{row['head_estimated_cycles']:>13}{row['cycles_change_pct']:>7.2f}%"
            f"{row['l1_misses']:>9}{row['ll_misses']:>8}{row['branch_mispredicts']:>8}{mark}"
        )

    if failed:
        for msg in failed:
            print(f"FAIL: {msg}", file=sys.stderr)
        print(
            f"\n{len(failed)} over limit (+{IR_LIMIT_PERCENT:g}% Ir / +{CYCLES_LIMIT_PERCENT:g}% "
            f"EstimatedCycles). Fix the rise, or add a line to {RISES} in this change saying why.",
            file=sys.stderr,
        )
        return 1
    print(f"\nchecked {compared} steady-state Callgrind counts against {base_rev} (JSONL: {jsonl_path})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
