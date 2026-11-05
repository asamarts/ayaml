#!/usr/bin/env python3
"""Enforce ayaml's dependency policy.

Reads `cargo tree` for the `ayaml` package with every feature on, following normal
and build dependencies. That covers optional dependencies and leaves out what only
the tests or other workspace members use, so anything new in what users compile
fails here until it's added to a list below with a reason.
"""

from __future__ import annotations

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

    print(f"OK: ayaml's direct dependencies are {sorted(direct)}; {len(graph)} packages in all, each classified")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
