#!/usr/bin/env python3
"""Check the Astro + Starlight site configuration, pinned benchmark JSONL artifacts,
page coverage, doctest coverage, code fences, em-dash prose hygiene, and relative links."""

from __future__ import annotations

import json
import math
from pathlib import Path
import re
import sys
from urllib.parse import urlparse

LINK_PATTERN = re.compile(r"\[[^\]]*\]\(([^)]+)\)")
HTML_HREF_PATTERN = re.compile(r'href="([^"]+)"')
INCLUDE_DOC_PATTERN = re.compile(r'include_str!\("\.\./\.\./\.\./([^"]+)"\)')
HEADING_PATTERN = re.compile(r"^#{1,6}\s+(.+?)\s*#*\s*$", re.MULTILINE)
SHA256_RE = re.compile(r"\A[0-9a-f]{64}\Z")
GIT_SHA_RE = re.compile(r"\A[0-9a-f]{40}\Z")
EM_DASH_PATTERN = re.compile(r"\u2014|&mdash;|&#0*8212;|&#x0*2014;", re.IGNORECASE)


def _leading_spaces(line: str) -> int:
    """Count leading ASCII spaces before the first non-space character."""
    return len(line) - len(line.lstrip(" "))


def _parse_opening_fence(line: str) -> tuple[str, int, str] | None:
    """Parse a CommonMark §4.5 opening code fence (0-3 spaces indent; backtick info has no backticks)."""
    if _leading_spaces(line) > 3:
        return None
    stripped = line.lstrip(" ")
    if not stripped.startswith(("```", "~~~")):
        return None
    ch = stripped[0]
    count = len(stripped) - len(stripped.lstrip(ch))
    if count < 3:
        return None
    info = stripped[count:].strip()
    if ch == "`" and "`" in info:
        return None
    return ch, count, info


def _is_closing_fence(line: str, fence_char: str, fence_len: int) -> bool:
    """Match a CommonMark §4.5 closing code fence (0-3 spaces indent, >= opening length, spaces only after)."""
    if _leading_spaces(line) > 3:
        return False
    stripped = line.lstrip(" ").rstrip(" \t\r\n")
    return len(stripped) >= fence_len and stripped == fence_char * len(stripped)


def strip_fenced_code_blocks(text: str) -> str:
    """Remove fenced code block bodies so link and heading checks ignore example snippets."""
    lines = text.splitlines(keepends=True)
    out: list[str] = []
    in_fence = False
    fence_char = ""
    fence_len = 0
    for line in lines:
        if not in_fence:
            opened = _parse_opening_fence(line)
            if opened is not None:
                fence_char, fence_len, _ = opened
                in_fence = True
                continue
            out.append(line)
        else:
            if _is_closing_fence(line, fence_char, fence_len):
                in_fence = False
    return "".join(out)


def heading_slugs(text: str) -> set[str]:
    """Compute GitHub-style heading anchors from Markdown prose."""
    prose = strip_fenced_code_blocks(text)
    counts: dict[str, int] = {}
    slugs: set[str] = set()
    for match in HEADING_PATTERN.finditer(prose):
        heading = match.group(1)
        # Strip inline markdown formatting characters while keeping code text.
        heading = re.sub(r"\[([^\]]+)\]\([^)]+\)", r"\1", heading)
        heading = heading.replace("`", "")
        raw = heading.strip().lower()
        slug = re.sub(r"[^a-z0-9 _-]", "", raw)
        slug = slug.replace(" ", "-").strip("-")
        if not slug:
            continue
        seen = counts.get(slug, 0)
        counts[slug] = seen + 1
        slugs.add(slug if seen == 0 else f"{slug}-{seen}")
    return slugs


def check_site_config(root: Path) -> list[str]:
    errors: list[str] = []
    pkg_json = root / "site/package.json"
    astro_cfg = root / "site/astro.config.mjs"
    sync_script = root / "site/scripts/sync-docs.mjs"
    link_script = root / "site/scripts/check-links.mjs"
    for required in (pkg_json, astro_cfg, sync_script, link_script):
        if not required.is_file():
            errors.append(f"{required.relative_to(root)}: missing site file")
    if errors:
        return errors
    try:
        pkg = json.loads(pkg_json.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        return [f"{pkg_json.relative_to(root)}: invalid JSON: {error}"]
    scripts = pkg.get("scripts", {})
    for script_name in ("sync", "build", "check:links"):
        if script_name not in scripts:
            errors.append(f"site/package.json: missing npm script {script_name!r}")
    cfg_text = astro_cfg.read_text(encoding="utf-8")
    if "base: '/ayaml'" not in cfg_text and 'base: "/ayaml"' not in cfg_text:
        errors.append("site/astro.config.mjs: must configure base: '/ayaml'")
    if "https://asamarts.github.io" not in cfg_text:
        errors.append("site/astro.config.mjs: must configure site: 'https://asamarts.github.io'")
    return errors


def check_benchmark_data(root: Path) -> list[str]:
    """Validate pinned benchmark JSONL artifacts and fingerprint metadata under site/src/data/benchmarks/."""
    bench_dir = root / "site/src/data/benchmarks"
    if not bench_dir.is_dir():
        if (root / "site/src/lib/bench.ts").is_file():
            return ["site/src/data/benchmarks/: missing benchmark data directory"]
        return []
    errors: list[str] = []
    fp_path = bench_dir / "fingerprint.json"
    if not fp_path.is_file():
        return ["site/src/data/benchmarks/fingerprint.json: missing fingerprint metadata"]
    try:
        fp = json.loads(fp_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        return [f"site/src/data/benchmarks/fingerprint.json: invalid JSON: {error}"]
    if not isinstance(fp, dict):
        return ["site/src/data/benchmarks/fingerprint.json: expected JSON object"]

    fp_schema = fp.get("schema_version")
    if isinstance(fp_schema, bool) or fp_schema != 1:
        errors.append("site/src/data/benchmarks/fingerprint.json: expected schema_version == 1")
    manifest_sha = fp.get("fixture_manifest_sha256", "")
    if not isinstance(manifest_sha, str) or not SHA256_RE.match(manifest_sha):
        errors.append("site/src/data/benchmarks/fingerprint.json: invalid fixture_manifest_sha256")
    for sha_field in ("public_git_sha", "baseline_v023_git_sha"):
        val = fp.get(sha_field)
        if not isinstance(val, str) or not GIT_SHA_RE.match(val):
            errors.append(f"site/src/data/benchmarks/fingerprint.json: invalid or missing {sha_field}")
    if "harness_git_sha" in fp:
        val = fp.get("harness_git_sha")
        if not isinstance(val, str) or not GIT_SHA_RE.match(val):
            errors.append("site/src/data/benchmarks/fingerprint.json: invalid harness_git_sha")
    if "harness_repo" in fp:
        repo_url = fp.get("harness_repo")
        parsed = urlparse(repo_url) if isinstance(repo_url, str) else None
        if (
            not isinstance(repo_url, str)
            or any(ch.isspace() for ch in repo_url)
            or parsed is None
            or parsed.scheme != "https"
            or not parsed.netloc
        ):
            errors.append("site/src/data/benchmarks/fingerprint.json: invalid harness_repo URL")
    if "criterion" in fp:
        crit = fp.get("criterion")
        if not isinstance(crit, dict):
            errors.append("site/src/data/benchmarks/fingerprint.json: criterion must be a JSON object")
        else:
            for k in ("warm_up_ms", "measurement_ms", "sample_size", "rounds"):
                v = crit.get(k)
                if isinstance(v, bool) or not isinstance(v, int) or v <= 0:
                    errors.append(
                        f"site/src/data/benchmarks/fingerprint.json: criterion.{k} must be a positive integer"
                    )

    def _is_nonneg_num(val: object) -> bool:
        return not isinstance(val, bool) and isinstance(val, (int, float)) and math.isfinite(val) and val >= 0

    def _is_nonneg_int(val: object) -> bool:
        return not isinstance(val, bool) and isinstance(val, int) and val >= 0

    def load_jsonl(
        name: str,
        required_keys: tuple[str, ...],
        *,
        int_keys: tuple[str, ...] = (),
        pos_int_keys: tuple[str, ...] = (),
        num_keys: tuple[str, ...] = (),
    ) -> tuple[bool, list[dict]]:
        path = bench_dir / name
        if not path.is_file():
            errors.append(f"site/src/data/benchmarks/{name}: missing benchmark file")
            return False, []
        rows: list[dict] = []
        for idx, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            line = raw.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError as error:
                errors.append(f"site/src/data/benchmarks/{name}:{idx}: invalid JSON: {error}")
                continue
            if not isinstance(row, dict):
                errors.append(f"site/src/data/benchmarks/{name}:{idx}: expected JSON object")
                continue
            row_schema = row.get("schema_version")
            if isinstance(row_schema, bool) or row_schema != 1:
                errors.append(f"site/src/data/benchmarks/{name}:{idx}: expected schema_version == 1")
            for key in required_keys:
                if key not in row:
                    errors.append(f"site/src/data/benchmarks/{name}:{idx}: missing field {key!r}")
            for key in int_keys:
                if key in row and not _is_nonneg_int(row[key]):
                    errors.append(
                        f"site/src/data/benchmarks/{name}:{idx}: field {key!r} must be a non-negative integer"
                    )
            for key in pos_int_keys:
                val = row.get(key)
                if key in row and (not _is_nonneg_int(val) or val <= 0):
                    errors.append(
                        f"site/src/data/benchmarks/{name}:{idx}: field {key!r} must be a positive integer"
                    )
            for key in num_keys:
                if key in row and not _is_nonneg_num(row[key]):
                    errors.append(
                        f"site/src/data/benchmarks/{name}:{idx}: field {key!r} must be a finite non-negative number"
                    )
            sha = row.get("fixture_sha256", "")
            if not isinstance(sha, str) or not SHA256_RE.match(sha):
                errors.append(f"site/src/data/benchmarks/{name}:{idx}: invalid fixture_sha256")
            rows.append(row)
        if not rows:
            errors.append(f"site/src/data/benchmarks/{name}: benchmark file contains no rows")
        return True, rows

    speed_keys = (
        "scenario",
        "operation",
        "implementation",
        "bytes",
        "median_ns",
        "throughput_mib_s",
        "rounds",
    )
    mem_keys = (
        "scenario",
        "operation",
        "implementation",
        "bytes",
        "peak_bytes",
        "live_bytes",
        "allocations",
    )
    has_speed, speed_rows = load_jsonl(
        "speed.jsonl",
        speed_keys,
        int_keys=("bytes",),
        pos_int_keys=("rounds",),
        num_keys=("median_ns", "throughput_mib_s"),
    )
    has_mem, mem_rows = load_jsonl(
        "memory.jsonl",
        mem_keys,
        int_keys=("bytes", "peak_bytes", "live_bytes", "allocations"),
    )
    _, v023_speed_rows = load_jsonl(
        "v023-speed.jsonl",
        speed_keys,
        int_keys=("bytes",),
        pos_int_keys=("rounds",),
        num_keys=("median_ns", "throughput_mib_s"),
    )
    _, v023_mem_rows = load_jsonl(
        "v023-memory.jsonl",
        mem_keys,
        int_keys=("bytes", "peak_bytes", "live_bytes", "allocations"),
    )

    speed_count = fp.get("speed_rows")
    if has_speed and (isinstance(speed_count, bool) or speed_count != len(speed_rows)):
        errors.append(
            f"site/src/data/benchmarks/fingerprint.json: speed_rows={speed_count} "
            f"!= speed.jsonl count {len(speed_rows)}"
        )
    mem_count = fp.get("memory_rows")
    if has_mem and (isinstance(mem_count, bool) or mem_count != len(mem_rows)):
        errors.append(
            f"site/src/data/benchmarks/fingerprint.json: memory_rows={mem_count} "
            f"!= memory.jsonl count {len(mem_rows)}"
        )

    scenario_hashes: dict[str, str] = {}
    for label, rows in (
        ("speed.jsonl", speed_rows),
        ("memory.jsonl", mem_rows),
        ("v023-speed.jsonl", v023_speed_rows),
        ("v023-memory.jsonl", v023_mem_rows),
    ):
        for row in rows:
            sc = row.get("scenario")
            sha = row.get("fixture_sha256")
            if isinstance(sc, str) and isinstance(sha, str):
                prev = scenario_hashes.setdefault(sc, sha)
                if prev != sha:
                    errors.append(
                        f"site/src/data/benchmarks/{label}: fixture_sha256 mismatch for scenario {sc!r}"
                    )
                    break
    return errors


def check_rust_fences(root: Path, path: Path, text: str) -> list[str]:
    """Ensure every code fence (backtick or tilde) in docs pages and README is tagged and no Rust block is skipped."""
    errors: list[str] = []
    rel = path.relative_to(root)
    in_fence = False
    fence_char = ""
    fence_len = 0
    for line_no, line in enumerate(text.splitlines(), 1):
        if not in_fence:
            opened = _parse_opening_fence(line)
            if opened is not None:
                fence_char, fence_len, info = opened
                in_fence = True
                if not info:
                    errors.append(
                        f"{rel}:{line_no}: fenced code block has no language tag "
                        "(rustdoc treats untagged fences as Rust)"
                    )
                    continue
                tokens = [tok.strip().lower() for tok in re.split(r"[,\s]+", info) if tok.strip()]
                if "rust" in tokens:
                    for forbidden in ("ignore", "no_run"):
                        if forbidden in tokens:
                            errors.append(
                                f"{rel}:{line_no}: Rust example uses '{forbidden}' ({info!r}); "
                                "documentation examples must compile and run as doctests"
                            )
        else:
            if _is_closing_fence(line, fence_char, fence_len):
                in_fence = False
    return errors


def strip_inline_code_spans(text: str) -> str:
    """Remove equal-length backtick inline code spans across a paragraph while preserving newlines."""
    out: list[str] = []
    i = 0
    n = len(text)
    while i < n:
        if text[i] == "`":
            # Backslash-escaped backtick (odd number of preceding backslashes) is literal prose.
            bs = 0
            while i - 1 - bs >= 0 and text[i - 1 - bs] == "\\":
                bs += 1
            if bs % 2 == 1:
                out.append("`")
                i += 1
                continue
            ticks = 0
            while i + ticks < n and text[i + ticks] == "`":
                ticks += 1
            j = i + ticks
            closed = -1
            while j < n:
                if text[j] == "`":
                    run = 0
                    while j + run < n and text[j + run] == "`":
                        run += 1
                    if run == ticks:
                        closed = j + run
                        break
                    j += run
                else:
                    j += 1
            if closed != -1:
                # Keep newline count exact so callers keep accurate 1-based line numbers.
                out.append("\n" * text[i:closed].count("\n"))
                i = closed
                continue
            out.append(text[i : i + ticks])
            i += ticks
            continue
        out.append(text[i])
        i += 1
    return "".join(out)


def check_no_em_dashes(root: Path, path: Path, text: str) -> list[str]:
    """Reject em dashes and HTML em-dash entities in Markdown prose outside code blocks/spans."""
    errors: list[str] = []
    rel = path.relative_to(root)
    in_fence = False
    fence_char = ""
    fence_len = 0
    # Collect prose paragraphs separated by blank lines or code fences so multiline inline code
    # spans stay balanced without leaking across paragraph boundaries, while keeping exact line numbers.
    para_lines: list[tuple[int, str]] = []

    def flush_paragraph() -> None:
        if not para_lines:
            return
        start_line = para_lines[0][0]
        joined = "\n".join(line for _, line in para_lines)
        stripped_para = strip_inline_code_spans(joined)
        for offset, prose_line in enumerate(stripped_para.split("\n")):
            match = EM_DASH_PATTERN.search(prose_line)
            if match:
                errors.append(
                    f"{rel}:{start_line + offset}: em dash ({match.group(0)!r}) is disallowed in docs prose; "
                    "use commas, colons, parentheses, or separate sentences"
                )
        para_lines.clear()

    for line_no, line in enumerate(text.splitlines(), 1):
        if not in_fence:
            opened = _parse_opening_fence(line)
            if opened is not None:
                flush_paragraph()
                fence_char, fence_len, _ = opened
                in_fence = True
                continue
            if not line.strip():
                flush_paragraph()
                continue
            para_lines.append((line_no, line))
        else:
            if _is_closing_fence(line, fence_char, fence_len):
                in_fence = False
    flush_paragraph()
    return errors


def check_links(root: Path, docs_dir: Path, files: list[Path]) -> list[str]:
    errors: list[str] = []
    docs_resolved = docs_dir.resolve()
    slug_cache: dict[Path, set[str]] = {}
    check_fences_in = {
        docs_resolved,
        (root / "README.md").resolve(),
        (root / "crates/ayaml-edit/README.md").resolve(),
    }

    def slugs_for(target_path: Path) -> set[str]:
        resolved = target_path.resolve()
        if resolved not in slug_cache:
            slug_cache[resolved] = heading_slugs(resolved.read_text(encoding="utf-8"))
        return slug_cache[resolved]

    for path in files:
        text = path.read_text(encoding="utf-8")
        resolved_path = path.resolve()
        in_docs = docs_resolved in resolved_path.parents
        if in_docs or resolved_path in check_fences_in:
            errors.extend(check_rust_fences(root, path, text))
            errors.extend(check_no_em_dashes(root, path, text))
        prose = strip_fenced_code_blocks(text)
        targets = [match.group(1) for match in LINK_PATTERN.finditer(prose)]
        if in_docs:
            targets += [
                re.sub(r"\.html(?=$|#)", ".md", match.group(1))
                for match in HTML_HREF_PATTERN.finditer(prose)
            ]
        for raw_target in targets:
            target = raw_target.strip()
            if not target or target.startswith(("http://", "https://", "mailto:")):
                continue
            if "#" in target:
                file_part, fragment = target.split("#", 1)
                file_part = file_part.strip()
                fragment = fragment.strip()
            else:
                file_part, fragment = target, ""
            resolved = resolved_path if not file_part else (path.parent / file_part).resolve()
            if not resolved.exists():
                errors.append(f"{path.relative_to(root)}: missing link target {raw_target!r}")
                continue
            if (
                file_part
                and in_docs
                and docs_resolved not in resolved.parents
                and resolved != docs_resolved
            ):
                errors.append(
                    f"{path.relative_to(root)}: relative link {raw_target!r} escapes docs/; "
                    "use a full GitHub URL for links outside docs/"
                )
                continue
            if fragment and resolved.is_file() and resolved.suffix == ".md":
                if fragment.lower() not in slugs_for(resolved):
                    errors.append(
                        f"{path.relative_to(root)}: missing heading anchor #{fragment} "
                        f"in {resolved.relative_to(root.resolve())}"
                    )
    return errors


def check_summary_and_doctests(root: Path, docs_dir: Path) -> list[str]:
    errors: list[str] = []
    summary_path = docs_dir / "SUMMARY.md"
    if not summary_path.is_file():
        return [f"{summary_path.relative_to(root)}: missing SUMMARY.md"]

    docs_resolved = docs_dir.resolve()
    pages = {
        page.resolve()
        for page in sorted(docs_dir.rglob("*.md"))
        if page.name != "SUMMARY.md"
    }

    summary_text = summary_path.read_text(encoding="utf-8")
    listed: set[Path] = set()
    for target in re.findall(r"\]\(([^)#]+\.md)\)", summary_text):
        resolved = (docs_dir / target).resolve()
        if not resolved.is_file():
            errors.append(f"docs/SUMMARY.md: listed page does not exist: {target!r}")
        elif docs_resolved not in resolved.parents:
            errors.append(f"docs/SUMMARY.md: listed page escapes docs/: {target!r}")
        elif resolved in listed:
            errors.append(f"docs/SUMMARY.md: duplicate entry for {target!r}")
        else:
            listed.add(resolved)

    for page in sorted(pages - listed):
        errors.append(f"{page.relative_to(root.resolve())}: not listed in docs/SUMMARY.md")

    doctest_file = root / "crates/ayaml-conformance/src/lib.rs"
    if not doctest_file.is_file():
        errors.append(f"{doctest_file.relative_to(root)}: missing book_doctests file")
        return errors

    doctest_text = doctest_file.read_text(encoding="utf-8")
    included = {
        (root / match.group(1)).resolve()
        for match in INCLUDE_DOC_PATTERN.finditer(doctest_text)
    }

    expected_included = set(pages)
    for extra_readme in ("README.md", "crates/ayaml-edit/README.md"):
        readme_path = (root / extra_readme).resolve()
        if readme_path.is_file():
            expected_included.add(readme_path)

    for page in sorted(expected_included - included):
        errors.append(
            f"{page.relative_to(root.resolve())}: not included in "
            "crates/ayaml-conformance/src/lib.rs book_doctests"
        )
    for extra in sorted(included - expected_included):
        rel = extra.relative_to(root.resolve()) if root.resolve() in extra.parents else extra
        errors.append(f"crates/ayaml-conformance/src/lib.rs: unexpected book_doctests include {rel}")

    return errors


def problems(root: Path) -> list[str]:
    docs_dir = root / "docs"
    errors = check_site_config(root)
    errors.extend(check_benchmark_data(root))
    if not docs_dir.is_dir():
        return errors + ["docs/: missing directory"]
    files = [
        path
        for path in [
            root / "README.md",
            root / "CONTRIBUTING.md",
            root / "RELEASING.md",
            root / "SECURITY.md",
            root / "crates/ayaml-edit/README.md",
            *sorted(docs_dir.rglob("*.md")),
        ]
        if path.is_file()
    ]
    errors.extend(check_links(root, docs_dir, files))
    errors.extend(check_summary_and_doctests(root, docs_dir))
    return errors


def main(argv: list[str] | None = None) -> int:
    root = Path(argv[0]).resolve() if argv else Path(__file__).resolve().parent.parent
    found = problems(root)
    if found:
        for problem in found:
            print(f"FAIL: {problem}", file=sys.stderr)
        return 1
    doc_count = len(list((root / "docs").rglob("*.md")))
    print(f"OK: site config, benchmark artifacts, SUMMARY.md, book_doctests, and links verified across {doc_count} docs files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
