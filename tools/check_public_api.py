#!/usr/bin/env python3
"""Compare a published workspace crate's public API against the last released tag.

usage:
  tools/check_public_api.py [--package PKG] [--base REV] [--head REV]
                            [--changelog PATH | --no-changelog] [--json]

Builds rustdoc JSON with the pinned nightly toolchain and lists the public API with the pinned
`ayaml-public-api` binary (installed by `tools/install-public-api.sh`) for each feature set that
changes the public API: the default `ayaml` crate and `libyaml-compat`, or `--package ayaml-edit`.
Also verifies that every feature in the crate's `Cargo.toml` is accounted for and that hidden
features such as `bench-internals` add no public items.

Any removed item or changed signature, trait implementation, or associated type fails unless the
active `### Changed` section of `CHANGELOG.md` names it. Purely additive changes pass without a
`### Changed` entry.

Exit status: 0 no undocumented breaks, 1 undocumented break(s) found, 2 the check could not run.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tomllib
from typing import Iterable

TOOLCHAIN = "nightly-2026-05-21"
TOOL_BIN = "ayaml-public-api"
TOOL_VERSION = "0.52.0"
PACKAGE = "ayaml"
PACKAGE_DIR = "crates/ayaml"
ROOT = Path(__file__).resolve().parent.parent
WORK = ROOT / "target" / "public-api"

# Feature sets that change the public API of `ayaml`, in check order.
FEATURE_SETS: tuple[tuple[str, tuple[str, ...]], ...] = (
    ("default", ()),
    ("libyaml-compat", ("libyaml-compat",)),
)
# Features declared in crates/ayaml/Cargo.toml that must not add to the public API.
NON_API_FEATURES: frozenset[str] = frozenset({"default", "bench-internals"})


@dataclass(frozen=True)
class PackageConfig:
    package: str
    package_dir: str
    crate_ident: str
    feature_sets: tuple[tuple[str, tuple[str, ...]], ...]
    non_api_features: frozenset[str]
    hidden_features: tuple[str, ...]


PACKAGE_CONFIGS: dict[str, PackageConfig] = {
    "ayaml": PackageConfig(
        package="ayaml",
        package_dir="crates/ayaml",
        crate_ident="ayaml",
        feature_sets=FEATURE_SETS,
        non_api_features=NON_API_FEATURES,
        hidden_features=("bench-internals",),
    ),
    "ayaml-edit": PackageConfig(
        package="ayaml-edit",
        package_dir="crates/ayaml-edit",
        crate_ident="ayaml_edit",
        feature_sets=(("default", ()),),
        non_api_features=frozenset({"default"}),
        hidden_features=(),
    ),
}

TAG_RE = re.compile(r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
SEMVER_RE = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")

# Compiler-internal unstable marker traits that stable Rust callers cannot name or bound on.
IGNORED_UNSTABLE_TRAITS: tuple[str, ...] = (
    "core::marker::Freeze",
    "core::marker::StructuralPartialEq",
    "core::marker::StructuralEq",
)

# Associated type names and the standard trait short names that define them.
ASSOC_TYPE_TRAITS: dict[str, tuple[str, ...]] = {
    "IntoIter": ("IntoIterator",),
    "Item": ("Iterator", "IntoIterator"),
    "Target": ("Deref", "DerefMut"),
    "Output": ("Index", "IndexMut", "Add", "Sub", "Mul", "Div", "FnOnce"),
    "Error": ("TryFrom", "TryInto", "Deserializer"),
    "Err": ("FromStr",),
}

ATTR_PREFIX_RE = re.compile(r"^(?:#\[[^\]]+\]\s+)+")
_PATH = r"[A-Za-z0-9_]+(?:<[^>()]*>)?(?:::[A-Za-z0-9_]+(?:<[^>()]*>)?)*"
_REF_PATH = rf"(?:&(?:'[A-Za-z0-9_]+\s+)?(?:mut\s+)?)?{_PATH}"
FN_RE = re.compile(rf"^pub\s+(?:const\s+)?(?:unsafe\s+)?fn\s+(?P<path>{_REF_PATH})")
TYPE_RE = re.compile(rf"^pub\s+type\s+(?P<path>{_REF_PATH})")
CONST_STATIC_RE = re.compile(rf"^pub\s+(?P<kind>const|static)\s+(?:mut\s+)?(?P<path>{_PATH})")
DECL_RE = re.compile(rf"^pub\s+(?P<kind>struct|enum|trait|union|mod)\s+(?P<path>{_PATH})")
_CRATE_IDENTS = "|".join(re.escape(cfg.crate_ident) for cfg in PACKAGE_CONFIGS.values())
MEMBER_RE = re.compile(rf"^pub\s+(?P<path>(?:{_CRATE_IDENTS})::{_PATH})")


def parse_impl_header(line: str) -> tuple[str | None, str] | None:
    """Parse an `impl` line into (trait_or_none, target), handling nested `<...>` generics."""
    if not line.startswith("impl"):
        return None
    rest = line[4:]
    if rest and rest[0] not in ("<", " "):
        return None
    rest = rest.lstrip()
    if rest.startswith("<"):
        depth = 0
        end = -1
        for idx, ch in enumerate(rest):
            if ch == "<":
                depth += 1
            elif ch == ">":
                depth -= 1
                if depth == 0:
                    end = idx
                    break
        if end < 0:
            return None
        rest = rest[end + 1 :].lstrip()
    if rest.startswith("unsafe "):
        rest = rest[7:].lstrip()
    # Strip trailing `where ...` clause at angle-bracket depth 0.
    where_pos = _find_top_level(rest, " where ")
    if where_pos >= 0:
        rest = rest[:where_pos].rstrip()
    for_pos = _find_top_level(rest, " for ")
    if for_pos >= 0:
        trait_part = rest[:for_pos].strip()
        target_part = rest[for_pos + 5 :].strip()
        return trait_part, target_part
    return None, rest.strip()


def _find_top_level(text: str, needle: str) -> int:
    depth = 0
    limit = len(text) - len(needle) + 1
    for idx in range(max(0, limit)):
        ch = text[idx]
        if ch in "<([":
            depth += 1
        elif ch in ">)]":
            depth = max(0, depth - 1)
        elif depth == 0 and text.startswith(needle, idx):
            return idx
    return -1


@dataclass(frozen=True)
class ApiBreak:
    kind: str  # "removed" or "changed"
    feature_set: str
    old: str
    new: str | None = None

    def to_dict(self) -> dict[str, str | None]:
        return {
            "kind": self.kind,
            "feature_set": self.feature_set,
            "old": self.old,
            "new": self.new,
        }


@dataclass(frozen=True)
class ApiDiff:
    removed: tuple[str, ...]
    changed: tuple[tuple[str, str], ...]
    added: tuple[str, ...]


def parse_semver(text: str) -> tuple[int, int, int] | None:
    match = SEMVER_RE.fullmatch(text.strip())
    if match is None:
        return None
    return int(match.group(1)), int(match.group(2)), int(match.group(3))


def parse_semver_tag(tag: str) -> tuple[int, int, int] | None:
    match = TAG_RE.fullmatch(tag.strip())
    if match is None:
        return None
    return int(match.group(1)), int(match.group(2)), int(match.group(3))


def run(
    *command: str | Path,
    cwd: Path | None = None,
    env: dict[str, str | None] | None = None,
) -> str:
    merged_env = os.environ.copy()
    if env:
        for key, value in env.items():
            if value is None:
                merged_env.pop(key, None)
            else:
                merged_env[key] = value
    completed = subprocess.run(
        [str(part) for part in command],
        cwd=cwd,
        env=merged_env,
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        if completed.stderr:
            sys.stderr.write(completed.stderr)
        raise RuntimeError(f"command failed ({completed.returncode}): {' '.join(str(p) for p in command)}")
    return completed.stdout


def last_release_tag(root: Path, head_ref: str = "HEAD") -> str:
    """Return the highest vMAJOR.MINOR.PATCH tag reachable from head_ref, excluding tags at head_ref."""
    head_commit = run("git", "-C", root, "rev-parse", f"{head_ref}^{{commit}}").strip()
    tags_out = run("git", "-C", root, "tag", "--merged", head_commit)
    candidates: list[tuple[tuple[int, int, int], str]] = []
    for raw in tags_out.splitlines():
        tag = raw.strip()
        version = parse_semver_tag(tag)
        if version is None:
            continue
        tag_commit = run("git", "-C", root, "rev-parse", f"refs/tags/{tag}^{{commit}}").strip()
        if tag_commit == head_commit:
            continue
        candidates.append((version, tag))
    if not candidates:
        raise RuntimeError(f"no release tag vMAJOR.MINOR.PATCH found before {head_ref}")
    candidates.sort()
    return candidates[-1][1]


def package_version(tree: Path, package_dir: str = PACKAGE_DIR) -> str:
    manifest = tomllib.loads((tree / package_dir / "Cargo.toml").read_text(encoding="utf-8"))
    version = manifest["package"]["version"]
    if isinstance(version, dict) and version.get("workspace") is True:
        workspace = tomllib.loads((tree / "Cargo.toml").read_text(encoding="utf-8"))
        version = workspace["workspace"]["package"]["version"]
    if not isinstance(version, str):
        raise RuntimeError(f"{package_dir}/Cargo.toml has no version")
    return version


def validate_manifest_features(
    manifest_text: str,
    *,
    package_dir: str = PACKAGE_DIR,
    feature_sets: tuple[tuple[str, tuple[str, ...]], ...] = FEATURE_SETS,
    non_api_features: frozenset[str] = NON_API_FEATURES,
) -> None:
    """Ensure every feature in Cargo.toml is covered by feature_sets or non_api_features."""
    manifest = tomllib.loads(manifest_text)
    declared = set(manifest.get("features", {}).keys())
    covered = set(non_api_features)
    for _, features in feature_sets:
        covered.update(features)
    unknown = sorted(declared - covered)
    if unknown:
        raise RuntimeError(
            f"unclassified feature(s) in {package_dir}/Cargo.toml: {', '.join(unknown)}; "
            "add each public feature to PACKAGE_CONFIGS in tools/check_public_api.py"
        )


def is_ignored_unstable_trait(line: str) -> bool:
    parsed = parse_impl_header(line)
    if parsed is None or parsed[0] is None:
        return False
    trait_text = parsed[0].lstrip("!").strip()
    return any(
        trait_text == ignored or trait_text.startswith(f"{ignored}<")
        for ignored in IGNORED_UNSTABLE_TRAITS
    )


def filter_listing(lines: Iterable[str]) -> set[str]:
    result: set[str] = set()
    for raw in lines:
        line = raw.strip()
        if not line or is_ignored_unstable_trait(line):
            continue
        result.add(line)
    return result


def strip_attributes(line: str) -> str:
    return ATTR_PREFIX_RE.sub("", line)


def strip_generic_args(path: str) -> str:
    """Drop balanced `<...>` generic parameter lists from a path."""
    out: list[str] = []
    depth = 0
    for ch in path:
        if ch == "<":
            depth += 1
        elif ch == ">":
            depth = max(0, depth - 1)
        elif depth == 0:
            out.append(ch)
    return "".join(out)


def split_ref_prefix(path: str) -> tuple[str, str]:
    """Return (ref_kind, inner_path) where ref_kind is '', '&', or '&mut '."""
    trimmed = path.strip()
    match = re.match(r"^&(?:'[A-Za-z0-9_]+\s+)?(mut\s+)?(.*)$", trimmed)
    if match is None:
        return "", trimmed
    return ("&mut " if match.group(1) else "&"), match.group(2).strip()


def normalize_ref_lifetimes(path: str) -> str:
    """Normalize lifetime names on reference targets (e.g. `&'a T` -> `&T`)."""
    ref_kind, inner = split_ref_prefix(path)
    return f"{ref_kind}{inner}"


def item_key(line: str) -> tuple[str, ...]:
    """Return a structural identity key for pairing changed items between base and head."""
    body = strip_attributes(line)
    if match := FN_RE.match(body):
        return ("fn", normalize_ref_lifetimes(strip_generic_args(match.group("path"))))
    if match := TYPE_RE.match(body):
        return ("type", normalize_ref_lifetimes(strip_generic_args(match.group("path"))))
    if match := CONST_STATIC_RE.match(body):
        return (match.group("kind"), strip_generic_args(match.group("path")))
    if match := DECL_RE.match(body):
        return (match.group("kind"), strip_generic_args(match.group("path")))
    if (parsed := parse_impl_header(body)) is not None:
        trait_part, target_part = parsed
        target_text = normalize_ref_lifetimes(target_part)
        if trait_part is not None:
            return ("trait_impl", trait_part.lstrip("!").strip(), target_text)
        return ("inherent_impl", target_text)
    if match := MEMBER_RE.match(body):
        return ("member", strip_generic_args(match.group("path")))
    return ("other", body)


def collect_traits(items: set[str]) -> set[str]:
    traits: set[str] = set()
    for line in items:
        body = strip_attributes(line)
        match = DECL_RE.match(body)
        if match and match.group("kind") == "trait":
            traits.add(strip_generic_args(match.group("path")))
    return traits


def canonicalize_self_for_method(
    line: str, trait_paths: set[str], crate_ident: str = PACKAGE
) -> str | None:
    """Replace standalone `Self` with the concrete owner type on non-trait methods."""
    body = strip_attributes(line)
    match = FN_RE.match(body)
    if match is None:
        return None
    full_path = match.group("path")
    ref_kind, inner = split_ref_prefix(full_path)
    if ref_kind or "::" not in inner:
        return None
    owner = inner.rsplit("::", 1)[0]
    owner_base = strip_generic_args(owner)
    if owner_base == crate_ident or owner_base in trait_paths:
        return None
    # Replace standalone `Self` (not `Self::Assoc`) with the concrete owner path.
    return re.sub(r"(?<![A-Za-z0-9_])Self(?![A-Za-z0-9_:])", owner, body)


def diff_listings(base: set[str], head: set[str], crate_ident: str = PACKAGE) -> ApiDiff:
    """Compute removed, changed, and added items between two filtered public API listings."""
    only_base = sorted(base - head)
    only_head = sorted(head - base)

    trait_paths = collect_traits(base) | collect_traits(head)

    # First pass: cancel semantically identical non-trait methods that differ only in `Self` vs
    # their concrete owner type (e.g. `#[derive(PartialEq)]` vs manual `impl PartialEq`).
    head_by_canon: dict[str, list[int]] = {}
    for idx, item in enumerate(only_head):
        canon = canonicalize_self_for_method(item, trait_paths, crate_ident=crate_ident)
        if canon is not None:
            head_by_canon.setdefault(canon, []).append(idx)

    matched_head: set[int] = set()
    remaining_base: list[str] = []
    for old in only_base:
        canon = canonicalize_self_for_method(old, trait_paths, crate_ident=crate_ident)
        if canon is not None and head_by_canon.get(canon):
            matched_head.add(head_by_canon[canon].pop(0))
        else:
            remaining_base.append(old)

    remaining_head = [item for idx, item in enumerate(only_head) if idx not in matched_head]

    # Second pass: pair remaining base/head items sharing the same structural item_key.
    head_by_key: dict[tuple[str, ...], list[str]] = {}
    for item in remaining_head:
        head_by_key.setdefault(item_key(item), []).append(item)

    removed: list[str] = []
    changed: list[tuple[str, str]] = []

    for old in remaining_base:
        key = item_key(old)
        candidates = head_by_key.get(key)
        if candidates:
            new = candidates.pop(0)
            changed.append((old, new))
        else:
            removed.append(old)

    added = sorted(item for remaining in head_by_key.values() for item in remaining)
    return ApiDiff(removed=tuple(removed), changed=tuple(changed), added=tuple(added))


def active_changed_section(
    changelog_text: str,
    head_version: str,
    base_version: str | None,
) -> str:
    """Return the `### Changed` subsection of the active release section, or '' if none applies.

    Rules:
    - If `## [Unreleased]` exists, its `### Changed` subsection is active.
    - Otherwise, if `head_version` is strictly newer than `base_version` (or `base_version` is
      not a semver tag), the `## [head_version] — YYYY-MM-DD` section's `### Changed` is active.
    - Otherwise (working on `main` right after a release before opening `## [Unreleased]`),
      no `Changed` section is active, so any break fails until `## [Unreleased]` names it.
    """
    section_body = _extract_h2_section(changelog_text, r"Unreleased")
    if section_body is None:
        head_sv = parse_semver(head_version)
        base_sv = parse_semver(base_version) if base_version is not None else None
        if head_sv is not None and (base_sv is None or head_sv > base_sv):
            pattern = rf"{re.escape(head_version)}\] (?:—|-) [0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}}"
            section_body = _extract_h2_section(changelog_text, pattern, closed_bracket=False)
    if not section_body:
        return ""
    heading = re.search(r"^### Changed\s*$", section_body, re.M)
    if heading is None:
        return ""
    rest = section_body[heading.end() :]
    following = re.search(r"^### ", rest, re.M)
    return (rest[: following.start()] if following else rest).strip()


def _extract_h2_section(
    changelog_text: str,
    inner_pattern: str,
    *,
    closed_bracket: bool = True,
) -> str | None:
    suffix = r"\]\s*$" if closed_bracket else r"\s*$"
    heading = re.search(rf"^## \[{inner_pattern}{suffix}", changelog_text, re.M)
    if heading is None:
        return None
    rest = changelog_text[heading.end() :]
    following = re.search(r"^## ", rest, re.M)
    return (rest[: following.start()] if following else rest).strip()


def split_bullets(changed_text: str) -> list[str]:
    """Split a `### Changed` markdown section into individual bullet/paragraph blocks."""
    if not changed_text.strip():
        return []
    blocks: list[str] = []
    current: list[str] = []
    for line in changed_text.splitlines():
        if re.match(r"^[-*]\s+", line):
            if current:
                blocks.append("\n".join(current))
                current = []
            current.append(line)
        elif not line.strip():
            if current:
                blocks.append("\n".join(current))
                current = []
        else:
            current.append(line)
    if current:
        blocks.append("\n".join(current))
    return blocks


def find_root_reexports(items: set[str], crate_ident: str = PACKAGE) -> set[str]:
    """Return names that appear directly at crate root (`<crate>::Name`)."""
    names: set[str] = set()
    for line in items:
        body = strip_attributes(line)
        for regex in (DECL_RE, FN_RE, CONST_STATIC_RE, TYPE_RE):
            match = regex.match(body)
            if match is None:
                continue
            path = strip_generic_args(match.group("path"))
            ref_kind, inner = split_ref_prefix(path)
            if ref_kind:
                continue
            parts = inner.split("::")
            if len(parts) == 2 and parts[0] == crate_ident:
                names.add(parts[1])
            break
    return names


def path_aliases(
    full_path: str, root_reexports: set[str], crate_ident: str = PACKAGE
) -> list[str]:
    """Return acceptable ways to name `<crate>::...` in `CHANGELOG.md`.

    Submodule items under `compat::` always keep `compat::`. Other submodule items may omit the
    defining module only when the item is re-exported at the crate root. For hyphenated crate
    names (such as `ayaml-edit` whose Rust identifier is `ayaml_edit`), both forms are accepted.
    """
    clean = strip_generic_args(full_path)
    parts = clean.split("::")
    if not parts or parts[0] != crate_ident:
        return [clean]
    pkg_name = crate_ident.replace("_", "-")
    prefixes = [crate_ident] if pkg_name == crate_ident else [crate_ident, pkg_name]
    if len(parts) == 1:
        return prefixes
    tail = "::".join(parts[1:])
    aliases = [f"{prefix}::{tail}" for prefix in prefixes] + [tail]
    if len(parts) == 2:
        return list(dict.fromkeys(aliases))
    # `<crate>::<mod>::<Type>[::<member>...]`
    module, head_name, *rest = parts[1:]
    if module != "compat" and head_name in root_reexports:
        short = "::".join([head_name, *rest])
        aliases.append(short)
        for prefix in prefixes:
            aliases.append(f"{prefix}::{short}")
    return list(dict.fromkeys(aliases))


_REF_BEFORE_RE = re.compile(r"&(?:'[A-Za-z0-9_]+\s+)?(?:mut\s+)?$")


def text_names_identifier(text: str, name: str) -> bool:
    """Whether `text` names Rust path `name` with clean token boundaries (not as `&T`/`&mut T`)."""
    escaped = re.escape(name)
    # Require that `name` is not part of a longer path/identifier, nor preceded by any
    # `&` / `&'a ` / `&mut ` / `&'a mut ` reference prefix, nor followed by `::`.
    pattern = rf"(?<![A-Za-z0-9_:])({escaped})(?!::)(?![A-Za-z0-9_])"
    for match in re.finditer(pattern, text):
        if _REF_BEFORE_RE.search(text[: match.start()]) is None:
            return True
    return False


def text_names_target(text: str, ref_kind: str, aliases: list[str]) -> bool:
    """Whether `text` names a type target, distinguishing owned `T`, `&T`, and `&mut T`."""
    for alias in aliases:
        if ref_kind == "":
            if text_names_identifier(text, alias):
                return True
            continue
        escaped = re.escape(alias)
        if ref_kind == "&":
            pattern = rf"(?<![A-Za-z0-9_&])&(?:'[A-Za-z0-9_]+\s+)?{escaped}(?!::)(?![A-Za-z0-9_])"
        else:
            pattern = (
                rf"(?<![A-Za-z0-9_&])&(?:'[A-Za-z0-9_]+\s+)?mut\s+{escaped}(?!::)(?![A-Za-z0-9_])"
            )
        if re.search(pattern, text) is not None:
            return True
    return False


def trait_short_name(trait_text: str) -> str:
    base = strip_generic_args(trait_text).strip().lstrip("!")
    return base.rsplit("::", 1)[-1]


def removed_top_level_types(removed: Iterable[str]) -> set[str]:
    types: set[str] = set()
    for line in removed:
        body = strip_attributes(line)
        match = DECL_RE.match(body)
        if match and match.group("kind") in ("struct", "enum", "trait", "union", "mod"):
            types.add(strip_generic_args(match.group("path")))
    return types


def item_owner_type(line: str) -> str | None:
    body = strip_attributes(line)
    if (parsed := parse_impl_header(body)) is not None:
        _, inner = split_ref_prefix(strip_generic_args(parsed[1]))
        return inner
    for regex in (FN_RE, TYPE_RE, CONST_STATIC_RE, DECL_RE, MEMBER_RE):
        if match := regex.match(body):
            _, inner = split_ref_prefix(strip_generic_args(match.group("path")))
            parts = inner.split("::")
            # e.g. ayaml::span::Span::start -> ayaml::span::Span, or ayaml::Event::Scalar::span -> ayaml::Event
            if len(parts) >= 3:
                return "::".join(parts[:-1])
    return None


def is_documented(
    entry: ApiBreak,
    bullets: list[str],
    root_reexports: set[str],
    removed_types: set[str],
    crate_ident: str = PACKAGE,
) -> bool:
    """Whether `entry` is explicitly named in `bullets` (the active `### Changed` section)."""
    if not bullets:
        return False

    # If an entire type/module was removed, naming that removed parent covers its members/impls.
    owner = item_owner_type(entry.old)
    if owner is not None:
        parts = owner.split("::")
        for length in range(len(parts), 1, -1):
            ancestor = "::".join(parts[:length])
            if ancestor in removed_types:
                aliases = path_aliases(ancestor, root_reexports, crate_ident=crate_ident)
                if any(text_names_identifier(b, a) for b in bullets for a in aliases):
                    return True

    body = strip_attributes(entry.old)

    # 1. Trait or inherent implementation.
    if (parsed := parse_impl_header(body)) is not None:
        trait_part, target_part = parsed
        ref_kind, inner = split_ref_prefix(strip_generic_args(target_part))
        target_aliases = path_aliases(inner, root_reexports, crate_ident=crate_ident)
        if trait_part is not None:
            trait_short = trait_short_name(trait_part)
            for bullet in bullets:
                if text_names_target(bullet, ref_kind, target_aliases) and text_names_identifier(
                    bullet, trait_short
                ):
                    return True
            return False
        return any(text_names_target(b, ref_kind, target_aliases) for b in bullets)

    # 3. Type alias or associated type (`pub type ...`).
    if match := TYPE_RE.match(body):
        full_path = strip_generic_args(match.group("path"))
        ref_kind, inner = split_ref_prefix(full_path)
        parts = inner.split("::")
        # Associated type on an impl (`&'a ayaml::mapping::Mapping::IntoIter` or `ayaml::mapping::Mapping::IntoIter`)
        # vs top-level/module type alias (`ayaml::Result` or `ayaml::compat::MyType`).
        if ref_kind or (len(parts) >= 3 and parts[-2][0].isupper()):
            assoc_name = parts[-1]
            target_path = "::".join(parts[:-1])
            target_aliases = path_aliases(target_path, root_reexports, crate_ident=crate_ident)
            explicit_aliases = [f"{a}::{assoc_name}" for a in target_aliases]
            if any(text_names_target(b, ref_kind, explicit_aliases) for b in bullets):
                return True
            allowed_traits = ASSOC_TYPE_TRAITS.get(assoc_name, ())
            for bullet in bullets:
                if not text_names_target(bullet, ref_kind, target_aliases):
                    continue
                if text_names_identifier(bullet, assoc_name) or any(
                    text_names_identifier(bullet, tr) for tr in allowed_traits
                ):
                    return True
            return False
        aliases = path_aliases(inner, root_reexports, crate_ident=crate_ident)
        return any(text_names_identifier(b, a) for b in bullets for a in aliases)

    # 4. Function or method (`pub [const] [unsafe] fn ...`).
    if match := FN_RE.match(body):
        full_path = strip_generic_args(match.group("path"))
        ref_kind, inner = split_ref_prefix(full_path)
        aliases = path_aliases(inner, root_reexports, crate_ident=crate_ident)
        if ref_kind:
            return any(text_names_target(b, ref_kind, aliases) for b in bullets)
        return any(text_names_identifier(b, a) for b in bullets for a in aliases)

    # 5. Const, static, struct, enum, trait, union, mod, field, variant.
    for regex in (CONST_STATIC_RE, DECL_RE, MEMBER_RE):
        if match := regex.match(body):
            inner = strip_generic_args(match.group("path"))
            aliases = path_aliases(inner, root_reexports, crate_ident=crate_ident)
            return any(text_names_identifier(b, a) for b in bullets for a in aliases)

    return False


def extract_revision(root: Path, rev: str) -> Path:
    """Extract git revision `rev` into a commit-keyed cache under WORK/trees."""
    commit = run("git", "-C", root, "rev-parse", f"{rev}^{{commit}}").strip()
    dest = WORK / "trees" / commit
    marker = dest / ".extracted"
    if marker.is_file():
        return dest
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    archive = subprocess.Popen(["git", "-C", str(root), "archive", commit], stdout=subprocess.PIPE)
    subprocess.run(["tar", "-x", "-C", str(dest)], stdin=archive.stdout, check=True)
    assert archive.stdout is not None
    archive.stdout.close()
    if archive.wait() != 0:
        shutil.rmtree(dest, ignore_errors=True)
        raise RuntimeError(f"git archive failed for {rev}")
    marker.write_text(commit + "\n", encoding="utf-8")
    return dest


def generate_listing(
    tree: Path,
    target_dir: Path,
    features: tuple[str, ...],
    tool_bin: str,
    package: str = PACKAGE,
    crate_ident: str = PACKAGE,
) -> set[str]:
    cmd: list[str | Path] = [
        "cargo",
        f"+{TOOLCHAIN}",
        "rustdoc",
        "-q",
        "--manifest-path",
        tree / "Cargo.toml",
        "-p",
        package,
        "--lib",
        "--locked",
    ]
    if features:
        cmd.extend(["--features", ",".join(features)])
    cmd.extend(["--", "-Z", "unstable-options", "--output-format", "json"])
    env: dict[str, str | None] = {
        "CARGO_TARGET_DIR": str(target_dir),
        "RUSTFLAGS": None,
        "RUSTDOCFLAGS": None,
        "CARGO_ENCODED_RUSTFLAGS": None,
        "CARGO_BUILD_RUSTFLAGS": None,
        "CARGO_ENCODED_RUSTDOCFLAGS": None,
    }
    run(*cmd, env=env)
    json_path = target_dir / "doc" / f"{crate_ident}.json"
    if not json_path.is_file():
        raise RuntimeError(f"rustdoc did not write {json_path}")
    raw = run(tool_bin, "list", json_path)
    return filter_listing(raw.splitlines())


def check_comparison(
    base_listings: dict[str, set[str]],
    head_listings: dict[str, set[str]],
    changelog_text: str,
    head_version: str,
    base_version: str | None,
    crate_ident: str = PACKAGE,
) -> tuple[list[ApiBreak], list[ApiBreak], dict[str, ApiDiff]]:
    """Compare listings across all feature sets and return (undocumented, documented, diffs)."""
    changed_text = active_changed_section(changelog_text, head_version, base_version)
    bullets = split_bullets(changed_text)
    diffs: dict[str, ApiDiff] = {}
    undocumented: list[ApiBreak] = []
    documented: list[ApiBreak] = []

    for label, base_items in base_listings.items():
        head_items = head_listings[label]
        diff = diff_listings(base_items, head_items, crate_ident=crate_ident)
        diffs[label] = diff
        root_reexports = find_root_reexports(base_items, crate_ident=crate_ident)
        removed_types = removed_top_level_types(diff.removed)

        entries: list[ApiBreak] = [
            ApiBreak(kind="removed", feature_set=label, old=item) for item in diff.removed
        ] + [
            ApiBreak(kind="changed", feature_set=label, old=old, new=new)
            for old, new in diff.changed
        ]
        for entry in entries:
            if is_documented(
                entry, bullets, root_reexports, removed_types, crate_ident=crate_ident
            ):
                documented.append(entry)
            else:
                undocumented.append(entry)

    return undocumented, documented, diffs


def format_break(entry: ApiBreak) -> str:
    if entry.kind == "removed":
        return f"  [{entry.feature_set}] removed: {entry.old}"
    return f"  [{entry.feature_set}] changed:\n    - {entry.old}\n    + {entry.new}"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Compare a public workspace crate's API with the last release.")
    parser.add_argument(
        "--package",
        choices=sorted(PACKAGE_CONFIGS),
        default=PACKAGE,
        help="Workspace crate to inspect (default: ayaml).",
    )
    parser.add_argument("--base", help="Base git revision or tag (default: last released tag).")
    parser.add_argument("--head", help="Head git revision or tag (default: working tree).")
    parser.add_argument("--changelog", type=Path, help="Path to CHANGELOG.md to check.")
    parser.add_argument(
        "--no-changelog",
        action="store_true",
        help="Treat the changelog's Changed section as empty (reports every break).",
    )
    parser.add_argument("--json", action="store_true", help="Print a machine-readable JSON summary.")
    args = parser.parse_args(argv)

    if args.changelog is not None and args.no_changelog:
        print("error: --changelog and --no-changelog are mutually exclusive", file=sys.stderr)
        return 2

    cfg = PACKAGE_CONFIGS[args.package]
    tool_bin = shutil.which(TOOL_BIN)
    if tool_bin is None:
        print(
            f"error: {TOOL_BIN} not found on PATH; install it with tools/install-public-api.sh",
            file=sys.stderr,
        )
        return 2

    try:
        reported_version = run(tool_bin, "--version").strip()
        expected_version = f"{TOOL_BIN} {TOOL_VERSION}"
        if reported_version != expected_version:
            raise RuntimeError(
                f"expected `{expected_version}` on PATH, found `{reported_version}`; "
                "reinstall with tools/install-public-api.sh"
            )

        head_ref = args.head or "HEAD"
        base_rev = args.base or last_release_tag(ROOT, head_ref)
        base_sv = parse_semver_tag(base_rev)
        base_version = f"{base_sv[0]}.{base_sv[1]}.{base_sv[2]}" if base_sv else None

        WORK.mkdir(parents=True, exist_ok=True)
        base_tree = extract_revision(ROOT, base_rev)
        base_key = base_tree.name[:12]
        if args.head:
            head_tree = extract_revision(ROOT, args.head)
            head_key = head_tree.name[:12]
        else:
            head_tree = ROOT
            head_key = "working-tree"

        head_manifest = head_tree / cfg.package_dir / "Cargo.toml"
        if not head_manifest.is_file():
            raise RuntimeError(
                f"{cfg.package_dir}/Cargo.toml does not exist in {args.head or 'working tree'}"
            )
        validate_manifest_features(
            head_manifest.read_text(encoding="utf-8"),
            package_dir=cfg.package_dir,
            feature_sets=cfg.feature_sets,
            non_api_features=cfg.non_api_features,
        )
        head_version = package_version(head_tree, cfg.package_dir)

        if args.no_changelog:
            changelog_text = ""
        elif args.changelog is not None:
            changelog_text = args.changelog.read_text(encoding="utf-8")
        else:
            changelog_text = (head_tree / "CHANGELOG.md").read_text(encoding="utf-8")

        base_has_crate = (base_tree / cfg.package_dir / "Cargo.toml").is_file()
        base_listings: dict[str, set[str]] = {}
        head_listings: dict[str, set[str]] = {}
        for label, features in cfg.feature_sets:
            if base_has_crate:
                base_listings[label] = generate_listing(
                    base_tree,
                    WORK / f"target-{cfg.package}-{base_key}-{label}",
                    features,
                    tool_bin,
                    package=cfg.package,
                    crate_ident=cfg.crate_ident,
                )
            head_listings[label] = generate_listing(
                head_tree,
                WORK / f"target-{cfg.package}-{head_key}-{label}",
                features,
                tool_bin,
                package=cfg.package,
                crate_ident=cfg.crate_ident,
            )

        for hidden in cfg.hidden_features:
            hidden_listing = generate_listing(
                head_tree,
                WORK / f"target-{cfg.package}-{head_key}-{hidden}",
                (hidden,),
                tool_bin,
                package=cfg.package,
                crate_ident=cfg.crate_ident,
            )
            leaked = sorted(hidden_listing - head_listings["default"])
            dropped = sorted(head_listings["default"] - hidden_listing)
            if leaked or dropped:
                print(f"error: feature `{hidden}` changes the public API of {cfg.package}:", file=sys.stderr)
                for item in leaked:
                    print(f"  + {item}", file=sys.stderr)
                for item in dropped:
                    print(f"  - {item}", file=sys.stderr)
                return 1

        if not base_has_crate:
            if args.json:
                payload = {
                    "package": cfg.package,
                    "base": base_rev,
                    "head": args.head or "working-tree",
                    "new_crate": True,
                    "undocumented": [],
                    "documented": [],
                    "summary": {
                        label: {
                            "base_items": 0,
                            "head_items": len(head_listings[label]),
                            "removed": 0,
                            "changed": 0,
                            "added": len(head_listings[label]),
                        }
                        for label, _ in cfg.feature_sets
                    },
                }
                print(json.dumps(payload, indent=2))
            else:
                for label, _ in cfg.feature_sets:
                    print(
                        f"public-api ({cfg.package} {label}): {len(head_listings[label])} items "
                        f"(new crate since {base_rev}; baseline diff starts after first release tag containing {cfg.package_dir})"
                    )
            return 0

        undocumented, documented, diffs = check_comparison(
            base_listings,
            head_listings,
            changelog_text,
            head_version,
            base_version,
            crate_ident=cfg.crate_ident,
        )
    except Exception as error:  # noqa: BLE001
        print(f"error: {error}", file=sys.stderr)
        return 2

    if args.json:
        payload = {
            "package": cfg.package,
            "base": base_rev,
            "head": args.head or "working-tree",
            "undocumented": [b.to_dict() for b in undocumented],
            "documented": [b.to_dict() for b in documented],
            "summary": {
                label: {
                    "base_items": len(base_listings[label]),
                    "head_items": len(head_listings[label]),
                    "removed": len(diffs[label].removed),
                    "changed": len(diffs[label].changed),
                    "added": len(diffs[label].added),
                }
                for label, _ in cfg.feature_sets
            },
        }
        print(json.dumps(payload, indent=2))
    else:
        for label, _ in cfg.feature_sets:
            diff = diffs[label]
            print(
                f"public-api ({cfg.package} {label}) vs {base_rev}: "
                f"{len(head_listings[label])} items "
                f"(+{len(diff.added)} added, ~{len(diff.changed)} changed, -{len(diff.removed)} removed)"
            )
        if documented:
            print(f"\n{len(documented)} breaking/changed item(s) named in CHANGELOG.md Changed:")
            for entry in documented:
                print(format_break(entry))
        if undocumented:
            print(
                f"\n{len(undocumented)} public API break(s) not named in the active "
                "CHANGELOG.md `### Changed` section:",
                file=sys.stderr,
            )
            for entry in undocumented:
                print(format_break(entry), file=sys.stderr)

    return 1 if undocumented else 0


if __name__ == "__main__":
    raise SystemExit(main())
