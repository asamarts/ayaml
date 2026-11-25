#!/usr/bin/env python3
"""Evaluate the unsafe census, so that it can fail rather than only being collected.

usage: tools/check_unsafe_census.py [--crate CRATE] GEIGER_JSON

GEIGER_JSON is what `cargo geiger --output-format Json` wrote. This fails if the target workspace
crate (or `ayaml` when checking `ayaml-edit`) has any unsafe code or doesn't forbid it, if a crate
with unsafe code isn't listed below, or if the census didn't cover everything it was asked to.
See docs/dependencies.md. Exit status: 0 the census passes, 1 it doesn't, 2 the file can't be read.
"""

from __future__ import annotations

import json
import re
import sys

# Crates that may contain unsafe code, and who brings each one in.
ALLOWED_UNSAFE = {
    "hashbrown": "indexmap's hash table",
    "indexmap": "ordered value-layer Mapping",
    "serde": "required Serde contract",
    "serde_core": "serde",
}

# Workspace crates that must forbid unsafe code and have zero unsafe items for each census target.
REQUIRED_SAFE_CRATES: dict[str, tuple[str, ...]] = {
    "ayaml": ("ayaml",),
    "ayaml-edit": ("ayaml-edit", "ayaml"),
}

# Build scripts generate these, and the census can't scan them. Anything else it couldn't scan
# could hide unsafe code.
UNSCANNED = re.compile(r"[/\\]build[/\\](serde|serde_core)-[0-9a-f]+[/\\]out[/\\]private\.rs$")


def unsafe_count(package: dict) -> int:
    return sum(counts["unsafe_"] for counts in package["unsafety"]["used"].values())


def problems(census: dict, target_crate: str = "ayaml") -> list[str]:
    """What's wrong with a census. A shape this doesn't expect raises KeyError or TypeError."""
    if target_crate not in REQUIRED_SAFE_CRATES:
        raise ValueError(f"unsupported census target crate {target_crate!r}")
    required = REQUIRED_SAFE_CRATES[target_crate]
    seen = {name: 0 for name in required}
    found = []
    for package in census["packages"]:
        name = package["package"]["id"]["name"]
        count = unsafe_count(package)
        if name in seen:
            seen[name] += 1
            if count:
                found.append(f"{name} has {count} unsafe items")
            if not package["unsafety"]["forbids_unsafe"]:
                found.append(f"{name} doesn't forbid unsafe code")
        elif count and name not in ALLOWED_UNSAFE:
            found.append(f"{name} has {count} unsafe items and isn't allowed to")
    for name in required:
        if seen[name] != 1:
            found.append(f"the census has {seen[name]} entries for {name}, not 1")
    for package in census["packages_without_metrics"]:
        found.append(f"the census has no numbers for {package}")
    for path in census["used_but_not_scanned_files"]:
        if not UNSCANNED.search(path):
            found.append(f"the census didn't scan {path}")
    return found


def main(arguments: list[str]) -> int:
    target_crate = "ayaml"
    args = list(arguments)
    if len(args) >= 2 and args[0] == "--crate":
        target_crate = args[1]
        args = args[2:]
    if len(args) != 1 or target_crate not in REQUIRED_SAFE_CRATES:
        print(__doc__, file=sys.stderr)
        return 2
    try:
        with open(args[0], encoding="utf-8") as file:
            census = json.load(file)
        found = problems(census, target_crate=target_crate)
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        print(f"FAIL: can't read {args[0]} as a census: {error!r}")
        return 2
    for problem in found:
        print(f"FAIL: {problem}")
    if found:
        return 1
    print(f"unsafe census ok ({target_crate}): {len(census['packages'])} crates")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
