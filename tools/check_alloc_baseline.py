#!/usr/bin/env python3
"""Gate structured `memcount --jsonl` allocation metrics against `tools/alloc-baseline.jsonl`.

Usage:
    python3 tools/check_alloc_baseline.py [--baseline FILE] [--current FILE]
    python3 tools/check_alloc_baseline.py --record [--baseline FILE]

In default mode, runs `memcount --jsonl --suite baseline` in release mode and
compares every `(scenario, operation)` row against `tools/alloc-baseline.jsonl`.
Any fixture SHA-256 drift, missing row, or regression past per-metric limits
prints exact diagnostics and exits 1.
"""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
import subprocess
import sys
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BASELINE = ROOT / "tools" / "alloc-baseline.jsonl"

# Per-metric headroom over the committed baseline: max(percent_allowance, absolute_floor).
BYTE_METRICS = ("peak_bytes", "live_bytes", "total_allocated_bytes")
MAX_BYTES_PERCENT = 10.0
MIN_BYTES_FLOOR = 512
MAX_ALLOCS_PERCENT = 5.0
MIN_ALLOCS_FLOOR = 4

REQUIRED_INT_FIELDS = (
    "schema_version",
    "bytes",
    "peak_bytes",
    "live_bytes",
    "allocations",
    "total_allocated_bytes",
)
REQUIRED_STR_FIELDS = ("scenario", "fixture_sha256", "operation")


def parse_jsonl(text: str, source: str = "<input>") -> dict[tuple[str, str], dict[str, Any]]:
    """Parse and strictly validate `memcount --jsonl` rows keyed by `(scenario, operation)`."""
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
        for key in REQUIRED_STR_FIELDS:
            val = obj.get(key)
            if not isinstance(val, str) or not val.strip():
                raise ValueError(f"{source}:{lineno}: missing string field {key!r}")
        for key in REQUIRED_INT_FIELDS:
            val = obj.get(key)
            if isinstance(val, bool) or not isinstance(val, int) or val < 0:
                raise ValueError(f"{source}:{lineno}: missing non-negative int field {key!r}")
        if obj["schema_version"] != 1:
            raise ValueError(f"{source}:{lineno}: unsupported schema_version {obj['schema_version']}")
        sha = obj["fixture_sha256"]
        if len(sha) != 64 or any(c not in "0123456789abcdef" for c in sha):
            raise ValueError(f"{source}:{lineno}: invalid fixture_sha256 {sha!r}")
        ratio = obj.get("peak_to_input_ratio")
        if isinstance(ratio, bool) or not isinstance(ratio, (int, float)) or not math.isfinite(ratio) or ratio < 0:
            raise ValueError(f"{source}:{lineno}: invalid peak_to_input_ratio {ratio!r}")
        key_pair = (obj["scenario"], obj["operation"])
        if key_pair in rows:
            raise ValueError(f"{source}:{lineno}: duplicate row for {key_pair}")
        rows[key_pair] = obj
    if not rows:
        raise ValueError(f"{source}: no allocation rows found")
    return rows


def allowed_limit(baseline_val: int, pct: float, floor: int) -> int:
    delta = max(int(math.ceil(baseline_val * pct / 100.0)), floor)
    return baseline_val + delta


def compare_baselines(
    baseline: dict[tuple[str, str], dict[str, Any]],
    current: dict[tuple[str, str], dict[str, Any]],
    *,
    max_bytes_pct: float = MAX_BYTES_PERCENT,
    min_bytes_floor: int = MIN_BYTES_FLOOR,
    max_allocs_pct: float = MAX_ALLOCS_PERCENT,
    min_allocs_floor: int = MIN_ALLOCS_FLOOR,
) -> list[str]:
    """Compare measured allocation rows against committed baseline; return violation messages."""
    violations: list[str] = []
    for key in sorted(baseline):
        base = baseline[key]
        cur = current.get(key)
        scenario, operation = key
        if cur is None:
            violations.append(f"{scenario} [{operation}]: missing from current run")
            continue
        if cur["fixture_sha256"] != base["fixture_sha256"]:
            violations.append(
                f"{scenario} [{operation}]: fixture_sha256 changed "
                f"({base['fixture_sha256'][:12]} -> {cur['fixture_sha256'][:12]})"
            )
            continue
        for metric in BYTE_METRICS:
            bval = int(base[metric])
            cval = int(cur[metric])
            limit = allowed_limit(bval, max_bytes_pct, min_bytes_floor)
            if cval > limit:
                pct = ((cval - bval) * 100.0 / max(bval, 1))
                violations.append(
                    f"{scenario} [{operation}]: {metric} regressed {bval} -> {cval} "
                    f"({pct:+.2f}%, limit {limit})"
                )
        b_allocs = int(base["allocations"])
        c_allocs = int(cur["allocations"])
        a_limit = allowed_limit(b_allocs, max_allocs_pct, min_allocs_floor)
        if c_allocs > a_limit:
            pct = ((c_allocs - b_allocs) * 100.0 / max(b_allocs, 1))
            violations.append(
                f"{scenario} [{operation}]: allocations regressed {b_allocs} -> {c_allocs} "
                f"({pct:+.2f}%, limit {a_limit})"
            )
    return violations


def run_memcount_baseline(repo_root: Path = ROOT) -> str:
    cmd = [
        "cargo",
        "run",
        "--quiet",
        "--release",
        "--locked",
        "--manifest-path",
        str(repo_root / "fuzz" / "Cargo.toml"),
        "--bin",
        "memcount",
        "--",
        "--jsonl",
        "--suite",
        "baseline",
    ]
    proc = subprocess.run(cmd, cwd=repo_root, capture_output=True, text=True)
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        raise SystemExit(f"memcount --jsonl failed with exit code {proc.returncode}")
    return proc.stdout


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, default=DEFAULT_BASELINE)
    parser.add_argument("--current", type=Path, default=None)
    parser.add_argument("--record", action="store_true")
    parser.add_argument("--max-bytes-pct", type=float, default=MAX_BYTES_PERCENT)
    parser.add_argument("--max-allocs-pct", type=float, default=MAX_ALLOCS_PERCENT)
    args = parser.parse_args(argv)

    if args.record:
        output = run_memcount_baseline()
        rows = parse_jsonl(output, "memcount")
        args.baseline.write_text(output, encoding="utf-8")
        print(f"recorded {len(rows)} baseline allocation rows to {args.baseline}")
        return 0

    if not args.baseline.is_file():
        print(f"allocation baseline missing: {args.baseline}", file=sys.stderr)
        return 2

    baseline_rows = parse_jsonl(args.baseline.read_text(encoding="utf-8"), str(args.baseline))
    current_text = (
        args.current.read_text(encoding="utf-8")
        if args.current is not None
        else run_memcount_baseline()
    )
    current_rows = parse_jsonl(current_text, str(args.current or "memcount"))
    violations = compare_baselines(
        baseline_rows,
        current_rows,
        max_bytes_pct=args.max_bytes_pct,
        max_allocs_pct=args.max_allocs_pct,
    )
    if violations:
        for v in violations:
            print(f"FAIL: {v}", file=sys.stderr)
        print(
            f"\n{len(violations)} allocation baseline violation(s) across {len(baseline_rows)} cases",
            file=sys.stderr,
        )
        return 1
    print(f"checked {len(baseline_rows)} structured JSONL allocation baseline cases: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
