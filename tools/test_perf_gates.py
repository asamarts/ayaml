#!/usr/bin/env python3
"""Unit tests for performance gates: check_instructions.py, check_alloc_baseline.py, and check_alloc_corpus.sh."""

from __future__ import annotations

import contextlib
import io
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_alloc_baseline
import check_instructions


class InstructionsBase(unittest.TestCase):
    def test_a_missing_or_zero_base_falls_back_to_the_parent(self) -> None:
        for given in ["", "0" * 40, "  "]:
            self.assertEqual(check_instructions.base_revision(given), "HEAD~1")
        self.assertEqual(check_instructions.base_revision("abc123"), "abc123")

    def test_the_probe_in_the_tree_has_every_operation(self) -> None:
        source = (check_instructions.ROOT / check_instructions.PROBE).read_text(encoding="utf-8")
        self.assertEqual(check_instructions.known_operations(source), check_instructions.OPERATIONS)
        self.assertIn("walk", check_instructions.OPERATIONS)

    def test_the_nested_key_is_a_key_at_every_level(self) -> None:
        self.assertEqual(check_instructions.nested_keys(0, 2), "{k0: 0, k1: 1}\n")
        self.assertEqual(check_instructions.nested_keys(2, 1), "{? {? {k0: 0}: null}: null}\n")
        self.assertIn("nested-keys", check_instructions.workloads())

    def test_an_operation_that_a_probe_lacks_is_not_one_it_knows(self) -> None:
        older = '"events" => {}\n"value" => {}\n"typed" => {}\n"emit" => {}\n// "walk" is to come\n'
        self.assertEqual(
            check_instructions.known_operations(older), ("events", "value", "typed", "emit")
        )
        self.assertEqual(check_instructions.known_operations(""), ())

    def test_callgrind_command_pins_cache_geometry(self) -> None:
        cmd = check_instructions.callgrind_command(
            Path("/bin/probe"), "value", Path("/in.yml"), 4, Path("/cg.out")
        )
        self.assertIn(f"--I1={check_instructions.CALLGRIND_I1}", cmd)
        self.assertIn(f"--LL={check_instructions.CALLGRIND_LL}", cmd)

    def test_callgrind_parsing_cycles_steady_state_and_thresholds(self) -> None:
        k1_txt = (
            "events: Ir Dr Dw I1mr D1mr D1mw ILmr DLmr DLmw Bc Bcm Bi Bim\n"
            "summary: 1000 200 100 10 5 2 1 1 0 50 4 10 1\n"
        )
        k2_txt = (
            "events: Ir Dr Dw I1mr D1mr D1mw ILmr DLmr DLmw Bc Bcm Bi Bim\n"
            "summary: 5000 1000 500 50 25 10 5 5 0 250 12 50 5\n"
        )
        k1 = check_instructions.parse_callgrind_out(k1_txt)
        k2 = check_instructions.parse_callgrind_out(k2_txt)
        delta = check_instructions.steady_state_delta(k1, k2, 4)
        self.assertEqual(delta["Ir"], 1000)
        # 1000 + 10*(10+5+2) + 100*(1+1+0) + 20*(2+1) = 1430
        self.assertEqual(delta["EstimatedCycles"], 1430)

        # Raw miss sums round half-up before dividing so tiny walk ops avoid floor quantization cliffs.
        r1 = check_instructions.parse_callgrind_out(
            "events: Ir I1mr D1mr D1mw ILmr DLmr DLmw Bcm Bim\nsummary: 1000 0 0 0 0 0 0 0 0\n"
        )
        r2 = check_instructions.parse_callgrind_out(
            "events: Ir I1mr D1mr D1mw ILmr DLmr DLmw Bcm Bim\nsummary: 5000 2 2 2 1 1 1 0 0\n"
        )
        rounded = check_instructions.steady_state_delta(r1, r2, 4)
        self.assertEqual(rounded["L1Misses"], 2)
        self.assertEqual(rounded["LLMisses"], 1)
        self.assertEqual(rounded["EstimatedCycles"], 1090)

        with self.assertRaises(ValueError):
            check_instructions.parse_callgrind_out("events: Dr\nsummary: 10\n")

        cells = [("value", "frontmatter-tiny"), ("borrowed", "frontmatter-tiny")]
        manifest = {"frontmatter-tiny": {"bytes": 340, "sha256": "a" * 64}}
        base = [{"Ir": 10000, "EstimatedCycles": 12000}, None]
        # +1.5% Ir and +4.0% cycles passes (+2% / +5% limits); new op passes.
        ok_head = [
            {"Ir": 10150, "EstimatedCycles": 12480},
            {"Ir": 5000, "EstimatedCycles": 6000},
        ]
        rows, failed = check_instructions.evaluate_cells(cells, base, ok_head, {}, manifest)
        self.assertEqual(failed, [])
        self.assertEqual(rows[0]["status"], "ok")
        self.assertEqual(rows[1]["status"], "new")

        # +2.5% Ir or +6.0% cycles fails unless accepted in instruction-rises.tsv.
        bad_head = [
            {"Ir": 10250, "EstimatedCycles": 12720},
            {"Ir": 5000, "EstimatedCycles": 6000},
        ]
        _, failed = check_instructions.evaluate_cells(cells, base, bad_head, {}, manifest)
        self.assertEqual(len(failed), 1)
        self.assertIn("Ir +2.50%", failed[0])
        self.assertIn("EstCycles +6.00%", failed[0])

        # EstCycles-only override MUST NOT relax Ir!
        _, still_ir_failed = check_instructions.evaluate_cells(
            cells, base, bad_head, {("value", "frontmatter-tiny"): (None, 7.0)}, manifest
        )
        self.assertEqual(len(still_ir_failed), 1)
        self.assertIn("Ir +2.50%", still_ir_failed[0])
        self.assertNotIn("EstCycles", still_ir_failed[0])

        # Ir-only override MUST NOT relax EstCycles!
        _, still_cyc_failed = check_instructions.evaluate_cells(
            cells, base, bad_head, {("value", "frontmatter-tiny"): (3.0, None)}, manifest
        )
        self.assertEqual(len(still_cyc_failed), 1)
        self.assertIn("EstCycles +6.00%", still_cyc_failed[0])
        self.assertNotIn("Ir +2.50%", still_cyc_failed[0])

        # Explicit separate overrides on both metrics allow the cell.
        _, allowed = check_instructions.evaluate_cells(
            cells, base, bad_head, {("value", "frontmatter-tiny"): (3.0, 7.0)}, manifest
        )
        self.assertEqual(allowed, [])

    def test_parse_rise_line_and_accepted_rises_enforce_five_column_split_format(self) -> None:
        self.assertEqual(
            check_instructions.parse_rise_line("borrowed\tfrontmatter-tiny\t-\t6\tlayout shift"),
            ("borrowed", "frontmatter-tiny", None, 6.0, "layout shift"),
        )
        self.assertEqual(
            check_instructions.parse_rise_line("typed\taliases-1000\t7\t7.5\tstreaming"),
            ("typed", "aliases-1000", 7.0, 7.5, "streaming"),
        )
        for bad in (
            "borrowed\tfrontmatter-tiny\t6\told four-column format",
            "bogus_op\tfrontmatter-tiny\t-\t6\tunknown op",
            "borrowed\tfrontmatter-tiny\t-\t-\tboth unset",
            "borrowed\tfrontmatter-tiny\t0\t6\tzero ir",
            "borrowed\tfrontmatter-tiny\t-1\t6\tnegative ir",
            "borrowed\tfrontmatter-tiny\tNaN\t6\tnan ir",
            "borrowed\tfrontmatter-tiny\tinf\t6\tinf ir",
            "borrowed\tfrontmatter-tiny\t-\t6\t   ",
        ):
            with self.subTest(bad=bad):
                with self.assertRaises(ValueError):
                    check_instructions.parse_rise_line(bad)

        # Repository's own migrated tools/instruction-rises.tsv must not activate stale rows vs HEAD.
        self.assertEqual(check_instructions.accepted_rises("HEAD"), {})

        # Unit test accepted_rises legacy migration, new rows, and multi-row merge.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "tools").mkdir()
            (root / "tools" / "instruction-rises.tsv").write_text(
                "# comment\n"
                "typed\taliases-1000\t7\t7\told legacy reason\n"
                "borrowed\tfrontmatter-tiny\t-\t6\tnew layout note\n"
                "borrowed\tfrontmatter-tiny\t3.5\t-\tnew ir note\n",
                encoding="utf-8",
            )
            fake_base = subprocess.CompletedProcess(
                args=[],
                returncode=0,
                stdout="typed\taliases-1000\t7\told legacy reason\n",
                stderr="",
            )
            with (
                mock.patch.object(check_instructions, "ROOT", root),
                mock.patch.object(subprocess, "run", return_value=fake_base),
            ):
                got = check_instructions.accepted_rises("base")
            self.assertEqual(got, {("borrowed", "frontmatter-tiny"): (3.5, 6.0)})

    def test_pinned_baseline_snapshot_catches_cumulative_creep_missing_rows_and_sha_drift(self) -> None:
        sha = "a" * 64
        snap_text = (
            '{"schema_version":1,"operation":"value","workload":"frontmatter-tiny",'
            f'"bytes":323,"fixture_sha256":"{sha}","ir":10000,"estimated_cycles":12000,'
            '"l1_misses":10,"ll_misses":1,"branch_mispredicts":5}\n'
        )
        snap = check_instructions.parse_baseline_snapshot(snap_text, "test")
        ok_rows = [
            {
                "operation": "value",
                "workload": "frontmatter-tiny",
                "fixture_sha256": sha,
                "head_ir": 10800,
                "head_estimated_cycles": 13500,
                "status": "ok",
            }
        ]
        self.assertEqual(check_instructions.evaluate_cumulative_drift(ok_rows, snap), [])
        self.assertEqual(ok_rows[0]["status"], "ok")

        creep_rows = [
            {
                "operation": "value",
                "workload": "frontmatter-tiny",
                "fixture_sha256": sha,
                "head_ir": 11200,  # +12% > +10%
                "head_estimated_cycles": 14000,  # +16.67% > +15%
                "status": "ok",
            }
        ]
        errs = check_instructions.evaluate_cumulative_drift(creep_rows, snap)
        self.assertEqual(len(errs), 1)
        self.assertIn("cumulative Ir +12.00%", errs[0])
        self.assertIn("cumulative EstCycles +16.67%", errs[0])
        self.assertEqual(creep_rows[0]["status"], "fail")

        drifted_rows = [{**ok_rows[0], "fixture_sha256": "b" * 64, "status": "ok"}]
        drift_errs = check_instructions.evaluate_cumulative_drift(drifted_rows, snap)
        self.assertEqual(len(drift_errs), 1)
        self.assertIn("fixture_sha256 drifted", drift_errs[0])
        self.assertEqual(drifted_rows[0]["status"], "fail")

        # Missing snapshot cell is flagged rather than silently skipped.
        missing_errs = check_instructions.evaluate_cumulative_drift([], snap)
        self.assertEqual(len(missing_errs), 1)
        self.assertIn("missing from current run", missing_errs[0])

        # Boolean schema_version=true and missing miss counters are rejected.
        for bad in (
            '{"schema_version":2}\n',
            snap_text.replace('"schema_version":1', '"schema_version":true'),
            snap_text.replace(',"branch_mispredicts":5', ""),
        ):
            with self.subTest(bad=bad):
                with self.assertRaises(ValueError):
                    check_instructions.parse_baseline_snapshot(bad, "bad")

    def test_main_rejects_bad_cli_flags_and_missing_explicit_snapshot(self) -> None:
        with mock.patch("builtins.print"):
            self.assertEqual(check_instructions.main(["--cumulative-ir-limit", "NaN", "HEAD"]), 2)
            self.assertEqual(check_instructions.main(["--cumulative-cycles-limit", "-1", "HEAD"]), 2)
            self.assertEqual(check_instructions.main(["--record-baseline", "HEAD"]), 2)
            self.assertEqual(
                check_instructions.main(["--record-baseline", "--no-baseline-snapshot"]), 2
            )
            self.assertEqual(
                check_instructions.main(
                    ["--baseline-snapshot", "/nonexistent/baseline.jsonl", "HEAD"]
                ),
                2,
            )
            self.assertEqual(
                check_instructions.main(
                    ["--no-baseline-snapshot", "--baseline-snapshot", "foo.jsonl", "HEAD"]
                ),
                2,
            )

    def test_scheduled_cells_cover_post_023_apis_and_depth_64(self) -> None:
        available = {
            "frontmatter-tiny",
            "boundary-4kb-below",
            "boundary-4kb-above",
            "ci-workflow",
            "container-compose",
            "escape-routes",
            "unicode-multibyte",
            "depth-64",
            "aliases-1000",
            "nested-keys",
            "typed-mismatch-late",
        }
        cells = set(check_instructions.scheduled_cells(available))
        for expected in (
            ("value", "depth-64"),
            ("edit", "frontmatter-tiny"),
            ("frontmatter", "frontmatter-tiny"),
            ("merge_keys", "container-compose"),
            ("spanned_typed", "frontmatter-tiny"),
            ("doc_de", "frontmatter-tiny"),
            ("from_reader", "frontmatter-tiny"),
            ("compat", "container-compose"),
            ("compat", "depth-64"),
        ):
            self.assertIn(expected, cells)

    def test_build_probe_remaps_source_paths_for_deterministic_binaries(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            old_tree = root / "old-tree"
            work = root / "work"
            (old_tree / "tools").mkdir(parents=True)
            (old_tree / "crates" / "ayaml").mkdir(parents=True)
            (old_tree / "crates" / "ayaml" / "Cargo.toml").write_text(
                '[package]\nname = "ayaml"\n', encoding="utf-8"
            )
            (old_tree / "tools" / "instruction-probe.rs").write_text(
                "fn main() {}\n", encoding="utf-8"
            )
            (old_tree / "Cargo.lock").write_text("# lock\n", encoding="utf-8")
            captured: list[dict[str, str]] = []

            def fake_run(*_cmd: object, env: dict[str, str] | None = None, **_kw: object) -> str:
                captured.append(env or {})
                out = work / "target" / "release" / "ayaml-instruction-probe"
                out.parent.mkdir(parents=True, exist_ok=True)
                out.write_bytes(b"bin")
                return ""

            with (
                mock.patch.object(check_instructions, "WORK", work),
                mock.patch.object(check_instructions, "run", side_effect=fake_run),
            ):
                check_instructions.build_probe("base", old_tree)

            self.assertEqual(len(captured), 1)
            rustflags = captured[0].get("RUSTFLAGS", "")
            self.assertIn(f"--remap-path-prefix={old_tree.resolve()}=/ayaml", rustflags)
            self.assertIn(f"--remap-path-prefix={(work / 'probe-base').resolve()}=/probe", rustflags)
            old_manifest = (work / "probe-base" / "Cargo.toml").read_text(encoding="utf-8")
            self.assertNotIn("libyaml-compat", old_manifest)
            self.assertNotIn("ayaml-edit", old_manifest)

            # Newer trees with `libyaml-compat` and `crates/ayaml-edit` wire both into the probe.
            new_tree = root / "new-tree"
            (new_tree / "tools").mkdir(parents=True)
            (new_tree / "crates" / "ayaml").mkdir(parents=True)
            (new_tree / "crates" / "ayaml-edit").mkdir(parents=True)
            (new_tree / "crates" / "ayaml" / "Cargo.toml").write_text(
                '[package]\nname = "ayaml"\n[features]\nlibyaml-compat = []\n',
                encoding="utf-8",
            )
            (new_tree / "tools" / "instruction-probe.rs").write_text(
                "use ayaml_edit::replace_scalar;\nfn main() {}\n", encoding="utf-8"
            )
            (new_tree / "Cargo.lock").write_text("# lock\n", encoding="utf-8")
            with (
                mock.patch.object(check_instructions, "WORK", work),
                mock.patch.object(check_instructions, "run", side_effect=fake_run),
            ):
                check_instructions.build_probe("head", new_tree)
            new_manifest = (work / "probe-head" / "Cargo.toml").read_text(encoding="utf-8")
            self.assertIn('features = ["libyaml-compat"]', new_manifest)
            self.assertIn("ayaml-edit =", new_manifest)

    def test_main_validates_revs_rises_and_snapshot_upfront_with_exit_2(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bad_snap = root / "bad.jsonl"
            bad_snap.write_text("{not-json\n", encoding="utf-8")
            stderr = io.StringIO()
            with (
                mock.patch.object(check_instructions.shutil, "which", return_value="/usr/bin/valgrind"),
                mock.patch.object(
                    check_instructions, "build_probe", side_effect=AssertionError("must fail before build")
                ),
                contextlib.redirect_stderr(stderr),
            ):
                self.assertEqual(
                    check_instructions.main(["--baseline-snapshot", str(bad_snap), "HEAD"]), 2
                )
                with self.assertRaises(SystemExit) as cm:
                    check_instructions.main(["--no-baseline-snapshot", "--bad-ref"])
                self.assertEqual(cm.exception.code, 2)
                self.assertEqual(
                    check_instructions.main(["--no-baseline-snapshot", "--", "--bad-ref"]), 2
                )
                self.assertEqual(
                    check_instructions.main(["--no-baseline-snapshot", "nonexistent-ref-xyz"]), 2
                )
                self.assertEqual(
                    check_instructions.main(
                        ["--no-baseline-snapshot", "--anchor-rev", "nonexistent-anchor-xyz", "HEAD"]
                    ),
                    2,
                )
            with mock.patch.object(
                check_instructions, "accepted_rises", side_effect=ValueError("bad rises row")
            ), mock.patch.object(
                check_instructions.shutil, "which", return_value="/usr/bin/valgrind"
            ), mock.patch.object(
                check_instructions, "build_probe", side_effect=AssertionError("must fail before build")
            ), contextlib.redirect_stderr(stderr):
                self.assertEqual(check_instructions.main(["--no-baseline-snapshot", "HEAD"]), 2)
            with self.assertRaises(ValueError):
                check_instructions.resolve_git_rev("-bad", "base")


class AllocationGate(unittest.TestCase):
    """A budget file with no row would pass by checking nothing, so the gate refuses it."""

    VALUE = (
        "# path\tbudgets\nv1\t11\t12\t13\t14\t15\t16\t17\t18\t19\n"
        "@v2\t21\t22\t23\t24\t25\t26\t27\t28\t29\n"
    )
    TYPED = "t1\t31\t32\t33\t34\t35\t36\t37\t38\t39"
    JSON = "# path\tjson_peak\tjson_allocs\tjson_total\nj1\t41\t42\t43\n\n@j2\t51\t52\t53"
    CARGO = "run --quiet --release --locked --manifest-path fuzz/Cargo.toml --bin memcount"

    def gate(
        self, value: str, typed: str, json: str, failing: str = ""
    ) -> tuple[subprocess.CompletedProcess[str], list[str]]:
        script = Path(__file__).resolve().parent / "check_alloc_corpus.sh"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths = []
            for name, text in (("value.tsv", value), ("typed.tsv", typed), ("json.tsv", json)):
                paths.append(root / name)
                paths[-1].write_text(text, encoding="utf-8")
            calls = root / "calls"
            calls.write_text("", encoding="utf-8")
            stub = root / "bin" / "cargo"
            stub.parent.mkdir()
            stub.write_text(
                "#!/usr/bin/env bash\n"
                'mode=value; [[ -n "${AYAML_TYPED:-}" ]] && mode=typed; [[ -n "${AYAML_JSON:-}" ]] && mode=json\n'
                "build=()\n"
                'while [[ $# -gt 0 && "$1" != "--" ]]; do build+=("$1"); shift; done\n'
                "shift\n"
                f'echo "$mode [${{build[*]}}] $*" >>{shlex.quote(str(calls))}\n'
                f'[[ "$mode" != {shlex.quote(failing)} ]]\n',
                encoding="utf-8",
            )
            stub.chmod(0o755)
            environment = dict(os.environ, PATH=f"{stub.parent}{os.pathsep}{os.environ['PATH']}")
            environment.pop("AYAML_TYPED", None)
            environment.pop("AYAML_JSON", None)
            completed = subprocess.run(
                [script, *paths], capture_output=True, text=True, timeout=60, env=environment
            )
            return completed, calls.read_text(encoding="utf-8").splitlines()

    def test_every_row_of_every_file_is_checked(self) -> None:
        completed, calls = self.gate(self.VALUE, self.TYPED, self.JSON)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(
            calls,
            [
                f"value [{self.CARGO}] v1 11 12 13 14 15 16 17 18 19",
                f"value [{self.CARGO}] @v2 21 22 23 24 25 26 27 28 29",
                f"typed [{self.CARGO}] t1 31 32 33 34 35 36 37 38 39",
                f"json [{self.CARGO}] j1 41 42 43",
                f"json [{self.CARGO}] @j2 51 52 53",
            ],
        )
        self.assertIn("checked 5 allocation budgets", completed.stdout)

    def test_a_file_with_no_row_fails_whichever_it_is(self) -> None:
        empty = ("", "# path\tparse_peak\n\n# nothing is budgeted\n")
        for text in empty:
            for missing in range(3):
                files = [self.VALUE, self.TYPED, self.JSON]
                files[missing] = text
                with self.subTest(missing=missing, text=text):
                    completed, calls = self.gate(*files)
                    self.assertEqual(completed.returncode, 1, completed.stderr)
                    self.assertIn("no budgets found in", completed.stderr)
                    self.assertIn(("value.tsv", "typed.tsv", "json.tsv")[missing], completed.stderr)
                    self.assertEqual(len(calls), (0, 2, 3)[missing], calls)

    def test_a_failed_measurement_fails_the_gate_whichever_file_it_is_of(self) -> None:
        for failing, measured in (("value", 1), ("typed", 3), ("json", 4)):
            with self.subTest(failing=failing):
                completed, calls = self.gate(self.VALUE, self.TYPED, self.JSON, failing=failing)
                self.assertNotEqual(completed.returncode, 0, completed.stdout)
                self.assertNotIn("checked", completed.stdout)
                self.assertEqual(len(calls), measured, calls)


class AllocBaselineGateTests(unittest.TestCase):
    """Structured JSONL `memcount` baseline parser, regression gate, and slack warnings."""

    def test_parse_and_compare_detects_metric_and_hash_regressions(self) -> None:
        gate = check_alloc_baseline
        sha = "1835e9e70495561c218b4649b15837452ced87973b2a1494d81847b4eeba2c4b"
        base_row = (
            '{"schema_version":1,"scenario":"openapi-spec","bytes":10000,'
            f'"fixture_sha256":"{sha}","operation":"parse_value",'
            '"peak_bytes":40000,"live_bytes":20000,"allocations":500,'
            '"total_allocated_bytes":60000,"peak_to_input_ratio":4.000}\n'
        )
        base = gate.parse_jsonl(base_row, "base")
        self.assertEqual(gate.compare_baselines(base, base), [])

        within = gate.parse_jsonl(base_row.replace("40000", "43000").replace("500", "520"), "within")
        self.assertEqual(gate.compare_baselines(base, within), [])

        regressed = gate.parse_jsonl(
            base_row.replace("40000", "50000").replace("500", "600"), "regressed"
        )
        errs = gate.compare_baselines(base, regressed)
        self.assertEqual(len(errs), 2)
        self.assertTrue(any("peak_bytes regressed" in e for e in errs))
        self.assertTrue(any("allocations regressed" in e for e in errs))

        drifted = gate.parse_jsonl(base_row.replace(sha, "0" * 64), "drifted")
        self.assertTrue(
            any("fixture_sha256 changed" in e for e in gate.compare_baselines(base, drifted))
        )

        self.assertTrue(
            any("missing from current run" in e for e in gate.compare_baselines(base, {}))
        )
        with self.assertRaises(ValueError):
            gate.parse_jsonl('{"schema_version":99}\n', "bad")

    def test_slack_warnings_and_strict_cli_validation(self) -> None:
        gate = check_alloc_baseline
        sha = "1835e9e70495561c218b4649b15837452ced87973b2a1494d81847b4eeba2c4b"
        base_row = (
            '{"schema_version":1,"scenario":"openapi-spec","bytes":10000,'
            f'"fixture_sha256":"{sha}","operation":"parse_value",'
            '"peak_bytes":40000,"live_bytes":20000,"allocations":500,'
            '"total_allocated_bytes":60000,"peak_to_input_ratio":4.000}\n'
        )
        base = gate.parse_jsonl(base_row, "base")

        # 15% drop stays quiet; >15% drop warns on peak_bytes and allocations.
        borderline = gate.parse_jsonl(
            base_row.replace("40000", "34000").replace("500", "425"), "borderline"
        )
        self.assertEqual(gate.find_slack_warnings(base, borderline), [])

        slacked_row = base_row.replace("40000", "30000").replace("500", "400")
        slacked = gate.parse_jsonl(slacked_row, "slacked")
        warnings = gate.find_slack_warnings(base, slacked)
        self.assertEqual(len(warnings), 2)
        self.assertTrue(any("peak_bytes dropped" in w for w in warnings))
        self.assertTrue(any("allocations dropped" in w for w in warnings))

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            base_path = root / "base.jsonl"
            cur_path = root / "cur.jsonl"
            bad_path = root / "bad.jsonl"
            base_path.write_text(base_row, encoding="utf-8")
            cur_path.write_text(slacked_row, encoding="utf-8")
            bad_path.write_text("{not-json\n", encoding="utf-8")
            with mock.patch("builtins.print"):
                self.assertEqual(
                    gate.main(["--baseline", str(base_path), "--current", str(cur_path)]), 0
                )
                self.assertEqual(
                    gate.main(
                        [
                            "--baseline",
                            str(base_path),
                            "--current",
                            str(cur_path),
                            "--strict-slack",
                        ]
                    ),
                    1,
                )
                # Strict CLI validation returns exit 2 cleanly (no unhandled tracebacks).
                self.assertEqual(gate.main(["--slack-warn-pct", "NaN"]), 2)
                self.assertEqual(gate.main(["--max-bytes-pct", "-1"]), 2)
                self.assertEqual(gate.main(["--max-allocs-pct", "inf"]), 2)
                self.assertEqual(gate.main(["--record", "--strict-slack"]), 2)
                self.assertEqual(gate.main(["--record", "--current", str(cur_path)]), 2)
                self.assertEqual(
                    gate.main(["--baseline", str(base_path), "--current", str(root / "missing")]),
                    2,
                )
                self.assertEqual(
                    gate.main(["--baseline", str(base_path), "--current", str(bad_path)]),
                    2,
                )


if __name__ == "__main__":
    unittest.main()
