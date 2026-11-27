#!/usr/bin/env python3
"""Fail when a change raises steady-state instructions (`Ir`) or `EstimatedCycles` on fixed inputs.

usage:
    tools/check_instructions.py [--jsonl-out PATH] [--baseline-snapshot PATH] [--no-baseline-snapshot] [--anchor-rev REV] BASE_REV
    tools/check_instructions.py --record-baseline [--baseline-snapshot PATH]

An empty BASE_REV, or the zero id that a tag push gives as its "before", means the
parent of HEAD: a release run, from a tag or started by hand, has no base of its own.

Builds tools/instruction-probe.rs against BASE_REV and against the working tree, and
runs Valgrind Callgrind with `--cache-sim=yes --branch-sim=yes` and pinned cache geometry
(`--I1=32768,8,64 --LL=8388608,16,64`) on shared `ayaml-fixtures` workloads.
Steady-state per-iteration counters subtract `K1 = 1` warmup iteration from `K1 + N` and
round half-up after summing raw event groups.

A rise above IR_LIMIT_PERCENT (+2.0% `Ir`) or CYCLES_LIMIT_PERCENT (+5.0%
`EstimatedCycles`) fails. If a change needs more, add a line to
tools/instruction-rises.tsv in that change, saying why:

    OPERATION<TAB>WORKLOAD<TAB>IR_PERCENT<TAB>CYCLES_PERCENT<TAB>REASON

Use `-` in IR_PERCENT or CYCLES_PERCENT to keep that metric at its default limit so an
EstCycles layout exception never relaxes Ir. Only lines the change itself adds count, so an
old allowance can't hide a new rise.

When `tools/instruction-baseline.jsonl` is present (or `--anchor-rev REV` is passed), each
matching `(operation, workload)` cell is also checked for cumulative multi-PR creep
(+10.0% `Ir` / +15.0% `EstimatedCycles`). Refresh the pinned snapshot explicitly with
`--record-baseline`.
Exit status: 0 within limits, 1 over a limit, 2 the check could not run.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from typing import Any

IR_LIMIT_PERCENT = 2.0
CYCLES_LIMIT_PERCENT = 5.0
CUMULATIVE_IR_LIMIT_PERCENT = 10.0
CUMULATIVE_CYCLES_LIMIT_PERCENT = 15.0
LIMIT_PERCENT = IR_LIMIT_PERCENT
WARMUP_ITERATIONS = 1
ITERATIONS = 4
EMIT_ITERATIONS = 12
WALK_ITERATIONS = 64
# Fixed simulated I1/LL cache geometry so Callgrind EstCycles does not swing with host LL size.
CALLGRIND_I1 = "32768,8,64"
CALLGRIND_LL = "8388608,16,64"
ROOT = Path(__file__).resolve().parent.parent
WORK = ROOT / "target" / "instructions"
RISES = "tools/instruction-rises.tsv"
PROBE = "tools/instruction-probe.rs"
DEFAULT_BASELINE_SNAPSHOT = ROOT / "tools" / "instruction-baseline.jsonl"
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
    "edit",
    "frontmatter",
    "merge_keys",
    "spanned_typed",
    "doc_de",
    "from_reader",
    "compat",
)

CORE_WORKLOADS = (
    "frontmatter-tiny",
    "boundary-4kb-below",
    "boundary-4kb-above",
    "ci-workflow",
    "escape-routes",
    "unicode-multibyte",
    "depth-64",
    "aliases-1000",
    "nested-keys",
)
CORE_OPS = ("events", "value", "typed", "spanned", "emit", "serialize", "walk")
STRUCT_WORKLOADS = ("frontmatter-tiny", "boundary-4kb-below", "boundary-4kb-above")
STRUCT_OPS = ("typed_struct", "borrowed")
FEATURE_CELLS: tuple[tuple[str, str], ...] = (
    ("edit", "frontmatter-tiny"),
    ("edit", "boundary-4kb-below"),
    ("frontmatter", "frontmatter-tiny"),
    ("merge_keys", "container-compose"),
    ("spanned_typed", "frontmatter-tiny"),
    ("spanned_typed", "boundary-4kb-below"),
    ("doc_de", "frontmatter-tiny"),
    ("doc_de", "boundary-4kb-below"),
    ("from_reader", "frontmatter-tiny"),
    ("from_reader", "boundary-4kb-below"),
    ("compat", "frontmatter-tiny"),
    ("compat", "boundary-4kb-below"),
    ("compat", "container-compose"),
    ("compat", "depth-64"),
)


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
    for op, name in FEATURE_CELLS:
        if name in available_inputs:
            cells.append((op, name))
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


def _div_round(numer: int, denom: int) -> int:
    return (numer + denom // 2) // denom


def steady_state_delta(
    k1_counters: dict[str, int],
    k2_counters: dict[str, int],
    delta_iterations: int,
) -> dict[str, int]:
    """Subtract warmup snapshot `k1` from `k2`, sum raw miss groups, and divide with half-up rounding."""
    if delta_iterations <= 0:
        raise ValueError("delta_iterations must be >= 1")
    keys = set(k1_counters) | set(k2_counters)
    raw = {k: max(0, k2_counters.get(k, 0) - k1_counters.get(k, 0)) for k in keys}
    out = {k: _div_round(v, delta_iterations) for k, v in raw.items()}
    raw_l1 = raw.get("I1mr", 0) + raw.get("D1mr", 0) + raw.get("D1mw", 0)
    raw_ll = raw.get("ILmr", 0) + raw.get("DLmr", 0) + raw.get("DLmw", 0)
    raw_br = raw.get("Bcm", 0) + raw.get("Bim", 0)
    raw_cycles = raw.get("Ir", 0) + 10 * raw_l1 + 100 * raw_ll + 20 * raw_br
    out["L1Misses"] = _div_round(raw_l1, delta_iterations)
    out["LLMisses"] = _div_round(raw_ll, delta_iterations)
    out["BranchMispredicts"] = _div_round(raw_br, delta_iterations)
    out["EstimatedCycles"] = _div_round(raw_cycles, delta_iterations)
    return out


def _extract_miss_totals(after: dict[str, int]) -> tuple[int, int, int]:
    l1 = after.get(
        "L1Misses",
        after.get("I1mr", 0) + after.get("D1mr", 0) + after.get("D1mw", 0),
    )
    ll = after.get(
        "LLMisses",
        after.get("ILmr", 0) + after.get("DLmr", 0) + after.get("DLmw", 0),
    )
    br = after.get("BranchMispredicts", after.get("Bcm", 0) + after.get("Bim", 0))
    return int(l1), int(ll), int(br)


def _parse_optional_pct(raw: str, field_name: str, line: str) -> float | None:
    token = raw.strip()
    if token == "-":
        return None
    try:
        value = float(token)
    except ValueError as exc:
        raise ValueError(f"{RISES}: invalid {field_name} {raw!r} in: {line}") from exc
    if not math.isfinite(value) or value <= 0.0:
        raise ValueError(f"{RISES}: {field_name} must be finite and > 0 (or '-') in: {line}")
    return value


def parse_rise_line(line: str) -> tuple[str, str, float | None, float | None, str]:
    """Parse one 5-column `instruction-rises.tsv` row into `(op, workload, ir_pct, cyc_pct, reason)`."""
    fields = line.split("\t")
    if len(fields) != 5 or not fields[0].strip() or not fields[1].strip() or not fields[4].strip():
        raise ValueError(
            f"{RISES}: expected OPERATION, WORKLOAD, IR_PERCENT, CYCLES_PERCENT and REASON in: {line}"
        )
    op = fields[0].strip()
    if op not in OPERATIONS:
        raise ValueError(f"{RISES}: unknown operation {op!r} in: {line}")
    ir_pct = _parse_optional_pct(fields[2], "IR_PERCENT", line)
    cyc_pct = _parse_optional_pct(fields[3], "CYCLES_PERCENT", line)
    if ir_pct is None and cyc_pct is None:
        raise ValueError(
            f"{RISES}: at least one of IR_PERCENT or CYCLES_PERCENT must be set in: {line}"
        )
    return op, fields[1].strip(), ir_pct, cyc_pct, fields[4].strip()


def _legacy_rise_signature(line: str) -> tuple[str, str, str] | None:
    """Return `(op, workload, reason)` for a legacy 4-column rises row during format migration."""
    fields = line.split("\t")
    if len(fields) == 4 and fields[0].strip() and fields[1].strip() and fields[3].strip():
        return (fields[0].strip(), fields[1].strip(), fields[3].strip())
    return None


def parse_baseline_snapshot(
    text: str, source: str = "<baseline>"
) -> dict[tuple[str, str], dict[str, Any]]:
    """Parse and strictly validate `tools/instruction-baseline.jsonl` keyed by `(operation, workload)`."""
    rows: dict[tuple[str, str], dict[str, Any]] = {}
    for lineno, raw in enumerate(text.splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError as exc:
            raise ValueError(f"{source}:{lineno}: invalid JSON: {exc}") from exc
        if not isinstance(obj, dict):
            raise ValueError(f"{source}:{lineno}: expected JSON object")
        sv = obj.get("schema_version")
        if isinstance(sv, bool) or sv != 1:
            raise ValueError(f"{source}:{lineno}: unsupported schema_version {sv!r}")
        for key in ("operation", "workload", "fixture_sha256"):
            val = obj.get(key)
            if not isinstance(val, str) or not val.strip():
                raise ValueError(f"{source}:{lineno}: missing string field {key!r}")
        sha = obj["fixture_sha256"]
        if len(sha) != 64 or any(c not in "0123456789abcdef" for c in sha):
            raise ValueError(f"{source}:{lineno}: invalid fixture_sha256 {sha!r}")
        for key in ("bytes", "ir", "estimated_cycles"):
            val = obj.get(key)
            if isinstance(val, bool) or not isinstance(val, int) or val <= 0:
                raise ValueError(f"{source}:{lineno}: missing positive int field {key!r}")
        for key in ("l1_misses", "ll_misses", "branch_mispredicts"):
            val = obj.get(key)
            if isinstance(val, bool) or not isinstance(val, int) or val < 0:
                raise ValueError(f"{source}:{lineno}: missing non-negative int field {key!r}")
        key_pair = (obj["operation"], obj["workload"])
        if key_pair in rows:
            raise ValueError(f"{source}:{lineno}: duplicate baseline row for {key_pair}")
        rows[key_pair] = obj
    if not rows:
        raise ValueError(f"{source}: no instruction baseline rows found")
    return rows


def evaluate_cumulative_drift(
    rows: list[dict[str, Any]],
    snapshot: dict[tuple[str, str], dict[str, Any]],
    *,
    ir_limit_pct: float = CUMULATIVE_IR_LIMIT_PERCENT,
    cycles_limit_pct: float = CUMULATIVE_CYCLES_LIMIT_PERCENT,
    label: str = "pinned baseline",
) -> list[str]:
    """Compare measured `head` counts against a pinned release-baseline snapshot to catch multi-PR creep."""
    failures: list[str] = []
    by_key = {(row["operation"], row["workload"]): row for row in rows}
    for key in sorted(snapshot):
        base = snapshot[key]
        cur = by_key.get(key)
        op, name = key
        if cur is None:
            failures.append(f"{op} {name}: present in {label} but missing from current run")
            continue
        if cur["fixture_sha256"] != base["fixture_sha256"]:
            cur["status"] = "fail"
            failures.append(
                f"{op} {name}: {label} fixture_sha256 drifted "
                f"({base['fixture_sha256'][:12]} -> {cur['fixture_sha256'][:12]}); "
                "refresh with --record-baseline"
            )
            continue
        ir_pct = (int(cur["head_ir"]) - int(base["ir"])) * 100.0 / max(int(base["ir"]), 1)
        cyc_pct = (
            (int(cur["head_estimated_cycles"]) - int(base["estimated_cycles"]))
            * 100.0
            / max(int(base["estimated_cycles"]), 1)
        )
        reasons = []
        if ir_pct > ir_limit_pct:
            reasons.append(f"cumulative Ir {ir_pct:+.2f}% > +{ir_limit_pct:g}% vs {label}")
        if cyc_pct > cycles_limit_pct:
            reasons.append(
                f"cumulative EstCycles {cyc_pct:+.2f}% > +{cycles_limit_pct:g}% vs {label}"
            )
        if reasons:
            cur["status"] = "fail"
            failures.append(f"{op} {name}: {', '.join(reasons)}")
    return failures


def evaluate_cells(
    cells: list[tuple[str, str]],
    base_counts: list[dict[str, int] | None],
    head_counts: list[dict[str, int]],
    accepted: dict[tuple[str, str], tuple[float | None, float | None]],
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
        l1, ll, br = _extract_miss_totals(after)
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
            ir_ovr, cyc_ovr = accepted.get((op, name), (None, None))
            eff_ir_lim = max(ir_limit_pct, ir_ovr) if ir_ovr is not None else ir_limit_pct
            eff_cyc_lim = (
                max(cycles_limit_pct, cyc_ovr) if cyc_ovr is not None else cycles_limit_pct
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


def extract_tree(rev: str, name: str = "base-tree") -> Path:
    tree = WORK / name
    tree.mkdir(parents=True)
    archive = subprocess.Popen(["git", "archive", rev], cwd=ROOT, stdout=subprocess.PIPE)
    subprocess.run(["tar", "-x", "-C", str(tree)], stdin=archive.stdout, check=True)
    assert archive.stdout is not None
    archive.stdout.close()
    if archive.wait() != 0:
        raise SystemExit(f"cannot read {rev}")
    return tree


def extract_base(rev: str) -> Path:
    return extract_tree(rev, "base-tree")


def probe_source(tree: Path) -> Path:
    """The probe that is built against `tree`: that tree's own, if it has one."""
    return tree / PROBE if (tree / PROBE).exists() else ROOT / PROBE


def known_operations(source: str) -> tuple[str, ...]:
    """The operations that a probe's source has an arm for."""
    return tuple(operation for operation in OPERATIONS if f'"{operation}" =>' in source)


def _ayaml_supports_libyaml_compat(tree: Path) -> bool:
    cargo_toml = tree / "crates" / "ayaml" / "Cargo.toml"
    return cargo_toml.is_file() and "libyaml-compat" in cargo_toml.read_text(encoding="utf-8")


def build_probe(side: str, tree: Path) -> Path:
    """Build a probe against `tree`, wiring `libyaml-compat`, `ayaml-edit`, `ayaml-fixtures`, and `serde`."""
    crate = WORK / f"probe-{side}"
    (crate / "src").mkdir(parents=True)
    src_path = probe_source(tree)
    src_text = src_path.read_text(encoding="utf-8")
    shutil.copy(src_path, crate / "src" / "main.rs")
    shutil.copy(tree / "Cargo.lock", crate / "Cargo.lock")
    if _ayaml_supports_libyaml_compat(tree):
        deps = [f'ayaml = {{ path = "{tree}/crates/ayaml", features = ["libyaml-compat"] }}']
    else:
        deps = [f'ayaml = {{ path = "{tree}/crates/ayaml" }}']
    if "ayaml_edit" in src_text and (tree / "crates" / "ayaml-edit").is_dir():
        deps.append(f'ayaml-edit = {{ path = "{tree}/crates/ayaml-edit" }}')
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


def callgrind_command(
    binary: Path, operation: str, path: Path, iterations: int, cg_out: Path
) -> list[str]:
    """Construct the Valgrind Callgrind command with fixed cache geometry."""
    return [
        "valgrind",
        "--tool=callgrind",
        "--cache-sim=yes",
        "--branch-sim=yes",
        f"--I1={CALLGRIND_I1}",
        f"--LL={CALLGRIND_LL}",
        f"--callgrind-out-file={cg_out}",
        str(binary),
        operation,
        str(path),
        str(iterations),
    ]


def collected(binary: Path, operation: str, path: Path, iterations: int) -> dict[str, int]:
    """Run Callgrind with fixed cache + branch simulation and return parsed summary counters."""
    with tempfile.NamedTemporaryFile(
        prefix="cg-", suffix=".out", dir=WORK, delete=False
    ) as handle:
        cg_out = Path(handle.name)
    try:
        completed = subprocess.run(
            callgrind_command(binary, operation, path, iterations, cg_out),
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


def accepted_rises(base_rev: str) -> dict[tuple[str, str], tuple[float | None, float | None]]:
    """Return the `(ir_pct, cycles_pct)` allowances this change adds to the rises file."""
    head_file = ROOT / RISES
    head_lines = head_file.read_text(encoding="utf-8").splitlines() if head_file.exists() else []
    shown = subprocess.run(
        ["git", "show", f"{base_rev}:{RISES}"], cwd=ROOT, capture_output=True, text=True
    )
    base_lines_list = shown.stdout.splitlines() if shown.returncode == 0 else []
    base_lines = set(base_lines_list)
    legacy_sigs = {
        sig for raw in base_lines_list if (sig := _legacy_rise_signature(raw)) is not None
    }
    accepted: dict[tuple[str, str], tuple[float | None, float | None]] = {}
    for line in head_lines:
        if not line.strip() or line.startswith("#"):
            continue
        op, workload, ir_pct, cyc_pct, reason = parse_rise_line(line)
        if line in base_lines or (op, workload, reason) in legacy_sigs:
            continue
        prev_ir, prev_cyc = accepted.get((op, workload), (None, None))
        merged_ir = (
            max(v for v in (prev_ir, ir_pct) if v is not None)
            if (prev_ir is not None or ir_pct is not None)
            else None
        )
        merged_cyc = (
            max(v for v in (prev_cyc, cyc_pct) if v is not None)
            if (prev_cyc is not None or cyc_pct is not None)
            else None
        )
        accepted[(op, workload)] = (merged_ir, merged_cyc)
    return accepted


def base_revision(given: str) -> str:
    """The revision to compare with: GIVEN, or HEAD's parent when there is none."""
    given = given.strip()
    if not given or set(given) == {"0"}:
        return "HEAD~1"
    return given


def resolve_git_rev(rev: str, label: str) -> str:
    """Validate that `rev` resolves to a commit without accepting option-like refs."""
    if not rev or rev.startswith("-"):
        raise ValueError(f"invalid {label} revision {rev!r}")
    completed = subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", "--end-of-options", f"{rev}^{{commit}}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0 or not completed.stdout.strip():
        raise ValueError(f"cannot resolve {label} revision {rev!r}")
    return completed.stdout.strip()


def format_baseline_snapshot(
    cells: list[tuple[str, str]],
    counts: list[dict[str, int]],
    manifest: dict[str, dict[str, Any]],
) -> str:
    """Format steady-state counts into schema-v1 `instruction-baseline.jsonl` lines."""
    lines: list[str] = []
    for (op, name), after in zip(cells, counts):
        meta = manifest.get(name, {})
        l1, ll, br = _extract_miss_totals(after)
        rec = {
            "schema_version": 1,
            "operation": op,
            "workload": name,
            "bytes": int(meta.get("bytes", 0)),
            "fixture_sha256": str(meta.get("sha256", "unknown")),
            "ir": int(after["Ir"]),
            "estimated_cycles": int(after["EstimatedCycles"]),
            "l1_misses": l1,
            "ll_misses": ll,
            "branch_mispredicts": br,
        }
        lines.append(json.dumps(rec, separators=(",", ":")) + "\n")
    return "".join(lines)


def _validate_nonneg_finite(name: str, value: float) -> str | None:
    if not math.isfinite(value) or value < 0.0:
        return f"invalid {name} value {value!r}: must be a finite number >= 0.0"
    return None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base_rev", nargs="?", default=None)
    parser.add_argument("--jsonl-out", type=Path, default=None)
    parser.add_argument("--record-baseline", action="store_true")
    parser.add_argument("--baseline-snapshot", type=Path, default=None)
    parser.add_argument("--no-baseline-snapshot", action="store_true")
    parser.add_argument("--anchor-rev", type=str, default=None)
    parser.add_argument("--cumulative-ir-limit", type=float, default=CUMULATIVE_IR_LIMIT_PERCENT)
    parser.add_argument(
        "--cumulative-cycles-limit", type=float, default=CUMULATIVE_CYCLES_LIMIT_PERCENT
    )
    args = parser.parse_args(argv)

    for flag, val in (
        ("--cumulative-ir-limit", args.cumulative_ir_limit),
        ("--cumulative-cycles-limit", args.cumulative_cycles_limit),
    ):
        err = _validate_nonneg_finite(flag, val)
        if err is not None:
            print(err, file=sys.stderr)
            return 2

    if args.record_baseline:
        if (
            args.base_rev is not None
            or args.no_baseline_snapshot
            or args.anchor_rev is not None
            or args.jsonl_out is not None
        ):
            print(
                "--record-baseline cannot be combined with BASE_REV, --no-baseline-snapshot, "
                "--anchor-rev, or --jsonl-out",
                file=sys.stderr,
            )
            return 2
    else:
        if args.base_rev is None:
            parser.print_help(sys.stderr)
            return 2
        if args.no_baseline_snapshot and args.baseline_snapshot is not None:
            print(
                "--no-baseline-snapshot cannot be combined with --baseline-snapshot",
                file=sys.stderr,
            )
            return 2
        if args.baseline_snapshot is not None and not args.baseline_snapshot.is_file():
            print(f"baseline snapshot missing: {args.baseline_snapshot}", file=sys.stderr)
            return 2

    if shutil.which("valgrind") is None:
        print("could not run: valgrind is required", file=sys.stderr)
        return 2

    in_head = known_operations((ROOT / PROBE).read_text(encoding="utf-8"))
    if in_head != OPERATIONS:
        print(
            f"could not run: {PROBE} has no arm for {set(OPERATIONS) - set(in_head)}",
            file=sys.stderr,
        )
        return 2

    base_rev: str | None = None
    accepted: dict[tuple[str, str], tuple[float | None, float | None]] = {}
    snapshot_path: Path | None = None
    snapshot: dict[tuple[str, str], dict[str, Any]] | None = None
    if not args.record_baseline:
        base_rev = base_revision(args.base_rev)
        try:
            resolve_git_rev(base_rev, "base")
            if args.anchor_rev is not None:
                resolve_git_rev(args.anchor_rev, "anchor")
            accepted = accepted_rises(base_rev)
            snapshot_path = (
                None
                if args.no_baseline_snapshot
                else (
                    args.baseline_snapshot
                    or (DEFAULT_BASELINE_SNAPSHOT if DEFAULT_BASELINE_SNAPSHOT.is_file() else None)
                )
            )
            if snapshot_path is not None:
                snapshot = parse_baseline_snapshot(
                    snapshot_path.read_text(encoding="utf-8"), str(snapshot_path)
                )
        except ValueError as exc:
            print(str(exc), file=sys.stderr)
            return 2

    if WORK.exists():
        shutil.rmtree(WORK)
    inputs_dir = WORK / "inputs"
    inputs_dir.mkdir(parents=True)

    head_probe = build_probe("head", ROOT)
    run(head_probe, "--dump-fixtures", inputs_dir)
    manifest: dict[str, dict[str, Any]] = json.loads(
        (inputs_dir / "manifest.json").read_text(encoding="utf-8")
    )
    inputs = {name: inputs_dir / f"{name}.yml" for name in manifest}
    cells = scheduled_cells(set(inputs))

    if args.record_baseline:
        snap_out = args.baseline_snapshot or DEFAULT_BASELINE_SNAPSHOT
        with ThreadPoolExecutor(max_workers=min(4, os.cpu_count() or 1)) as pool:
            head_counts = list(
                pool.map(lambda cell: per_iteration(head_probe, cell[0], inputs[cell[1]]), cells)
            )
        text = format_baseline_snapshot(cells, head_counts, manifest)
        try:
            parsed = parse_baseline_snapshot(text, str(snap_out))
        except ValueError as exc:
            print(str(exc), file=sys.stderr)
            return 2
        snap_out.parent.mkdir(parents=True, exist_ok=True)
        snap_out.write_text(text, encoding="utf-8")
        print(f"recorded {len(parsed)} steady-state instruction baseline cells to {snap_out}")
        return 0

    assert base_rev is not None
    base_tree = extract_base(base_rev)
    in_base = known_operations(probe_source(base_tree).read_text(encoding="utf-8"))
    base_probe = build_probe("base", base_tree)

    def count_base(cell: tuple[str, str]) -> dict[str, int] | None:
        return per_iteration(base_probe, cell[0], inputs[cell[1]]) if cell[0] in in_base else None

    with ThreadPoolExecutor(max_workers=min(4, os.cpu_count() or 1)) as pool:
        base_counts = list(pool.map(count_base, cells))
        head_counts = list(
            pool.map(lambda cell: per_iteration(head_probe, cell[0], inputs[cell[1]]), cells)
        )

    rows, failed = evaluate_cells(cells, base_counts, head_counts, accepted, manifest)

    baseline_checked = 0
    anchor_checked = 0
    cumulative_failed: list[str] = []
    if snapshot is not None and snapshot_path is not None:
        baseline_checked = len(snapshot)
        cumulative_failed.extend(
            evaluate_cumulative_drift(
                rows,
                snapshot,
                ir_limit_pct=args.cumulative_ir_limit,
                cycles_limit_pct=args.cumulative_cycles_limit,
                label=f"pinned baseline ({snapshot_path.name})",
            )
        )

    if args.anchor_rev is not None:
        anchor_tree = extract_tree(args.anchor_rev, "anchor-tree")
        in_anchor = known_operations(probe_source(anchor_tree).read_text(encoding="utf-8"))
        anchor_cells = [cell for cell in cells if cell[0] in in_anchor]
        if not anchor_cells:
            print(
                f"anchor revision {args.anchor_rev!r} shares zero known operations with HEAD",
                file=sys.stderr,
            )
            return 2
        anchor_probe = build_probe("anchor", anchor_tree)
        with ThreadPoolExecutor(max_workers=min(4, os.cpu_count() or 1)) as pool:
            anchor_counts = list(
                pool.map(lambda cell: per_iteration(anchor_probe, cell[0], inputs[cell[1]]), anchor_cells)
            )
        anchor_snapshot = parse_baseline_snapshot(
            format_baseline_snapshot(anchor_cells, anchor_counts, manifest),
            f"anchor:{args.anchor_rev}",
        )
        anchor_checked = len(anchor_snapshot)
        cumulative_failed.extend(
            evaluate_cumulative_drift(
                rows,
                anchor_snapshot,
                ir_limit_pct=args.cumulative_ir_limit,
                cycles_limit_pct=args.cumulative_cycles_limit,
                label=f"anchor ({args.anchor_rev})",
            )
        )

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
        mark = "  FAIL" if row["status"] == "fail" else ""
        if row["base_ir"] is None:
            print(
                f"{row['operation']:<15}{row['workload']:<20}{'':>12}{row['head_ir']:>12}{'new':>8}"
                f"{'':>13}{row['head_estimated_cycles']:>13}{'new':>8}"
                f"{row['l1_misses']:>9}{row['ll_misses']:>8}{row['branch_mispredicts']:>8}{mark}"
            )
            continue
        compared += 1
        print(
            f"{row['operation']:<15}{row['workload']:<20}{row['base_ir']:>12}{row['head_ir']:>12}"
            f"{row['ir_change_pct']:>7.2f}%{row['base_estimated_cycles']:>13}"
            f"{row['head_estimated_cycles']:>13}{row['cycles_change_pct']:>7.2f}%"
            f"{row['l1_misses']:>9}{row['ll_misses']:>8}{row['branch_mispredicts']:>8}{mark}"
        )

    all_failed = failed + cumulative_failed
    if all_failed:
        for msg in all_failed:
            print(f"FAIL: {msg}", file=sys.stderr)
        snap_name = snapshot_path.name if snapshot_path is not None else DEFAULT_BASELINE_SNAPSHOT.name
        print(
            f"\n{len(all_failed)} over limit (+{IR_LIMIT_PERCENT:g}% Ir / +{CYCLES_LIMIT_PERCENT:g}% "
            f"EstimatedCycles vs {base_rev}; +{args.cumulative_ir_limit:g}% Ir / "
            f"+{args.cumulative_cycles_limit:g}% EstimatedCycles vs cumulative baseline). "
            f"Fix the rise, add a line to {RISES}, or refresh {snap_name}.",
            file=sys.stderr,
        )
        return 1
    extras: list[str] = []
    if snapshot_path is not None and baseline_checked:
        extras.append(f"{baseline_checked} pinned baseline cells ({snapshot_path.name})")
    if args.anchor_rev is not None and anchor_checked:
        extras.append(f"{anchor_checked} anchor cells ({args.anchor_rev})")
    suffix = f" and {' and '.join(extras)}" if extras else ""
    print(f"\nchecked {compared} steady-state Callgrind counts against {base_rev}{suffix} (JSONL: {jsonl_path})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
