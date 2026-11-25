#!/usr/bin/env python3
"""Unit tests for tools/check_docs.py (site config, benchmark JSONL, fences, links, coverage)."""

from __future__ import annotations

import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_docs

SAMPLE_SHA = "a" * 64
ALT_SHA = "b" * 64


class DocsCheck(unittest.TestCase):
    """tools/check_docs.py checks site config, benchmark data, SUMMARY.md, doctests, links, and fences."""

    def make_tree(self) -> Path:
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        root = Path(scratch.name)
        for d in ("docs", "site/scripts", "crates/ayaml-conformance/src"):
            (root / d).mkdir(parents=True)
        (root / "site/package.json").write_text(
            '{"scripts":{"sync":"x","build":"x","check:links":"x"}}\n', encoding="utf-8"
        )
        (root / "site/astro.config.mjs").write_text(
            "export default { site: 'https://asamarts.github.io', base: '/ayaml' };\n",
            encoding="utf-8",
        )
        (root / "site/scripts/sync-docs.mjs").write_text("// sync\n", encoding="utf-8")
        (root / "site/scripts/check-links.mjs").write_text("// check\n", encoding="utf-8")
        (root / "README.md").write_text("# ayaml\n\nSee [Index](docs/index.md).\n", encoding="utf-8")
        (root / "docs/SUMMARY.md").write_text(
            "# Summary\n\n[Introduction](index.md)\n- [Security](security.md)\n",
            encoding="utf-8",
        )
        (root / "docs/index.md").write_text("# Intro\n\nSee [Security](security.md).\n", encoding="utf-8")
        (root / "docs/security.md").write_text(
            "# Security\n\n```rust\nassert_eq!(1 + 1, 2);\n```\n", encoding="utf-8"
        )
        (root / "crates/ayaml-conformance/src/lib.rs").write_text(
            '#[doc = include_str!("../../../README.md")]\n'
            '#[doc = include_str!("../../../docs/index.md")]\n'
            '#[doc = include_str!("../../../docs/security.md")]\n',
            encoding="utf-8",
        )
        return root

    def add_benchmark_data(self, root: Path) -> Path:
        bench_dir = root / "site/src/data/benchmarks"
        bench_dir.mkdir(parents=True, exist_ok=True)
        speed_row = {
            "schema_version": 1,
            "scenario": "openapi",
            "operation": "parse_value",
            "implementation": "ayaml",
            "bytes": 1000,
            "median_ns": 5000,
            "throughput_mib_s": 200.0,
            "fixture_sha256": SAMPLE_SHA,
        }
        mem_row = {
            "schema_version": 1,
            "scenario": "openapi",
            "operation": "parse_value",
            "implementation": "ayaml",
            "bytes": 1000,
            "peak_bytes": 4000,
            "live_bytes": 2000,
            "allocations": 12,
            "fixture_sha256": SAMPLE_SHA,
        }
        (bench_dir / "fingerprint.json").write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "fixture_manifest_sha256": SAMPLE_SHA,
                    "speed_rows": 1,
                    "memory_rows": 1,
                }
            )
            + "\n",
            encoding="utf-8",
        )
        (bench_dir / "speed.jsonl").write_text(json.dumps(speed_row) + "\n", encoding="utf-8")
        (bench_dir / "memory.jsonl").write_text(json.dumps(mem_row) + "\n", encoding="utf-8")
        (bench_dir / "v023-speed.jsonl").write_text(json.dumps(speed_row) + "\n", encoding="utf-8")
        (bench_dir / "v023-memory.jsonl").write_text(json.dumps(mem_row) + "\n", encoding="utf-8")
        return bench_dir

    def test_valid_tree_passes(self) -> None:
        root = self.make_tree()
        self.add_benchmark_data(root)
        self.assertEqual(check_docs.problems(root), [])

    def test_unlisted_page_fails_summary_and_doctests(self) -> None:
        root = self.make_tree()
        (root / "docs/extra.md").write_text("# Extra\n", encoding="utf-8")
        found = check_docs.problems(root)
        self.assertTrue(any("not listed in docs/SUMMARY.md" in msg for msg in found), found)
        self.assertTrue(any("not included in" in msg for msg in found), found)

    def test_broken_and_escaping_links_fail(self) -> None:
        root = self.make_tree()
        (root / "docs/index.md").write_text(
            "# Intro\n\n[Missing](missing.md) and [Escape](../README.md)\n", encoding="utf-8"
        )
        found = check_docs.problems(root)
        self.assertTrue(any("missing link target 'missing.md'" in msg for msg in found), found)
        self.assertTrue(any("escapes docs/" in msg for msg in found), found)

    def test_ayaml_edit_readme_broken_link_and_fences_fail(self) -> None:
        root = self.make_tree()
        edit_dir = root / "crates/ayaml-edit"
        edit_dir.mkdir(parents=True)
        (edit_dir / "README.md").write_text(
            "# ayaml-edit\n\nSee [Missing](missing.md).\n\n```\nuntagged\n```\n",
            encoding="utf-8",
        )
        found = check_docs.problems(root)
        self.assertTrue(any("crates/ayaml-edit/README.md: missing link target 'missing.md'" in msg for msg in found), found)
        self.assertTrue(any("crates/ayaml-edit/README.md" in msg and "no language tag" in msg for msg in found), found)
        self.assertTrue(
            any("crates/ayaml-edit/README.md: not included in" in msg for msg in found),
            found,
        )

    def test_ignored_or_no_run_or_untagged_fence_fails_for_backticks_and_tildes(self) -> None:
        for fence, expected in (
            ("```rust,ignore\nlet x = 1;\n```\n", "uses 'ignore'"),
            ("```rust,no_run\nlet x = 1;\n```\n", "uses 'no_run'"),
            ("```\nplain block\n```\n", "no language tag"),
            ("~~~rust,ignore\nlet x = 1;\n~~~\n", "uses 'ignore'"),
            ("~~~rust,no_run\nlet x = 1;\n~~~\n", "uses 'no_run'"),
            ("~~~\nplain tilde block\n~~~\n", "no language tag"),
        ):
            with self.subTest(fence=fence, expected=expected):
                root = self.make_tree()
                (root / "docs/security.md").write_text(f"# Security\n\n{fence}", encoding="utf-8")
                found = check_docs.problems(root)
                self.assertTrue(any(expected in msg for msg in found), found)

    def test_missing_anchor_and_duplicate_summary_fail(self) -> None:
        root = self.make_tree()
        (root / "docs/index.md").write_text(
            "# Intro\n\nSee [Missing anchor](security.md#does-not-exist).\n", encoding="utf-8"
        )
        (root / "docs/SUMMARY.md").write_text(
            "# Summary\n\n[Introduction](index.md)\n- [Security](security.md)\n- [Again](security.md)\n",
            encoding="utf-8",
        )
        found = check_docs.problems(root)
        self.assertTrue(any("missing heading anchor #does-not-exist" in msg for msg in found), found)
        self.assertTrue(any("duplicate entry for 'security.md'" in msg for msg in found), found)

    def test_benchmark_data_checks_fingerprint_counts_fields_and_hashes(self) -> None:
        root = self.make_tree()
        bench_dir = self.add_benchmark_data(root)

        # Row-count mismatch in fingerprint.json fails.
        fp_path = bench_dir / "fingerprint.json"
        fp = json.loads(fp_path.read_text(encoding="utf-8"))
        fp["speed_rows"] = 99
        fp_path.write_text(json.dumps(fp) + "\n", encoding="utf-8")
        found = check_docs.problems(root)
        self.assertTrue(any("speed_rows=99 != speed.jsonl count 1" in msg for msg in found), found)

        # Cross-file fixture_sha256 mismatch for the same scenario fails.
        fp["speed_rows"] = 1
        fp_path.write_text(json.dumps(fp) + "\n", encoding="utf-8")
        mem_path = bench_dir / "memory.jsonl"
        mem = json.loads(mem_path.read_text(encoding="utf-8").strip())
        mem["fixture_sha256"] = ALT_SHA
        mem_path.write_text(json.dumps(mem) + "\n", encoding="utf-8")
        found = check_docs.problems(root)
        self.assertTrue(any("fixture_sha256 mismatch for scenario 'openapi'" in msg for msg in found), found)

        # Missing required JSONL field (including live_bytes) fails.
        del mem["live_bytes"]
        mem["fixture_sha256"] = SAMPLE_SHA
        mem_path.write_text(json.dumps(mem) + "\n", encoding="utf-8")
        found = check_docs.problems(root)
        self.assertTrue(any("missing field 'live_bytes'" in msg for msg in found), found)

        # v023-*.jsonl SHA-256 mismatch is caught across files.
        mem["live_bytes"] = 2000
        mem_path.write_text(json.dumps(mem) + "\n", encoding="utf-8")
        v023_speed = bench_dir / "v023-speed.jsonl"
        row = json.loads(v023_speed.read_text(encoding="utf-8").strip())
        row["fixture_sha256"] = ALT_SHA
        v023_speed.write_text(json.dumps(row) + "\n", encoding="utf-8")
        found = check_docs.problems(root)
        self.assertTrue(any("v023-speed.jsonl: fixture_sha256 mismatch" in msg for msg in found), found)

        # Empty benchmark JSONL fails both empty check and fingerprint row-count check.
        row["fixture_sha256"] = SAMPLE_SHA
        v023_speed.write_text(json.dumps(row) + "\n", encoding="utf-8")
        (bench_dir / "speed.jsonl").write_text("", encoding="utf-8")
        found = check_docs.problems(root)
        self.assertTrue(any("speed.jsonl: benchmark file contains no rows" in msg for msg in found), found)
        self.assertTrue(any("speed_rows=1 != speed.jsonl count 0" in msg for msg in found), found)

        # Non-object JSON in fingerprint.json or JSONL fails cleanly without crashing.
        (bench_dir / "speed.jsonl").write_text("[]\n", encoding="utf-8")
        found = check_docs.problems(root)
        self.assertTrue(any("speed.jsonl:1: expected JSON object" in msg for msg in found), found)
        fp_path.write_text("[]\n", encoding="utf-8")
        found = check_docs.problems(root)
        self.assertTrue(any("fingerprint.json: expected JSON object" in msg for msg in found), found)

        # Missing fingerprint.json fails when benchmarks directory exists.
        fp_path.unlink()
        found = check_docs.problems(root)
        self.assertTrue(any("missing fingerprint metadata" in msg for msg in found), found)

        # Missing benchmarks directory fails if site/src/lib/bench.ts is present.
        root2 = self.make_tree()
        (root2 / "site/src/lib").mkdir(parents=True)
        (root2 / "site/src/lib/bench.ts").write_text("export {};\n", encoding="utf-8")
        found2 = check_docs.problems(root2)
        self.assertTrue(any("missing benchmark data directory" in msg for msg in found2), found2)

    def test_em_dash_prose_fails_while_code_blocks_and_inline_code_pass(self) -> None:
        for bad in (
            "Avoid em dashes \u2014 use normal punctuation.\n",
            "Entity &mdash; rejected too.\n",
            "Numeric &#8212; rejected too.\n",
            "Leading-zero decimal &#08212; rejected too.\n",
            "Hex &#x2014; rejected too.\n",
            "Leading-zero hex &#X002014; rejected too.\n",
            "## Heading \u2014 rejected\n",
            "| Col |\n| --- |\n| Table \u2014 cell |\n",
            "```code``` \u2014 inline triple backtick at start of line\n",
            "\\`escaped backtick \u2014 is prose`\n",
            "    ```text\nNot a fence \u2014 still prose\n",
        ):
            with self.subTest(bad=bad):
                root = self.make_tree()
                (root / "docs/index.md").write_text(f"# Intro\n\n{bad}", encoding="utf-8")
                found = check_docs.problems(root)
                self.assertTrue(any("em dash" in msg and "docs/index.md:" in msg for msg in found), found)

        # README.md and crates/ayaml-edit/README.md are also checked.
        root = self.make_tree()
        (root / "README.md").write_text("# ayaml\n\nFast \u2014 safe.\n", encoding="utf-8")
        edit_dir = root / "crates/ayaml-edit"
        edit_dir.mkdir(parents=True)
        (edit_dir / "README.md").write_text("# ayaml-edit\n\nSafe \u2014 edits.\n", encoding="utf-8")
        found = check_docs.problems(root)
        self.assertTrue(any("README.md:3: em dash" in msg for msg in found), found)
        self.assertTrue(any("crates/ayaml-edit/README.md:3: em dash" in msg for msg in found), found)

        # Code fences (up to 3 spaces indent, trailing spaces on close, backticks/tildes),
        # even-backslash inline code spans, and multiline inline code spans are ignored.
        root_ok = self.make_tree()
        (root_ok / "docs/security.md").write_text(
            "# Security\n\nInline `\u2014`, `&mdash;`, and \\\\`\u2014` are fine.\n\n"
            "Multiline `code\n\u2014 still code` is fine too.\n\n"
            "   ```text\nraw \u2014 inside 3-space indented fence\n   ```   \n\n"
            "~~~text\nraw &mdash; inside tilde fence\n~~~  \n",
            encoding="utf-8",
        )
        self.assertEqual(check_docs.problems(root_ok), [])

    def test_repository_tree_passes(self) -> None:
        repo = Path(__file__).resolve().parent.parent
        self.assertEqual(check_docs.problems(repo), [])


if __name__ == "__main__":
    unittest.main()
