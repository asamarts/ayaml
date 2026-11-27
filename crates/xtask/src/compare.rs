//! Baseline-to-head comparison diffs, deterministic gating logic, and Markdown report rendering.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::schema::{
    CallgrindRecord, Fingerprint, MemoryRecord, ScaleRecord, SpeedRecord, read_callgrind_jsonl,
    read_memory_jsonl, read_scale_jsonl, read_speed_jsonl,
};

/// Regression thresholds enforced by `cargo xtask gate` when comparing baseline vs current JSONL.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GateThresholds {
    /// Allowed percentage rise on memory byte metrics (`peak_bytes`, `live_bytes`, `total_allocated_bytes`).
    pub(crate) max_bytes_pct: f64,
    /// Minimum absolute byte headroom before failing tiny fixtures.
    pub(crate) min_bytes_floor: u64,
    /// Allowed percentage rise on allocation counts.
    pub(crate) max_allocs_pct: f64,
    /// Minimum absolute allocation headroom before failing tiny fixtures.
    pub(crate) min_allocs_floor: u64,
    /// Allowed percentage rise on steady-state Callgrind `Ir`.
    pub(crate) max_ir_pct: f64,
    /// Allowed percentage rise on steady-state Callgrind `EstimatedCycles`.
    pub(crate) max_cycles_pct: f64,
    /// Allowed percentage slowdown on optional `speed.jsonl` median wall time.
    pub(crate) max_speed_pct: f64,
}

impl Default for GateThresholds {
    fn default() -> Self {
        Self {
            max_bytes_pct: 10.0,
            min_bytes_floor: 512,
            max_allocs_pct: 5.0,
            min_allocs_floor: 4,
            max_ir_pct: 2.0,
            max_cycles_pct: 5.0,
            max_speed_pct: 10.0,
        }
    }
}

/// Compare two result directories containing any combination of `speed.jsonl`, `memory.jsonl`,
/// `callgrind.jsonl`, and `scale.jsonl` and return a Markdown diff report.
///
/// # Errors
///
/// Returns an error if no matching JSONL artifacts exist or any fixture SHA-256 drifted.
pub(crate) fn compare_dirs(base_dir: &Path, cur_dir: &Path) -> Result<String, String> {
    let mut out = String::from("# Benchmark Comparison\n\n");
    let mut sections = 0_usize;

    let base_speed = base_dir.join("speed.jsonl");
    let cur_speed = cur_dir.join("speed.jsonl");
    if base_speed.is_file() && cur_speed.is_file() {
        let b = read_speed_jsonl(&base_speed)?;
        let c = read_speed_jsonl(&cur_speed)?;
        out.push_str(&format_speed_diff(&b, &c)?);
        sections += 1;
    }

    let base_mem = base_dir.join("memory.jsonl");
    let cur_mem = cur_dir.join("memory.jsonl");
    if base_mem.is_file() && cur_mem.is_file() {
        let b = read_memory_jsonl(&base_mem)?;
        let c = read_memory_jsonl(&cur_mem)?;
        out.push_str(&format_memory_diff(&b, &c)?);
        sections += 1;
    }

    let base_cg = base_dir.join("callgrind.jsonl");
    let cur_cg = cur_dir.join("callgrind.jsonl");
    if base_cg.is_file() && cur_cg.is_file() {
        let b = read_callgrind_jsonl(&base_cg)?;
        let c = read_callgrind_jsonl(&cur_cg)?;
        out.push_str(&format_callgrind_diff(&b, &c)?);
        sections += 1;
    }

    if sections == 0 {
        return Err(format!(
            "no matching speed.jsonl, memory.jsonl, or callgrind.jsonl found in {} and {}",
            base_dir.display(),
            cur_dir.display()
        ));
    }
    Ok(out)
}

fn row_label(implementation: Option<&str>, scenario: &str, operation: &str) -> String {
    match implementation {
        Some(impl_name) => format!("{impl_name}/{scenario} [{operation}]"),
        None => format!("{scenario} [{operation}]"),
    }
}

/// Render a Markdown diff table for [`SpeedRecord`] rows, rejecting fixture SHA-256 drift.
///
/// # Errors
///
/// Returns an error if a shared `(implementation, scenario, operation)` key has differing
/// `fixture_sha256`.
pub(crate) fn format_speed_diff(
    baseline: &[SpeedRecord],
    current: &[SpeedRecord],
) -> Result<String, String> {
    let base_map: BTreeMap<_, _> = baseline.iter().map(|r| (r.key(), r)).collect();
    let show_impl = baseline.iter().any(|r| r.implementation.is_some())
        || current.iter().any(|r| r.implementation.is_some());
    let mut out = if show_impl {
        String::from(
            "## Speed (`median_ns`)\n\n\
             | implementation | scenario | operation | baseline (ns) | current (ns) | delta | throughput (MiB/s) |\n\
             | --- | --- | --- | ---: | ---: | ---: | ---: |\n",
        )
    } else {
        String::from(
            "## Speed (`median_ns`)\n\n\
             | scenario | operation | baseline (ns) | current (ns) | delta | throughput (MiB/s) |\n\
             | --- | --- | ---: | ---: | ---: | ---: |\n",
        )
    };
    let mut matched = 0_usize;
    for cur in current {
        let Some(base) = base_map.get(&cur.key()) else {
            continue;
        };
        if base.fixture_sha256 != cur.fixture_sha256 {
            return Err(format!(
                "fixture SHA-256 drift on {}: {} -> {}",
                row_label(
                    cur.implementation
                        .as_deref()
                        .or(base.implementation.as_deref()),
                    &cur.scenario,
                    &cur.operation
                ),
                base.fixture_sha256,
                cur.fixture_sha256
            ));
        }
        matched += 1;
        let delta = (cur.median_ns - base.median_ns) / base.median_ns * 100.0;
        if show_impl {
            let impl_name = cur
                .implementation
                .as_deref()
                .or(base.implementation.as_deref())
                .unwrap_or("ayaml");
            let _ = writeln!(
                out,
                "| {impl_name} | {} | {} | {:.1} | {:.1} | {:+.2}% | {:.2} |",
                cur.scenario,
                cur.operation,
                base.median_ns,
                cur.median_ns,
                delta,
                cur.throughput_mib_s
            );
        } else {
            let _ = writeln!(
                out,
                "| {} | {} | {:.1} | {:.1} | {:+.2}% | {:.2} |",
                cur.scenario,
                cur.operation,
                base.median_ns,
                cur.median_ns,
                delta,
                cur.throughput_mib_s
            );
        }
    }
    if matched == 0 && !baseline.is_empty() && !current.is_empty() {
        out.push_str(
            "\n> Note: zero matching `(implementation, scenario, operation)` rows between baseline and current `speed.jsonl`.\n",
        );
    }
    out.push('\n');
    Ok(out)
}

/// Render a Markdown diff table for [`MemoryRecord`] rows, rejecting fixture SHA-256 drift.
///
/// # Errors
///
/// Returns an error if a shared `(implementation, scenario, operation)` key has differing
/// `fixture_sha256`.
pub(crate) fn format_memory_diff(
    baseline: &[MemoryRecord],
    current: &[MemoryRecord],
) -> Result<String, String> {
    let base_map: BTreeMap<_, _> = baseline.iter().map(|r| (r.key(), r)).collect();
    let show_impl = baseline.iter().any(|r| r.implementation.is_some())
        || current.iter().any(|r| r.implementation.is_some());
    let mut out = if show_impl {
        String::from(
            "## Memory (`peak_bytes` / `allocations`)\n\n\
             | implementation | scenario | operation | base peak (B) | cur peak (B) | peak delta | base allocs | cur allocs | alloc delta |\n\
             | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |\n",
        )
    } else {
        String::from(
            "## Memory (`peak_bytes` / `allocations`)\n\n\
             | scenario | operation | base peak (B) | cur peak (B) | peak delta | base allocs | cur allocs | alloc delta |\n\
             | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |\n",
        )
    };
    let mut matched = 0_usize;
    for cur in current {
        let Some(base) = base_map.get(&cur.key()) else {
            continue;
        };
        if base.fixture_sha256 != cur.fixture_sha256 {
            return Err(format!(
                "fixture SHA-256 drift on {}: {} -> {}",
                row_label(
                    cur.implementation
                        .as_deref()
                        .or(base.implementation.as_deref()),
                    &cur.scenario,
                    &cur.operation
                ),
                base.fixture_sha256,
                cur.fixture_sha256
            ));
        }
        matched += 1;
        if show_impl {
            let impl_name = cur
                .implementation
                .as_deref()
                .or(base.implementation.as_deref())
                .unwrap_or("ayaml");
            let _ = writeln!(
                out,
                "| {impl_name} | {} | {} | {} | {} | {} | {} | {} | {} |",
                cur.scenario,
                cur.operation,
                base.peak_bytes,
                cur.peak_bytes,
                pct_u64(base.peak_bytes, cur.peak_bytes),
                base.allocations,
                cur.allocations,
                pct_u64(base.allocations, cur.allocations),
            );
        } else {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                cur.scenario,
                cur.operation,
                base.peak_bytes,
                cur.peak_bytes,
                pct_u64(base.peak_bytes, cur.peak_bytes),
                base.allocations,
                cur.allocations,
                pct_u64(base.allocations, cur.allocations),
            );
        }
    }
    if matched == 0 && !baseline.is_empty() && !current.is_empty() {
        out.push_str(
            "\n> Note: zero matching `(implementation, scenario, operation)` rows between baseline and current `memory.jsonl`.\n",
        );
    }
    out.push('\n');
    Ok(out)
}

/// Render a Markdown diff table for [`CallgrindRecord`] rows (`Ir` and `EstimatedCycles`).
///
/// # Errors
///
/// Returns an error if a shared `(workload, operation)` key has differing `fixture_sha256`.
pub(crate) fn format_callgrind_diff(
    baseline: &[CallgrindRecord],
    current: &[CallgrindRecord],
) -> Result<String, String> {
    let base_map: BTreeMap<_, _> = baseline.iter().map(|r| (r.key(), r)).collect();
    let mut out = String::from(
        "## Steady-state Callgrind (`Ir` / `EstimatedCycles`)\n\n\
         | workload | operation | base Ir | cur Ir | Ir delta | base EstCycles | cur EstCycles | EstCycles delta |\n\
         | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |\n",
    );
    let mut matched = 0_usize;
    for cur in current {
        let Some(base) = base_map.get(&cur.key()) else {
            continue;
        };
        if base.fixture_sha256 != cur.fixture_sha256 {
            return Err(format!(
                "fixture SHA-256 drift on {} ({}): {} -> {}",
                cur.workload, cur.operation, base.fixture_sha256, cur.fixture_sha256
            ));
        }
        matched += 1;
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            cur.workload,
            cur.operation,
            base.head_ir,
            cur.head_ir,
            pct_u64(base.head_ir, cur.head_ir),
            base.head_estimated_cycles,
            cur.head_estimated_cycles,
            pct_u64(base.head_estimated_cycles, cur.head_estimated_cycles),
        );
    }
    if matched == 0 && !baseline.is_empty() && !current.is_empty() {
        out.push_str(
            "\n> Note: zero matching `(workload, operation)` rows between baseline and current \
             `callgrind.jsonl` (for example, comparing `--legacy-probe` against Criterion-shimmed `det`).\n",
        );
    }
    out.push('\n');
    Ok(out)
}

/// Evaluate memory regression gates between `baseline` and `current` rows.
#[must_use]
pub(crate) fn evaluate_memory_gate(
    baseline: &[MemoryRecord],
    current: &[MemoryRecord],
    thresholds: GateThresholds,
) -> Vec<String> {
    let cur_map: BTreeMap<_, _> = current.iter().map(|r| (r.key(), r)).collect();
    let mut violations = Vec::new();
    for base in baseline {
        let key = base.key();
        let label = row_label(
            base.implementation.as_deref(),
            &base.scenario,
            &base.operation,
        );
        let Some(cur) = cur_map.get(&key) else {
            violations.push(format!("{label}: missing from current memory run"));
            continue;
        };
        if base.fixture_sha256 != cur.fixture_sha256 {
            violations.push(format!(
                "{label}: fixture_sha256 changed ({} -> {})",
                base.fixture_sha256, cur.fixture_sha256
            ));
            continue;
        }
        for (metric, bval, cval) in [
            ("peak_bytes", base.peak_bytes, cur.peak_bytes),
            ("live_bytes", base.live_bytes, cur.live_bytes),
            (
                "total_allocated_bytes",
                base.total_allocated_bytes,
                cur.total_allocated_bytes,
            ),
        ] {
            let limit = allowed_limit(bval, thresholds.max_bytes_pct, thresholds.min_bytes_floor);
            if cval > limit {
                violations.push(format!(
                    "{label}: {metric} regressed {bval} -> {cval} (limit {limit})"
                ));
            }
        }
        let alloc_limit = allowed_limit(
            base.allocations,
            thresholds.max_allocs_pct,
            thresholds.min_allocs_floor,
        );
        if cur.allocations > alloc_limit {
            violations.push(format!(
                "{label}: allocations regressed {} -> {} (limit {alloc_limit})",
                base.allocations, cur.allocations
            ));
        }
    }
    violations
}

/// Evaluate steady-state Callgrind `Ir` and `EstimatedCycles` regression gates.
#[must_use]
pub(crate) fn evaluate_callgrind_gate(
    baseline: &[CallgrindRecord],
    current: &[CallgrindRecord],
    thresholds: GateThresholds,
) -> Vec<String> {
    let cur_map: BTreeMap<_, _> = current.iter().map(|r| (r.key(), r)).collect();
    let mut violations = Vec::new();
    for base in baseline {
        let key = base.key();
        let Some(cur) = cur_map.get(&key) else {
            violations.push(format!(
                "{} [{}]: missing from current callgrind run",
                base.workload, base.operation
            ));
            continue;
        };
        if base.fixture_sha256 != cur.fixture_sha256 {
            violations.push(format!(
                "{} [{}]: fixture_sha256 changed",
                base.workload, base.operation
            ));
            continue;
        }
        let ir_pct = raw_pct_u64(base.head_ir, cur.head_ir);
        if ir_pct > thresholds.max_ir_pct {
            violations.push(format!(
                "{} [{}]: Ir regressed {} -> {} ({ir_pct:+.2}% > +{:.2}%)",
                base.workload, base.operation, base.head_ir, cur.head_ir, thresholds.max_ir_pct
            ));
        }
        let cyc_pct = raw_pct_u64(base.head_estimated_cycles, cur.head_estimated_cycles);
        if cyc_pct > thresholds.max_cycles_pct {
            violations.push(format!(
                "{} [{}]: EstimatedCycles regressed {} -> {} ({cyc_pct:+.2}% > +{:.2}%)",
                base.workload,
                base.operation,
                base.head_estimated_cycles,
                cur.head_estimated_cycles,
                thresholds.max_cycles_pct
            ));
        }
    }
    violations
}

/// Evaluate wall-clock speed regression gates between `baseline` and `current` rows.
#[must_use]
pub(crate) fn evaluate_speed_gate(
    baseline: &[SpeedRecord],
    current: &[SpeedRecord],
    thresholds: GateThresholds,
) -> Vec<String> {
    let cur_map: BTreeMap<_, _> = current.iter().map(|r| (r.key(), r)).collect();
    let mut violations = Vec::new();
    for base in baseline {
        let key = base.key();
        let label = row_label(
            base.implementation.as_deref(),
            &base.scenario,
            &base.operation,
        );
        let Some(cur) = cur_map.get(&key) else {
            violations.push(format!("{label}: missing from current speed run"));
            continue;
        };
        if base.fixture_sha256 != cur.fixture_sha256 {
            violations.push(format!("{label}: fixture_sha256 changed"));
            continue;
        }
        let pct = (cur.median_ns - base.median_ns) / base.median_ns * 100.0;
        if pct > thresholds.max_speed_pct {
            violations.push(format!(
                "{label}: median_ns slowed {:.1} -> {:.1} ({pct:+.2}% > +{:.2}%)",
                base.median_ns, cur.median_ns, thresholds.max_speed_pct
            ));
        }
    }
    violations
}

/// Render a complete Markdown summary report for all JSONL artifacts in `dir`.
///
/// # Errors
///
/// Returns an error if no valid JSONL artifacts exist or any file fails schema validation.
pub(crate) fn render_report(dir: &Path, title: &str) -> Result<String, String> {
    let mut out = format!("# {title}\n\n");
    if let Some(fp) = read_any_fingerprint(dir)? {
        let sha_short: String = fp.git_sha.chars().take(12).collect();
        let _ = writeln!(
            out,
            "- **Platform:** `{}/{}` ({}, {} logical CPUs)",
            fp.os, fp.arch, fp.cpu, fp.cpu_count
        );
        if let Some(ref kernel) = fp.kernel {
            let _ = writeln!(out, "- **Kernel:** `{kernel}`");
        }
        let _ = writeln!(out, "- **Compiler:** `{}`", fp.rustc);
        let _ = writeln!(out, "- **Commit:** `{sha_short}`");
        if let Some(ref captured_date) = fp.captured_date {
            let _ = writeln!(out, "- **Captured:** `{captured_date}`");
        }
        let _ = writeln!(
            out,
            "- **Fixture manifest SHA-256:** `{}`\n",
            fp.fixture_manifest_sha256
        );
    }

    let mut sections = 0_usize;

    let speed_path = dir.join("speed.jsonl");
    if speed_path.is_file() {
        let rows = read_speed_jsonl(&speed_path)?;
        let show_impl = rows.iter().any(|r| r.implementation.is_some());
        if show_impl {
            out.push_str(
                "## Speed\n\n\
                 | implementation | scenario | operation | bytes | median (ns) | stddev (ns) | throughput (MiB/s) |\n\
                 | --- | --- | --- | ---: | ---: | ---: | ---: |\n",
            );
            for r in &rows {
                let impl_name = r.implementation.as_deref().unwrap_or("ayaml");
                let _ = writeln!(
                    out,
                    "| {impl_name} | {} | {} | {} | {:.1} | {:.1} | {:.2} |",
                    r.scenario, r.operation, r.bytes, r.median_ns, r.stddev_ns, r.throughput_mib_s
                );
            }
        } else {
            out.push_str(
                "## Speed\n\n\
                 | scenario | operation | bytes | median (ns) | stddev (ns) | throughput (MiB/s) |\n\
                 | --- | --- | ---: | ---: | ---: | ---: |\n",
            );
            for r in &rows {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {:.1} | {:.1} | {:.2} |",
                    r.scenario, r.operation, r.bytes, r.median_ns, r.stddev_ns, r.throughput_mib_s
                );
            }
        }
        out.push('\n');
        sections += 1;
    }

    let mem_path = dir.join("memory.jsonl");
    if mem_path.is_file() {
        let rows = read_memory_jsonl(&mem_path)?;
        let show_impl = rows.iter().any(|r| r.implementation.is_some());
        if show_impl {
            out.push_str(
                "## Memory\n\n\
                 | implementation | scenario | operation | bytes | peak (B) | live (B) | allocs | total (B) | peak/input |\n\
                 | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |\n",
            );
            for r in &rows {
                let impl_name = r.implementation.as_deref().unwrap_or("ayaml");
                let _ = writeln!(
                    out,
                    "| {impl_name} | {} | {} | {} | {} | {} | {} | {} | {:.2}x |",
                    r.scenario,
                    r.operation,
                    r.bytes,
                    r.peak_bytes,
                    r.live_bytes,
                    r.allocations,
                    r.total_allocated_bytes,
                    r.peak_to_input_ratio
                );
            }
        } else {
            out.push_str(
                "## Memory\n\n\
                 | scenario | operation | bytes | peak (B) | live (B) | allocs | total (B) | peak/input |\n\
                 | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |\n",
            );
            for r in &rows {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} | {} | {} | {:.2}x |",
                    r.scenario,
                    r.operation,
                    r.bytes,
                    r.peak_bytes,
                    r.live_bytes,
                    r.allocations,
                    r.total_allocated_bytes,
                    r.peak_to_input_ratio
                );
            }
        }
        out.push('\n');
        sections += 1;
    }

    let cg_path = dir.join("callgrind.jsonl");
    if cg_path.is_file() {
        let rows = read_callgrind_jsonl(&cg_path)?;
        out.push_str(
            "## Steady-state Callgrind\n\n\
             | workload | operation | head Ir | head EstCycles | l1_misses | ll_misses | branch_mispredicts | status |\n\
             | --- | --- | ---: | ---: | ---: | ---: | ---: | --- |\n",
        );
        for r in &rows {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                r.workload,
                r.operation,
                r.head_ir,
                r.head_estimated_cycles,
                r.l1_misses,
                r.ll_misses,
                r.branch_mispredicts,
                r.status
            );
        }
        out.push('\n');
        sections += 1;
    }

    let scale_path = dir.join("scale.jsonl");
    if scale_path.is_file() {
        let rows = read_scale_jsonl(&scale_path)?;
        out.push_str(&format_scale_table(&rows));
        sections += 1;
    }

    if sections == 0 {
        return Err(format!(
            "no speed.jsonl, memory.jsonl, callgrind.jsonl, or scale.jsonl found in {}",
            dir.display()
        ));
    }
    Ok(out)
}

/// Format a Markdown table of 10x linear scaling verification results.
#[must_use]
pub(crate) fn format_scale_table(rows: &[ScaleRecord]) -> String {
    let mut out = String::from(
        "## Linear Scaling (10x ladders)\n\n\
         | family | step | size ratio | time ratio (norm) | peak ratio (norm) | allocs ratio (norm) | result |\n\
         | --- | --- | ---: | ---: | ---: | ---: | --- |\n",
    );
    for r in rows {
        let status = if r.passed { "PASS" } else { "FAIL" };
        let _ = writeln!(
            out,
            "| {} | {} -> {} | {:.2}x | {:.2}x | {:.2}x | {:.2}x | {status} |",
            r.family,
            r.small_scenario,
            r.large_scenario,
            r.size_ratio,
            r.normalized_time_ratio,
            r.normalized_peak_ratio,
            r.normalized_allocs_ratio
        );
    }
    out.push('\n');
    out
}

fn read_any_fingerprint(dir: &Path) -> Result<Option<Fingerprint>, String> {
    for name in [
        "speed.fingerprint.json",
        "memory.fingerprint.json",
        "callgrind.fingerprint.json",
        "scale.fingerprint.json",
        "fingerprint.json",
    ] {
        let p = dir.join(name);
        if p.is_file() {
            let txt =
                fs::read_to_string(&p).map_err(|e| format!("reading {}: {e}", p.display()))?;
            return Ok(Some(Fingerprint::parse_json(&txt)?));
        }
    }
    Ok(None)
}

fn allowed_limit(baseline: u64, pct: f64, floor: u64) -> u64 {
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "baseline byte/allocation headroom calculation on non-negative values"
    )]
    let pct_delta = ((baseline as f64) * pct / 100.0).ceil() as u64;
    baseline.saturating_add(pct_delta.max(floor))
}

fn raw_pct_u64(base: u64, cur: u64) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "percentage delta display over counter magnitudes"
    )]
    let denom = base.max(1) as f64;
    #[allow(
        clippy::cast_precision_loss,
        reason = "percentage delta display over counter magnitudes"
    )]
    let diff = (cur as f64) - (base as f64);
    diff * 100.0 / denom
}

fn pct_u64(base: u64, cur: u64) -> String {
    if base == 0 {
        return if cur == 0 {
            "+0.00%".to_owned()
        } else {
            "n/a".to_owned()
        };
    }
    format!("{:+.2}%", raw_pct_u64(base, cur))
}
