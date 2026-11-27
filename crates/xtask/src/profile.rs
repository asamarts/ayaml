//! Deterministic bottleneck profiling (`cargo xtask profile`) powered by Valgrind
//! Callgrind (`callgrind_annotate` instruction, cache-miss, and branch-miss tables + `--base`
//! diffs) and Valgrind DHAT (per-call-site at-peak bytes, allocation counts, and total bytes).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::det::{
    TempDirGuard, WorktreeGuard, build_git_ref_bench, build_working_tree_bench, det_workdir,
    ensure_valgrind, estimated_cycles, resolve_git_sha, run_callgrind_once,
};

/// Profiling backend selected by `--tool`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProfileTool {
    /// Valgrind Callgrind function-level instruction/cache/branch attribution.
    Callgrind,
    /// Valgrind DHAT call-site peak-heap, allocation-count, and total-byte attribution.
    Dhat,
}

impl ProfileTool {
    /// Parse a `--tool` flag value (`callgrind` or `dhat`).
    ///
    /// # Errors
    ///
    /// Returns an error for unknown tool names.
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "callgrind" => Ok(Self::Callgrind),
            "dhat" => Ok(Self::Dhat),
            other => Err(format!(
                "invalid --tool value {other:?} (expected `callgrind` or `dhat`)"
            )),
        }
    }
}

/// Target workload to profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProfileTarget {
    /// An `ayaml-bench` Criterion benchmark ID (e.g. `layers/parse_value/openapi-spec`).
    Bench(String),
    /// A single `memcount` fixture and mode (e.g. `scale-map-100kb`, `parse_value`).
    Fixture {
        /// Catalog fixture ID.
        fixture: String,
        /// `memcount` mode (`parse_value`, `typed_struct`, `typed_borrowed`, `spanned_tree`, ...).
        mode: String,
    },
}

/// Configuration for `cargo xtask profile`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProfileOptions {
    /// Selected Valgrind tool (`callgrind` or `dhat`).
    pub(crate) tool: ProfileTool,
    /// Benchmark ID or fixture/mode target.
    pub(crate) target: ProfileTarget,
    /// Optional base git ref for before/after attribution diffing.
    pub(crate) base_rev: Option<String>,
    /// Number of top functions or call sites to display.
    pub(crate) top: usize,
    /// Use `--inclusive=yes` in `callgrind_annotate`.
    pub(crate) inclusive: bool,
    /// Iteration count for Callgrind `--bench` runs (after 1 uncollected warmup iteration).
    pub(crate) iterations: usize,
    /// Optional path to write the rendered Markdown report.
    pub(crate) out: Option<PathBuf>,
}

/// One parsed function row from `callgrind_annotate`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallgrindFunctionRow {
    /// Instruction count (`Ir`) attributed to this function.
    pub(crate) ir: u64,
    /// Combined first-level cache misses (`I1mr + D1mr + D1mw`).
    pub(crate) l1_misses: u64,
    /// Combined last-level cache misses (`ILmr + DLmr + DLmw`).
    pub(crate) ll_misses: u64,
    /// Combined branch mispredictions (`Bcm + Bim`).
    pub(crate) branch_mispredicts: u64,
    /// Weighted simulated cycles (`Ir + 10*(I1mr+D1mr+D1mw) + 100*(ILmr+DLmr+DLmw) + 20*(Bcm+Bim)`).
    pub(crate) estimated_cycles: u64,
    /// Normalized source-file and function identifier.
    pub(crate) symbol: String,
}

/// Parsed summary of `callgrind_annotate` output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallgrindProfile {
    /// Total `Ir` across the profiled region (`PROGRAM TOTALS`).
    pub(crate) total_ir: u64,
    /// Total first-level cache misses (`I1mr + D1mr + D1mw`).
    pub(crate) total_l1_misses: u64,
    /// Total last-level cache misses (`ILmr + DLmr + DLmw`).
    pub(crate) total_ll_misses: u64,
    /// Total branch mispredictions (`Bcm + Bim`).
    pub(crate) total_branch_mispredicts: u64,
    /// Total weighted simulated cycles.
    pub(crate) total_estimated_cycles: u64,
    /// Ranked functions sorted descending by `Ir`.
    pub(crate) functions: Vec<CallgrindFunctionRow>,
}

/// One aggregated allocation site from Valgrind DHAT JSON output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DhatSite {
    /// Bytes live at global heap peak (`gb`, `at-t-gmax`).
    pub(crate) at_peak_bytes: u64,
    /// Maximum simultaneous live bytes at this site (`mb`, non-additive across sites).
    pub(crate) site_max_bytes: u64,
    /// Total allocations (`tbk`).
    pub(crate) allocations: u64,
    /// Total cumulative bytes allocated (`tb`).
    pub(crate) total_bytes: u64,
    /// Compact stack summary pointing to the responsible `ayaml` / allocator site.
    pub(crate) stack: String,
}

/// Parsed summary of a Valgrind DHAT run after filtering setup frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DhatProfile {
    /// Sum of `at_peak_bytes` across retained sites (0 when global peak occurred outside scope).
    pub(crate) total_at_peak_bytes: u64,
    /// Sum of `allocations` across retained sites.
    pub(crate) total_allocations: u64,
    /// Sum of `total_bytes` across retained sites.
    pub(crate) total_allocated_bytes: u64,
    /// Whether this profile used strict boundary frame scoping (`__ayaml_det_measure` or
    /// `__ayaml_memcount_measure`).
    pub(crate) strict_scoped: bool,
    /// Retained call sites (aggregated by normalized stack summary).
    pub(crate) sites: Vec<DhatSite>,
}

/// Execute `cargo xtask profile`.
///
/// # Errors
///
/// Returns an error if Valgrind or `callgrind_annotate` is unavailable, `--base` is invalid,
/// builds fail, or profile output cannot be parsed.
pub(crate) fn run_profile(repo_root: &Path, opts: &ProfileOptions) -> Result<(), String> {
    ensure_valgrind()?;
    if opts.tool == ProfileTool::Callgrind {
        ensure_callgrind_annotate()?;
    }
    if let Some(ref rev) = opts.base_rev {
        drop(resolve_git_sha(repo_root, rev)?);
    }

    let workdir = det_workdir(repo_root)?;
    let pid = std::process::id();
    let prof_guard = TempDirGuard::new(workdir.join(format!("profile-{pid}")))?;
    let prof_dir = prof_guard.path();

    let markdown = match opts.tool {
        ProfileTool::Callgrind => run_callgrind_profile(repo_root, &workdir, prof_dir, opts)?,
        ProfileTool::Dhat => run_dhat_profile(repo_root, &workdir, prof_dir, opts)?,
    };

    if let Some(ref out_path) = opts.out {
        if let Some(parent) = out_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        fs::write(out_path, &markdown)
            .map_err(|e| format!("writing {}: {e}", out_path.display()))?;
    } else {
        print!("{markdown}");
    }
    Ok(())
}

fn ensure_callgrind_annotate() -> Result<(), String> {
    // Valgrind's `callgrind_annotate` prints usage/version and exits non-zero (255) on `--version`,
    // so check that the binary can be spawned from PATH.
    let _status = Command::new("callgrind_annotate")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("callgrind_annotate is required for `--tool callgrind` ({e})"))?;
    Ok(())
}

fn run_callgrind_profile(
    repo_root: &Path,
    workdir: &Path,
    prof_dir: &Path,
    opts: &ProfileOptions,
) -> Result<String, String> {
    let head_out = prof_dir.join("head.callgrind.out");
    capture_callgrind_target(
        repo_root,
        workdir,
        None,
        &opts.target,
        opts.iterations,
        &head_out,
    )?;
    let head = annotate_callgrind(&head_out, opts.inclusive)?;

    let title = format_target_label(&opts.target);
    let mut md = format!("# Callgrind Profile: `{title}`\n\n");
    md.push_str(&render_callgrind_table(&head, opts.top, opts.inclusive));

    if let Some(ref base_rev) = opts.base_rev {
        let base_out = prof_dir.join("base.callgrind.out");
        capture_callgrind_target(
            repo_root,
            workdir,
            Some(base_rev),
            &opts.target,
            opts.iterations,
            &base_out,
        )?;
        let base = annotate_callgrind(&base_out, opts.inclusive)?;
        md.push('\n');
        md.push_str(&render_callgrind_diff(
            &base,
            &head,
            base_rev,
            opts.top,
            opts.inclusive,
        ));
    }
    Ok(md)
}

fn run_dhat_profile(
    repo_root: &Path,
    workdir: &Path,
    prof_dir: &Path,
    opts: &ProfileOptions,
) -> Result<String, String> {
    let head_out = prof_dir.join("head.dhat.json");
    let bench_scoped = matches!(opts.target, ProfileTarget::Bench(_));
    capture_dhat_target(repo_root, workdir, None, &opts.target, &head_out)?;
    let head_text = fs::read_to_string(&head_out)
        .map_err(|e| format!("reading {}: {e}", head_out.display()))?;
    let head = parse_dhat_json(&head_text, bench_scoped)?;

    let title = format_target_label(&opts.target);
    let mut md = format!("# Valgrind DHAT Profile: `{title}`\n\n");
    md.push_str(&render_dhat_report(&head, opts.top));

    if let Some(ref base_rev) = opts.base_rev {
        let base_out = prof_dir.join("base.dhat.json");
        capture_dhat_target(repo_root, workdir, Some(base_rev), &opts.target, &base_out)?;
        let base_text = fs::read_to_string(&base_out)
            .map_err(|e| format!("reading {}: {e}", base_out.display()))?;
        let base = parse_dhat_json(&base_text, bench_scoped)?;
        md.push('\n');
        md.push_str(&render_dhat_diff(&base, &head, base_rev));
    }
    Ok(md)
}

fn capture_callgrind_target(
    repo_root: &Path,
    workdir: &Path,
    git_ref: Option<&str>,
    target: &ProfileTarget,
    iterations: usize,
    out_file: &Path,
) -> Result<(), String> {
    let ref_label = git_ref.map_or_else(|| "working tree".to_owned(), |r| format!("ref `{r}`"));
    match target {
        ProfileTarget::Bench(bench_id) => {
            let built = match git_ref {
                Some(rev) => build_git_ref_bench(repo_root, workdir, rev, "prof-base")?,
                None => build_working_tree_bench(repo_root, workdir)?,
            };
            if !built.ids.iter().any(|id| id == bench_id) {
                return Err(format!(
                    "benchmark `{bench_id}` not found in {ref_label} \
                     (run `cargo xtask det --list` to see valid working-tree IDs)"
                ));
            }
            // Run 1 uncollected warmup iteration so allocator/cache/OnceLock state matches
            // steady-state behavior across head and base profiles.
            drop(run_callgrind_once(
                &built.binary,
                bench_id,
                iterations.max(1),
                1,
                out_file,
            )?);
            Ok(())
        }
        ProfileTarget::Fixture { fixture, mode } => {
            let built = build_memcount_debug(repo_root, workdir, git_ref)?;
            let out = Command::new("valgrind")
                .args([
                    "--tool=callgrind",
                    "--cache-sim=yes",
                    "--I1=32768,8,64",
                    "--LL=8388608,16,64",
                    "--branch-sim=yes",
                    "--collect-atstart=no",
                    "--toggle-collect=*__ayaml_memcount_measure*",
                    "--quiet",
                ])
                .arg(format!("--callgrind-out-file={}", out_file.display()))
                .arg(&built.binary)
                .args(["--jsonl", "--fixture", fixture, "--mode", mode])
                .output()
                .map_err(|e| format!("launching valgrind callgrind on memcount: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "valgrind callgrind failed on `{fixture}:{mode}` ({ref_label}): {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            let text = fs::read_to_string(out_file)
                .map_err(|e| format!("reading {}: {e}", out_file.display()))?;
            crate::det::parse_callgrind_out(&text).map_err(|err| {
                format!(
                    "callgrind capture for `{fixture}:{mode}` in {ref_label} failed ({err}); \
                     historical commits predating `__ayaml_memcount_measure` cannot be toggled in fixture mode"
                )
            })?;
            Ok(())
        }
    }
}

fn capture_dhat_target(
    repo_root: &Path,
    workdir: &Path,
    git_ref: Option<&str>,
    target: &ProfileTarget,
    out_file: &Path,
) -> Result<(), String> {
    let ref_label = git_ref.map_or_else(|| "working tree".to_owned(), |r| format!("ref `{r}`"));
    match target {
        ProfileTarget::Bench(bench_id) => {
            let built = match git_ref {
                Some(rev) => build_git_ref_bench(repo_root, workdir, rev, "dhat-base")?,
                None => build_working_tree_bench(repo_root, workdir)?,
            };
            if !built.ids.iter().any(|id| id == bench_id) {
                return Err(format!(
                    "benchmark `{bench_id}` not found in {ref_label} \
                     (run `cargo xtask det --list` to see valid working-tree IDs)"
                ));
            }
            let out = Command::new("valgrind")
                .args([
                    "--tool=dhat",
                    "--num-callers=64",
                    "--merge-recursive-frames=8",
                    "--quiet",
                ])
                .arg(format!("--dhat-out-file={}", out_file.display()))
                .arg(&built.binary)
                .env("AYAML_DET_FILTER", bench_id)
                .env("AYAML_DET_K", "1")
                .env("AYAML_DET_WARMUP", "0")
                .env("AYAML_BENCH_SKIP_VALIDATE", "1")
                .env_remove("AYAML_DET_LIST")
                .output()
                .map_err(|e| format!("launching valgrind dhat for `{bench_id}`: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "valgrind dhat failed for `{bench_id}` ({ref_label}): {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            Ok(())
        }
        ProfileTarget::Fixture { fixture, mode } => {
            let built = build_memcount_debug(repo_root, workdir, git_ref)?;
            let out = Command::new("valgrind")
                .args([
                    "--tool=dhat",
                    "--num-callers=64",
                    "--merge-recursive-frames=8",
                    "--quiet",
                ])
                .arg(format!("--dhat-out-file={}", out_file.display()))
                .arg(&built.binary)
                .args(["--jsonl", "--fixture", fixture, "--mode", mode])
                .output()
                .map_err(|e| format!("launching valgrind dhat on memcount: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "valgrind dhat failed on `{fixture}:{mode}` ({ref_label}): {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            Ok(())
        }
    }
}

struct BuiltMemcount {
    binary: PathBuf,
    _dir_guard: TempDirGuard,
}

fn build_memcount_debug(
    repo_root: &Path,
    workdir: &Path,
    git_ref: Option<&str>,
) -> Result<BuiltMemcount, String> {
    if let Some(rev) = git_ref {
        let sha = resolve_git_sha(repo_root, rev)?;
        let short = &sha[..sha.len().min(12)];
        let pid = std::process::id();
        let wt_path = workdir.join(format!("wt-memcount-{short}-{pid}"));
        let wt_guard = WorktreeGuard::create(repo_root, wt_path, &sha, rev)?;
        return compile_memcount_in(
            wt_guard.path(),
            workdir,
            &format!("memcount-{short}"),
            false,
        );
    }
    compile_memcount_in(repo_root, workdir, "memcount-head", true)
}

fn compile_memcount_in(
    checkout_root: &Path,
    workdir: &Path,
    label: &str,
    is_head: bool,
) -> Result<BuiltMemcount, String> {
    let target_dir = workdir.join(if is_head {
        "memcount-target-head"
    } else {
        "memcount-target-ref"
    });
    fs::create_dir_all(&target_dir)
        .map_err(|e| format!("creating {}: {e}", target_dir.display()))?;
    let status = Command::new("cargo")
        .current_dir(checkout_root)
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("CARGO_PROFILE_RELEASE_DEBUG", "line-tables-only")
        .env("CARGO_PROFILE_RELEASE_STRIP", "false")
        .args([
            "build",
            "--release",
            "--locked",
            "--manifest-path",
            "fuzz/Cargo.toml",
            "--bin",
            "memcount",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("building memcount ({label}): {e}"))?;
    if !status.success() {
        return Err(format!("cargo build memcount failed for `{label}`"));
    }
    let pid = std::process::id();
    let dir_guard = TempDirGuard::new(workdir.join(format!("bin-{label}-{pid}")))?;
    let src = target_dir.join("release").join("memcount");
    let dst = dir_guard.path().join("memcount");
    fs::copy(&src, &dst)
        .map_err(|e| format!("copying {} to {}: {e}", src.display(), dst.display()))?;
    Ok(BuiltMemcount {
        binary: dst,
        _dir_guard: dir_guard,
    })
}

fn annotate_callgrind(out_file: &Path, inclusive: bool) -> Result<CallgrindProfile, String> {
    let mut cmd = Command::new("callgrind_annotate");
    cmd.args(["--auto=no", "--threshold=100", "--show-percs=no"]);
    if inclusive {
        cmd.arg("--inclusive=yes");
    }
    cmd.arg(out_file);
    let out = cmd
        .output()
        .map_err(|e| format!("running callgrind_annotate: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "callgrind_annotate failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_callgrind_annotate(&String::from_utf8_lossy(&out.stdout))
}

#[derive(Default)]
struct EventColumns {
    count: usize,
    ir: Option<usize>,
    i1mr: Option<usize>,
    d1mr: Option<usize>,
    d1mw: Option<usize>,
    ilmr: Option<usize>,
    dlmr: Option<usize>,
    dlmw: Option<usize>,
    bcm: Option<usize>,
    bim: Option<usize>,
}

impl EventColumns {
    fn from_header(line: &str) -> Option<Self> {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.first().copied() != Some("Ir") {
            return None;
        }
        let mut cols = Self {
            count: toks.len(),
            ..Self::default()
        };
        for (idx, tok) in toks.iter().enumerate() {
            match *tok {
                "Ir" => cols.ir = Some(idx),
                "I1mr" => cols.i1mr = Some(idx),
                "D1mr" => cols.d1mr = Some(idx),
                "D1mw" => cols.d1mw = Some(idx),
                "ILmr" => cols.ilmr = Some(idx),
                "DLmr" => cols.dlmr = Some(idx),
                "DLmw" => cols.dlmw = Some(idx),
                "Bcm" => cols.bcm = Some(idx),
                "Bim" => cols.bim = Some(idx),
                _ => {}
            }
        }
        Some(cols)
    }

    fn extract(&self, vals: &[u64]) -> (u64, u64, u64, u64, u64) {
        let get = |idx: Option<usize>| idx.and_then(|i| vals.get(i).copied()).unwrap_or(0);
        let ir = get(self.ir);
        let l1 = get(self.i1mr)
            .saturating_add(get(self.d1mr))
            .saturating_add(get(self.d1mw));
        let ll = get(self.ilmr)
            .saturating_add(get(self.dlmr))
            .saturating_add(get(self.dlmw));
        let br = get(self.bcm).saturating_add(get(self.bim));
        let cyc = estimated_cycles(ir, l1, ll, br);
        (ir, l1, ll, br, cyc)
    }
}

/// Parse `callgrind_annotate` stdout (with or without `--show-percs=no`) into totals and
/// ranked per-function instruction, cache-miss, and branch-miss rows.
///
/// # Errors
///
/// Returns an error if `PROGRAM TOTALS` cannot be located or `Ir == 0`.
pub(crate) fn parse_callgrind_annotate(text: &str) -> Result<CallgrindProfile, String> {
    let mut cols: Option<EventColumns> = None;
    let mut totals: Option<(u64, u64, u64, u64, u64)> = None;
    let mut functions = Vec::new();
    let mut in_table = false;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("---") {
            continue;
        }
        if !in_table {
            if let Some(parsed_cols) = EventColumns::from_header(line) {
                cols = Some(parsed_cols);
                continue;
            }
            if line.contains("PROGRAM TOTALS") {
                let col_spec = cols.get_or_insert_with(|| EventColumns {
                    count: 1,
                    ir: Some(0),
                    ..EventColumns::default()
                });
                let Some((vals, _)) = parse_event_row(line, col_spec.count) else {
                    return Err("malformed PROGRAM TOTALS line".to_owned());
                };
                totals = Some(col_spec.extract(&vals));
                in_table = true;
                continue;
            }
            continue;
        }

        let Some(ref col_spec) = cols else {
            continue;
        };
        let Some((vals, tail)) = parse_event_row(line, col_spec.count) else {
            continue;
        };
        if let Some(symbol) = clean_annotate_symbol(tail) {
            let (ir, l1_misses, ll_misses, branch_mispredicts, estimated_cycles) =
                col_spec.extract(&vals);
            functions.push(CallgrindFunctionRow {
                ir,
                l1_misses,
                ll_misses,
                branch_mispredicts,
                estimated_cycles,
                symbol,
            });
        }
    }

    let (
        total_ir,
        total_l1_misses,
        total_ll_misses,
        total_branch_mispredicts,
        total_estimated_cycles,
    ) = totals.ok_or_else(|| "callgrind_annotate output missing `PROGRAM TOTALS`".to_owned())?;
    if total_ir == 0 {
        return Err(
            "callgrind_annotate recorded 0 instructions (`Ir = 0`); measurement toggle did not fire"
                .to_owned(),
        );
    }
    functions.sort_by(|a, b| b.ir.cmp(&a.ir).then_with(|| a.symbol.cmp(&b.symbol)));
    Ok(CallgrindProfile {
        total_ir,
        total_l1_misses,
        total_ll_misses,
        total_branch_mispredicts,
        total_estimated_cycles,
        functions,
    })
}

/// Consume `expected_cols` comma-formatted integer columns (skipping optional `(XX.XX%)`
/// percentage tokens) from the front of `line`, returning the parsed values and remaining tail.
fn parse_event_row(line: &str, expected_cols: usize) -> Option<(Vec<u64>, &str)> {
    let mut vals = Vec::with_capacity(expected_cols);
    let mut rest = line;
    while vals.len() < expected_cols {
        rest = rest.trim_start();
        if rest.is_empty() {
            return None;
        }
        // Skip optional `( 40.00%)` percentage column if `--show-percs=no` was not passed.
        if rest.starts_with('(') {
            let close = rest.find(')')?;
            rest = &rest[close + 1..];
            continue;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let tok = &rest[..end];
        let val = parse_comma_u64(tok).ok()?;
        vals.push(val);
        rest = &rest[end..];
    }
    // Consume one trailing optional `(XX.XX%)` after the last numeric column.
    let trimmed = rest.trim_start();
    if trimmed.starts_with('(')
        && let Some(close) = trimmed.find(')')
        && trimmed[..close].ends_with('%')
    {
        rest = &trimmed[close + 1..];
    }
    Some((vals, rest.trim()))
}

fn parse_comma_u64(token: &str) -> Result<u64, String> {
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_digit() || b == b',') {
        return Err(format!("invalid integer `{token}`"));
    }
    let cleaned: String = token.chars().filter(|&ch| ch != ',').collect();
    cleaned
        .parse::<u64>()
        .map_err(|e| format!("invalid integer `{token}`: {e}"))
}

fn clean_annotate_symbol(raw: &str) -> Option<String> {
    let sym = raw.trim();
    if sym.is_empty() {
        return None;
    }
    // Strip trailing ` [/abs/path/to/bin]` only when the bracketed suffix is a binary path,
    // never truncating Rust array types like `-> [u8; 32]`.
    let cleaned = if let Some(inner) = sym.strip_suffix(']')
        && let Some((head, bracketed)) = inner.rsplit_once(" [")
        && (bracketed.starts_with('/')
            || bracketed.starts_with("./")
            || bracketed.contains('/')
            || bracketed == "???")
    {
        head.trim()
    } else {
        sym
    };
    if cleaned.is_empty() {
        return None;
    }
    Some(normalize_annotate_symbol(cleaned))
}

/// Normalize source paths inside `file:symbol` entries so worktree/toolchain/local absolute
/// prefixes never leak into reports and `--base REV` diffs match identical functions cleanly.
fn normalize_annotate_symbol(raw: &str) -> String {
    let Some((file_part, fn_part)) = raw.split_once(':') else {
        return raw.to_owned();
    };
    let norm_file = if let Some(idx) = file_part.rfind("/crates/") {
        &file_part[idx + 1..]
    } else if let Some(idx) = file_part.find("crates/") {
        &file_part[idx..]
    } else if let Some(idx) = file_part.rfind("/fuzz/") {
        &file_part[idx + 1..]
    } else if file_part.contains("criterion-shim") {
        "criterion-shim/src/lib.rs"
    } else if let Some(idx) = file_part.find("/library/") {
        return format!("rustlib{}:{fn_part}", &file_part[idx..]);
    } else if let Some(idx) = file_part.find("/registry/src/") {
        let after = &file_part[idx + "/registry/src/".len()..];
        let trimmed = after.split_once('/').map_or(after, |(_, rest)| rest);
        return format!("deps/{trimmed}:{fn_part}");
    } else if let Some(idx) = file_part.find("/git/checkouts/") {
        let after = &file_part[idx + "/git/checkouts/".len()..];
        return format!("git-deps/{after}:{fn_part}");
    } else if let Some(idx) = file_part.rfind("/glibc/") {
        return format!("glibc/{}:{fn_part}", &file_part[idx + "/glibc/".len()..]);
    } else if file_part.starts_with("/home/") || file_part.starts_with("/Users/") {
        file_part.rsplit('/').next().unwrap_or(file_part)
    } else {
        file_part
    };
    format!("{norm_file}:{fn_part}")
}

/// Return `true` for outer runtime/harness entry-point frames that dominate `--inclusive=yes`
/// tables at 100% without attributing work inside the benchmarked workload.
fn is_inclusive_harness_wrapper(symbol: &str) -> bool {
    let fn_part = symbol.split_once(':').map_or(symbol, |(_, f)| f);
    symbol.starts_with("criterion-shim/")
        || symbol.starts_with("???:0x")
        || fn_part == "main"
        || fn_part == "_start"
        || fn_part.starts_with("(below main)")
        || fn_part.starts_with("__libc_start_main")
        || fn_part.starts_with("std::rt::lang_start")
        || fn_part.starts_with("std::sys::backtrace::__rust_begin_short_backtrace")
        || fn_part.starts_with("std::panicking::")
        || fn_part.starts_with("std::panic::catch_unwind")
        || fn_part.starts_with("core::ops::function::FnOnce::call_once")
        || fn_part.starts_with("throughput::main")
        || fn_part.starts_with("throughput::benches")
        || fn_part.starts_with("throughput::bench_")
        || fn_part.starts_with("throughput::layers")
        || fn_part.starts_with("throughput::workloads")
        || fn_part.starts_with("memcount::")
        || fn_part.contains("__ayaml_memcount_measure")
        || fn_part.contains("ayaml_fixtures::")
        || fn_part.starts_with("criterion::")
        || fn_part.starts_with("<criterion::")
}

/// Aggregate rows by normalized symbol consistently across tables and diffs:
/// sum in self mode, keep max in inclusive mode.
fn aggregate_callgrind_rows(
    profile: &CallgrindProfile,
    inclusive: bool,
) -> Vec<CallgrindFunctionRow> {
    if inclusive {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        return profile
            .functions
            .iter()
            .filter(|row| !is_inclusive_harness_wrapper(&row.symbol))
            .filter(|row| seen.insert(row.symbol.as_str()))
            .cloned()
            .collect();
    }
    let mut by_sym: BTreeMap<&str, CallgrindFunctionRow> = BTreeMap::new();
    for row in &profile.functions {
        by_sym
            .entry(row.symbol.as_str())
            .and_modify(|acc| {
                acc.ir = acc.ir.saturating_add(row.ir);
                acc.l1_misses = acc.l1_misses.saturating_add(row.l1_misses);
                acc.ll_misses = acc.ll_misses.saturating_add(row.ll_misses);
                acc.branch_mispredicts = acc
                    .branch_mispredicts
                    .saturating_add(row.branch_mispredicts);
                acc.estimated_cycles = acc.estimated_cycles.saturating_add(row.estimated_cycles);
            })
            .or_insert_with(|| row.clone());
    }
    let mut merged: Vec<CallgrindFunctionRow> = by_sym.into_values().collect();
    merged.sort_by(|a, b| b.ir.cmp(&a.ir).then_with(|| a.symbol.cmp(&b.symbol)));
    merged
}

/// Render a Callgrind hotspot table for the top `top` functions with instructions, simulated
/// cycles, cache misses, and branch mispredictions.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "percentage formatting over instruction counts fits cleanly in f64"
)]
pub(crate) fn render_callgrind_table(
    profile: &CallgrindProfile,
    top: usize,
    inclusive: bool,
) -> String {
    let mode = if inclusive { "inclusive" } else { "self" };
    let mut out = format!(
        "Totals (mode: `{mode}`): `Ir` = **{}**, `EstCycles` = **{}**, `Cache1` = **{}**, `LL` = **{}**, `BrMiss` = **{}**\n\n\
         | rank | Ir | share | EstCycles | Cache1 | LL | BrMiss | function |\n\
         | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |\n",
        profile.total_ir,
        profile.total_estimated_cycles,
        profile.total_l1_misses,
        profile.total_ll_misses,
        profile.total_branch_mispredicts,
    );
    let denom = profile.total_ir.max(1) as f64;
    let rows = aggregate_callgrind_rows(profile, inclusive);
    for (idx, row) in rows.iter().take(top).enumerate() {
        let pct = (row.ir as f64) * 100.0 / denom;
        let _ = writeln!(
            out,
            "| {} | {} | {:.2}% | {} | {} | {} | {} | `{}` |",
            idx + 1,
            row.ir,
            pct,
            row.estimated_cycles,
            row.l1_misses,
            row.ll_misses,
            row.branch_mispredicts,
            row.symbol
        );
    }
    out
}

/// Render a per-function `Ir` and `EstCycles` delta diff between a base and head Callgrind profile.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "signed percentage formatting over instruction counts fits cleanly in f64"
)]
pub(crate) fn render_callgrind_diff(
    base: &CallgrindProfile,
    head: &CallgrindProfile,
    base_rev: &str,
    top: usize,
    inclusive: bool,
) -> String {
    let ir_delta = i128::from(head.total_ir) - i128::from(base.total_ir);
    let ir_pct = fmt_pct_delta(base.total_ir, head.total_ir);
    let cyc_delta =
        i128::from(head.total_estimated_cycles) - i128::from(base.total_estimated_cycles);
    let cyc_pct = fmt_pct_delta(base.total_estimated_cycles, head.total_estimated_cycles);
    let mut out = format!(
        "## Diff vs `{base_rev}`\n\n\
         Total `Ir`: {} -> {} ({ir_delta:+} / {ir_pct}), \
         `EstCycles`: {} -> {} ({cyc_delta:+} / {cyc_pct})\n\n\
         | function | base Ir | head Ir | delta Ir | delta EstCycles |\n\
         | --- | ---: | ---: | ---: | ---: |\n",
        base.total_ir, head.total_ir, base.total_estimated_cycles, head.total_estimated_cycles,
    );

    let base_rows = aggregate_callgrind_rows(base, inclusive);
    let head_rows = aggregate_callgrind_rows(head, inclusive);
    let base_map: BTreeMap<&str, &CallgrindFunctionRow> =
        base_rows.iter().map(|r| (r.symbol.as_str(), r)).collect();
    let head_map: BTreeMap<&str, &CallgrindFunctionRow> =
        head_rows.iter().map(|r| (r.symbol.as_str(), r)).collect();

    let mut deltas: Vec<(&str, u64, u64, i128, i128)> = Vec::new();
    for (&sym, h) in &head_map {
        let (b_ir, b_cyc) = base_map
            .get(sym)
            .map_or((0, 0), |b| (b.ir, b.estimated_cycles));
        let d_ir = i128::from(h.ir) - i128::from(b_ir);
        let d_cyc = i128::from(h.estimated_cycles) - i128::from(b_cyc);
        if d_ir != 0 || d_cyc != 0 {
            deltas.push((sym, b_ir, h.ir, d_ir, d_cyc));
        }
    }
    for (&sym, b) in &base_map {
        if !head_map.contains_key(sym) && (b.ir > 0 || b.estimated_cycles > 0) {
            deltas.push((
                sym,
                b.ir,
                0,
                -i128::from(b.ir),
                -i128::from(b.estimated_cycles),
            ));
        }
    }
    deltas.sort_by(|a, b| b.3.abs().cmp(&a.3.abs()).then_with(|| a.0.cmp(b.0)));

    for (sym, b_ir, h_ir, d_ir, d_cyc) in deltas.into_iter().take(top) {
        let _ = writeln!(out, "| `{sym}` | {b_ir} | {h_ir} | {d_ir:+} | {d_cyc:+} |");
    }
    out
}

/// Parse a Valgrind DHAT JSON output document, excluding fixture-generation and setup frames.
///
/// When `bench_scoped` is `true` (`--bench` mode), only allocation sites whose stack trace
/// passes through `__ayaml_det_measure` are retained. When `bench_scoped` is `false`
/// (`--fixture` mode), stacks are scoped strictly to `__ayaml_memcount_measure` whenever that
/// boundary symbol is present (falling back to excluding `ayaml_fixtures` for historical commits).
///
/// # Errors
///
/// Returns an error if the DHAT JSON is malformed or missing `ftbl`/`pps`.
pub(crate) fn parse_dhat_json(text: &str, bench_scoped: bool) -> Result<DhatProfile, String> {
    let val: Value =
        serde_json::from_str(text).map_err(|e| format!("invalid DHAT JSON output: {e}"))?;
    let ftbl_arr = val
        .get("ftbl")
        .and_then(Value::as_array)
        .ok_or_else(|| "DHAT JSON missing `ftbl` array".to_owned())?;
    let ftbl: Vec<&str> = ftbl_arr.iter().map(|v| v.as_str().unwrap_or("")).collect();
    let pps = val
        .get("pps")
        .and_then(Value::as_array)
        .ok_or_else(|| "DHAT JSON missing `pps` array".to_owned())?;

    let has_memcount_boundary =
        !bench_scoped && ftbl.iter().any(|f| f.contains("__ayaml_memcount_measure"));
    let strict_scoped = bench_scoped || has_memcount_boundary;

    let mut by_stack: BTreeMap<String, DhatSite> = BTreeMap::new();
    let mut total_at_peak = 0_u64;
    let mut total_allocs = 0_u64;
    let mut total_bytes = 0_u64;

    for pp in pps {
        let Some(fs_arr) = pp.get("fs").and_then(Value::as_array) else {
            continue;
        };
        let frames: Vec<&str> = fs_arr
            .iter()
            .filter_map(Value::as_u64)
            .filter_map(|idx| usize::try_from(idx).ok())
            .filter_map(|idx| ftbl.get(idx).copied())
            .collect();

        if frames.iter().any(|f| f.contains("ayaml_fixtures")) {
            continue;
        }
        if bench_scoped {
            if !frames
                .iter()
                .any(|f| f.contains("__ayaml_det_measure") || f.contains("__ayaml_dhat_measure"))
            {
                continue;
            }
        } else if has_memcount_boundary
            && !frames
                .iter()
                .any(|f| f.contains("__ayaml_memcount_measure"))
        {
            continue;
        }

        let at_peak = pp.get("gb").and_then(Value::as_u64).unwrap_or(0);
        let site_max = pp.get("mb").and_then(Value::as_u64).unwrap_or(0);
        let allocs = pp.get("tbk").and_then(Value::as_u64).unwrap_or(0);
        let bytes = pp.get("tb").and_then(Value::as_u64).unwrap_or(0);

        total_at_peak = total_at_peak.saturating_add(at_peak);
        total_allocs = total_allocs.saturating_add(allocs);
        total_bytes = total_bytes.saturating_add(bytes);

        let stack = summarize_dhat_frames(&frames);
        by_stack
            .entry(stack.clone())
            .and_modify(|site| {
                site.at_peak_bytes = site.at_peak_bytes.saturating_add(at_peak);
                site.site_max_bytes = site.site_max_bytes.max(site_max);
                site.allocations = site.allocations.saturating_add(allocs);
                site.total_bytes = site.total_bytes.saturating_add(bytes);
            })
            .or_insert(DhatSite {
                at_peak_bytes: at_peak,
                site_max_bytes: site_max,
                allocations: allocs,
                total_bytes: bytes,
                stack,
            });
    }

    Ok(DhatProfile {
        total_at_peak_bytes: total_at_peak,
        total_allocations: total_allocs,
        total_allocated_bytes: total_bytes,
        strict_scoped,
        sites: by_stack.into_values().collect(),
    })
}

fn clean_dhat_frame(frame: &str) -> String {
    let without_addr = frame.split_once(": ").map_or(frame, |(_, rest)| rest);
    // Strip `(in /abs/local/path/to/binary)` suffixes on fallback frames so local paths never leak.
    if let Some(idx) = without_addr.rfind(" (in /")
        && without_addr.ends_with(')')
    {
        return without_addr[..idx].trim().to_owned();
    }
    without_addr.trim().to_owned()
}

fn summarize_dhat_frames(frames: &[&str]) -> String {
    let mut chosen: Vec<&str> = Vec::with_capacity(3);
    let mut has_ayaml = false;
    for &f in frames {
        if f.contains("__ayaml_det_measure") || f.contains("__ayaml_memcount_measure") {
            continue;
        }
        let is_ayaml = f.contains("ayaml::");
        let is_interesting = is_ayaml
            || f.contains("indexmap::")
            || f.contains("hashbrown::")
            || f.contains("serde");
        if !is_interesting {
            continue;
        }
        match chosen.len().cmp(&2) {
            std::cmp::Ordering::Less => {
                chosen.push(f);
                has_ayaml |= is_ayaml;
            }
            std::cmp::Ordering::Equal => {
                // Reserve the third slot for an `ayaml::` caller when the first two are container internals.
                if has_ayaml || is_ayaml {
                    chosen.push(f);
                    break;
                }
            }
            std::cmp::Ordering::Greater => break,
        }
    }
    // If we found 2 container frames and no `ayaml::` frame above them, fill the 3rd slot with
    // the next interesting frame if any exists.
    if chosen.len() == 2 && !has_ayaml {
        for &f in frames {
            if (f.contains("indexmap::") || f.contains("hashbrown::") || f.contains("serde"))
                && !chosen.contains(&f)
            {
                chosen.push(f);
                break;
            }
        }
    }
    if chosen.is_empty() {
        chosen = frames
            .iter()
            .copied()
            .filter(|f| {
                !f.contains("__ayaml_det_measure") && !f.contains("__ayaml_memcount_measure")
            })
            .take(3)
            .collect();
    }
    chosen
        .into_iter()
        .map(clean_dhat_frame)
        .collect::<Vec<_>>()
        .join(" <- ")
}

/// Render ranked DHAT tables by at-peak bytes, allocation count, and total allocated bytes.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "KiB and percentage display formatting over byte/block counts"
)]
pub(crate) fn render_dhat_report(profile: &DhatProfile, top: usize) -> String {
    let global_peak_in_scope = profile.total_at_peak_bytes > 0;
    let scope_note = if profile.strict_scoped {
        "setup excluded"
    } else {
        "ayaml_fixtures excluded"
    };
    let mut out = if global_peak_in_scope {
        format!(
            "Summary ({scope_note}): at-peak = **{:.1} KiB** (`{} B`), allocations = **{}**, total allocated = **{:.1} KiB** (`{} B`)\n\n",
            (profile.total_at_peak_bytes as f64) / 1024.0,
            profile.total_at_peak_bytes,
            profile.total_allocations,
            (profile.total_allocated_bytes as f64) / 1024.0,
            profile.total_allocated_bytes,
        )
    } else {
        format!(
            "Summary ({scope_note}): at-peak = **n/a** (global `t-gmax` occurred outside scoped region), allocations = **{}**, total allocated = **{:.1} KiB** (`{} B`)\n\n",
            profile.total_allocations,
            (profile.total_allocated_bytes as f64) / 1024.0,
            profile.total_allocated_bytes,
        )
    };

    // 1. Peak heap attribution (`at-t-gmax` when global peak is in scope; otherwise per-site max)
    if global_peak_in_scope {
        out.push_str("## Top Call Sites by At-Peak Bytes (`at-t-gmax`)\n\n");
        out.push_str("| rank | peak KiB | share | allocs | total KiB | call site |\n| ---: | ---: | ---: | ---: | ---: | --- |\n");
        let mut by_peak = profile.sites.clone();
        by_peak.sort_by(|a, b| {
            b.at_peak_bytes
                .cmp(&a.at_peak_bytes)
                .then_with(|| b.total_bytes.cmp(&a.total_bytes))
                .then_with(|| a.stack.cmp(&b.stack))
        });
        let peak_denom = profile.total_at_peak_bytes.max(1) as f64;
        for (idx, s) in by_peak.iter().take(top).enumerate() {
            if s.at_peak_bytes == 0 {
                continue;
            }
            let _ = writeln!(
                out,
                "| {} | {:.1} | {:.1}% | {} | {:.1} | `{}` |",
                idx + 1,
                (s.at_peak_bytes as f64) / 1024.0,
                (s.at_peak_bytes as f64) * 100.0 / peak_denom,
                s.allocations,
                (s.total_bytes as f64) / 1024.0,
                s.stack
            );
        }
    } else {
        out.push_str("## Top Call Sites by Site-Max Live Bytes (`mb`, non-additive)\n\n");
        out.push_str("| rank | site-max KiB | allocs | total KiB | call site |\n| ---: | ---: | ---: | ---: | --- |\n");
        let mut by_peak = profile.sites.clone();
        by_peak.sort_by(|a, b| {
            let ka = a.site_max_bytes.max(a.at_peak_bytes);
            let kb = b.site_max_bytes.max(b.at_peak_bytes);
            kb.cmp(&ka)
                .then_with(|| b.total_bytes.cmp(&a.total_bytes))
                .then_with(|| a.stack.cmp(&b.stack))
        });
        for (idx, s) in by_peak.iter().take(top).enumerate() {
            if s.total_bytes == 0 && s.allocations == 0 {
                continue;
            }
            let site_peak = s.site_max_bytes.max(s.at_peak_bytes);
            let _ = writeln!(
                out,
                "| {} | {:.1} | {} | {:.1} | `{}` |",
                idx + 1,
                (site_peak as f64) / 1024.0,
                s.allocations,
                (s.total_bytes as f64) / 1024.0,
                s.stack
            );
        }
    }

    // 2. Allocation count attribution
    let peak_col = if global_peak_in_scope {
        "peak KiB"
    } else {
        "site-max KiB"
    };
    let _ = write!(
        out,
        "\n## Top Call Sites by Allocation Count (`tbk`)\n\n\
         | rank | allocs | share | {peak_col} | total KiB | call site |\n\
         | ---: | ---: | ---: | ---: | ---: | --- |\n"
    );
    let mut by_allocs = profile.sites.clone();
    by_allocs.sort_by(|a, b| {
        b.allocations
            .cmp(&a.allocations)
            .then_with(|| b.total_bytes.cmp(&a.total_bytes))
            .then_with(|| a.stack.cmp(&b.stack))
    });
    let alloc_denom = profile.total_allocations.max(1) as f64;
    for (idx, s) in by_allocs.iter().take(top).enumerate() {
        if s.allocations == 0 {
            continue;
        }
        let peak_val = if global_peak_in_scope {
            s.at_peak_bytes
        } else {
            s.site_max_bytes.max(s.at_peak_bytes)
        };
        let _ = writeln!(
            out,
            "| {} | {} | {:.1}% | {:.1} | {:.1} | `{}` |",
            idx + 1,
            s.allocations,
            (s.allocations as f64) * 100.0 / alloc_denom,
            (peak_val as f64) / 1024.0,
            (s.total_bytes as f64) / 1024.0,
            s.stack
        );
    }

    // 3. Total allocated bytes attribution
    let _ = write!(
        out,
        "\n## Top Call Sites by Total Allocated Bytes (`tb`)\n\n\
         | rank | total KiB | share | allocs | {peak_col} | call site |\n\
         | ---: | ---: | ---: | ---: | ---: | --- |\n"
    );
    let mut by_total = profile.sites.clone();
    by_total.sort_by(|a, b| {
        b.total_bytes
            .cmp(&a.total_bytes)
            .then_with(|| b.allocations.cmp(&a.allocations))
            .then_with(|| a.stack.cmp(&b.stack))
    });
    let tot_denom = profile.total_allocated_bytes.max(1) as f64;
    for (idx, s) in by_total.iter().take(top).enumerate() {
        if s.total_bytes == 0 {
            continue;
        }
        let peak_val = if global_peak_in_scope {
            s.at_peak_bytes
        } else {
            s.site_max_bytes.max(s.at_peak_bytes)
        };
        let _ = writeln!(
            out,
            "| {} | {:.1} | {:.1}% | {} | {:.1} | `{}` |",
            idx + 1,
            (s.total_bytes as f64) / 1024.0,
            (s.total_bytes as f64) * 100.0 / tot_denom,
            s.allocations,
            (peak_val as f64) / 1024.0,
            s.stack
        );
    }

    out
}

/// Render an aggregate DHAT diff between a base and head run.
#[must_use]
pub(crate) fn render_dhat_diff(base: &DhatProfile, head: &DhatProfile, base_rev: &str) -> String {
    let fmt_row = |label: &str, b: u64, h: u64| -> String {
        let delta = i128::from(h) - i128::from(b);
        let pct = fmt_pct_delta(b, h);
        format!("| {label} | {b} | {h} | {delta:+} ({pct}) |\n")
    };
    let mut out = format!(
        "## DHAT Diff vs `{base_rev}`\n\n\
         | metric | base | head | delta |\n\
         | --- | ---: | ---: | ---: |\n"
    );
    if base.total_at_peak_bytes > 0 && head.total_at_peak_bytes > 0 {
        out.push_str(&fmt_row(
            "at-peak bytes",
            base.total_at_peak_bytes,
            head.total_at_peak_bytes,
        ));
    } else {
        let b_str = if base.total_at_peak_bytes > 0 {
            base.total_at_peak_bytes.to_string()
        } else {
            "n/a".to_owned()
        };
        let h_str = if head.total_at_peak_bytes > 0 {
            head.total_at_peak_bytes.to_string()
        } else {
            "n/a".to_owned()
        };
        let _ = writeln!(out, "| at-peak bytes | {b_str} | {h_str} | n/a |");
    }
    out.push_str(&fmt_row(
        "allocations",
        base.total_allocations,
        head.total_allocations,
    ));
    out.push_str(&fmt_row(
        "total allocated bytes",
        base.total_allocated_bytes,
        head.total_allocated_bytes,
    ));
    out
}

#[allow(
    clippy::cast_precision_loss,
    reason = "percentage formatting over counters fits cleanly in f64"
)]
fn fmt_pct_delta(base: u64, head: u64) -> String {
    if base == 0 {
        return if head == 0 {
            "+0.00%".to_owned()
        } else {
            "n/a".to_owned()
        };
    }
    let delta = (head as f64) - (base as f64);
    format!("{:+.2}%", delta * 100.0 / (base as f64))
}

fn format_target_label(target: &ProfileTarget) -> String {
    match target {
        ProfileTarget::Bench(id) => id.clone(),
        ProfileTarget::Fixture { fixture, mode } => format!("{fixture}:{mode}"),
    }
}
