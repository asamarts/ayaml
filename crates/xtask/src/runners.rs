//! Subcommand execution helpers for `cargo xtask`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::compare::{
    GateThresholds, compare_dirs, evaluate_callgrind_gate, evaluate_memory_gate,
    evaluate_speed_gate, format_scale_table, render_report,
};
use crate::scale::{
    SCALING_LADDERS, ScaleLimits, ScalePoint, evaluate_scaling_step, measure_parse_median_ns,
};
use crate::schema::{
    Fingerprint, MemoryRecord, SCHEMA_VERSION, SpeedRecord, read_callgrind_jsonl,
    read_memory_jsonl, read_speed_jsonl,
};

/// Execute `cargo xtask fixtures [--json] [--verify] [--out-dir DIR]`.
///
/// # Errors
///
/// Returns an error if digest verification or artifact output fails.
pub(crate) fn run_fixtures(
    repo_root: &Path,
    as_json: bool,
    verify: bool,
    out_dir: Option<&Path>,
) -> Result<(), String> {
    if verify {
        ayaml_fixtures::verify_manifest_digests()?;
    }
    let manifest = ayaml_fixtures::manifest();
    if let Some(dir) = out_dir {
        fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        fs::write(dir.join("manifest.json"), manifest.to_json())
            .map_err(|e| format!("writing manifest.json: {e}"))?;
        let fp = Fingerprint::capture(repo_root, manifest.manifest_sha256.clone());
        fs::write(dir.join("manifest.fingerprint.json"), fp.to_pretty_json()?)
            .map_err(|e| format!("writing manifest.fingerprint.json: {e}"))?;
    }
    if as_json {
        print!("{}", manifest.to_json());
    } else {
        println!(
            "ayaml-fixtures manifest v{} ({} fixtures, sha256={})",
            manifest.schema_version,
            manifest.entries.len(),
            manifest.manifest_sha256
        );
        println!(
            "{:<26} {:<11} {:>9} {:<6} sha256",
            "id", "category", "bytes", "valid"
        );
        for entry in &manifest.entries {
            println!(
                "{:<26} {:<11} {:>9} {:<6} {}",
                entry.id,
                entry.category.as_str(),
                entry.bytes,
                entry.expect_valid,
                &entry.sha256[..16]
            );
        }
    }
    Ok(())
}

/// Execute `cargo xtask speed [--filter REGEX] [--quick] [--out-dir DIR]`.
///
/// # Errors
///
/// Returns an error if Criterion fails or no timing estimates can be exported.
pub(crate) fn run_speed(
    repo_root: &Path,
    filter: Option<&str>,
    quick: bool,
    out_dir: &Path,
) -> Result<(), String> {
    fs::create_dir_all(out_dir).map_err(|e| format!("creating {}: {e}", out_dir.display()))?;
    let criterion_dir = out_dir.join(".criterion-tmp");
    drop(fs::remove_dir_all(&criterion_dir));
    fs::create_dir_all(&criterion_dir)
        .map_err(|e| format!("creating {}: {e}", criterion_dir.display()))?;
    // Cargo executes package benchmarks with cwd set to the crate manifest directory,
    // so CRITERION_HOME must be an absolute path.
    let criterion_dir = fs::canonicalize(&criterion_dir).unwrap_or(criterion_dir);

    let mut cmd = Command::new("cargo");
    cmd.current_dir(repo_root)
        .env("CRITERION_HOME", &criterion_dir)
        .args([
            "bench",
            "-p",
            "ayaml-bench",
            "--locked",
            "--bench",
            "throughput",
            "--",
            "--noplot",
        ])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if quick {
        cmd.args([
            "--warm-up-time",
            "0.2",
            "--measurement-time",
            "0.4",
            "--sample-size",
            "10",
        ]);
    }
    if let Some(pat) = filter {
        cmd.arg(pat);
    }
    let status = cmd
        .status()
        .map_err(|e| format!("failed to launch cargo bench: {e}"))?;
    if !status.success() {
        drop(fs::remove_dir_all(&criterion_dir));
        return Err(format!(
            "cargo bench exited with status {}",
            status.code().unwrap_or(1)
        ));
    }

    let manifest = ayaml_fixtures::manifest();
    let meta: BTreeMap<String, (u64, String)> = manifest
        .entries
        .iter()
        .map(|e| (e.id.clone(), (e.bytes as u64, e.sha256.clone())))
        .collect();
    let records = collect_criterion_records(&criterion_dir, &meta);
    drop(fs::remove_dir_all(&criterion_dir));
    let records = records?;
    let mut jsonl = String::new();
    for rec in &records {
        jsonl.push_str(&rec.to_json_line()?);
        jsonl.push('\n');
    }
    fs::write(out_dir.join("speed.jsonl"), jsonl)
        .map_err(|e| format!("writing speed.jsonl: {e}"))?;
    let fp = Fingerprint::capture(repo_root, manifest.manifest_sha256);
    fs::write(out_dir.join("speed.fingerprint.json"), fp.to_pretty_json()?)
        .map_err(|e| format!("writing speed.fingerprint.json: {e}"))?;
    println!(
        "wrote {} validated speed records to {}",
        records.len(),
        out_dir.join("speed.jsonl").display()
    );
    Ok(())
}

/// Walk Criterion's `target/criterion/**/new/estimates.json` tree and build [`SpeedRecord`] rows.
///
/// # Errors
///
/// Returns an error if no valid estimates are found or JSON parsing fails.
pub(crate) fn collect_criterion_records(
    criterion_dir: &Path,
    fixture_meta: &BTreeMap<String, (u64, String)>,
) -> Result<Vec<SpeedRecord>, String> {
    let mut estimate_paths = Vec::new();
    find_estimates(criterion_dir, &mut estimate_paths)?;
    estimate_paths.sort();

    let mut records = Vec::new();
    for est_path in estimate_paths {
        let Some(bench_dir) = est_path.parent().and_then(Path::parent) else {
            continue;
        };
        let bench_meta = read_benchmark_json(&est_path.with_file_name("benchmark.json"));
        let rel = bench_meta.as_ref().map_or_else(
            || {
                bench_dir
                    .strip_prefix(criterion_dir)
                    .unwrap_or(bench_dir)
                    .to_string_lossy()
                    .replace('\\', "/")
            },
            |(full_id, _)| full_id.clone(),
        );
        let (operation, scenario) = split_criterion_rel(&rel);
        let text = fs::read_to_string(&est_path)
            .map_err(|e| format!("reading {}: {e}", est_path.display()))?;
        let (median_ns, mean_ns, stddev_ns) = parse_estimates_json(&text)?;
        let (meta_bytes, sha) = fixture_meta
            .get(&scenario)
            .cloned()
            .unwrap_or_else(|| (0, "unknown".to_owned()));
        let bytes = bench_meta
            .and_then(|(_, throughput_bytes)| throughput_bytes.filter(|&b| b > 0))
            .unwrap_or(meta_bytes);
        #[allow(
            clippy::cast_precision_loss,
            reason = "throughput MiB/s calculation over byte counts"
        )]
        let throughput_mib_s = if bytes > 0 && median_ns > 0.0 {
            ((bytes as f64) / 1_048_576.0) / (median_ns / 1e9)
        } else {
            0.0
        };
        records.push(SpeedRecord {
            schema_version: SCHEMA_VERSION,
            implementation: None,
            scenario,
            operation,
            bytes,
            fixture_sha256: sha,
            median_ns: (median_ns * 1000.0).round() / 1000.0,
            mean_ns: (mean_ns * 1000.0).round() / 1000.0,
            stddev_ns: (stddev_ns * 1000.0).round() / 1000.0,
            throughput_mib_s: (throughput_mib_s * 1000.0).round() / 1000.0,
            rounds: None,
        });
    }
    if records.is_empty() {
        return Err(format!(
            "no Criterion estimates.json files found under {}",
            criterion_dir.display()
        ));
    }
    Ok(records)
}

fn find_estimates(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if !dir.is_dir() {
        return Ok(());
    }
    let entries = fs::read_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "report") {
                continue;
            }
            let est = path.join("new").join("estimates.json");
            if est.is_file() {
                out.push(est);
            } else {
                find_estimates(&path, out)?;
            }
        }
    }
    Ok(())
}

fn split_criterion_rel(rel: &str) -> (String, String) {
    let parts: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    match parts.as_slice() {
        [group, op, scenario] => (format!("{group}/{op}"), (*scenario).to_owned()),
        [op, scenario] => ((*op).to_owned(), (*scenario).to_owned()),
        [only] => ("bench".to_owned(), (*only).to_owned()),
        _ => (
            parts[..parts.len().saturating_sub(1)].join("/"),
            parts.last().copied().unwrap_or("unknown").to_owned(),
        ),
    }
}

fn read_benchmark_json(path: &Path) -> Option<(String, Option<u64>)> {
    let text = fs::read_to_string(path).ok()?;
    let val: Value = serde_json::from_str(&text).ok()?;
    let full_id = val.get("full_id")?.as_str()?.to_owned();
    let throughput_bytes = val
        .get("throughput")
        .and_then(|t| t.get("Bytes"))
        .and_then(Value::as_u64);
    Some((full_id, throughput_bytes))
}

fn parse_estimates_json(text: &str) -> Result<(f64, f64, f64), String> {
    let val: Value =
        serde_json::from_str(text).map_err(|e| format!("invalid estimates.json: {e}"))?;
    let median = val
        .get("median")
        .and_then(|m| m.get("point_estimate"))
        .and_then(Value::as_f64)
        .ok_or_else(|| "estimates.json missing median.point_estimate".to_owned())?;
    let mean = val
        .get("mean")
        .and_then(|m| m.get("point_estimate"))
        .and_then(Value::as_f64)
        .unwrap_or(median);
    let stddev = val
        .get("std_dev")
        .and_then(|m| m.get("point_estimate"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    Ok((median, mean, stddev))
}

/// Execute `cargo xtask memory [--suite baseline|full] [--fixture ID] [--out-dir DIR]`.
///
/// # Errors
///
/// Returns an error if `memcount` fails or outputs invalid schema-v1 JSONL.
pub(crate) fn run_memory(
    repo_root: &Path,
    suite: &str,
    fixture: Option<&str>,
    out_dir: &Path,
) -> Result<(), String> {
    if fixture.is_none() && !matches!(suite, "baseline" | "full") {
        return Err(format!(
            "invalid --suite value {suite:?} (expected `baseline` or `full`)"
        ));
    }
    let output = run_memcount_jsonl(repo_root, suite, fixture)?;
    let mut rows = Vec::new();
    for line in output.lines().filter(|l| !l.trim().is_empty()) {
        rows.push(MemoryRecord::parse_json_line(line)?);
    }
    if rows.is_empty() {
        return Err("memcount --jsonl produced zero records".to_owned());
    }
    fs::create_dir_all(out_dir).map_err(|e| format!("creating {}: {e}", out_dir.display()))?;
    let mut normalized = String::new();
    for row in &rows {
        normalized.push_str(&row.to_json_line()?);
        normalized.push('\n');
    }
    fs::write(out_dir.join("memory.jsonl"), normalized)
        .map_err(|e| format!("writing memory.jsonl: {e}"))?;
    let manifest = ayaml_fixtures::manifest();
    let fp = Fingerprint::capture(repo_root, manifest.manifest_sha256);
    fs::write(
        out_dir.join("memory.fingerprint.json"),
        fp.to_pretty_json()?,
    )
    .map_err(|e| format!("writing memory.fingerprint.json: {e}"))?;
    println!(
        "wrote {} validated memory records to {}",
        rows.len(),
        out_dir.join("memory.jsonl").display()
    );
    Ok(())
}

/// Execute `cargo xtask det [--base-rev REV] [--out-dir DIR]`.
///
/// # Errors
///
/// Returns an error if Valgrind Callgrind is missing, fails, or records a regression.
pub(crate) fn run_det(repo_root: &Path, base_rev: &str, out_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(out_dir).map_err(|e| format!("creating {}: {e}", out_dir.display()))?;
    let abs_out_dir =
        fs::canonicalize(out_dir).map_err(|e| format!("resolving {}: {e}", out_dir.display()))?;
    let jsonl_path = abs_out_dir.join("callgrind.jsonl");
    let status = Command::new("python3")
        .current_dir(repo_root)
        .args([
            "tools/check_instructions.py",
            "--jsonl-out",
            &jsonl_path.to_string_lossy(),
            base_rev,
        ])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("failed to invoke tools/check_instructions.py: {e}"))?;
    if !status.success() {
        return Err(format!(
            "tools/check_instructions.py exited with status {}",
            status.code().unwrap_or(1)
        ));
    }
    let rows = read_callgrind_jsonl(&jsonl_path)?;
    let mut normalized = String::new();
    for row in &rows {
        normalized.push_str(&row.to_json_line()?);
        normalized.push('\n');
    }
    fs::write(&jsonl_path, normalized).map_err(|e| format!("writing callgrind.jsonl: {e}"))?;
    let manifest = ayaml_fixtures::manifest();
    let fp = Fingerprint::capture(repo_root, manifest.manifest_sha256);
    fs::write(
        out_dir.join("callgrind.fingerprint.json"),
        fp.to_pretty_json()?,
    )
    .map_err(|e| format!("writing callgrind.fingerprint.json: {e}"))?;
    println!(
        "validated {} steady-state Callgrind records in {}",
        rows.len(),
        jsonl_path.display()
    );
    Ok(())
}

/// Execute `cargo xtask scale [--samples N] [--out-dir DIR]` verifying 10x linear time & memory growth.
///
/// # Errors
///
/// Returns an error if any 10x scaling step exceeds linear bounds or measurement fails.
pub(crate) fn run_scale(
    repo_root: &Path,
    samples: usize,
    limits: ScaleLimits,
    out_dir: &Path,
) -> Result<(), String> {
    ayaml_fixtures::verify_manifest_digests()?;

    // Collect distinct scaling fixture IDs in deterministic order.
    let mut ids = Vec::new();
    for &(_, small, large) in SCALING_LADDERS {
        if !ids.contains(&small) {
            ids.push(small);
        }
        if !ids.contains(&large) {
            ids.push(large);
        }
    }

    let mut points = BTreeMap::new();
    for id in ids {
        let fx = ayaml_fixtures::generate(id)
            .ok_or_else(|| format!("missing scaling fixture `{id}`"))?;
        let median_ns = measure_parse_median_ns(&fx, samples)?;
        let mem_out = run_memcount_jsonl(repo_root, "baseline", Some(id))?;
        let parse_row = mem_out
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(MemoryRecord::parse_json_line)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .find(|r| r.operation == "parse_value")
            .ok_or_else(|| format!("memcount did not emit parse_value for `{id}`"))?;
        points.insert(
            id.to_owned(),
            ScalePoint {
                scenario: id.to_owned(),
                bytes: fx.bytes as u64,
                median_ns,
                peak_bytes: parse_row.peak_bytes,
                allocations: parse_row.allocations,
            },
        );
    }

    let mut records = Vec::with_capacity(SCALING_LADDERS.len());
    let mut failures = Vec::new();
    for &(family, small_id, large_id) in SCALING_LADDERS {
        let small = &points[small_id];
        let large = &points[large_id];
        let rec = evaluate_scaling_step(family, small, large, limits)?;
        if !rec.passed {
            failures.push(format!(
                "{family} ({small_id} -> {large_id}): time={:.2}x (limit {:.2}x), peak={:.2}x (limit {:.2}x), allocs={:.2}x (limit {:.2}x)",
                rec.normalized_time_ratio,
                limits.time,
                rec.normalized_peak_ratio,
                limits.peak,
                rec.normalized_allocs_ratio,
                limits.allocs,
            ));
        }
        records.push(rec);
    }

    fs::create_dir_all(out_dir).map_err(|e| format!("creating {}: {e}", out_dir.display()))?;
    let mut jsonl = String::new();
    for rec in &records {
        jsonl.push_str(&rec.to_json_line()?);
        jsonl.push('\n');
    }
    fs::write(out_dir.join("scale.jsonl"), jsonl)
        .map_err(|e| format!("writing scale.jsonl: {e}"))?;
    let manifest = ayaml_fixtures::manifest();
    let fp = Fingerprint::capture(repo_root, manifest.manifest_sha256);
    fs::write(out_dir.join("scale.fingerprint.json"), fp.to_pretty_json()?)
        .map_err(|e| format!("writing scale.fingerprint.json: {e}"))?;

    print!("{}", format_scale_table(&records));
    if !failures.is_empty() {
        return Err(format!(
            "linear scaling check failed on {} step(s):\n  {}",
            failures.len(),
            failures.join("\n  ")
        ));
    }
    Ok(())
}

/// Execute `cargo xtask compare --baseline DIR --current DIR [--out PATH]`.
///
/// # Errors
///
/// Returns an error if comparison or writing output fails.
pub(crate) fn run_compare(
    baseline: &Path,
    current: &Path,
    out: Option<&Path>,
) -> Result<(), String> {
    let md = compare_dirs(baseline, current)?;
    if let Some(path) = out {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        fs::write(path, md).map_err(|e| format!("writing {}: {e}", path.display()))?;
    } else {
        print!("{md}");
    }
    Ok(())
}

/// Execute `cargo xtask gate`.
///
/// When `--baseline DIR --current DIR` are given, validates and gates every matching JSONL
/// artifact between the two directories. Otherwise runs the repository's live allocation gates
/// (`tools/check_alloc_corpus.sh` and `tools/check_alloc_baseline.py`), plus optional `--base-rev`
/// Callgrind gate and `--scale` 10x linearity gate.
///
/// # Errors
///
/// Returns an error if any gate check fails.
pub(crate) fn run_gate(
    repo_root: &Path,
    baseline_dir: Option<&Path>,
    current_dir: Option<&Path>,
    base_rev: Option<&str>,
    with_scale: bool,
    thresholds: GateThresholds,
) -> Result<(), String> {
    if baseline_dir.is_some() != current_dir.is_some() {
        return Err("gate requires both --baseline DIR and --current DIR together".to_owned());
    }
    if let (Some(base), Some(cur)) = (baseline_dir, current_dir) {
        let mut violations = Vec::new();
        let mut checked = 0_usize;

        if base.join("memory.jsonl").is_file() && cur.join("memory.jsonl").is_file() {
            let b = read_memory_jsonl(&base.join("memory.jsonl"))?;
            let c = read_memory_jsonl(&cur.join("memory.jsonl"))?;
            violations.extend(evaluate_memory_gate(&b, &c, thresholds));
            checked += 1;
        }
        if base.join("callgrind.jsonl").is_file() && cur.join("callgrind.jsonl").is_file() {
            let b = read_callgrind_jsonl(&base.join("callgrind.jsonl"))?;
            let c = read_callgrind_jsonl(&cur.join("callgrind.jsonl"))?;
            violations.extend(evaluate_callgrind_gate(&b, &c, thresholds));
            checked += 1;
        }
        if base.join("speed.jsonl").is_file() && cur.join("speed.jsonl").is_file() {
            let b = read_speed_jsonl(&base.join("speed.jsonl"))?;
            let c = read_speed_jsonl(&cur.join("speed.jsonl"))?;
            violations.extend(evaluate_speed_gate(&b, &c, thresholds));
            checked += 1;
        }
        if checked == 0 {
            return Err(format!(
                "no matching JSONL files found between {} and {}",
                base.display(),
                cur.display()
            ));
        }
        if !violations.is_empty() {
            return Err(format!(
                "gate failed with {} violation(s):\n  {}",
                violations.len(),
                violations.join("\n  ")
            ));
        }
        println!("gate passed across {checked} artifact set(s)");
        return Ok(());
    }

    // Live repository gates: fixture digest check + allocation corpus + structured JSONL baseline.
    ayaml_fixtures::verify_manifest_digests()?;
    run_checked_cmd(repo_root, "bash", &["tools/check_alloc_corpus.sh"])?;
    run_checked_cmd(repo_root, "python3", &["tools/check_alloc_baseline.py"])?;
    if let Some(rev) = base_rev {
        let out_dir = repo_root.join("target").join("xtask-gate");
        run_det(repo_root, rev, &out_dir)?;
    }
    if with_scale {
        let out_dir = repo_root.join("target").join("xtask-gate");
        run_scale(repo_root, 5, ScaleLimits::default(), &out_dir)?;
    }
    println!("all requested gates passed");
    Ok(())
}

/// Execute `cargo xtask report --dir DIR [--title TEXT] [--out PATH]`.
///
/// # Errors
///
/// Returns an error if rendering or writing output fails.
pub(crate) fn run_report(dir: &Path, title: &str, out: Option<&Path>) -> Result<(), String> {
    let md = render_report(dir, title)?;
    if let Some(path) = out {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        fs::write(path, md).map_err(|e| format!("writing {}: {e}", path.display()))?;
    } else {
        print!("{md}");
    }
    Ok(())
}

fn run_memcount_jsonl(
    repo_root: &Path,
    suite: &str,
    fixture: Option<&str>,
) -> Result<String, String> {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(repo_root).args([
        "run",
        "--quiet",
        "--release",
        "--locked",
        "--manifest-path",
        "fuzz/Cargo.toml",
        "--bin",
        "memcount",
        "--",
        "--jsonl",
    ]);
    if let Some(id) = fixture {
        cmd.args(["--fixture", id]);
    } else {
        cmd.args(["--suite", suite]);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("failed to run memcount --jsonl: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("memcount --jsonl failed: {stderr}"));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("memcount output was not UTF-8: {e}"))
}

fn run_checked_cmd(repo_root: &Path, program: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(program)
        .current_dir(repo_root)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("failed to run {program}: {e}"))?;
    if !status.success() {
        return Err(format!(
            "{program} {} exited with status {}",
            args.join(" "),
            status.code().unwrap_or(1)
        ));
    }
    Ok(())
}
