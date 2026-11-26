//! Unified workspace `xtask` CLI for `ayaml` performance fixtures, timing benchmarks,
//! JSONL memory probes, steady-state Callgrind checks, Valgrind Callgrind/DHAT profiling,
//! 10x linear scaling checks, baseline comparisons, regression gates, and Markdown reports.

mod compare;
mod det;
mod profile;
mod runners;
mod scale;
mod schema;

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use compare::GateThresholds;
use det::{DetOptions, DetSuite, run_det_suite};
use profile::{ProfileOptions, ProfileTarget, ProfileTool, run_profile};
use runners::{
    run_compare, run_det, run_fixtures, run_gate, run_memory, run_report, run_scale, run_speed,
};
use scale::ScaleLimits;

const USAGE: &str = "\
cargo xtask <fixtures|speed|memory|det|profile|scale|compare|gate|report|help> [options]

Subcommands:
  fixtures   List or verify deterministic SHA-256 fixture catalog
             [--json] [--verify] [--out-dir DIR]

  speed      Run Criterion benchmarks and export schema-v1 speed.jsonl + fingerprint
             [--filter REGEX] [--quick] [--out-dir target/xtask]

  memory     Run counting-allocator probe and export schema-v1 memory.jsonl + fingerprint
             [--suite baseline|full] [--fixture ID] [--out-dir target/xtask]

  det        Run steady-state Valgrind Callgrind over ayaml-bench via Criterion shim
             [--suite pr|full] [--filter SUBSTR] [--base REV] [--anchor REV]
             [--list] [--max-ir-pct 2.0] [--max-cycles-pct 5.0]
             [--anchor-ir-pct 10.0] [--anchor-cycles-pct 15.0]
             [--jobs N] [--out-dir target/xtask] [--legacy-probe --base-rev REV]

  profile    Run deterministic Valgrind Callgrind or DHAT hotspot attribution
             --tool callgrind|dhat (--bench ID | --fixture ID [--mode parse_value])
             [--base REV] [--top 15] [--inclusive] [--iterations 1] [--out PATH]

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
        Some("det") => match parse_det_args(&args[1..], &default_out)? {
            DetCommand::Help => {
                print!("{USAGE}");
                Ok(())
            }
            DetCommand::Legacy { base_rev, out_dir } => run_det(&root, &base_rev, &out_dir),
            DetCommand::Shim(opts) => run_det_suite(&root, &opts),
        },
        Some("profile") => match parse_profile_args(&args[1..])? {
            ProfileCommand::Help => {
                print!("{USAGE}");
                Ok(())
            }
            ProfileCommand::Run(opts) => run_profile(&root, &opts),
        },
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

#[derive(Debug, PartialEq)]
enum DetCommand {
    Help,
    Legacy { base_rev: String, out_dir: PathBuf },
    Shim(DetOptions),
}

#[derive(Debug, PartialEq, Eq)]
enum ProfileCommand {
    Help,
    Run(ProfileOptions),
}

fn next_flag_value<'a>(
    flag: &str,
    inline: Option<&'a str>,
    iter: &mut std::slice::Iter<'a, String>,
) -> Result<&'a str, String> {
    if let Some(val) = inline {
        if val.is_empty() {
            return Err(format!("missing value for `{flag}`"));
        }
        return Ok(val);
    }
    let Some(next) = iter.next() else {
        return Err(format!("missing value for `{flag}`"));
    };
    if next.starts_with("--") {
        return Err(format!(
            "missing value for `{flag}` (got flag `{next}` instead)"
        ));
    }
    Ok(next.as_str())
}

fn parse_nonneg_finite_f64(flag: &str, raw: &str) -> Result<f64, String> {
    let val = raw
        .parse::<f64>()
        .map_err(|e| format!("invalid {flag} value {raw:?}: {e}"))?;
    if !val.is_finite() || val < 0.0 {
        return Err(format!(
            "invalid {flag} value {raw:?}: must be a finite number >= 0.0"
        ));
    }
    Ok(val)
}

fn parse_positive_usize(flag: &str, raw: &str) -> Result<usize, String> {
    let val = raw
        .parse::<usize>()
        .map_err(|e| format!("invalid {flag} value {raw:?}: {e}"))?;
    if val == 0 {
        return Err(format!("invalid {flag} value 0: must be >= 1"));
    }
    Ok(val)
}

#[allow(
    clippy::too_many_lines,
    reason = "explicit strict CLI flag parser for `xtask det`"
)]
fn parse_det_args(args: &[String], default_out: &Path) -> Result<DetCommand, String> {
    let mut legacy_probe = false;
    let mut suite: Option<DetSuite> = None;
    let mut filter: Option<String> = None;
    let mut base: Option<String> = None;
    let mut base_rev: Option<String> = None;
    let mut anchor_rev: Option<String> = None;
    let mut list_only = false;
    let mut max_ir_pct: Option<f64> = None;
    let mut max_cycles_pct: Option<f64> = None;
    let mut anchor_ir_pct: Option<f64> = None;
    let mut anchor_cycles_pct: Option<f64> = None;
    let mut jobs: Option<usize> = None;
    let mut out_dir: Option<PathBuf> = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((k, v)) if k.starts_with('-') => (k, Some(v)),
            _ => (arg.as_str(), None),
        };
        match flag {
            "-h" | "--help" => return Ok(DetCommand::Help),
            "--legacy-probe" => {
                if inline.is_some() {
                    return Err("`--legacy-probe` does not accept a value".to_owned());
                }
                legacy_probe = true;
            }
            "--list" => {
                if inline.is_some() {
                    return Err("`--list` does not accept a value".to_owned());
                }
                list_only = true;
            }
            "--suite" => {
                let raw = next_flag_value(flag, inline, &mut iter)?;
                suite = Some(DetSuite::parse(raw)?);
            }
            "--filter" => {
                filter = Some(next_flag_value(flag, inline, &mut iter)?.to_owned());
            }
            "--base" => {
                base = Some(next_flag_value(flag, inline, &mut iter)?.to_owned());
            }
            "--base-rev" => {
                base_rev = Some(next_flag_value(flag, inline, &mut iter)?.to_owned());
            }
            "--anchor" => {
                anchor_rev = Some(next_flag_value(flag, inline, &mut iter)?.to_owned());
            }
            "--max-ir-pct" => {
                let raw = next_flag_value(flag, inline, &mut iter)?;
                max_ir_pct = Some(parse_nonneg_finite_f64(flag, raw)?);
            }
            "--max-cycles-pct" => {
                let raw = next_flag_value(flag, inline, &mut iter)?;
                max_cycles_pct = Some(parse_nonneg_finite_f64(flag, raw)?);
            }
            "--anchor-ir-pct" => {
                let raw = next_flag_value(flag, inline, &mut iter)?;
                anchor_ir_pct = Some(parse_nonneg_finite_f64(flag, raw)?);
            }
            "--anchor-cycles-pct" => {
                let raw = next_flag_value(flag, inline, &mut iter)?;
                anchor_cycles_pct = Some(parse_nonneg_finite_f64(flag, raw)?);
            }
            "--jobs" => {
                let raw = next_flag_value(flag, inline, &mut iter)?;
                jobs = Some(parse_positive_usize(flag, raw)?);
            }
            "--out-dir" => {
                out_dir = Some(PathBuf::from(next_flag_value(flag, inline, &mut iter)?));
            }
            other => {
                return Err(format!("unknown `det` argument `{other}`"));
            }
        }
    }

    if base.is_some() && base_rev.is_some() {
        return Err("`det` accepts either `--base` or `--base-rev`, not both".to_owned());
    }
    let effective_base = base.or(base_rev);
    let out_dir = out_dir.unwrap_or_else(|| default_out.to_path_buf());

    if legacy_probe {
        if suite.is_some()
            || filter.is_some()
            || anchor_rev.is_some()
            || list_only
            || max_ir_pct.is_some()
            || max_cycles_pct.is_some()
            || anchor_ir_pct.is_some()
            || anchor_cycles_pct.is_some()
            || jobs.is_some()
        {
            return Err(
                "`det --legacy-probe` only accepts `--base`/`--base-rev` and `--out-dir`"
                    .to_owned(),
            );
        }
        return Ok(DetCommand::Legacy {
            base_rev: effective_base.unwrap_or_else(|| "HEAD~1".to_owned()),
            out_dir,
        });
    }

    let default_jobs = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
    Ok(DetCommand::Shim(DetOptions {
        suite: suite.unwrap_or(DetSuite::Pr),
        filter,
        base_rev: effective_base,
        anchor_rev,
        list_only,
        max_ir_pct: max_ir_pct.unwrap_or(2.0),
        max_cycles_pct: max_cycles_pct.unwrap_or(5.0),
        anchor_ir_pct: anchor_ir_pct.unwrap_or(10.0),
        anchor_cycles_pct: anchor_cycles_pct.unwrap_or(15.0),
        jobs: jobs.unwrap_or(default_jobs),
        out_dir,
    }))
}

#[allow(
    clippy::too_many_lines,
    reason = "explicit strict CLI flag parser for `xtask profile`"
)]
fn parse_profile_args(args: &[String]) -> Result<ProfileCommand, String> {
    let mut tool: Option<ProfileTool> = None;
    let mut bench: Option<String> = None;
    let mut fixture: Option<String> = None;
    let mut mode: Option<String> = None;
    let mut base: Option<String> = None;
    let mut base_rev: Option<String> = None;
    let mut top: Option<usize> = None;
    let mut inclusive = false;
    let mut iterations: Option<usize> = None;
    let mut out: Option<PathBuf> = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((k, v)) if k.starts_with('-') => (k, Some(v)),
            _ => (arg.as_str(), None),
        };
        match flag {
            "-h" | "--help" => return Ok(ProfileCommand::Help),
            "--inclusive" => {
                if inline.is_some() {
                    return Err("`--inclusive` does not accept a value".to_owned());
                }
                inclusive = true;
            }
            "--tool" => {
                let raw = next_flag_value(flag, inline, &mut iter)?;
                tool = Some(ProfileTool::parse(raw)?);
            }
            "--bench" => {
                bench = Some(next_flag_value(flag, inline, &mut iter)?.to_owned());
            }
            "--fixture" => {
                fixture = Some(next_flag_value(flag, inline, &mut iter)?.to_owned());
            }
            "--mode" => {
                mode = Some(next_flag_value(flag, inline, &mut iter)?.to_owned());
            }
            "--base" => {
                base = Some(next_flag_value(flag, inline, &mut iter)?.to_owned());
            }
            "--base-rev" => {
                base_rev = Some(next_flag_value(flag, inline, &mut iter)?.to_owned());
            }
            "--top" => {
                let raw = next_flag_value(flag, inline, &mut iter)?;
                top = Some(parse_positive_usize(flag, raw)?);
            }
            "--iterations" => {
                let raw = next_flag_value(flag, inline, &mut iter)?;
                iterations = Some(parse_positive_usize(flag, raw)?);
            }
            "--out" => {
                out = Some(PathBuf::from(next_flag_value(flag, inline, &mut iter)?));
            }
            other => {
                return Err(format!("unknown `profile` argument `{other}`"));
            }
        }
    }

    if base.is_some() && base_rev.is_some() {
        return Err("`profile` accepts either `--base` or `--base-rev`, not both".to_owned());
    }
    let tool = tool.ok_or_else(|| "profile requires --tool callgrind|dhat".to_owned())?;
    let target = match (bench, fixture) {
        (Some(id), None) => {
            if mode.is_some() {
                return Err("`--mode` is only valid with `--fixture`, not `--bench`".to_owned());
            }
            ProfileTarget::Bench(id)
        }
        (None, Some(id)) => ProfileTarget::Fixture {
            fixture: id,
            mode: mode.unwrap_or_else(|| "parse_value".to_owned()),
        },
        (Some(_), Some(_)) => {
            return Err("profile accepts either --bench ID or --fixture ID, not both".to_owned());
        }
        (None, None) => {
            return Err("profile requires either --bench ID or --fixture ID".to_owned());
        }
    };
    if tool == ProfileTool::Dhat {
        if inclusive {
            return Err("`--inclusive` is only supported with `--tool callgrind`".to_owned());
        }
        if iterations.is_some() {
            return Err(
                "`--iterations` is only supported with `--tool callgrind --bench`".to_owned(),
            );
        }
    } else if matches!(target, ProfileTarget::Fixture { .. }) && iterations.is_some() {
        return Err("`--iterations` is only supported with `--tool callgrind --bench`".to_owned());
    }

    Ok(ProfileCommand::Run(ProfileOptions {
        tool,
        target,
        base_rev: base.or(base_rev),
        top: top.unwrap_or(15),
        inclusive,
        iterations: iterations.unwrap_or(1),
        out,
    }))
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
        assert_eq!(
            evaluate_speed_gate(&base_speed, &faster, GateThresholds::default()),
            Vec::<String>::new()
        );

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
        assert_eq!(
            evaluate_memory_gate(&base_mem, &better_mem, GateThresholds::default()),
            Vec::<String>::new()
        );

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

    #[test]
    fn det_shim_parsers_and_anchor_gating_catch_cumulative_creep() {
        use crate::det::{
            CallgrindCounters, DetSuite, evaluate_det_comparisons, iteration_counts_for,
            locked_version, parse_callgrind_out, render_det_table, select_benchmarks,
            steady_state_delta,
        };

        let sample_lock = "\
[[package]]
name = \"serde\"
version = \"1.0.220\"

[[package]]
name = \"criterion\"
version = \"0.7.0\"
";
        assert_eq!(
            locked_version(sample_lock, "criterion").as_deref(),
            Some("0.7.0")
        );
        assert_eq!(locked_version(sample_lock, "missing"), None);

        assert_eq!(
            iteration_counts_for("layers/from_str_typed_owned/frontmatter-tiny"),
            (5, 69)
        );
        assert_eq!(
            iteration_counts_for("scaling/parse_value/scale-map-1mb"),
            (1, 4)
        );
        assert_eq!(
            iteration_counts_for("scaling/parse_value/scale-seq-100kb"),
            (1, 4)
        );
        assert_eq!(
            iteration_counts_for("domains/parse_documents/ci-workflow"),
            (2, 10)
        );

        let avail = vec![
            "layers/parse_value/openapi-spec".to_owned(),
            "structural/parse_value/depth-64".to_owned(),
            "custom/extra/bench".to_owned(),
        ];
        assert_eq!(
            select_benchmarks(&avail, DetSuite::Pr, None),
            vec![
                "layers/parse_value/openapi-spec".to_owned(),
                "structural/parse_value/depth-64".to_owned()
            ]
        );
        assert_eq!(
            select_benchmarks(&avail, DetSuite::Full, Some("extra")),
            vec!["custom/extra/bench".to_owned()]
        );

        assert!(parse_callgrind_out("events: Ir\nsummary:\n").is_err());
        assert!(parse_callgrind_out("events: Ir\nsummary: 0\n").is_err());

        let k1_txt = "events: Ir Dr Dw I1mr D1mr D1mw ILmr DLmr DLmw Bc Bcm Bi Bim\nsummary: 10000 100 100 10 20 30 1 2 3 50 5 10 1\n";
        let k2_txt = "events: Ir Dr Dw I1mr D1mr D1mw ILmr DLmr DLmw Bc Bcm Bi Bim\nsummary: 50000 500 500 50 100 150 5 10 15 250 25 50 5\n";
        let c1 = parse_callgrind_out(k1_txt).expect("parse k1");
        let c2 = parse_callgrind_out(k2_txt).expect("parse k2");
        assert!(steady_state_delta(&c2, &c1, 4).is_err());
        let delta = steady_state_delta(&c1, &c2, 4).expect("steady state");
        assert_eq!(delta.ir, 10_000);
        assert_eq!(delta.l1_misses, 60);
        assert_eq!(delta.ll_misses, 6);
        assert_eq!(delta.branch_mispredicts, 6);
        assert_eq!(delta.estimated_cycles, 10_000 + 600 + 600 + 120);

        // Sum raw deltas before dividing so small per-event misses do not truncate to zero.
        let r1 = parse_callgrind_out(
            "events: Ir I1mr D1mr D1mw ILmr DLmr DLmw Bcm Bim\nsummary: 1000 0 0 0 0 0 0 0 0\n",
        )
        .expect("r1");
        let r2 = parse_callgrind_out(
            "events: Ir I1mr D1mr D1mw ILmr DLmr DLmw Bcm Bim\nsummary: 5000 2 2 2 1 1 1 0 0\n",
        )
        .expect("r2");
        let rounded = steady_state_delta(&r1, &r2, 4).expect("rounded");
        assert_eq!(rounded.l1_misses, 2);
        assert_eq!(rounded.ll_misses, 1);
        assert_eq!(rounded.estimated_cycles, 1090);

        let id = "structural/parse_value/depth-64".to_owned();
        let new_id = "layers/scan/openapi-spec".to_owned();
        let ids = vec![id.clone(), new_id.clone()];
        let mut anchor = BTreeMap::new();
        anchor.insert(
            id.clone(),
            CallgrindCounters {
                ir: 1_000_000,
                estimated_cycles: 1_200_000,
                l1_misses: 100,
                ll_misses: 10,
                branch_mispredicts: 20,
            },
        );
        let mut base = BTreeMap::new();
        base.insert(
            id.clone(),
            CallgrindCounters {
                ir: 1_095_000,
                estimated_cycles: 1_310_000,
                l1_misses: 110,
                ll_misses: 11,
                branch_mispredicts: 22,
            },
        );
        // +1.37% over base (passes base +2.0% gate), but +11.0% over anchor (fails anchor +10.0% gate!).
        let mut head = BTreeMap::new();
        head.insert(
            id,
            CallgrindCounters {
                ir: 1_110_000,
                estimated_cycles: 1_330_000,
                l1_misses: 112,
                ll_misses: 11,
                branch_mispredicts: 22,
            },
        );
        head.insert(
            new_id,
            CallgrindCounters {
                ir: 500_000,
                estimated_cycles: 600_000,
                l1_misses: 50,
                ll_misses: 5,
                branch_mispredicts: 10,
            },
        );

        let cmp = evaluate_det_comparisons(
            &ids,
            &head,
            Some(&base),
            Some(&anchor),
            2.0,
            5.0,
            10.0,
            15.0,
        );
        assert_eq!(cmp.len(), 2);
        assert_eq!(cmp[0].status, "fail");
        assert_eq!(cmp[0].failures.len(), 1);
        assert!(cmp[0].failures[0].contains("vs anchor"));
        assert_eq!(cmp[1].status, "new");
        let md = render_det_table(&cmp);
        assert!(md.contains("anchor Ir %"));
        assert!(md.contains("structural/parse_value/depth-64"));

        // Anchor-only mode must also mark benchmarks absent from anchor as `"new"`.
        let anchor_only =
            evaluate_det_comparisons(&ids, &head, None, Some(&anchor), 2.0, 5.0, 10.0, 15.0);
        assert_eq!(anchor_only[1].status, "new");
    }

    #[test]
    fn profile_parsers_filter_setup_and_render_callgrind_and_dhat_diffs() {
        use crate::profile::{
            parse_callgrind_annotate, parse_dhat_json, render_callgrind_diff,
            render_callgrind_table, render_dhat_diff, render_dhat_report,
        };

        let cg_base_txt = "\
--------------------------------------------------------------------------------
Ir        Dr      Dw     I1mr D1mr D1mw ILmr DLmr DLmw Bc    Bcm Bi   Bim
--------------------------------------------------------------------------------
1,000,000 200,000 50,000 10   20   30   1    2    3    5,000 10  500  2   PROGRAM TOTALS
1,000,000 200,000 50,000 10   20   30   1    2    3    5,000 10  500  2   /usr/src/debug/glibc/glibc/csu/libc-start.c:__libc_start_main [/bin]
  400,000  80,000 20,000  4    8   12   0    1    1    2,000  5  100  1   target/xtask-det/wt-base-abc123/crates/ayaml/src/scanner.rs:next_token -> [u8; 32]
  250,000  50,000 10,000  2    4    6   0    0    1    1,000  2   50  0   /tmp/crates/outer/crates/ayaml/src/parser.rs:read_lines [/bin]
";
        let cg_head_txt = "\
--------------------------------------------------------------------------------
Ir      Dr      Dw     I1mr D1mr D1mw ILmr DLmr DLmw Bc    Bcm Bi   Bim
--------------------------------------------------------------------------------
850,000 170,000 40,000 8    16   24   1    1    2    4,000  8  400  1   PROGRAM TOTALS
850,000 170,000 40,000 8    16   24   1    1    2    4,000  8  400  1   /usr/src/debug/glibc/glibc/csu/libc-start.c:__libc_start_main [/bin]
350,000  70,000 15,000 3    6    9    0    1    0    1,500  4   80  0   crates/ayaml/src/scanner.rs:next_token -> [u8; 32]
150,000  30,000  8,000 1    2    3    0    0    1      800  1   40  0   crates/ayaml/src/parser.rs:read_lines [/bin]
";
        let cg_base = parse_callgrind_annotate(cg_base_txt).expect("parse base cg");
        let cg_head = parse_callgrind_annotate(cg_head_txt).expect("parse head cg");
        assert_eq!(cg_base.total_ir, 1_000_000);
        assert_eq!(cg_base.total_l1_misses, 60);
        assert_eq!(cg_base.total_ll_misses, 6);
        assert_eq!(cg_base.total_branch_mispredicts, 12);
        assert_eq!(cg_head.functions.len(), 3);
        // Binary stripping must not truncate Rust array syntax `[u8; 32]` when no binary suffix exists.
        assert_eq!(
            cg_base.functions[1].symbol,
            "crates/ayaml/src/scanner.rs:next_token -> [u8; 32]"
        );
        // rfind("/crates/") must keep only the innermost workspace `crates/...` path.
        assert_eq!(
            cg_base.functions[2].symbol,
            "crates/ayaml/src/parser.rs:read_lines"
        );
        let cg_tbl = render_callgrind_table(&cg_head, 5, true);
        assert!(cg_tbl.contains("850000"));
        assert!(cg_tbl.contains("EstCycles"));
        assert!(cg_tbl.contains("BrMiss"));
        assert!(!cg_tbl.contains("__libc_start_main"));
        assert!(cg_tbl.contains("crates/ayaml/src/scanner.rs:next_token -> [u8; 32]"));
        let cg_diff = render_callgrind_diff(&cg_base, &cg_head, "HEAD~1", 5, true);
        assert!(cg_diff.contains("-150000"));
        assert!(cg_diff.contains("-50000"));
        assert!(cg_diff.contains("-100000"));
        assert!(!cg_diff.contains("__libc_start_main"));

        let dhat_json = r#"{
          "ftbl": [
            "0x1: ayaml_fixtures::generate (lib.rs:10)",
            "0x2: criterion::__ayaml_det_measure (lib.rs:25)",
            "0x3: ayaml::scanner::scan_double_quoted (scalars.rs:372)",
            "0x4: indexmap::Bucket::alloc (mod.rs:384)",
            "0x5: memcount::__ayaml_memcount_measure (memcount.rs:438)",
            "0x6: std::rt::lang_start_internal (in /opt/test/bin/memcount)"
          ],
          "pps": [
            {"gb": 500000, "mb": 500000, "tbk": 5, "tb": 500000, "fs": [0]},
            {"gb": 4096, "mb": 4096, "tbk": 2, "tb": 4096, "fs": [5]},
            {"gb": 6000, "mb": 6000, "tbk": 188, "tb": 6000, "fs": [2, 1, 4]},
            {"gb": 60000, "mb": 60000, "tbk": 4, "tb": 100000, "fs": [3, 1, 4]},
            {"gb": 60000, "mb": 40000, "tbk": 6, "tb": 140000, "fs": [3, 1, 4]}
          ]
        }"#;
        let parsed_fixture = parse_dhat_json(dhat_json, false).expect("parse fixture dhat");
        // Strictly scoped to `__ayaml_memcount_measure` -> excludes `ayaml_fixtures` and `std::rt`,
        // and aggregates the two identical `indexmap::Bucket::alloc` call sites into 1 row.
        assert_eq!(parsed_fixture.sites.len(), 2);
        assert_eq!(parsed_fixture.total_at_peak_bytes, 126_000);
        assert_eq!(parsed_fixture.total_allocations, 198);

        let parsed_bench = parse_dhat_json(dhat_json, true).expect("parse bench dhat");
        assert_eq!(parsed_bench.sites.len(), 2);
        let dhat_md = render_dhat_report(&parsed_bench, 5);
        assert!(dhat_md.contains("ayaml::scanner::scan_double_quoted"));
        assert!(dhat_md.contains("indexmap::Bucket::alloc"));
        let dhat_diff = render_dhat_diff(&parsed_fixture, &parsed_bench, "HEAD~1");
        assert!(dhat_diff.contains("at-peak bytes"));

        // When global peak occurred outside the scoped region (gb=0) and mb=0 on an active site,
        // the site must still appear and diff must report `n/a` rather than `+0.00%`.
        let outside_peak_json = r#"{
          "ftbl": [
            "0x1: criterion::__ayaml_det_measure (lib.rs:25)",
            "0x2: ayaml::parser::read_lines (parser.rs:351)"
          ],
          "pps": [
            {"gb": 0, "mb": 0, "tbk": 3, "tb": 12000, "fs": [1, 0]}
          ]
        }"#;
        let outside = parse_dhat_json(outside_peak_json, true).expect("outside peak");
        let outside_md = render_dhat_report(&outside, 5);
        assert!(outside_md.contains("outside scoped region"));
        assert!(outside_md.contains("ayaml::parser::read_lines"));
        let outside_diff = render_dhat_diff(&outside, &parsed_bench, "HEAD~1");
        assert!(outside_diff.contains("| at-peak bytes | n/a | 126000 | n/a |"));
    }

    #[test]
    fn det_and_profile_cli_parsers_enforce_strict_validation() {
        use super::{
            DetCommand, DetSuite, ProfileCommand, ProfileTarget, ProfileTool, parse_det_args,
            parse_profile_args,
        };
        let out = PathBuf::from("target/xtask");
        let sv = |xs: &[&str]| xs.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();

        assert_eq!(
            parse_det_args(&sv(&["--help"]), &out).expect("det help"),
            DetCommand::Help
        );
        assert_eq!(
            parse_profile_args(&sv(&["--help"])).expect("profile help"),
            ProfileCommand::Help
        );

        // `--flag=value` syntax works on both commands.
        let det_cmd = parse_det_args(
            &sv(&[
                "--suite=full",
                "--base=HEAD~1",
                "--anchor=main",
                "--jobs=2",
                "--max-ir-pct=1.5",
            ]),
            &out,
        )
        .expect("det eq flags");
        match det_cmd {
            DetCommand::Shim(opts) => {
                assert_eq!(opts.suite, DetSuite::Full);
                assert_eq!(opts.base_rev.as_deref(), Some("HEAD~1"));
                assert_eq!(opts.anchor_rev.as_deref(), Some("main"));
                assert_eq!(opts.jobs, 2);
                assert!((opts.max_ir_pct - 1.5).abs() < f64::EPSILON);
            }
            other => panic!("expected Shim, got {other:?}"),
        }

        // Reject typos, missing values, NaN thresholds, zero jobs, and conflicting flags.
        assert!(parse_det_args(&sv(&["--sutie", "full"]), &out).is_err());
        assert!(parse_det_args(&sv(&["--base"]), &out).is_err());
        assert!(parse_det_args(&sv(&["--base", "--anchor", "main"]), &out).is_err());
        assert!(parse_det_args(&sv(&["--max-ir-pct", "NaN"]), &out).is_err());
        assert!(parse_det_args(&sv(&["--anchor-cycles-pct", "-1.0"]), &out).is_err());
        assert!(parse_det_args(&sv(&["--jobs", "0"]), &out).is_err());
        assert!(parse_det_args(&sv(&["--base", "a", "--base-rev", "b"]), &out).is_err());
        assert!(parse_det_args(&sv(&["--legacy-probe", "--anchor", "main"]), &out).is_err());

        // Strict profile CLI validation.
        assert!(parse_profile_args(&sv(&["--tool=dhat", "--fixture=x", "--inclusive"])).is_err());
        assert!(parse_profile_args(&sv(&["--tool=dhat", "--bench=x", "--iterations=2"])).is_err());
        assert!(parse_profile_args(&sv(&["--tool=callgrind", "--bench=x", "--mode=m"])).is_err());
        assert!(parse_profile_args(&sv(&["--tool=callgrind", "--bench=x", "--top=0"])).is_err());
        assert!(parse_profile_args(&sv(&["--tool=callgrind", "--bench=x", "--bogus"])).is_err());
        assert!(
            parse_profile_args(&sv(&["--tool=callgrind", "--fixture=x", "--iterations=3"]))
                .is_err()
        );
        let prof = parse_profile_args(&sv(&[
            "--tool=callgrind",
            "--fixture=openapi-spec",
            "--mode=parse_value",
            "--inclusive",
            "--top=10",
        ]))
        .expect("valid profile fixture");
        match prof {
            ProfileCommand::Run(opts) => {
                assert_eq!(opts.tool, ProfileTool::Callgrind);
                assert!(opts.inclusive);
                assert_eq!(opts.top, 10);
                assert_eq!(
                    opts.target,
                    ProfileTarget::Fixture {
                        fixture: "openapi-spec".to_owned(),
                        mode: "parse_value".to_owned()
                    }
                );
            }
            ProfileCommand::Help => panic!("expected Run"),
        }
    }
}
