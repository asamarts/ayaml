//! Unified workspace `xtask` CLI for `ayaml` performance fixtures, timing benchmarks,
//! JSONL memory probes, steady-state Callgrind checks, 10x linear scaling checks,
//! baseline comparisons, regression gates, and Markdown report rendering.

mod compare;
mod runners;
mod scale;
mod schema;

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use compare::GateThresholds;
use runners::{
    run_compare, run_det, run_fixtures, run_gate, run_memory, run_report, run_scale, run_speed,
};
use scale::ScaleLimits;

const USAGE: &str = "\
cargo xtask <fixtures|speed|memory|det|scale|compare|gate|report|help> [options]

Subcommands:
  fixtures   List or verify deterministic SHA-256 fixture catalog
             [--json] [--verify] [--out-dir DIR]

  speed      Run Criterion benchmarks and export schema-v1 speed.jsonl + fingerprint
             [--filter REGEX] [--quick] [--out-dir target/xtask]

  memory     Run counting-allocator probe and export schema-v1 memory.jsonl + fingerprint
             [--suite baseline|full] [--fixture ID] [--out-dir target/xtask]

  det        Run steady-state Valgrind Callgrind check and validate callgrind.jsonl + fingerprint
             [--base-rev HEAD~1] [--out-dir target/xtask]

  scale      Verify linear time and memory growth across 10x fixture ladders
             [--samples 5] [--max-time-ratio 1.80] [--max-peak-ratio 1.80]
             [--max-allocs-ratio 1.50] [--out-dir target/xtask]

  compare    Render baseline-to-head Markdown diff across JSONL artifacts
             --baseline DIR --current DIR [--out PATH]

  gate       Run live allocation/Callgrind/scaling gates or compare two JSONL directories
             [--baseline DIR --current DIR] [--base-rev REV] [--scale]
             [--max-bytes-pct 10.0] [--max-allocs-pct 5.0]
             [--max-ir-pct 2.0] [--max-cycles-pct 5.0] [--max-speed-pct 10.0]

  report     Render Markdown summary from a directory of JSONL artifacts
             --dir DIR [--title TEXT] [--out PATH]
";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match dispatch(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("xtask error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(args: &[String]) -> Result<(), String> {
    let root = repo_root()?;
    let default_out = root.join("target").join("xtask");
    match args.first().map(String::as_str) {
        Some("fixtures") => {
            let sub = &args[1..];
            let as_json = has_flag(sub, "--json");
            let verify = has_flag(sub, "--verify");
            let out_dir = option_value(sub, "--out-dir").map(PathBuf::from);
            run_fixtures(&root, as_json, verify, out_dir.as_deref())
        }
        Some("speed") => {
            let sub = &args[1..];
            let filter = option_value(sub, "--filter");
            let quick = has_flag(sub, "--quick");
            let out_dir =
                option_value(sub, "--out-dir").map_or_else(|| default_out.clone(), PathBuf::from);
            run_speed(&root, filter, quick, &out_dir)
        }
        Some("memory") => {
            let sub = &args[1..];
            let suite = option_value(sub, "--suite").unwrap_or("baseline");
            let fixture = option_value(sub, "--fixture");
            let out_dir =
                option_value(sub, "--out-dir").map_or_else(|| default_out.clone(), PathBuf::from);
            run_memory(&root, suite, fixture, &out_dir)
        }
        Some("det") => {
            let sub = &args[1..];
            let base_rev = option_value(sub, "--base-rev").unwrap_or("HEAD~1");
            let out_dir =
                option_value(sub, "--out-dir").map_or_else(|| default_out.clone(), PathBuf::from);
            run_det(&root, base_rev, &out_dir)
        }
        Some("scale") => {
            let sub = &args[1..];
            let samples: usize = parse_opt(sub, "--samples", 5)?;
            let def = ScaleLimits::default();
            let limits = ScaleLimits {
                time: parse_opt(sub, "--max-time-ratio", def.time)?,
                peak: parse_opt(sub, "--max-peak-ratio", def.peak)?,
                allocs: parse_opt(sub, "--max-allocs-ratio", def.allocs)?,
            };
            let out_dir =
                option_value(sub, "--out-dir").map_or_else(|| default_out.clone(), PathBuf::from);
            run_scale(&root, samples, limits, &out_dir)
        }
        Some("compare") => {
            let sub = &args[1..];
            let baseline = option_value(sub, "--baseline")
                .ok_or_else(|| "compare requires --baseline DIR".to_owned())?;
            let current = option_value(sub, "--current")
                .ok_or_else(|| "compare requires --current DIR".to_owned())?;
            let out = option_value(sub, "--out").map(PathBuf::from);
            run_compare(Path::new(baseline), Path::new(current), out.as_deref())
        }
        Some("gate") => {
            let sub = &args[1..];
            let baseline = option_value(sub, "--baseline").map(PathBuf::from);
            let current = option_value(sub, "--current").map(PathBuf::from);
            let base_rev = option_value(sub, "--base-rev");
            let with_scale = has_flag(sub, "--scale");
            let def = GateThresholds::default();
            let thresholds = GateThresholds {
                max_bytes_pct: parse_opt(sub, "--max-bytes-pct", def.max_bytes_pct)?,
                min_bytes_floor: def.min_bytes_floor,
                max_allocs_pct: parse_opt(sub, "--max-allocs-pct", def.max_allocs_pct)?,
                min_allocs_floor: def.min_allocs_floor,
                max_ir_pct: parse_opt(sub, "--max-ir-pct", def.max_ir_pct)?,
                max_cycles_pct: parse_opt(sub, "--max-cycles-pct", def.max_cycles_pct)?,
                max_speed_pct: parse_opt(sub, "--max-speed-pct", def.max_speed_pct)?,
            };
            run_gate(
                &root,
                baseline.as_deref(),
                current.as_deref(),
                base_rev,
                with_scale,
                thresholds,
            )
        }
        Some("report") => {
            let sub = &args[1..];
            let dir =
                option_value(sub, "--dir").ok_or_else(|| "report requires --dir DIR".to_owned())?;
            let title = option_value(sub, "--title").unwrap_or("ayaml Performance Report");
            let out = option_value(sub, "--out").map(PathBuf::from);
            run_report(Path::new(dir), title, out.as_deref())
        }
        Some("help" | "-h" | "--help") | None => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown subcommand `{other}`\n\n{USAGE}")),
    }
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn option_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find_map(|pair| (pair[0] == flag).then_some(pair[1].as_str()))
}

fn parse_opt<T: std::str::FromStr>(args: &[String], flag: &str, default: T) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    match option_value(args, flag) {
        None => Ok(default),
        Some(raw) => raw
            .parse::<T>()
            .map_err(|e| format!("invalid {flag} value {raw:?}: {e}")),
    }
}

fn repo_root() -> Result<PathBuf, String> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "unable to resolve workspace root from CARGO_MANIFEST_DIR".to_owned())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    use crate::compare::{
        GateThresholds, compare_dirs, evaluate_callgrind_gate, evaluate_memory_gate,
        evaluate_speed_gate, format_callgrind_diff, format_memory_diff, format_speed_diff,
        render_report,
    };
    use crate::runners::collect_criterion_records;
    use crate::scale::{ScaleLimits, ScalePoint, evaluate_scaling_step};
    use crate::schema::{
        CallgrindRecord, Fingerprint, MemoryRecord, SCHEMA_VERSION, ScaleRecord, SpeedRecord,
    };
    use crate::{repo_root, run_gate};

    const SAMPLE_SHA: &str = "171af420cc6c70f1f488e734beed00b7602e2dbd5e1825a4783f625224a644a2";
    const OTHER_SHA: &str = "40478a17c8b3d2ab7ae0d5748351b54ad8bd34f0daeb76f16752371f91b7d978";

    fn scratch(label: &str) -> PathBuf {
        let dir = repo_root()
            .expect("repo root")
            .join("target")
            .join(format!("xtask-unit-{label}-{}", std::process::id()));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(&dir).expect("mkdir scratch");
        dir
    }

    #[test]
    fn jsonl_and_fingerprint_schemas_roundtrip_and_reject_malformed_rows() {
        let fp = Fingerprint {
            schema_version: SCHEMA_VERSION,
            os: "linux".into(),
            arch: "x86_64".into(),
            kernel: "Linux 6.12".into(),
            cpu: "Generic x86_64".into(),
            cpu_count: 8,
            rustc: "rustc 1.88.0".into(),
            git_sha: "abcdef0123456789".into(),
            fixture_manifest_sha256: SAMPLE_SHA.into(),
            timestamp_unix: 1_795_000_000,
        };
        let fp_json = fp.to_pretty_json().expect("serialize fp");
        assert!(!fp_json.contains("hostname"));
        assert_eq!(Fingerprint::parse_json(&fp_json).expect("parse fp"), fp);
        assert!(Fingerprint::parse_json(&fp_json.replace(SAMPLE_SHA, "bad")).is_err());

        let speed = SpeedRecord {
            schema_version: SCHEMA_VERSION,
            scenario: "openapi-spec".into(),
            operation: "parse_value".into(),
            bytes: 173_342,
            fixture_sha256: SAMPLE_SHA.into(),
            median_ns: 2_500_000.0,
            mean_ns: 2_510_000.0,
            stddev_ns: 12_000.0,
            throughput_mib_s: 66.1,
        };
        let s_line = speed.to_json_line().expect("serialize speed");
        assert_eq!(
            SpeedRecord::parse_json_line(&s_line).expect("parse speed"),
            speed
        );
        assert!(SpeedRecord::parse_json_line(&s_line.replace("2500000.0", "0.0")).is_err());

        let mem = MemoryRecord {
            schema_version: SCHEMA_VERSION,
            scenario: "openapi-spec".into(),
            operation: "parse_value".into(),
            bytes: 173_342,
            fixture_sha256: SAMPLE_SHA.into(),
            peak_bytes: 3_570_330,
            live_bytes: 2_410_628,
            allocations: 22_560,
            total_allocated_bytes: 4_622_409,
            peak_to_input_ratio: 20.597,
        };
        let m_line = mem.to_json_line().expect("serialize mem");
        assert_eq!(
            MemoryRecord::parse_json_line(&m_line).expect("parse mem"),
            mem
        );
        assert!(
            MemoryRecord::parse_json_line(
                &m_line.replace("\"schema_version\":1", "\"schema_version\":9")
            )
            .is_err()
        );

        let cg = CallgrindRecord {
            schema_version: SCHEMA_VERSION,
            operation: "value".into(),
            workload: "ci-workflow".into(),
            bytes: 21_735,
            fixture_sha256: SAMPLE_SHA.into(),
            base_ir: Some(1_000_000),
            head_ir: 990_000,
            ir_change_pct: Some(-1.0),
            base_estimated_cycles: Some(1_200_000),
            head_estimated_cycles: 1_190_000,
            cycles_change_pct: Some(-0.833),
            l1_misses: 500,
            ll_misses: 50,
            branch_mispredicts: 120,
            status: "ok".into(),
        };
        let c_line = cg.to_json_line().expect("serialize cg");
        assert_eq!(
            CallgrindRecord::parse_json_line(&c_line).expect("parse cg"),
            cg
        );
        assert!(CallgrindRecord::parse_json_line(&c_line.replace("\"ok\"", "\"bogus\"")).is_err());

        let scale = ScaleRecord {
            schema_version: SCHEMA_VERSION,
            family: "scale-map".into(),
            small_scenario: "scale-map-10kb".into(),
            large_scenario: "scale-map-100kb".into(),
            small_bytes: 10_000,
            large_bytes: 100_000,
            size_ratio: 10.0,
            small_median_ns: 100_000.0,
            large_median_ns: 1_080_000.0,
            normalized_time_ratio: 1.08,
            small_peak_bytes: 200_000,
            large_peak_bytes: 2_100_000,
            normalized_peak_ratio: 1.05,
            small_allocations: 800,
            large_allocations: 8_200,
            normalized_allocs_ratio: 1.025,
            passed: true,
        };
        let sc_line = scale.to_json_line().expect("serialize scale");
        assert_eq!(
            ScaleRecord::parse_json_line(&sc_line).expect("parse scale"),
            scale
        );
    }

    #[test]
    fn scaling_ratio_checks_pass_linear_growth_and_reject_quadratic_blowup() {
        let small = ScalePoint {
            scenario: "scale-map-10kb".into(),
            bytes: 10_000,
            median_ns: 100_000.0,
            peak_bytes: 250_000,
            allocations: 840,
        };
        let linear_large = ScalePoint {
            scenario: "scale-map-100kb".into(),
            bytes: 100_000,
            median_ns: 1_120_000.0,
            peak_bytes: 2_600_000,
            allocations: 8_500,
        };
        let ok = evaluate_scaling_step("scale-map", &small, &linear_large, ScaleLimits::default())
            .expect("linear step");
        assert!(ok.passed, "expected linear pass: {ok:?}");
        assert!((ok.normalized_time_ratio - 1.12).abs() < 0.01);
        assert!((ok.normalized_peak_ratio - 1.04).abs() < 0.01);

        let quadratic_large = ScalePoint {
            scenario: "scale-map-100kb".into(),
            bytes: 100_000,
            median_ns: 9_800_000.0,
            peak_bytes: 2_600_000,
            allocations: 8_500,
        };
        let bad_time = evaluate_scaling_step(
            "scale-map",
            &small,
            &quadratic_large,
            ScaleLimits::default(),
        )
        .expect("quadratic step");
        assert!(!bad_time.passed);
        assert!(bad_time.normalized_time_ratio > 9.0);

        let mem_blowup = ScalePoint {
            scenario: "scale-map-100kb".into(),
            bytes: 100_000,
            median_ns: 1_050_000.0,
            peak_bytes: 5_000_000,
            allocations: 8_500,
        };
        let bad_mem =
            evaluate_scaling_step("scale-map", &small, &mem_blowup, ScaleLimits::default())
                .expect("mem blowup");
        assert!(!bad_mem.passed);
        assert!(bad_mem.normalized_peak_ratio >= 2.0);
    }

    #[test]
    fn comparison_diffs_gating_and_report_rendering_work_and_detect_regressions() {
        let base_speed = vec![SpeedRecord {
            schema_version: SCHEMA_VERSION,
            scenario: "openapi-spec".into(),
            operation: "parse_value".into(),
            bytes: 173_342,
            fixture_sha256: SAMPLE_SHA.into(),
            median_ns: 2_000_000.0,
            mean_ns: 2_020_000.0,
            stddev_ns: 10_000.0,
            throughput_mib_s: 82.6,
        }];
        let mut faster = base_speed.clone();
        faster[0].median_ns = 1_500_000.0;
        let diff = format_speed_diff(&base_speed, &faster).expect("speed diff");
        assert!(diff.contains("-25.00%"));
        assert!(evaluate_speed_gate(&base_speed, &faster, GateThresholds::default()).is_empty());

        let mut slower = base_speed.clone();
        slower[0].median_ns = 2_400_000.0;
        assert_eq!(
            evaluate_speed_gate(&base_speed, &slower, GateThresholds::default()).len(),
            1
        );

        let mut drifted = faster.clone();
        drifted[0].fixture_sha256 = OTHER_SHA.into();
        assert!(format_speed_diff(&base_speed, &drifted).is_err());

        let base_mem = vec![MemoryRecord {
            schema_version: SCHEMA_VERSION,
            scenario: "openapi-spec".into(),
            operation: "parse_value".into(),
            bytes: 173_342,
            fixture_sha256: SAMPLE_SHA.into(),
            peak_bytes: 1_000_000,
            live_bytes: 500_000,
            allocations: 10_000,
            total_allocated_bytes: 1_500_000,
            peak_to_input_ratio: 5.77,
        }];
        let mut better_mem = base_mem.clone();
        better_mem[0].peak_bytes = 900_000;
        let mem_diff = format_memory_diff(&base_mem, &better_mem).expect("mem diff");
        assert!(mem_diff.contains("-10.00%"));
        assert!(evaluate_memory_gate(&base_mem, &better_mem, GateThresholds::default()).is_empty());

        let mut worse_mem = base_mem.clone();
        worse_mem[0].peak_bytes = 1_250_000;
        worse_mem[0].allocations = 11_000;
        assert_eq!(
            evaluate_memory_gate(&base_mem, &worse_mem, GateThresholds::default()).len(),
            2
        );

        let base_cg = vec![CallgrindRecord {
            schema_version: SCHEMA_VERSION,
            operation: "value".into(),
            workload: "ci-workflow".into(),
            bytes: 21_735,
            fixture_sha256: SAMPLE_SHA.into(),
            base_ir: Some(1_000_000),
            head_ir: 1_000_000,
            ir_change_pct: Some(0.0),
            base_estimated_cycles: Some(1_200_000),
            head_estimated_cycles: 1_200_000,
            cycles_change_pct: Some(0.0),
            l1_misses: 500,
            ll_misses: 50,
            branch_mispredicts: 100,
            status: "ok".into(),
        }];
        let mut worse_cg = base_cg.clone();
        worse_cg[0].head_ir = 1_050_000;
        worse_cg[0].head_estimated_cycles = 1_300_000;
        let cg_diff = format_callgrind_diff(&base_cg, &worse_cg).expect("cg diff");
        assert!(cg_diff.contains("+5.00%"));
        assert_eq!(
            evaluate_callgrind_gate(&base_cg, &worse_cg, GateThresholds::default()).len(),
            2
        );

        // End-to-end directory compare, gate, Criterion collection, and report rendering.
        let tmp = scratch("e2e");
        let crit = tmp.join("criterion/domains/parse_value/openapi-spec/new");
        fs::create_dir_all(&crit).expect("mkdir crit");
        fs::write(
            crit.join("estimates.json"),
            r#"{"median":{"point_estimate":2000000.0},"mean":{"point_estimate":2010000.0},"std_dev":{"point_estimate":5000.0}}"#,
        )
        .expect("write est");
        let sanitized = tmp.join("criterion/layers/scan_openapi-spec/new");
        fs::create_dir_all(&sanitized).expect("mkdir sanitized");
        fs::write(
            sanitized.join("benchmark.json"),
            r#"{"group_id":"layers","function_id":"scan/openapi-spec","full_id":"layers/scan/openapi-spec","throughput":{"Bytes":173342}}"#,
        )
        .expect("write benchmark.json");
        fs::write(
            sanitized.join("estimates.json"),
            r#"{"median":{"point_estimate":1000000.0},"mean":{"point_estimate":1005000.0},"std_dev":{"point_estimate":2000.0}}"#,
        )
        .expect("write sanitized est");
        let mut meta = BTreeMap::new();
        meta.insert(
            "openapi-spec".to_owned(),
            (173_342_u64, SAMPLE_SHA.to_owned()),
        );
        let collected = collect_criterion_records(&tmp.join("criterion"), &meta).expect("collect");
        assert_eq!(collected.len(), 2);
        assert!(
            collected
                .iter()
                .all(|r| r.scenario == "openapi-spec" && r.fixture_sha256 == SAMPLE_SHA)
        );
        assert!(collected.iter().any(|r| r.operation == "layers/scan"));
        assert!(collected[0].throughput_mib_s > 50.0);

        let base_dir = tmp.join("base");
        let cur_dir = tmp.join("cur");
        fs::create_dir_all(&base_dir).expect("mkdir base");
        fs::create_dir_all(&cur_dir).expect("mkdir cur");
        fs::write(
            base_dir.join("memory.jsonl"),
            format!("{}\n", base_mem[0].to_json_line().expect("line")),
        )
        .expect("write base mem");
        fs::write(
            cur_dir.join("memory.jsonl"),
            format!("{}\n", better_mem[0].to_json_line().expect("line")),
        )
        .expect("write cur mem");
        let fp = Fingerprint::capture(&repo_root().expect("root"), SAMPLE_SHA.into());
        fs::write(
            cur_dir.join("memory.fingerprint.json"),
            fp.to_pretty_json().expect("fp"),
        )
        .expect("write fp");

        let compared = compare_dirs(&base_dir, &cur_dir).expect("compare dirs");
        assert!(compared.contains("## Memory"));
        run_gate(
            &repo_root().expect("root"),
            Some(&base_dir),
            Some(&cur_dir),
            None,
            false,
            GateThresholds::default(),
        )
        .expect("gate pass");
        let report = render_report(&cur_dir, "Test Report").expect("report");
        assert!(report.contains("# Test Report"));
        assert!(report.contains("openapi-spec"));
        drop(fs::remove_dir_all(&tmp));
    }
}
