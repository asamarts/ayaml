#!/usr/bin/env python3
"""Enforce ayaml's tiered dependency policy.

The parser graph is checked separately from the test and benchmark tooling.
Cargo metadata cannot see module-level graph boundaries, so this script
enforces the direct `ayaml` dependency allowlist and the normal transitive
package set.
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

# A complete YAML parser or native YAML stack must never be in ayaml's normal
# runtime graph. Test and benchmark tooling may use a YAML oracle as a dev dependency.
FORBIDDEN_NORMAL = {
    "saphyr",
    "saphyr-parser",
    "yaml-rust",
    "yaml-rust2",
    "yaml_serde",
    "libyaml-rs",
    "unsafe-libyaml",
    "unsafe-libyaml-norway",
    "serde-saphyr",
    "serde_yaml",
    "serde_yaml_ng",
    "serde_yaml_neo",
    "serde_yaml_bw",
    "noyalib",
    "noyalib-serde-yaml",
    "yaml-rt",
    "yaml-rt-core",
    "yaml-rt-serde",
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
        package = packages[dep_id]
        if package["name"] in FORBIDDEN_NORMAL:
            print(
                "FAIL: forbidden YAML parser in ayaml's normal graph: "
                f"{package['name']} {package['version']}"
            )
            return 1
        stack.extend(normal_dep_ids(nodes[dep_id]))

    print(
        "OK: ayaml direct deps "
        f"{sorted(direct_names)}; normal graph {len(seen)} packages; "
        "no forbidden YAML parser"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
