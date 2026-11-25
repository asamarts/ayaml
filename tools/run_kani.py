#!/usr/bin/env python3
"""Run Kani bounded model-checking harnesses sequentially under hard memory and time bounds.

Each harness runs in its own session and process group under:
- a kernel virtual-memory ceiling (`RLIMIT_AS`),
- a per-harness wall-clock timeout (both via Kani and via process-group SIGTERM/SIGKILL),
- an exact kernel peak-RSS (`ru_maxrss`) budget check.

This prevents a runaway CBMC/SAT formula from exhausting a shared workstation or hosted CI runner.

Exit status: 0 all harnesses verified within budget, 1 verification or budget failure,
2 configuration or invocation error.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import resource
import signal
import subprocess
import sys
import tempfile
import time

MIB = 1024 * 1024
_ACTIVE_PGID: int | None = None


def _kill_pgid(pgid: int, grace_s: float = 1.0) -> None:
    try:
        os.killpg(pgid, signal.SIGTERM)
    except ProcessLookupError:
        return
    deadline = time.monotonic() + grace_s
    while time.monotonic() < deadline:
        try:
            os.killpg(pgid, 0)
        except ProcessLookupError:
            return
        time.sleep(0.05)
    try:
        os.killpg(pgid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def _handle_signal(signum: int, _frame: object) -> None:
    if _ACTIVE_PGID is not None:
        _kill_pgid(_ACTIVE_PGID, grace_s=0.5)
    sys.exit(128 + signum)


def _apply_limits(vmem_mib: int, nice_inc: int) -> None:
    os.setsid()
    if nice_inc > 0:
        try:
            os.nice(nice_inc)
        except OSError:
            pass
    vmem_bytes = vmem_mib * MIB
    soft, hard = resource.getrlimit(resource.RLIMIT_AS)
    effective_soft = vmem_bytes
    if soft != resource.RLIM_INFINITY:
        effective_soft = min(effective_soft, soft)
    if hard != resource.RLIM_INFINITY:
        effective_soft = min(effective_soft, hard)
    resource.setrlimit(resource.RLIMIT_AS, (effective_soft, hard))


def run_bounded(
    cmd: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    vmem_mib: int,
    timeout_s: float,
    nice_inc: int = 10,
) -> tuple[int, float, float, str, str, bool]:
    """Run one command sequentially with RLIMIT_AS, process-group timeout, and peak RSS."""
    global _ACTIVE_PGID
    t0 = time.monotonic()
    timed_out = False
    with (
        tempfile.TemporaryFile(mode="w+", encoding="utf-8", errors="replace") as out_f,
        tempfile.TemporaryFile(mode="w+", encoding="utf-8", errors="replace") as err_f,
    ):
        proc = subprocess.Popen(
            cmd,
            cwd=cwd,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=out_f,
            stderr=err_f,
            preexec_fn=lambda: _apply_limits(vmem_mib, nice_inc),
        )
        _ACTIVE_PGID = proc.pid
        try:
            deadline = t0 + timeout_s
            while True:
                pid, status, rusage = os.wait4(proc.pid, os.WNOHANG)
                if pid != 0:
                    rc = os.waitstatus_to_exitcode(status)
                    proc.returncode = rc
                    break
                if time.monotonic() >= deadline:
                    timed_out = True
                    _kill_pgid(proc.pid, grace_s=1.0)
                    _, status, rusage = os.wait4(proc.pid, 0)
                    rc = 124
                    proc.returncode = rc
                    break
                time.sleep(0.05)
        finally:
            _ACTIVE_PGID = None
        # Ensure nothing in the child's process group survived after its leader exited.
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        elapsed = time.monotonic() - t0
        # On Linux ru_maxrss is in KiB; on macOS it is in bytes.
        rss_mib = (
            rusage.ru_maxrss / float(MIB)
            if sys.platform == "darwin"
            else rusage.ru_maxrss / 1024.0
        )
        out_f.seek(0)
        err_f.seek(0)
        return rc, elapsed, rss_mib, out_f.read(), err_f.read(), timed_out


def extract_harnesses(data: dict) -> list[str]:
    """Extract sorted standard Kani harness names from `kani-list.json`."""
    harnesses: list[str] = []
    for _file, items in sorted(data.get("standard-harnesses", {}).items()):
        for item in items:
            if isinstance(item, str):
                harnesses.append(item)
    return sorted(dict.fromkeys(harnesses))


def discover_harnesses(
    *,
    repo_root: Path,
    package: str,
    all_features: bool,
    env: dict[str, str],
    vmem_mib: int,
    timeout_s: float,
) -> list[str]:
    cmd = ["cargo", "kani", "-p", package]
    if all_features:
        cmd.append("--all-features")
    cmd.extend(["list", "--format", "json"])
    list_path = repo_root / "kani-list.json"
    existed_before = list_path.exists()
    try:
        rc, elapsed, rss_mib, out, err, timed_out = run_bounded(
            cmd,
            cwd=repo_root,
            env=env,
            vmem_mib=vmem_mib,
            timeout_s=timeout_s,
        )
        if rc != 0:
            reason = "timed out" if timed_out else f"exit {rc}"
            raise RuntimeError(
                f"cargo kani list failed ({reason}, {elapsed:.1f}s, {rss_mib:.1f} MiB):\n"
                f"{err[-2000:]}\n{out[-1000:]}"
            )
        if not list_path.is_file():
            raise RuntimeError(f"cargo kani list did not write {list_path}")
        data = json.loads(list_path.read_text(encoding="utf-8"))
        return extract_harnesses(data)
    finally:
        if not existed_before:
            list_path.unlink(missing_ok=True)


def render_markdown_summary(
    results: list[dict],
    *,
    max_rss_mib: float,
    vmem_mib: int,
    package: str | None = None,
) -> str:
    heading = (
        f"### Kani Bounded Model Checking Summary (`{package}`)"
        if package
        else "### Kani Bounded Model Checking Summary"
    )
    lines = [
        heading,
        "",
        f"- Virtual-memory cap per harness (`RLIMIT_AS`): **{vmem_mib} MiB**",
        f"- Peak RSS budget per harness: **{max_rss_mib:.0f} MiB**",
        "",
        "| Harness | Status | Time (s) | Peak RSS (MiB) |",
        "| --- | --- | ---: | ---: |",
    ]
    for row in results:
        lines.append(
            f"| `{row['harness']}` | {row['status']} | {row['elapsed_s']:.2f} | {row['peak_rss_mib']:.1f} |"
        )
    peak = max((row["peak_rss_mib"] for row in results), default=0.0)
    total_s = sum(row["elapsed_s"] for row in results)
    lines.extend(
        [
            "",
            f"Verified **{sum(1 for r in results if r['ok'])}/{len(results)}** harnesses in **{total_s:.1f}s** (max peak RSS **{peak:.1f} MiB**).",
        ]
    )
    return "\n".join(lines) + "\n"


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", default="ayaml", help="Cargo package to verify")
    parser.add_argument("--all-features", action="store_true", help="Pass --all-features to Kani")
    parser.add_argument(
        "--harness",
        action="append",
        default=[],
        help="Verify only matching harness substring(s); repeatable",
    )
    parser.add_argument(
        "--codegen-vmem-mib",
        type=int,
        default=8192,
        help="RLIMIT_AS ceiling (MiB) for harness discovery and codegen (default: 8192)",
    )
    parser.add_argument(
        "--harness-vmem-mib",
        type=int,
        default=3072,
        help="RLIMIT_AS ceiling (MiB) per Kani harness verification (default: 3072)",
    )
    parser.add_argument(
        "--max-rss-mib",
        type=float,
        default=2048.0,
        help="Maximum allowed peak RSS (MiB) per harness (default: 2048)",
    )
    parser.add_argument(
        "--harness-timeout",
        type=int,
        default=120,
        help="Per-harness wall-clock timeout in seconds (default: 120)",
    )
    parser.add_argument(
        "--codegen-timeout",
        type=int,
        default=300,
        help="Discovery/codegen timeout in seconds (default: 300)",
    )
    parser.add_argument(
        "--json-out",
        type=Path,
        default=None,
        help="Optional path to write machine-readable JSON results",
    )
    parser.add_argument(
        "--step-summary",
        action="store_true",
        help="Append a Markdown table to $GITHUB_STEP_SUMMARY when set",
    )
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="Repository root directory",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    if args.codegen_vmem_mib <= 0 or args.harness_vmem_mib <= 0 or args.max_rss_mib <= 0:
        print("memory limits must be positive", file=sys.stderr)
        return 2
    if args.harness_timeout <= 0 or args.codegen_timeout <= 0:
        print("timeouts must be positive", file=sys.stderr)
        return 2

    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, _handle_signal)

    env = os.environ.copy()
    cargo_bin = Path(env.get("CARGO_HOME", str(Path.home() / ".cargo"))) / "bin"
    if cargo_bin.is_dir():
        path_parts = env.get("PATH", "").split(os.pathsep)
        if str(cargo_bin) not in path_parts:
            env["PATH"] = f"{cargo_bin}{os.pathsep}{env.get('PATH', '')}"

    try:
        harnesses = discover_harnesses(
            repo_root=args.repo_root,
            package=args.package,
            all_features=args.all_features,
            env=env,
            vmem_mib=args.codegen_vmem_mib,
            timeout_s=float(args.codegen_timeout),
        )
    except (OSError, RuntimeError, ValueError) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 2

    if args.harness:
        harnesses = [h for h in harnesses if any(filt in h for filt in args.harness)]
    if not harnesses:
        print("FAIL: no Kani harnesses matched", file=sys.stderr)
        return 2

    # Compile Kani codegen once under the codegen ceiling before bounded per-harness solving.
    codegen_cmd = ["cargo", "kani", "-p", args.package]
    if args.all_features:
        codegen_cmd.append("--all-features")
    codegen_cmd.append("--only-codegen")
    rc, cg_dt, cg_rss, cg_out, cg_err, cg_to = run_bounded(
        codegen_cmd,
        cwd=args.repo_root,
        env=env,
        vmem_mib=args.codegen_vmem_mib,
        timeout_s=float(args.codegen_timeout),
    )
    if rc != 0:
        reason = "timed out" if cg_to else f"exit {rc}"
        print(
            f"FAIL: Kani codegen failed ({reason}, {cg_dt:.1f}s, {cg_rss:.1f} MiB)\n"
            f"{cg_err[-2000:]}\n{cg_out[-1000:]}",
            file=sys.stderr,
        )
        return 1

    print(
        f"Kani codegen ok ({cg_dt:.2f}s, {cg_rss:.1f} MiB); verifying {len(harnesses)} harness(es) "
        f"(RLIMIT_AS={args.harness_vmem_mib} MiB, max_rss={args.max_rss_mib:.0f} MiB, "
        f"timeout={args.harness_timeout}s each):",
        flush=True,
    )

    results: list[dict] = []
    failed = 0
    kani_timeout = max(1, args.harness_timeout - 5)
    for harness in harnesses:
        cmd = ["cargo", "kani", "-p", args.package]
        if args.all_features:
            cmd.append("--all-features")
        cmd.extend(
            [
                "--harness",
                harness,
                "--exact",
                "-Z",
                "unstable-options",
                "--harness-timeout",
                f"{kani_timeout}s",
            ]
        )
        rc, elapsed, rss_mib, out, err, timed_out = run_bounded(
            cmd,
            cwd=args.repo_root,
            env=env,
            vmem_mib=args.harness_vmem_mib,
            timeout_s=float(args.harness_timeout),
        )
        if timed_out:
            status = "TIMEOUT"
            ok = False
        elif rc != 0:
            status = f"FAIL(exit {rc})"
            ok = False
        elif rss_mib > args.max_rss_mib:
            status = f"RSS_EXCEEDED({rss_mib:.1f}>{args.max_rss_mib:.0f})"
            ok = False
        else:
            status = "PASS"
            ok = True

        results.append(
            {
                "harness": harness,
                "status": status,
                "ok": ok,
                "exit_code": rc,
                "elapsed_s": round(elapsed, 3),
                "peak_rss_mib": round(rss_mib, 2),
            }
        )
        print(f"  {harness:68s} {status:16s} {elapsed:6.2f}s  {rss_mib:7.1f} MiB", flush=True)
        if not ok:
            failed += 1
            if err.strip():
                print("    --- stderr (tail) ---\n" + err[-1200:], file=sys.stderr, flush=True)
            if out.strip():
                print("    --- stdout (tail) ---\n" + out[-1200:], file=sys.stderr, flush=True)

    peak_rss = max((row["peak_rss_mib"] for row in results), default=0.0)
    total_s = sum(row["elapsed_s"] for row in results)
    print(
        f"Kani summary: passed={len(results) - failed}/{len(results)} "
        f"failed={failed} total_time={total_s:.2f}s max_peak_rss={peak_rss:.1f} MiB",
        flush=True,
    )

    if args.json_out is not None:
        args.json_out.parent.mkdir(parents=True, exist_ok=True)
        args.json_out.write_text(
            json.dumps(
                {
                    "package": args.package,
                    "harness_vmem_mib": args.harness_vmem_mib,
                    "max_rss_mib": args.max_rss_mib,
                    "harness_timeout_s": args.harness_timeout,
                    "max_peak_rss_mib": round(peak_rss, 2),
                    "total_elapsed_s": round(total_s, 3),
                    "harnesses": results,
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

    if args.step_summary:
        summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
        if summary_path:
            md = render_markdown_summary(
                results,
                max_rss_mib=args.max_rss_mib,
                vmem_mib=args.harness_vmem_mib,
                package=args.package,
            )
            with open(summary_path, "a", encoding="utf-8") as handle:
                handle.write(md)

    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
