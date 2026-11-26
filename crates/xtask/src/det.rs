//! Deterministic steady-state Callgrind benchmarking over the standard Criterion `ayaml-bench`
//! suite via a lightweight counting Criterion shim, with optional `--base` and long-horizon
//! `--anchor` regression gating.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde_json::Value;

use crate::schema::{CallgrindRecord, Fingerprint, SCHEMA_VERSION};

const SHIM_SOURCE: &str = include_str!("criterion_shim.rs");

/// Curated fast, high-signal PR suite spanning every parser/serializer layer,
/// 4 KiB strategy boundary, core domain, scaling point, structural hotspot,
/// spanned parsing, and late typed decode error.
pub(crate) const PR_SUITE_IDS: &[&str] = &[
    "layers/scan/openapi-spec",
    "layers/events/openapi-spec",
    "layers/parse_value/openapi-spec",
    "layers/from_str_value/openapi-spec",
    "layers/from_str_typed_owned/frontmatter-tiny",
    "layers/from_str_typed_borrowed/frontmatter-tiny",
    "layers/emit_value/openapi-spec",
    "boundary_4kb/from_str_owned/boundary-4kb-below",
    "boundary_4kb/from_str_borrowed/boundary-4kb-below",
    "boundary_4kb/from_str_owned/boundary-4kb-above",
    "boundary_4kb/from_str_borrowed/boundary-4kb-above",
    "domains/parse_documents/ci-workflow",
    "domains/parse_documents/json-in-yaml",
    "domains/parse_documents/lockfile-wide",
    "domains/parse_documents/escape-routes",
    "domains/parse_documents/unicode-multibyte",
    "scaling/parse_value/scale-map-100kb",
    "structural/parse_value/depth-64",
    "structural/parse_value/aliases-1000",
    "structural/parse_value/large-scalar-1mb",
    "spans/spanned_tree/ci-workflow",
    "errors/from_str_err/typed-mismatch-late",
];

/// Suite selection for `cargo xtask det`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DetSuite {
    /// Curated fast PR suite covering every layer and hotspot.
    Pr,
    /// Every Criterion benchmark registered in `ayaml-bench`.
    Full,
}

impl DetSuite {
    /// Parse a CLI `--suite` argument (`pr` or `full`).
    ///
    /// # Errors
    ///
    /// Returns an error for unrecognized suite names.
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "pr" => Ok(Self::Pr),
            "full" => Ok(Self::Full),
            other => Err(format!(
                "invalid --suite value {other:?} (expected `pr` or `full`)"
            )),
        }
    }
}

/// Configuration for `cargo xtask det`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DetOptions {
    /// Selected benchmark suite (`pr` or `full`).
    pub(crate) suite: DetSuite,
    /// Optional substring filter applied to benchmark IDs.
    pub(crate) filter: Option<String>,
    /// Optional git ref to compare `HEAD`/working tree against.
    pub(crate) base_rev: Option<String>,
    /// Optional long-horizon anchor git ref to catch multi-PR cumulative creep.
    pub(crate) anchor_rev: Option<String>,
    /// Print discovered benchmark IDs without executing Callgrind.
    pub(crate) list_only: bool,
    /// Allowed percentage rise on `Ir` vs `--base`.
    pub(crate) max_ir_pct: f64,
    /// Allowed percentage rise on `EstimatedCycles` vs `--base`.
    pub(crate) max_cycles_pct: f64,
    /// Allowed percentage rise on `Ir` vs `--anchor`.
    pub(crate) anchor_ir_pct: f64,
    /// Allowed percentage rise on `EstimatedCycles` vs `--anchor`.
    pub(crate) anchor_cycles_pct: f64,
    /// Maximum concurrent Valgrind workers.
    pub(crate) jobs: usize,
    /// Output directory for `callgrind.jsonl` and fingerprint artifacts.
    pub(crate) out_dir: PathBuf,
}

/// Steady-state per-iteration Callgrind hardware counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CallgrindCounters {
    /// Instructions executed (`Ir`).
    pub(crate) ir: u64,
    /// Weighted simulated cycles (`Ir + 10*(I1mr+D1mr+D1mw) + 100*(ILmr+DLmr+DLmw) + 20*(Bcm+Bim)`).
    pub(crate) estimated_cycles: u64,
    /// Combined first-level cache misses (`I1mr + D1mr + D1mw`).
    pub(crate) l1_misses: u64,
    /// Combined last-level cache misses (`ILmr + DLmr + DLmw`).
    pub(crate) ll_misses: u64,
    /// Combined branch mispredictions (`Bcm + Bim`).
    pub(crate) branch_mispredicts: u64,
}

/// Result of comparing one benchmark cell against optional `--base` and `--anchor` refs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DetComparison {
    /// Canonical benchmark ID (`group/op/scenario`).
    pub(crate) bench_id: String,
    /// Working-tree steady-state counters.
    pub(crate) head: CallgrindCounters,
    /// Optional `--base` steady-state counters.
    pub(crate) base: Option<CallgrindCounters>,
    /// Optional `--anchor` steady-state counters.
    pub(crate) anchor: Option<CallgrindCounters>,
    /// Percentage delta of `head.ir` vs `base.ir`.
    pub(crate) base_ir_pct: Option<f64>,
    /// Percentage delta of `head.estimated_cycles` vs `base.estimated_cycles`.
    pub(crate) base_cycles_pct: Option<f64>,
    /// Percentage delta of `head.ir` vs `anchor.ir`.
    pub(crate) anchor_ir_pct: Option<f64>,
    /// Percentage delta of `head.estimated_cycles` vs `anchor.estimated_cycles`.
    pub(crate) anchor_cycles_pct: Option<f64>,
    /// `"ok"`, `"new"`, or `"fail"`.
    pub(crate) status: String,
    /// Human-readable violation reasons, if any.
    pub(crate) failures: Vec<String>,
}

/// RAII guard that removes a temporary directory on drop.
#[derive(Debug)]
pub(crate) struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    pub(crate) fn new(path: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&path).map_err(|e| format!("creating {}: {e}", path.display()))?;
        Ok(Self { path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        drop(fs::remove_dir_all(&self.path));
    }
}

/// Built shimmed `ayaml-bench` binary and its discovered benchmark IDs.
#[derive(Debug)]
pub(crate) struct BuiltBench {
    /// Path to the copied bench binary under a PID-scoped scratch directory.
    pub(crate) binary: PathBuf,
    /// All benchmark IDs reported via `AYAML_DET_LIST=1`.
    pub(crate) ids: Vec<String>,
    _dir_guard: TempDirGuard,
}

/// RAII guard that restores the repository's `Cargo.lock` after a temporary patched build.
struct LockfileGuard {
    path: PathBuf,
    original: Vec<u8>,
}

impl LockfileGuard {
    fn capture(repo_root: &Path) -> Result<Self, String> {
        let path = repo_root.join("Cargo.lock");
        let original = fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        Ok(Self { path, original })
    }

    fn restore(&self) {
        if fs::read(&self.path).ok().as_deref() != Some(self.original.as_slice())
            && let Err(err) = fs::write(&self.path, &self.original)
        {
            eprintln!(
                "xtask warning: failed to restore {} after shimmed build: {err}",
                self.path.display()
            );
        }
    }
}

impl Drop for LockfileGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// RAII guard that creates and cleans up a temporary detached git worktree.
#[derive(Debug)]
pub(crate) struct WorktreeGuard {
    repo_root: PathBuf,
    worktree: PathBuf,
}

impl WorktreeGuard {
    /// Create a detached temporary worktree for `sha` under `worktree`.
    ///
    /// # Errors
    ///
    /// Returns an error if `git worktree add` fails.
    pub(crate) fn create(
        repo_root: &Path,
        worktree: PathBuf,
        sha: &str,
        git_ref: &str,
    ) -> Result<Self, String> {
        drop(
            Command::new("git")
                .current_dir(repo_root)
                .args(["worktree", "prune"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
        );
        if worktree.exists() {
            drop(
                Command::new("git")
                    .current_dir(repo_root)
                    .args(["worktree", "remove", "--force", &worktree.to_string_lossy()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status(),
            );
            drop(fs::remove_dir_all(&worktree));
        }
        let status = Command::new("git")
            .current_dir(repo_root)
            .args([
                "worktree",
                "add",
                "--detach",
                &worktree.to_string_lossy(),
                sha,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|e| format!("running git worktree add for `{git_ref}`: {e}"))?;
        if !status.success() {
            return Err(format!("git worktree add failed for ref `{git_ref}`"));
        }
        Ok(Self {
            repo_root: repo_root.to_path_buf(),
            worktree,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.worktree
    }
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        drop(
            Command::new("git")
                .current_dir(&self.repo_root)
                .args([
                    "worktree",
                    "remove",
                    "--force",
                    &self.worktree.to_string_lossy(),
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
        );
        drop(fs::remove_dir_all(&self.worktree));
    }
}

/// Run the Criterion-shimmed deterministic Callgrind suite.
///
/// # Errors
///
/// Returns an error if Valgrind is unavailable, git refs are invalid, builds fail,
/// `--base`/`--anchor` share zero benchmarks with the selected suite, or any gate fails.
pub(crate) fn run_det_suite(repo_root: &Path, opts: &DetOptions) -> Result<(), String> {
    ayaml_fixtures::verify_manifest_digests()?;
    let workdir = det_workdir(repo_root)?;

    // Fail fast on bad git refs or missing Valgrind before building or measuring anything.
    let base_sha = opts
        .base_rev
        .as_deref()
        .map(|rev| resolve_git_sha(repo_root, rev))
        .transpose()?;
    let anchor_sha = opts
        .anchor_rev
        .as_deref()
        .map(|rev| resolve_git_sha(repo_root, rev))
        .transpose()?;
    if !opts.list_only {
        ensure_valgrind()?;
    }

    let head = build_working_tree_bench(repo_root, &workdir)?;
    let selected = select_benchmarks(&head.ids, opts.suite, opts.filter.as_deref());
    if opts.list_only {
        for id in &selected {
            println!("{id}");
        }
        eprintln!(
            "listed {} benchmark(s) (total discovered: {})",
            selected.len(),
            head.ids.len()
        );
        return Ok(());
    }
    if selected.is_empty() {
        return Err("no ayaml-bench benchmarks matched the selected suite/filter".to_owned());
    }

    eprintln!(
        "[xtask det] measuring {} benchmark(s) on working tree ({} worker(s))",
        selected.len(),
        opts.jobs.max(1)
    );
    let head_counts = measure_bench_list(&head.binary, &selected, &workdir, "head", opts.jobs)?;

    let base_counts = if let Some(ref rev) = opts.base_rev {
        let built = build_git_ref_bench(repo_root, &workdir, rev, "base")?;
        let common: Vec<String> = selected
            .iter()
            .filter(|id| built.ids.iter().any(|b| b == *id))
            .cloned()
            .collect();
        if common.is_empty() {
            return Err(format!(
                "base ref `{rev}` shares 0 selected benchmark IDs with the working tree \
                 (cannot gate against a ref predating the current ayaml-bench benchmark IDs)"
            ));
        }
        eprintln!(
            "[xtask det] measuring {} common benchmark(s) on base `{rev}`",
            common.len()
        );
        Some(measure_bench_list(
            &built.binary,
            &common,
            &workdir,
            "base",
            opts.jobs,
        )?)
    } else {
        None
    };

    let anchor_counts = if let Some(ref rev) = opts.anchor_rev {
        if base_sha.is_some() && base_sha == anchor_sha {
            eprintln!("[xtask det] anchor `{rev}` matches base commit; reusing base measurements");
            base_counts.clone()
        } else {
            let built = build_git_ref_bench(repo_root, &workdir, rev, "anchor")?;
            let common: Vec<String> = selected
                .iter()
                .filter(|id| built.ids.iter().any(|b| b == *id))
                .cloned()
                .collect();
            if common.is_empty() {
                return Err(format!(
                    "anchor ref `{rev}` shares 0 selected benchmark IDs with the working tree \
                     (cannot gate against a ref predating the current ayaml-bench benchmark IDs)"
                ));
            }
            eprintln!(
                "[xtask det] measuring {} common benchmark(s) on anchor `{rev}`",
                common.len()
            );
            Some(measure_bench_list(
                &built.binary,
                &common,
                &workdir,
                "anchor",
                opts.jobs,
            )?)
        }
    } else {
        None
    };

    let comparisons = evaluate_det_comparisons(
        &selected,
        &head_counts,
        base_counts.as_ref(),
        anchor_counts.as_ref(),
        opts.max_ir_pct,
        opts.max_cycles_pct,
        opts.anchor_ir_pct,
        opts.anchor_cycles_pct,
    );

    write_det_outputs(repo_root, &opts.out_dir, &comparisons)?;
    print!("{}", render_det_table(&comparisons));

    let failures: Vec<String> = comparisons
        .iter()
        .flat_map(|c| c.failures.iter().cloned())
        .collect();
    if !failures.is_empty() {
        return Err(format!(
            "deterministic Callgrind gate failed with {} violation(s):\n  {}",
            failures.len(),
            failures.join("\n  ")
        ));
    }
    Ok(())
}

/// Extract the exact locked version of `pkg_name` from a `Cargo.lock` document.
#[must_use]
pub(crate) fn locked_version(lock_text: &str, pkg_name: &str) -> Option<String> {
    let target = format!("name = \"{pkg_name}\"");
    let mut in_pkg = false;
    for line in lock_text.lines() {
        if line.starts_with("[[package]]") {
            in_pkg = false;
        } else if line.trim() == target {
            in_pkg = true;
        } else if in_pkg && let Some(ver) = line.trim().strip_prefix("version = \"") {
            return ver.strip_suffix('"').map(str::to_owned);
        }
    }
    None
}

/// Return the `(K1, K2)` steady-state Callgrind iteration pair for a benchmark ID.
///
/// `K1` warms lazy process-wide state (`OnceLock` hash seeds, allocators, thread caches) and is
/// subtracted from `K2` so `(counts(K2) - counts(K1)) / (K2 - K1)` reflects pure steady-state
/// work with `< 0.1%` run-to-run variance even on small randomized-hash workloads.
#[must_use]
pub(crate) fn iteration_counts_for(bench_id: &str) -> (usize, usize) {
    if bench_id.contains("frontmatter-tiny")
        || bench_id.contains("depth-64")
        || bench_id.contains("err-syntax-early")
        || bench_id.contains("err-depth-limit")
        || bench_id.contains("line_index_new")
        || bench_id.contains("line_index_queries")
        || bench_id.contains("boundary-4kb-below")
        || bench_id.contains("boundary-4kb-above")
        || bench_id.contains("aliases-1000")
        || bench_id.contains("unicode-multibyte")
        || bench_id.contains("escape-routes")
    {
        return (5, 69);
    }
    if bench_id.contains("-1mb")
        || bench_id.contains("-20000")
        || bench_id.contains("-256kb")
        || bench_id.contains("-100kb")
        || bench_id.contains("bulk_32")
        || bench_id.contains("nested-keys")
        || bench_id.contains("openapi-spec")
        || bench_id.contains("cluster-manifests")
        || bench_id.contains("lockfile-wide")
    {
        return (1, 4);
    }
    (2, 10)
}

/// Select benchmarks matching the requested suite and optional substring filter.
#[must_use]
pub(crate) fn select_benchmarks(
    available: &[String],
    suite: DetSuite,
    filter: Option<&str>,
) -> Vec<String> {
    let available_set: BTreeSet<&str> = available.iter().map(String::as_str).collect();
    let base_list: Vec<String> = match suite {
        DetSuite::Pr => PR_SUITE_IDS
            .iter()
            .copied()
            .filter(|id| available_set.contains(id))
            .map(str::to_owned)
            .collect(),
        DetSuite::Full => available.to_vec(),
    };
    match filter {
        None => base_list,
        Some(needle) => base_list
            .into_iter()
            .filter(|id| id.contains(needle))
            .collect(),
    }
}

/// Parse raw `events:` and `summary:` totals from a Valgrind Callgrind output file.
///
/// # Errors
///
/// Returns an error if the output is malformed, has an empty summary, or reports zero `Ir`.
pub(crate) fn parse_callgrind_out(text: &str) -> Result<BTreeMap<String, u64>, String> {
    let mut names: Option<Vec<&str>> = None;
    let mut counts: Option<Vec<u64>> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("events:") {
            names = Some(rest.split_whitespace().collect());
        } else if let Some(rest) = line.strip_prefix("summary:") {
            let parsed: Result<Vec<u64>, _> = rest
                .split_whitespace()
                .map(|tok| {
                    tok.parse::<u64>()
                        .map_err(|e| format!("invalid Callgrind counter {tok:?}: {e}"))
                })
                .collect();
            counts = Some(parsed?);
        }
    }
    let names = names.ok_or_else(|| "Callgrind output missing `events:` header".to_owned())?;
    let counts = counts.ok_or_else(|| "Callgrind output missing `summary:` line".to_owned())?;
    if counts.is_empty() {
        return Err("Callgrind output has an empty `summary:` line".to_owned());
    }
    if counts.len() > names.len() {
        return Err(format!(
            "Callgrind summary has {} counts for {} events",
            counts.len(),
            names.len()
        ));
    }
    let mut out = BTreeMap::new();
    for (idx, name) in names.iter().enumerate() {
        out.insert((*name).to_owned(), counts.get(idx).copied().unwrap_or(0));
    }
    match out.get("Ir").copied() {
        None => return Err("Callgrind output missing required `Ir` counter".to_owned()),
        Some(0) => {
            return Err(
                "Callgrind recorded 0 instructions (`Ir = 0`); measurement toggle symbol did not fire"
                    .to_owned(),
            );
        }
        Some(_) => {}
    }
    Ok(out)
}

/// Compute simulated `EstimatedCycles = Ir + 10*(I1mr+D1mr+D1mw) + 100*(ILmr+DLmr+DLmw) + 20*(Bcm+Bim)`.
#[must_use]
pub(crate) fn estimated_cycles(ir: u64, l1: u64, ll: u64, br: u64) -> u64 {
    ir.saturating_add(10_u64.saturating_mul(l1))
        .saturating_add(100_u64.saturating_mul(ll))
        .saturating_add(20_u64.saturating_mul(br))
}

fn div_round_u64(numer: u64, denom: u64) -> u64 {
    u64::try_from((u128::from(numer) + u128::from(denom) / 2) / u128::from(denom))
        .unwrap_or(u64::MAX)
}

/// Subtract warmup `k1` counters from `k2` counters and divide by `delta_iterations`
/// using half-up rounding after summing raw event groups.
///
/// # Errors
///
/// Returns an error if `delta_iterations == 0` or if `k2["Ir"] <= k1["Ir"]`.
pub(crate) fn steady_state_delta(
    k1: &BTreeMap<String, u64>,
    k2: &BTreeMap<String, u64>,
    delta_iterations: u64,
) -> Result<CallgrindCounters, String> {
    if delta_iterations == 0 {
        return Err("delta_iterations must be >= 1".to_owned());
    }
    let ir1 = k1.get("Ir").copied().unwrap_or(0);
    let ir2 = k2.get("Ir").copied().unwrap_or(0);
    if ir2 <= ir1 {
        return Err(format!(
            "invalid Callgrind steady-state counters: k2 Ir ({ir2}) <= k1 Ir ({ir1})"
        ));
    }
    let raw_diff = |key: &str| -> u64 {
        k2.get(key)
            .copied()
            .unwrap_or(0)
            .saturating_sub(k1.get(key).copied().unwrap_or(0))
    };
    let ir_raw = ir2 - ir1;
    let l1_raw = raw_diff("I1mr")
        .saturating_add(raw_diff("D1mr"))
        .saturating_add(raw_diff("D1mw"));
    let ll_raw = raw_diff("ILmr")
        .saturating_add(raw_diff("DLmr"))
        .saturating_add(raw_diff("DLmw"));
    let br_raw = raw_diff("Bcm").saturating_add(raw_diff("Bim"));
    let raw_cycles = estimated_cycles(ir_raw, l1_raw, ll_raw, br_raw);

    Ok(CallgrindCounters {
        ir: div_round_u64(ir_raw, delta_iterations),
        estimated_cycles: div_round_u64(raw_cycles, delta_iterations),
        l1_misses: div_round_u64(l1_raw, delta_iterations),
        ll_misses: div_round_u64(ll_raw, delta_iterations),
        branch_mispredicts: div_round_u64(br_raw, delta_iterations),
    })
}

/// Evaluate base and long-horizon anchor regression thresholds across all measured cells.
#[must_use]
#[allow(
    clippy::too_many_arguments,
    reason = "explicit threshold parameters keep unit tests direct without wrapper boilerplate"
)]
pub(crate) fn evaluate_det_comparisons(
    bench_ids: &[String],
    head_map: &BTreeMap<String, CallgrindCounters>,
    base_map: Option<&BTreeMap<String, CallgrindCounters>>,
    anchor_map: Option<&BTreeMap<String, CallgrindCounters>>,
    max_ir_pct: f64,
    max_cycles_pct: f64,
    anchor_ir_pct: f64,
    anchor_cycles_pct: f64,
) -> Vec<DetComparison> {
    let mut rows = Vec::with_capacity(bench_ids.len());
    for id in bench_ids {
        let Some(&head) = head_map.get(id) else {
            continue;
        };
        let base = base_map.and_then(|m| m.get(id).copied());
        let anchor = anchor_map.and_then(|m| m.get(id).copied());
        let base_ir_pct = base.and_then(|b| pct_delta(b.ir, head.ir));
        let base_cycles_pct =
            base.and_then(|b| pct_delta(b.estimated_cycles, head.estimated_cycles));
        let anchor_ir_pct_val = anchor.and_then(|a| pct_delta(a.ir, head.ir));
        let anchor_cycles_pct_val =
            anchor.and_then(|a| pct_delta(a.estimated_cycles, head.estimated_cycles));

        let mut failures = Vec::new();
        if let Some(pct) = base_ir_pct
            && pct > max_ir_pct
        {
            failures.push(format!(
                "{id}: Ir rose {pct:+.2}% vs base (limit +{max_ir_pct:.2}%)"
            ));
        }
        if let Some(pct) = base_cycles_pct
            && pct > max_cycles_pct
        {
            failures.push(format!(
                "{id}: EstimatedCycles rose {pct:+.2}% vs base (limit +{max_cycles_pct:.2}%)"
            ));
        }
        if let Some(pct) = anchor_ir_pct_val
            && pct > anchor_ir_pct
        {
            failures.push(format!(
                "{id}: Ir rose {pct:+.2}% vs anchor (limit +{anchor_ir_pct:.2}%)"
            ));
        }
        if let Some(pct) = anchor_cycles_pct_val
            && pct > anchor_cycles_pct
        {
            failures.push(format!(
                "{id}: EstimatedCycles rose {pct:+.2}% vs anchor (limit +{anchor_cycles_pct:.2}%)"
            ));
        }

        let is_new =
            (base_map.is_some() && base.is_none()) || (anchor_map.is_some() && anchor.is_none());
        let status = if !failures.is_empty() {
            "fail".to_owned()
        } else if is_new {
            "new".to_owned()
        } else {
            "ok".to_owned()
        };

        rows.push(DetComparison {
            bench_id: id.clone(),
            head,
            base,
            anchor,
            base_ir_pct,
            base_cycles_pct,
            anchor_ir_pct: anchor_ir_pct_val,
            anchor_cycles_pct: anchor_cycles_pct_val,
            status,
            failures,
        });
    }
    rows
}

#[allow(
    clippy::cast_precision_loss,
    reason = "percentage deltas over Callgrind counters fit comfortably in f64 precision"
)]
fn pct_delta(before: u64, after: u64) -> Option<f64> {
    if before == 0 {
        return None;
    }
    Some(((after as f64) - (before as f64)) / (before as f64) * 100.0)
}

/// Render a readable Markdown table summarizing deterministic Callgrind results.
#[must_use]
pub(crate) fn render_det_table(rows: &[DetComparison]) -> String {
    let has_base = rows.iter().any(|r| r.base.is_some());
    let has_anchor = rows.iter().any(|r| r.anchor.is_some());
    let mut out = String::from("# Deterministic Callgrind Results (`ayaml-bench`)\n\n");
    out.push_str("| benchmark | head Ir | head EstCycles |");
    if has_base {
        out.push_str(" base Ir % | base Cyc % |");
    }
    if has_anchor {
        out.push_str(" anchor Ir % | anchor Cyc % |");
    }
    out.push_str(" Cache1 | LL | BrMiss | status |\n");

    out.push_str("| --- | ---: | ---: |");
    if has_base {
        out.push_str(" ---: | ---: |");
    }
    if has_anchor {
        out.push_str(" ---: | ---: |");
    }
    out.push_str(" ---: | ---: | ---: | --- |\n");

    for row in rows {
        let _ = write!(
            out,
            "| `{}` | {} | {} |",
            row.bench_id, row.head.ir, row.head.estimated_cycles
        );
        if has_base {
            let missing_label = if row.base.is_none() { "new" } else { "n/a" };
            let ir = row
                .base_ir_pct
                .map_or_else(|| missing_label.to_owned(), |v| format!("{v:+.2}%"));
            let cyc = row
                .base_cycles_pct
                .map_or_else(|| missing_label.to_owned(), |v| format!("{v:+.2}%"));
            let _ = write!(out, " {ir} | {cyc} |");
        }
        if has_anchor {
            let missing_label = if row.anchor.is_none() { "new" } else { "n/a" };
            let ir = row
                .anchor_ir_pct
                .map_or_else(|| missing_label.to_owned(), |v| format!("{v:+.2}%"));
            let cyc = row
                .anchor_cycles_pct
                .map_or_else(|| missing_label.to_owned(), |v| format!("{v:+.2}%"));
            let _ = write!(out, " {ir} | {cyc} |");
        }
        let _ = writeln!(
            out,
            " {} | {} | {} | {} |",
            row.head.l1_misses, row.head.ll_misses, row.head.branch_mispredicts, row.status
        );
    }
    out
}

/// Resolve and create `target/xtask-det/` scratch directory on disk.
pub(crate) fn det_workdir(repo_root: &Path) -> Result<PathBuf, String> {
    let dir = repo_root.join("target").join("xtask-det");
    fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    fs::canonicalize(&dir).map_err(|e| format!("canonicalizing {}: {e}", dir.display()))
}

/// Verify `valgrind` is present in `PATH`.
pub(crate) fn ensure_valgrind() -> Result<(), String> {
    let status = Command::new("valgrind")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("valgrind is required for deterministic profiling ({e})"))?;
    if !status.success() {
        return Err("valgrind --version exited with non-zero status".to_owned());
    }
    Ok(())
}

/// Resolve a git ref to its full commit SHA, rejecting flag-like inputs starting with `-`.
pub(crate) fn resolve_git_sha(repo_root: &Path, git_ref: &str) -> Result<String, String> {
    if git_ref.is_empty() || git_ref.starts_with('-') {
        return Err(format!("invalid git ref `{git_ref}`"));
    }
    let out = Command::new("git")
        .current_dir(repo_root)
        .args([
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{git_ref}^{{commit}}"),
        ])
        .output()
        .map_err(|e| format!("resolving git ref `{git_ref}`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "unknown git ref `{git_ref}`: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Build the shimmed `ayaml-bench` binary for the current working tree without mutating `Cargo.lock`.
pub(crate) fn build_working_tree_bench(
    repo_root: &Path,
    workdir: &Path,
) -> Result<BuiltBench, String> {
    let lock_guard = LockfileGuard::capture(repo_root)?;
    let built = build_shimmed_bench_in(repo_root, workdir, "head", true, Some(&lock_guard));
    lock_guard.restore();
    built
}

/// Build the shimmed `ayaml-bench` binary for an arbitrary git ref inside a detached worktree.
pub(crate) fn build_git_ref_bench(
    repo_root: &Path,
    workdir: &Path,
    git_ref: &str,
    label: &str,
) -> Result<BuiltBench, String> {
    let sha = resolve_git_sha(repo_root, git_ref)?;
    let short = &sha[..sha.len().min(12)];
    let pid = std::process::id();
    let wt_path = workdir.join(format!("wt-{label}-{short}-{pid}"));
    let wt_guard = WorktreeGuard::create(repo_root, wt_path, &sha, git_ref)?;
    patch_historical_worktree_skip_validate(wt_guard.path());
    build_shimmed_bench_in(
        wt_guard.path(),
        workdir,
        &format!("{label}-{short}"),
        false,
        None,
    )
}

/// Allow historical commits (predating `AYAML_BENCH_SKIP_VALIDATE`) inside a disposable detached
/// worktree to skip 40-fixture pre-timing validation under Valgrind, keeping historical `--base`
/// and `--anchor` runs fast and avoiding allocator-state skew from unrelated setup fixtures.
fn patch_historical_worktree_skip_validate(worktree: &Path) {
    let bench_file = worktree.join("crates/ayaml-bench/benches/throughput.rs");
    let Ok(src) = fs::read_to_string(&bench_file) else {
        return;
    };
    if src.contains("AYAML_BENCH_SKIP_VALIDATE") {
        return;
    }
    let needle = "fn ensure_validated() {\n    VALIDATED.call_once(";
    if !src.contains(needle) {
        return;
    }
    let replacement = "fn ensure_validated() {\n    if std::env::var_os(\"AYAML_BENCH_SKIP_VALIDATE\").is_some() {\n        return;\n    }\n    VALIDATED.call_once(";
    let patched = src.replacen(needle, replacement, 1);
    drop(fs::write(&bench_file, patched));
}

fn write_criterion_shim(workdir: &Path, version: &str) -> Result<PathBuf, String> {
    let shim_dir = workdir.join(format!("criterion-shim-{version}"));
    let src_dir = shim_dir.join("src");
    fs::create_dir_all(&src_dir).map_err(|e| format!("creating {}: {e}", src_dir.display()))?;
    let toml = format!(
        "[package]\nname = \"criterion\"\nversion = \"{version}\"\nedition = \"2021\"\npublish = false\n\n[lib]\npath = \"src/lib.rs\"\n\n[workspace]\n"
    );
    let toml_path = shim_dir.join("Cargo.toml");
    if fs::read_to_string(&toml_path).ok().as_deref() != Some(toml.as_str()) {
        fs::write(&toml_path, toml).map_err(|e| format!("writing shim Cargo.toml: {e}"))?;
    }
    let lib_path = src_dir.join("lib.rs");
    if fs::read_to_string(&lib_path).ok().as_deref() != Some(SHIM_SOURCE) {
        fs::write(&lib_path, SHIM_SOURCE).map_err(|e| format!("writing shim src/lib.rs: {e}"))?;
    }
    Ok(shim_dir)
}

fn build_shimmed_bench_in(
    checkout_root: &Path,
    workdir: &Path,
    label: &str,
    is_head: bool,
    lock_guard: Option<&LockfileGuard>,
) -> Result<BuiltBench, String> {
    let lock_text = fs::read_to_string(checkout_root.join("Cargo.lock"))
        .map_err(|e| format!("reading Cargo.lock in {}: {e}", checkout_root.display()))?;
    let criterion_version = locked_version(&lock_text, "criterion")
        .ok_or_else(|| "Cargo.lock does not contain `criterion`".to_owned())?;
    let shim_dir = write_criterion_shim(workdir, &criterion_version)?;
    let cargo_target = workdir.join(if is_head {
        "cargo-target-head"
    } else {
        "cargo-target-ref"
    });
    fs::create_dir_all(&cargo_target)
        .map_err(|e| format!("creating {}: {e}", cargo_target.display()))?;

    let canon_checkout =
        fs::canonicalize(checkout_root).unwrap_or_else(|_| checkout_root.to_path_buf());
    let canon_shim = fs::canonicalize(&shim_dir).unwrap_or_else(|_| shim_dir.clone());
    let mut rustflags = env::var("RUSTFLAGS").unwrap_or_default();
    let remap = format!(
        "--remap-path-prefix={}=/ayaml --remap-path-prefix={}=/criterion-shim",
        canon_checkout.display(),
        canon_shim.display(),
    );
    if !rustflags.is_empty() {
        rustflags.push(' ');
    }
    rustflags.push_str(&remap);

    let patch_arg = format!(
        "patch.crates-io.criterion.path={:?}",
        shim_dir.to_string_lossy()
    );
    let out = Command::new("cargo")
        .current_dir(checkout_root)
        .env("CARGO_TARGET_DIR", &cargo_target)
        .env("RUSTFLAGS", &rustflags)
        .args([
            "bench",
            "--no-run",
            "-p",
            "ayaml-bench",
            "--bench",
            "throughput",
            "--message-format=json",
            "--config",
            &patch_arg,
            "--config",
            "profile.bench.debug=\"line-tables-only\"",
            "--config",
            "profile.bench.strip=false",
        ])
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("spawning shimmed cargo bench --no-run ({label}): {e}"))?;

    // Restore workspace Cargo.lock immediately once cargo exits so the mutation window is minimal.
    if let Some(guard) = lock_guard {
        guard.restore();
    }

    if !out.status.success() {
        return Err(format!(
            "shimmed cargo bench --no-run failed for `{label}` (status {})",
            out.status.code().unwrap_or(1)
        ));
    }

    let mut compiled_bin: Option<PathBuf> = None;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(val) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if val.get("reason").and_then(Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let target_name = val
            .get("target")
            .and_then(|t| t.get("name"))
            .and_then(Value::as_str);
        if target_name == Some("throughput")
            && let Some(exe) = val.get("executable").and_then(Value::as_str)
        {
            compiled_bin = Some(PathBuf::from(exe));
        }
    }
    let compiled_bin = compiled_bin.ok_or_else(|| {
        format!("cargo bench --no-run did not report a `throughput` binary for `{label}`")
    })?;

    let pid = std::process::id();
    let dir_guard = TempDirGuard::new(workdir.join(format!("bin-{label}-{pid}")))?;
    let stable_bin = dir_guard.path().join("throughput");
    fs::copy(&compiled_bin, &stable_bin).map_err(|e| {
        format!(
            "copying {} to {}: {e}",
            compiled_bin.display(),
            stable_bin.display()
        )
    })?;

    let ids = list_bench_ids(&stable_bin)?;
    Ok(BuiltBench {
        binary: stable_bin,
        ids,
        _dir_guard: dir_guard,
    })
}

fn list_bench_ids(binary: &Path) -> Result<Vec<String>, String> {
    let out = Command::new(binary)
        .env("AYAML_DET_LIST", "1")
        .env("AYAML_BENCH_SKIP_VALIDATE", "1")
        .env_remove("AYAML_DET_FILTER")
        .env_remove("AYAML_DET_WARMUP")
        .output()
        .map_err(|e| format!("listing benchmarks from {}: {e}", binary.display()))?;
    if !out.status.success() {
        return Err(format!(
            "benchmark listing failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut ids = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(id) = line.strip_prefix("AYAML_DET_ID\t") {
            ids.push(id.to_owned());
        }
    }
    if ids.is_empty() {
        return Err(format!(
            "benchmark binary {} reported zero benchmarks",
            binary.display()
        ));
    }
    Ok(ids)
}

fn measure_bench_list(
    binary: &Path,
    bench_ids: &[String],
    workdir: &Path,
    label: &str,
    jobs: usize,
) -> Result<BTreeMap<String, CallgrindCounters>, String> {
    let pid = std::process::id();
    let out_guard = TempDirGuard::new(workdir.join(format!("cg-{label}-{pid}")))?;
    let out_dir = out_guard.path();

    let workers = jobs.max(1).min(bench_ids.len().max(1));
    let queue = Mutex::new(bench_ids.iter().enumerate());
    let results: Mutex<BTreeMap<String, CallgrindCounters>> = Mutex::new(BTreeMap::new());
    let first_err: Mutex<Option<String>> = Mutex::new(None);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    if first_err.lock().map_or(true, |guard| guard.is_some()) {
                        break;
                    }
                    let next = queue.lock().ok().and_then(|mut iter| iter.next());
                    let Some((idx, bench_id)) = next else {
                        break;
                    };
                    match measure_single_bench(binary, bench_id, out_dir, idx) {
                        Ok(counters) => {
                            if let Ok(mut map) = results.lock() {
                                map.insert(bench_id.clone(), counters);
                            }
                        }
                        Err(err) => {
                            if let Ok(mut guard) = first_err.lock()
                                && guard.is_none()
                            {
                                *guard = Some(err);
                            }
                            break;
                        }
                    }
                }
            });
        }
    });

    if let Some(err) = first_err
        .into_inner()
        .map_err(|_| "worker error mutex poisoned".to_owned())?
    {
        return Err(err);
    }
    results
        .into_inner()
        .map_err(|_| "results mutex poisoned".to_owned())
}

fn measure_single_bench(
    binary: &Path,
    bench_id: &str,
    out_dir: &Path,
    idx: usize,
) -> Result<CallgrindCounters, String> {
    let (k1, k2) = iteration_counts_for(bench_id);
    let delta = u64::try_from(k2.saturating_sub(k1))
        .map_err(|_| "invalid iteration count delta".to_owned())?;
    let c1 = run_callgrind_once(
        binary,
        bench_id,
        k1,
        0,
        &out_dir.join(format!("b{idx}-k1.out")),
    )?;
    let c2 = run_callgrind_once(
        binary,
        bench_id,
        k2,
        0,
        &out_dir.join(format!("b{idx}-k2.out")),
    )?;
    steady_state_delta(&c1, &c2, delta)
}

/// Run Valgrind Callgrind on one shimmed benchmark ID for `k` iterations (after `warmup`
/// uncollected warmup iterations) and return its raw totals.
pub(crate) fn run_callgrind_once(
    binary: &Path,
    bench_id: &str,
    k: usize,
    warmup: usize,
    out_file: &Path,
) -> Result<BTreeMap<String, u64>, String> {
    let out = Command::new("valgrind")
        .args([
            "--tool=callgrind",
            "--cache-sim=yes",
            "--branch-sim=yes",
            "--collect-atstart=no",
            "--toggle-collect=*__ayaml_det_measure*",
            "--quiet",
        ])
        .arg(format!("--callgrind-out-file={}", out_file.display()))
        .arg(binary)
        .env("AYAML_DET_FILTER", bench_id)
        .env("AYAML_DET_K", k.to_string())
        .env("AYAML_DET_WARMUP", warmup.to_string())
        .env("AYAML_BENCH_SKIP_VALIDATE", "1")
        .env_remove("AYAML_DET_LIST")
        .output()
        .map_err(|e| format!("failed to launch valgrind callgrind for `{bench_id}`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "valgrind callgrind failed for `{bench_id}` (k={k}): {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text =
        fs::read_to_string(out_file).map_err(|e| format!("reading {}: {e}", out_file.display()))?;
    parse_callgrind_out(&text)
}

fn write_det_outputs(
    repo_root: &Path,
    out_dir: &Path,
    rows: &[DetComparison],
) -> Result<(), String> {
    fs::create_dir_all(out_dir).map_err(|e| format!("creating {}: {e}", out_dir.display()))?;
    let manifest = ayaml_fixtures::manifest();
    let mut fixture_meta: BTreeMap<String, (u64, String)> = manifest
        .entries
        .iter()
        .map(|e| (e.id.clone(), (e.bytes as u64, e.sha256.clone())))
        .collect();
    if let Some((_, sha)) = fixture_meta.get("automation-bulk").cloned() {
        let bulk_bytes: u64 = ayaml_fixtures::bulk_automation_files(32)
            .iter()
            .map(|f| f.bytes as u64)
            .sum();
        fixture_meta.insert("automation-playbooks".to_owned(), (bulk_bytes, sha));
    }

    let mut jsonl = String::new();
    for row in rows {
        let (operation, workload) = split_bench_id(&row.bench_id);
        let (bytes, sha) = fixture_meta
            .get(&workload)
            .cloned()
            .unwrap_or_else(|| (0, "unknown".to_owned()));
        let rec = CallgrindRecord {
            schema_version: SCHEMA_VERSION,
            operation,
            workload,
            bytes,
            fixture_sha256: sha,
            base_ir: row.base.map(|b| b.ir),
            head_ir: row.head.ir,
            ir_change_pct: row.base_ir_pct.map(|v| (v * 1000.0).round() / 1000.0),
            base_estimated_cycles: row.base.map(|b| b.estimated_cycles),
            head_estimated_cycles: row.head.estimated_cycles,
            cycles_change_pct: row.base_cycles_pct.map(|v| (v * 1000.0).round() / 1000.0),
            l1_misses: row.head.l1_misses,
            ll_misses: row.head.ll_misses,
            branch_mispredicts: row.head.branch_mispredicts,
            status: row.status.clone(),
        };
        jsonl.push_str(&rec.to_json_line()?);
        jsonl.push('\n');
    }
    fs::write(out_dir.join("callgrind.jsonl"), jsonl)
        .map_err(|e| format!("writing callgrind.jsonl: {e}"))?;
    let fp = Fingerprint::capture(repo_root, manifest.manifest_sha256);
    fs::write(
        out_dir.join("callgrind.fingerprint.json"),
        fp.to_pretty_json()?,
    )
    .map_err(|e| format!("writing callgrind.fingerprint.json: {e}"))?;
    Ok(())
}

fn split_bench_id(bench_id: &str) -> (String, String) {
    let parts: Vec<&str> = bench_id.split('/').filter(|s| !s.is_empty()).collect();
    match parts.as_slice() {
        [] => ("bench".to_owned(), "unknown".to_owned()),
        [only] => ("bench".to_owned(), (*only).to_owned()),
        [op, scenario] => ((*op).to_owned(), (*scenario).to_owned()),
        [group, op, scenario] => (format!("{group}/{op}"), (*scenario).to_owned()),
        _ => (
            parts[..parts.len().saturating_sub(1)].join("/"),
            parts.last().copied().unwrap_or("unknown").to_owned(),
        ),
    }
}
