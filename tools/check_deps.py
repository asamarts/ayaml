#!/usr/bin/env python3
"""Enforce ayaml's tiered dependency policy.

The parser graph is checked separately from the test and benchmark tooling.
Cargo metadata cannot see module-level graph boundaries, so this script
enforces the direct `ayaml` dependency allowlist and an allowlist for the whole
normal transitive package set.
"""

from __future__ import annotations

import json
import subprocess
import sys

# Direct normal dependencies of the public ayaml crate. Additions require a
# documented dependency-policy update.
AYAML_ALLOWED_DIRECT = {
    "indexmap",  # ordered value-layer Mapping
    "serde",     # required Serde contract
}

# Every package allowed in ayaml's normal graph, and why it is there. Anything
# else fails, so a YAML parser or native YAML stack cannot arrive through a
# transitive dependency. Cargo metadata unifies features across the workspace,
# so the derive stack that the tests enable for `serde` appears here too.
AYAML_ALLOWED_NORMAL = {
    "equivalent": "indexmap",
    "hashbrown": "indexmap",
    "indexmap": "ordered value-layer Mapping",
    "proc-macro2": "serde_derive",
    "quote": "serde_derive",
    "serde": "required Serde contract",
    "serde_core": "serde",
    "serde_derive": "serde's derive feature, enabled by the tests",
    "syn": "serde_derive",
    "unicode-ident": "proc-macro2",
}


def cargo_metadata() -> dict:
    output = subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        text=True,
    )
    return json.loads(output)


def normal_dep_ids(node: dict) -> list[str]:
    result = []
    for dep in node.get("deps", []):
        kinds = dep.get("dep_kinds", [])
        # Cargo metadata uses kind: null for a normal dependency.
        if any(kind.get("kind") in (None, "normal") for kind in kinds):
            result.append(dep["pkg"])
    return result


def main() -> int:
    metadata = cargo_metadata()
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}

    ayaml_ids = [
        package["id"]
        for package in metadata["packages"]
        if package["name"] == "ayaml"
    ]
    if len(ayaml_ids) != 1:
        print(f"FAIL: expected one ayaml package, found {len(ayaml_ids)}")
        return 1
    ayaml_id = ayaml_ids[0]

    direct = normal_dep_ids(nodes[ayaml_id])
    direct_names = {packages[dep]["name"] for dep in direct}
    unexpected = direct_names - AYAML_ALLOWED_DIRECT
    if unexpected:
        print(f"FAIL: unapproved ayaml direct dependency: {sorted(unexpected)}")
        return 1

    seen = set()
    stack = list(direct)
    while stack:
        dep_id = stack.pop()
        if dep_id in seen:
            continue
        seen.add(dep_id)
        stack.extend(normal_dep_ids(nodes[dep_id]))

    unclassified = sorted(
        f"{packages[dep_id]['name']} {packages[dep_id]['version']}"
        for dep_id in seen
        if packages[dep_id]["name"] not in AYAML_ALLOWED_NORMAL
    )
    if unclassified:
        print(f"FAIL: unclassified package in ayaml's normal graph: {unclassified}")
        return 1

    print(
        "OK: ayaml direct deps "
        f"{sorted(direct_names)}; normal graph {len(seen)} packages, all classified"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
