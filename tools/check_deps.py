#!/usr/bin/env python3
"""Enforce ayaml's dependency policy.

Reads `cargo tree` for the `ayaml` package with every feature on, following normal
and build dependencies. That covers optional dependencies and leaves out what only
the tests or other workspace members use, so anything new in what users compile
fails here until it's added to a list below with a reason.
"""

from __future__ import annotations

from pathlib import Path
import subprocess

# Direct dependencies of the public crate.
ALLOWED_DIRECT = {
    "indexmap": "ordered value-layer Mapping",
    "serde": "required Serde contract",
}

# Everything else that users of the crate compile.
ALLOWED_TRANSITIVE = {
    "equivalent": "indexmap",
    "hashbrown": "indexmap",
    "serde_core": "serde",
}

# The scanner is kept free of third-party imports mechanically. These are the
# crate-local modules it may use besides core, alloc, std, self, and super.
SCANNER_CRATE_MODULES = {"error", "event", "limits", "position", "span", "tag"}
SCANNER_SELF_MODULES = {"line", "scalars"}


def package_names(*extra: str) -> set[str]:
    output = subprocess.check_output(
        [
            "cargo",
            "tree",
            "-p",
            "ayaml",
            "-e",
            "normal,build",
            "--all-features",
            "--locked",
            "--prefix",
            "none",
            "--format",
            "{p}",
            *extra,
        ],
        text=True,
    )
    names = {line.split()[0] for line in output.splitlines() if line.strip()}
    names.discard("ayaml")
    return names


def scanner_import_root(spec: str) -> str:
    """The root of a `use` path, keeping `crate::module` for the allowlist."""
    spec = spec.strip()
    parts = spec.split("::")
    if not parts:
        return ""
    if parts[0] == "crate" and len(parts) > 1:
        return "crate::" + parts[1].split("::")[0]
    if parts[0] == "self" and len(parts) > 1:
        return "self::" + parts[1].split("::")[0]
    if parts[0] == "super":
        return "super"
    return parts[0]


def scanner_problem(root: Path) -> str | None:
    """Return the first disallowed scanner import, or None."""
    files = [
        root / "scanner.rs",
        root / "scanner" / "line.rs",
        root / "scanner" / "scalars.rs",
    ]
    for path in files:
        if not path.is_file():
            return f"scanner source is missing: {path}"
        for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            stripped = line.strip()
            if stripped.startswith("use "):
                spec = stripped[4:].split(";", 1)[0].strip()
                import_root = scanner_import_root(spec)
                if import_root in {"std", "core", "alloc", "super"}:
                    continue
                if import_root.startswith("crate::") and import_root[7:] in SCANNER_CRATE_MODULES:
                    continue
                if import_root.startswith("self::") and import_root[6:] in SCANNER_SELF_MODULES:
                    continue
                return f"{path}:{line_number}: disallowed scanner import {spec!r}"
            if stripped.startswith("extern crate "):
                spec = stripped[len("extern crate "):].split(";", 1)[0].strip()
                return f"{path}:{line_number}: disallowed scanner import {spec!r}"
    return None


def main() -> int:
    direct = package_names("--depth", "1")
    unexpected = sorted(direct - ALLOWED_DIRECT.keys())
    if unexpected:
        print(f"FAIL: unapproved direct dependency of ayaml: {unexpected}")
        return 1

    graph = package_names()
    unclassified = sorted(graph - ALLOWED_DIRECT.keys() - ALLOWED_TRANSITIVE.keys())
    if unclassified:
        print(f"FAIL: unclassified package in ayaml's dependency graph: {unclassified}")
        return 1

    scanner = scanner_problem(Path(__file__).resolve().parent.parent / "crates/ayaml/src")
    if scanner:
        print(f"FAIL: {scanner}")
        return 1

    print(f"OK: ayaml's direct dependencies are {sorted(direct)}; {len(graph)} packages in all, each classified")
    print("OK: scanner imports are core, alloc, std, or approved crate-local modules")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
