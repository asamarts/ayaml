#!/usr/bin/env python3
"""Enforce ayaml's dependency policy.

Reads `crates/ayaml/Cargo.toml` for all normal, build, and target-specific direct
dependencies, and reads `cargo tree` for the `ayaml` package with every feature on
for the host and supported cross targets, following normal and build dependencies.
That covers optional and platform-specific dependencies and leaves out what only
the tests or other workspace members use, so anything new in what users compile
fails here until it's added to a list below with a reason.
"""

from __future__ import annotations

from pathlib import Path
import re
import subprocess
import tomllib

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

# Platform targets checked in CI whose target-specific dependency edges must also be classified.
CROSS_TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-pc-windows-gnu",
)

# The scanner is kept free of third-party imports mechanically. These are the
# crate-local modules it may use besides core, alloc, std, self, and super.
SCANNER_CRATE_MODULES = {"error", "event", "limits", "position", "span", "tag"}
SCANNER_SELF_MODULES = {"line", "scalars"}
DISALLOWED_CRATES = set(ALLOWED_DIRECT) | set(ALLOWED_TRANSITIVE) | {"serde_json", "proptest"}

USE_PATTERN = re.compile(
    r"(?:^|[\s;{}\]])(?:pub(?:\s*\([^)]*\))?\s+)?use\s+([^;]+)"
)
EXTERN_CRATE_PATTERN = re.compile(
    r"(?:^|[\s;{}\]])(?:pub(?:\s*\([^)]*\))?\s+)?extern\s+crate\s+([^;]+)"
)
CRATE_PATH_PATTERN = re.compile(r"\bcrate::([A-Za-z_][A-Za-z0-9_]*)")
SUPER_ESCAPE_PATTERN = re.compile(r"\bsuper::super\b")
ROOT_PATH_PATTERN = re.compile(r"(?:^|[^A-Za-z0-9_:])::([A-Za-z_][A-Za-z0-9_]*)::")
QUALIFIED_PATH_PATTERN = re.compile(r"(?:^|[^A-Za-z0-9_:])([A-Za-z_][A-Za-z0-9_]*)::")


def manifest_direct_dependencies(manifest_path: Path) -> set[str]:
    """All direct runtime and build dependency package names declared in a Cargo.toml,
    including target-specific `[target.<cfg>.dependencies]` and `build-dependencies`."""
    manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    names: set[str] = set()

    def collect(table: object) -> None:
        if not isinstance(table, dict):
            return
        for key, spec in table.items():
            if isinstance(spec, dict) and isinstance(spec.get("package"), str):
                names.add(spec["package"])
            else:
                names.add(key)

    collect(manifest.get("dependencies"))
    collect(manifest.get("build-dependencies"))
    targets = manifest.get("target")
    if isinstance(targets, dict):
        for section in targets.values():
            if isinstance(section, dict):
                collect(section.get("dependencies"))
                collect(section.get("build-dependencies"))
    return names


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


def strip_rust_comments_and_literals(source: str) -> str:
    """Replace Rust comments and string/char literals with spaces while preserving line breaks."""
    out: list[str] = []
    i = 0
    n = len(source)
    while i < n:
        ch = source[i]
        # Line comment.
        if ch == "/" and i + 1 < n and source[i + 1] == "/":
            while i < n and source[i] != "\n":
                out.append(" ")
                i += 1
            continue
        # Nested block comment.
        if ch == "/" and i + 1 < n and source[i + 1] == "*":
            depth = 1
            out.extend((" ", " "))
            i += 2
            while i < n and depth > 0:
                if source[i] == "/" and i + 1 < n and source[i + 1] == "*":
                    depth += 1
                    out.extend((" ", " "))
                    i += 2
                elif source[i] == "*" and i + 1 < n and source[i + 1] == "/":
                    depth -= 1
                    out.extend((" ", " "))
                    i += 2
                else:
                    out.append("\n" if source[i] == "\n" else " ")
                    i += 1
            continue
        # Raw string literal: r"...", r#"..."#, br#"..."#.
        if ch in ("r", "b") and (i == 0 or not (source[i - 1].isalnum() or source[i - 1] == "_")):
            j = i + 1 if ch == "r" else (i + 2 if i + 1 < n and source[i + 1] == "r" else -1)
            if j != -1:
                hashes = 0
                while j + hashes < n and source[j + hashes] == "#":
                    hashes += 1
                if j + hashes < n and source[j + hashes] == '"':
                    end_quote = j + hashes + 1
                    while i < end_quote:
                        out.append(" ")
                        i += 1
                    closing = '"' + "#" * hashes
                    while i < n:
                        if source.startswith(closing, i):
                            out.extend(" " for _ in closing)
                            i += len(closing)
                            break
                        out.append("\n" if source[i] == "\n" else " ")
                        i += 1
                    continue
        # Normal string literal: "..." or b"...".
        if ch == '"':
            out.append(" ")
            i += 1
            while i < n:
                if source[i] == "\\" and i + 1 < n:
                    out.append("\n" if source[i] == "\n" else " ")
                    out.append("\n" if source[i + 1] == "\n" else " ")
                    i += 2
                    continue
                if source[i] == '"':
                    out.append(" ")
                    i += 1
                    break
                out.append("\n" if source[i] == "\n" else " ")
                i += 1
            continue
        # Char or byte literal: 'a', '\'', '\u{feff}' (leaving lifetimes like 'a intact).
        if ch == "'":
            if i + 1 < n and source[i + 1] == "\\":
                end = source.find("'", i + 2)
                newline = source.find("\n", i + 2)
                if end != -1 and (newline == -1 or end < newline):
                    out.extend(" " for _ in range(end - i + 1))
                    i = end + 1
                    continue
            elif i + 2 < n and source[i + 1] != "\n" and source[i + 2] == "'":
                out.extend((" ", " ", " "))
                i += 3
                continue
        out.append(ch)
        i += 1
    return "".join(out)


def scanner_import_root(spec: str) -> str:
    """The root of a `use` path, keeping `crate::module` for the allowlist."""
    spec = spec.strip()
    if spec.startswith("::"):
        spec = spec[2:]
    parts = [part.strip() for part in spec.split("::")]
    if not parts or not parts[0]:
        return ""
    if parts[0] == "crate" and len(parts) > 1:
        return "crate::" + parts[1].split("{", 1)[0].strip()
    if parts[0] == "self" and len(parts) > 1:
        return "self::" + parts[1].split("{", 1)[0].strip()
    if parts[0] == "super":
        if len(parts) > 1 and parts[1].split("{", 1)[0].strip() == "super":
            return "super::super"
        return "super"
    return parts[0].split("{", 1)[0].strip()


def scanner_files(root: Path) -> tuple[list[Path], str | None]:
    """Every production Rust source file in the scanner module tree."""
    required = [
        root / "scanner.rs",
        root / "scanner" / "line.rs",
        root / "scanner" / "scalars.rs",
    ]
    for path in required:
        if not path.is_file():
            return [], f"scanner source is missing: {path}"
    tests_file = (root / "scanner" / "tests.rs").resolve()
    discovered = [root / "scanner.rs"]
    for path in sorted((root / "scanner").rglob("*.rs")):
        if path.resolve() == tests_file:
            continue
        discovered.append(path)
    return discovered, None


def scanner_problem(root: Path) -> str | None:
    """Return the first disallowed scanner import or inline dependency reference, or None."""
    files, missing = scanner_files(root)
    if missing:
        return missing
    for path in files:
        cleaned = strip_rust_comments_and_literals(path.read_text(encoding="utf-8"))
        for line_number, line in enumerate(cleaned.splitlines(), 1):
            extern_match = EXTERN_CRATE_PATTERN.search(line)
            if extern_match:
                spec = extern_match.group(1).strip()
                return f"{path}:{line_number}: disallowed scanner import {spec!r}"
            for use_match in USE_PATTERN.finditer(line):
                spec = use_match.group(1).strip()
                import_root = scanner_import_root(spec)
                if import_root in {"std", "core", "alloc", "super"}:
                    continue
                if import_root.startswith("crate::") and import_root[7:] in SCANNER_CRATE_MODULES:
                    continue
                if import_root.startswith("self::") and import_root[6:] in SCANNER_SELF_MODULES:
                    continue
                return f"{path}:{line_number}: disallowed scanner import {spec!r}"
            non_use = USE_PATTERN.sub(" ", line)
            if SUPER_ESCAPE_PATTERN.search(non_use):
                return f"{path}:{line_number}: disallowed scanner path 'super::super'"
            for match in CRATE_PATH_PATTERN.finditer(non_use):
                module = match.group(1)
                if module not in SCANNER_CRATE_MODULES and module != "scanner":
                    return f"{path}:{line_number}: disallowed scanner path 'crate::{module}'"
            for match in ROOT_PATH_PATTERN.finditer(non_use):
                crate = match.group(1)
                if crate not in {"std", "core", "alloc"}:
                    return f"{path}:{line_number}: disallowed scanner path '::{crate}'"
            for match in QUALIFIED_PATH_PATTERN.finditer(non_use):
                crate = match.group(1)
                if crate in DISALLOWED_CRATES:
                    return f"{path}:{line_number}: disallowed scanner path '{crate}::'"
    return None


def main() -> int:
    repo = Path(__file__).resolve().parent.parent
    manifest_direct = manifest_direct_dependencies(repo / "crates/ayaml/Cargo.toml")
    unexpected_manifest = sorted(manifest_direct - ALLOWED_DIRECT.keys())
    if unexpected_manifest:
        print(f"FAIL: unapproved direct dependency in crates/ayaml/Cargo.toml: {unexpected_manifest}")
        return 1

    direct = package_names("--depth", "1")
    unexpected = sorted(direct - ALLOWED_DIRECT.keys())
    if unexpected:
        print(f"FAIL: unapproved direct dependency of ayaml: {unexpected}")
        return 1

    graph = package_names()
    for target in CROSS_TARGETS:
        graph |= package_names("--target", target)
    unclassified = sorted(graph - ALLOWED_DIRECT.keys() - ALLOWED_TRANSITIVE.keys())
    if unclassified:
        print(f"FAIL: unclassified package in ayaml's dependency graph: {unclassified}")
        return 1

    scanner = scanner_problem(repo / "crates/ayaml/src")
    if scanner:
        print(f"FAIL: {scanner}")
        return 1

    print(
        f"OK: ayaml's direct dependencies are {sorted(direct | manifest_direct)}; "
        f"{len(graph)} packages in all, each classified"
    )
    print("OK: scanner imports are core, alloc, std, or approved crate-local modules")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
