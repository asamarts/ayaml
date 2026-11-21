#!/usr/bin/env python3
"""Check the mdBook configuration, page coverage, doctest coverage, and relative links."""

from __future__ import annotations

from pathlib import Path
import re
import sys
import tomllib

LINK_PATTERN = re.compile(r"\[[^\]]*\]\(([^)]+)\)")
HTML_HREF_PATTERN = re.compile(r'href="([^"]+)"')
INCLUDE_DOC_PATTERN = re.compile(r'include_str!\("\.\./\.\./\.\./([^"]+)"\)')
HEADING_PATTERN = re.compile(r"^#{1,6}\s+(.+?)\s*#*\s*$", re.MULTILINE)


def strip_fenced_code_blocks(text: str) -> str:
    """Remove fenced code block bodies so link and heading checks ignore example snippets."""
    lines = text.splitlines(keepends=True)
    out: list[str] = []
    in_fence = False
    fence_char = ""
    fence_len = 0
    for line in lines:
        stripped = line.lstrip()
        if not in_fence:
            if stripped.startswith(("```", "~~~")):
                ch = stripped[0]
                count = len(stripped) - len(stripped.lstrip(ch))
                if count >= 3:
                    in_fence = True
                    fence_char = ch
                    fence_len = count
                    continue
            out.append(line)
        else:
            if stripped.startswith(fence_char * fence_len) and stripped.strip() == fence_char * len(
                stripped.strip()
            ):
                in_fence = False
    return "".join(out)


def heading_slugs(text: str) -> set[str]:
    """Compute GitHub/mdBook-style heading anchors from Markdown prose."""
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
        slug = re.sub(r"\s+", "-", slug).strip("-")
        if not slug:
            continue
        seen = counts.get(slug, 0)
        counts[slug] = seen + 1
        slugs.add(slug if seen == 0 else f"{slug}-{seen}")
    return slugs


def check_book_toml(root: Path) -> list[str]:
    errors: list[str] = []
    book_toml = root / "book.toml"
    if not book_toml.is_file():
        return [f"{book_toml}: missing book.toml"]
    try:
        config = tomllib.loads(book_toml.read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError as error:
        return [f"{book_toml}: invalid TOML: {error}"]
    book = config.get("book", {})
    if book.get("src") != "docs":
        errors.append(f"{book_toml}: book.src must be 'docs', got {book.get('src')!r}")
    build = config.get("build", {})
    if build.get("build-dir") != "target/book":
        errors.append(
            f"{book_toml}: build.build-dir must be 'target/book', got {build.get('build-dir')!r}"
        )
    if build.get("create-missing") is not False:
        errors.append(f"{book_toml}: build.create-missing must be false")
    return errors


def check_rust_fences(root: Path, path: Path, text: str) -> list[str]:
    """Ensure every code fence in book pages and README is tagged and no Rust block is skipped."""
    errors: list[str] = []
    rel = path.relative_to(root)
    in_fence = False
    for line_no, line in enumerate(text.splitlines(), 1):
        stripped = line.lstrip()
        if not in_fence:
            if stripped.startswith("```"):
                info = stripped[3:].strip()
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
                                "book examples must compile and run as doctests"
                            )
        elif stripped.startswith("```"):
            in_fence = False
    return errors


def check_links(root: Path, docs_dir: Path, files: list[Path]) -> list[str]:
    errors: list[str] = []
    docs_resolved = docs_dir.resolve()
    slug_cache: dict[Path, set[str]] = {}
    check_fences_in = {docs_resolved, (root / "README.md").resolve()}

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
                    "use a full GitHub URL for links outside the book"
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
    readme = (root / "README.md").resolve()
    if readme.is_file():
        expected_included.add(readme)

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
    errors = check_book_toml(root)
    if not docs_dir.is_dir():
        return errors + ["docs/: missing directory"]
    files = [
        path
        for path in [
            root / "README.md",
            root / "CONTRIBUTING.md",
            root / "RELEASING.md",
            root / "SECURITY.md",
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
    print(f"OK: book.toml, SUMMARY.md, book_doctests, and links verified across {doc_count} book files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
