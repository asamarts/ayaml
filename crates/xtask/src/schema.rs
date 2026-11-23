//! Schema definitions, validation, and local hardware/toolchain fingerprinting for `cargo xtask`.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

/// Current JSONL and fingerprint schema version.
pub(crate) const SCHEMA_VERSION: u64 = 1;

/// Hardware, OS, toolchain, commit, and fixture-manifest provenance stored beside JSONL outputs.
///
/// Deliberately omits machine hostnames and local filesystem paths so artifacts remain safe to
/// share in public reports and pull requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    /// Schema version (currently `1`).
    pub(crate) schema_version: u64,
    /// Operating system identifier (e.g. `linux`, `macos`, `windows`).
    pub(crate) os: String,
    /// Target CPU architecture (e.g. `x86_64`, `aarch64`).
    pub(crate) arch: String,
    /// Kernel release string (e.g. `Linux 6.12.0`).
    pub(crate) kernel: String,
    /// CPU model string when available.
    pub(crate) cpu: String,
    /// Logical CPU count.
    pub(crate) cpu_count: u64,
    /// Rust compiler version string (`rustc --version`).
    pub(crate) rustc: String,
    /// Workspace git commit SHA (`HEAD`) when available.
    pub(crate) git_sha: String,
    /// Combined SHA-256 digest of the `ayaml-fixtures` catalog manifest.
    pub(crate) fixture_manifest_sha256: String,
    /// UNIX timestamp in seconds when captured.
    pub(crate) timestamp_unix: u64,
}

impl Fingerprint {
    /// Capture local hardware, OS, toolchain, git commit, and fixture manifest provenance.
    #[must_use]
    pub(crate) fn capture(repo_root: &Path, fixture_manifest_sha256: String) -> Self {
        let cpu = fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|contents| {
                contents
                    .lines()
                    .find_map(|line| line.strip_prefix("model name"))
                    .and_then(|rest| rest.split_once(':').map(|(_, v)| v.trim().to_owned()))
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_owned());
        let cpu_count =
            std::thread::available_parallelism().map_or(1, |n| u64::try_from(n.get()).unwrap_or(1));
        let kernel = command_stdout("uname", &["-sr"]).unwrap_or_else(|| "unknown".to_owned());
        let rustc = command_stdout("rustc", &["--version"]).unwrap_or_else(|| "unknown".to_owned());
        let git_sha = git_head(repo_root).unwrap_or_else(|| "unknown".to_owned());
        let timestamp_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());

        Self {
            schema_version: SCHEMA_VERSION,
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            kernel,
            cpu,
            cpu_count,
            rustc,
            git_sha,
            fixture_manifest_sha256,
            timestamp_unix,
        }
    }

    /// Serialize as deterministic pretty-printed JSON.
    ///
    /// # Errors
    ///
    /// Returns an error if JSON serialization fails.
    pub(crate) fn to_pretty_json(&self) -> Result<String, String> {
        let mut map = Map::new();
        map.insert("schema_version".into(), Value::from(self.schema_version));
        map.insert("os".into(), Value::from(self.os.clone()));
        map.insert("arch".into(), Value::from(self.arch.clone()));
        map.insert("kernel".into(), Value::from(self.kernel.clone()));
        map.insert("cpu".into(), Value::from(self.cpu.clone()));
        map.insert("cpu_count".into(), Value::from(self.cpu_count));
        map.insert("rustc".into(), Value::from(self.rustc.clone()));
        map.insert("git_sha".into(), Value::from(self.git_sha.clone()));
        map.insert(
            "fixture_manifest_sha256".into(),
            Value::from(self.fixture_manifest_sha256.clone()),
        );
        map.insert("timestamp_unix".into(), Value::from(self.timestamp_unix));
        serde_json::to_string_pretty(&Value::Object(map))
            .map(|mut s| {
                s.push('\n');
                s
            })
            .map_err(|e| format!("failed to serialize fingerprint: {e}"))
    }

    /// Parse and strictly validate a fingerprint JSON document.
    ///
    /// # Errors
    ///
    /// Returns an error if required fields are missing, malformed, or unsupported.
    pub(crate) fn parse_json(text: &str) -> Result<Self, String> {
        let value: Value =
            serde_json::from_str(text).map_err(|e| format!("invalid fingerprint JSON: {e}"))?;
        let obj = value
            .as_object()
            .ok_or_else(|| "fingerprint must be a JSON object".to_owned())?;
        let schema_version = req_u64(obj, "schema_version")?;
        if schema_version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported fingerprint schema_version {schema_version}"
            ));
        }
        let fp = Self {
            schema_version,
            os: req_nonempty_str(obj, "os")?,
            arch: req_nonempty_str(obj, "arch")?,
            kernel: req_nonempty_str(obj, "kernel")?,
            cpu: req_nonempty_str(obj, "cpu")?,
            cpu_count: req_u64(obj, "cpu_count")?,
            rustc: req_nonempty_str(obj, "rustc")?,
            git_sha: req_nonempty_str(obj, "git_sha")?,
            fixture_manifest_sha256: req_sha256_or_unknown(obj, "fixture_manifest_sha256")?,
            timestamp_unix: req_u64(obj, "timestamp_unix")?,
        };
        if fp.cpu_count == 0 {
            return Err("fingerprint cpu_count must be >= 1".to_owned());
        }
        Ok(fp)
    }
}

/// One Criterion wall-clock timing record in `speed.jsonl`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpeedRecord {
    /// Schema version (`1`).
    pub(crate) schema_version: u64,
    /// Fixture/scenario identifier.
    pub(crate) scenario: String,
    /// Operation/layer measured (e.g. `parse_value`, `typed_struct`).
    pub(crate) operation: String,
    /// Input byte length (`0` if composite/synthetic).
    pub(crate) bytes: u64,
    /// Pinned fixture SHA-256 digest (or `"unknown"` for multi-file batches).
    pub(crate) fixture_sha256: String,
    /// Criterion median point estimate in nanoseconds.
    pub(crate) median_ns: f64,
    /// Criterion mean point estimate in nanoseconds.
    pub(crate) mean_ns: f64,
    /// Criterion standard deviation in nanoseconds.
    pub(crate) stddev_ns: f64,
    /// Throughput in MiB/s derived from `bytes` and `median_ns`.
    pub(crate) throughput_mib_s: f64,
}

impl SpeedRecord {
    /// Stable lookup key `(scenario, operation)`.
    #[must_use]
    pub(crate) fn key(&self) -> (String, String) {
        (self.scenario.clone(), self.operation.clone())
    }

    /// Serialize to a single JSONL line.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    pub(crate) fn to_json_line(&self) -> Result<String, String> {
        let mut map = Map::new();
        map.insert("schema_version".into(), Value::from(self.schema_version));
        map.insert("scenario".into(), Value::from(self.scenario.clone()));
        map.insert("operation".into(), Value::from(self.operation.clone()));
        map.insert("bytes".into(), Value::from(self.bytes));
        map.insert(
            "fixture_sha256".into(),
            Value::from(self.fixture_sha256.clone()),
        );
        map.insert("median_ns".into(), Value::from(self.median_ns));
        map.insert("mean_ns".into(), Value::from(self.mean_ns));
        map.insert("stddev_ns".into(), Value::from(self.stddev_ns));
        map.insert(
            "throughput_mib_s".into(),
            Value::from(self.throughput_mib_s),
        );
        serde_json::to_string(&Value::Object(map))
            .map_err(|e| format!("failed to serialize speed record: {e}"))
    }

    /// Parse and strictly validate one `speed.jsonl` row.
    ///
    /// # Errors
    ///
    /// Returns an error if the row violates schema v1 constraints.
    pub(crate) fn parse_json_line(line: &str) -> Result<Self, String> {
        let value: Value =
            serde_json::from_str(line).map_err(|e| format!("invalid speed JSONL row: {e}"))?;
        let obj = value
            .as_object()
            .ok_or_else(|| "speed row must be a JSON object".to_owned())?;
        let schema_version = req_u64(obj, "schema_version")?;
        if schema_version != SCHEMA_VERSION {
            return Err(format!("unsupported speed schema_version {schema_version}"));
        }
        Ok(Self {
            schema_version,
            scenario: req_nonempty_str(obj, "scenario")?,
            operation: req_nonempty_str(obj, "operation")?,
            bytes: req_u64(obj, "bytes")?,
            fixture_sha256: req_sha256_or_unknown(obj, "fixture_sha256")?,
            median_ns: req_pos_f64(obj, "median_ns")?,
            mean_ns: req_pos_f64(obj, "mean_ns")?,
            stddev_ns: req_nonneg_f64(obj, "stddev_ns")?,
            throughput_mib_s: req_nonneg_f64(obj, "throughput_mib_s")?,
        })
    }
}

/// One counting-allocator record in `memory.jsonl` (matches `memcount --jsonl`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MemoryRecord {
    /// Schema version (`1`).
    pub(crate) schema_version: u64,
    /// Fixture identifier.
    pub(crate) scenario: String,
    /// Operation measured (`parse_value`, `typed_struct`, etc.).
    pub(crate) operation: String,
    /// Input byte length.
    pub(crate) bytes: u64,
    /// Pinned lowercase hex SHA-256 digest.
    pub(crate) fixture_sha256: String,
    /// Peak live heap bytes.
    pub(crate) peak_bytes: u64,
    /// Retained live heap bytes at completion.
    pub(crate) live_bytes: u64,
    /// Total allocator calls.
    pub(crate) allocations: u64,
    /// Cumulative allocated bytes.
    pub(crate) total_allocated_bytes: u64,
    /// `peak_bytes / bytes`.
    pub(crate) peak_to_input_ratio: f64,
}

impl MemoryRecord {
    /// Stable lookup key `(scenario, operation)`.
    #[must_use]
    pub(crate) fn key(&self) -> (String, String) {
        (self.scenario.clone(), self.operation.clone())
    }

    /// Serialize to a single JSONL line.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    pub(crate) fn to_json_line(&self) -> Result<String, String> {
        let mut map = Map::new();
        map.insert("schema_version".into(), Value::from(self.schema_version));
        map.insert("scenario".into(), Value::from(self.scenario.clone()));
        map.insert("operation".into(), Value::from(self.operation.clone()));
        map.insert("bytes".into(), Value::from(self.bytes));
        map.insert(
            "fixture_sha256".into(),
            Value::from(self.fixture_sha256.clone()),
        );
        map.insert("peak_bytes".into(), Value::from(self.peak_bytes));
        map.insert("live_bytes".into(), Value::from(self.live_bytes));
        map.insert("allocations".into(), Value::from(self.allocations));
        map.insert(
            "total_allocated_bytes".into(),
            Value::from(self.total_allocated_bytes),
        );
        map.insert(
            "peak_to_input_ratio".into(),
            Value::from(self.peak_to_input_ratio),
        );
        serde_json::to_string(&Value::Object(map))
            .map_err(|e| format!("failed to serialize memory record: {e}"))
    }

    /// Parse and strictly validate one `memory.jsonl` row.
    ///
    /// # Errors
    ///
    /// Returns an error if the row violates schema v1 constraints.
    pub(crate) fn parse_json_line(line: &str) -> Result<Self, String> {
        let value: Value =
            serde_json::from_str(line).map_err(|e| format!("invalid memory JSONL row: {e}"))?;
        let obj = value
            .as_object()
            .ok_or_else(|| "memory row must be a JSON object".to_owned())?;
        let schema_version = req_u64(obj, "schema_version")?;
        if schema_version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported memory schema_version {schema_version}"
            ));
        }
        Ok(Self {
            schema_version,
            scenario: req_nonempty_str(obj, "scenario")?,
            operation: req_nonempty_str(obj, "operation")?,
            bytes: req_u64(obj, "bytes")?,
            fixture_sha256: req_sha256_or_unknown(obj, "fixture_sha256")?,
            peak_bytes: req_u64(obj, "peak_bytes")?,
            live_bytes: req_u64(obj, "live_bytes")?,
            allocations: req_u64(obj, "allocations")?,
            total_allocated_bytes: req_u64(obj, "total_allocated_bytes")?,
            peak_to_input_ratio: req_nonneg_f64(obj, "peak_to_input_ratio")?,
        })
    }
}

/// One steady-state Callgrind record in `callgrind.jsonl` (matches `tools/check_instructions.py`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CallgrindRecord {
    /// Schema version (`1`).
    pub(crate) schema_version: u64,
    /// Operation measured (`events`, `value`, `typed`, etc.).
    pub(crate) operation: String,
    /// Workload fixture identifier.
    pub(crate) workload: String,
    /// Input byte length.
    pub(crate) bytes: u64,
    /// Pinned fixture SHA-256 digest.
    pub(crate) fixture_sha256: String,
    /// Baseline steady-state instructions (`Ir`), if present on base revision.
    pub(crate) base_ir: Option<u64>,
    /// Head steady-state instructions (`Ir`).
    pub(crate) head_ir: u64,
    /// Percentage change in `Ir`.
    pub(crate) ir_change_pct: Option<f64>,
    /// Baseline `EstimatedCycles`, if present on base revision.
    pub(crate) base_estimated_cycles: Option<u64>,
    /// Head `EstimatedCycles`.
    pub(crate) head_estimated_cycles: u64,
    /// Percentage change in `EstimatedCycles`.
    pub(crate) cycles_change_pct: Option<f64>,
    /// Combined level-1 cache misses (`I1mr + D1mr + D1mw`).
    pub(crate) l1_misses: u64,
    /// Combined last-level cache misses (`ILmr + DLmr + DLmw`).
    pub(crate) ll_misses: u64,
    /// Combined branch mispredicts (`Bcm + Bim`).
    pub(crate) branch_mispredicts: u64,
    /// Gate status (`ok`, `new`, or `fail`).
    pub(crate) status: String,
}

impl CallgrindRecord {
    /// Stable lookup key `(workload, operation)`.
    #[must_use]
    pub(crate) fn key(&self) -> (String, String) {
        (self.workload.clone(), self.operation.clone())
    }

    /// Serialize to a single JSONL line.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    pub(crate) fn to_json_line(&self) -> Result<String, String> {
        let mut map = Map::new();
        map.insert("schema_version".into(), Value::from(self.schema_version));
        map.insert("operation".into(), Value::from(self.operation.clone()));
        map.insert("workload".into(), Value::from(self.workload.clone()));
        map.insert("bytes".into(), Value::from(self.bytes));
        map.insert(
            "fixture_sha256".into(),
            Value::from(self.fixture_sha256.clone()),
        );
        map.insert(
            "base_ir".into(),
            self.base_ir.map_or(Value::Null, Value::from),
        );
        map.insert("head_ir".into(), Value::from(self.head_ir));
        map.insert(
            "ir_change_pct".into(),
            self.ir_change_pct.map_or(Value::Null, Value::from),
        );
        map.insert(
            "base_estimated_cycles".into(),
            self.base_estimated_cycles.map_or(Value::Null, Value::from),
        );
        map.insert(
            "head_estimated_cycles".into(),
            Value::from(self.head_estimated_cycles),
        );
        map.insert(
            "cycles_change_pct".into(),
            self.cycles_change_pct.map_or(Value::Null, Value::from),
        );
        map.insert("l1_misses".into(), Value::from(self.l1_misses));
        map.insert("ll_misses".into(), Value::from(self.ll_misses));
        map.insert(
            "branch_mispredicts".into(),
            Value::from(self.branch_mispredicts),
        );
        map.insert("status".into(), Value::from(self.status.clone()));
        serde_json::to_string(&Value::Object(map))
            .map_err(|e| format!("failed to serialize callgrind record: {e}"))
    }

    /// Parse and strictly validate one `callgrind.jsonl` row.
    ///
    /// # Errors
    ///
    /// Returns an error if the row violates schema v1 constraints.
    pub(crate) fn parse_json_line(line: &str) -> Result<Self, String> {
        let value: Value =
            serde_json::from_str(line).map_err(|e| format!("invalid callgrind JSONL row: {e}"))?;
        let obj = value
            .as_object()
            .ok_or_else(|| "callgrind row must be a JSON object".to_owned())?;
        let schema_version = req_u64(obj, "schema_version")?;
        if schema_version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported callgrind schema_version {schema_version}"
            ));
        }
        let status = req_nonempty_str(obj, "status")?;
        if !matches!(status.as_str(), "ok" | "new" | "fail") {
            return Err(format!("invalid callgrind status {status:?}"));
        }
        Ok(Self {
            schema_version,
            operation: req_nonempty_str(obj, "operation")?,
            workload: req_nonempty_str(obj, "workload")?,
            bytes: req_u64(obj, "bytes")?,
            fixture_sha256: req_sha256_or_unknown(obj, "fixture_sha256")?,
            base_ir: opt_u64(obj, "base_ir")?,
            head_ir: req_u64(obj, "head_ir")?,
            ir_change_pct: opt_f64(obj, "ir_change_pct")?,
            base_estimated_cycles: opt_u64(obj, "base_estimated_cycles")?,
            head_estimated_cycles: req_u64(obj, "head_estimated_cycles")?,
            cycles_change_pct: opt_f64(obj, "cycles_change_pct")?,
            l1_misses: req_u64(obj, "l1_misses")?,
            ll_misses: req_u64(obj, "ll_misses")?,
            branch_mispredicts: req_u64(obj, "branch_mispredicts")?,
            status,
        })
    }
}

/// One 10x linear scaling verification record in `scale.jsonl`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ScaleRecord {
    /// Schema version (`1`).
    pub(crate) schema_version: u64,
    /// Ladder family (`scale-map`, `scale-seq`, `scale-openapi`).
    pub(crate) family: String,
    /// Smaller step fixture ID (e.g. `scale-map-10kb`).
    pub(crate) small_scenario: String,
    /// Larger step fixture ID (e.g. `scale-map-100kb`).
    pub(crate) large_scenario: String,
    /// Smaller input byte length.
    pub(crate) small_bytes: u64,
    /// Larger input byte length.
    pub(crate) large_bytes: u64,
    /// Input size ratio (`large_bytes / small_bytes`, ~10.0).
    pub(crate) size_ratio: f64,
    /// Small-step median wall time in nanoseconds.
    pub(crate) small_median_ns: f64,
    /// Large-step median wall time in nanoseconds.
    pub(crate) large_median_ns: f64,
    /// Normalized time scaling ratio `(time_ratio / size_ratio)` (`~1.0` = strictly linear).
    pub(crate) normalized_time_ratio: f64,
    /// Small-step peak heap bytes.
    pub(crate) small_peak_bytes: u64,
    /// Large-step peak heap bytes.
    pub(crate) large_peak_bytes: u64,
    /// Normalized peak-memory scaling ratio `(peak_ratio / size_ratio)` (`~1.0` = strictly linear).
    pub(crate) normalized_peak_ratio: f64,
    /// Small-step allocation count.
    pub(crate) small_allocations: u64,
    /// Large-step allocation count.
    pub(crate) large_allocations: u64,
    /// Normalized allocation-count scaling ratio `(allocs_ratio / size_ratio)`.
    pub(crate) normalized_allocs_ratio: f64,
    /// Whether all linear bounds passed.
    pub(crate) passed: bool,
}

impl ScaleRecord {
    /// Serialize to a single JSONL line.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    pub(crate) fn to_json_line(&self) -> Result<String, String> {
        let mut map = Map::new();
        map.insert("schema_version".into(), Value::from(self.schema_version));
        map.insert("family".into(), Value::from(self.family.clone()));
        map.insert(
            "small_scenario".into(),
            Value::from(self.small_scenario.clone()),
        );
        map.insert(
            "large_scenario".into(),
            Value::from(self.large_scenario.clone()),
        );
        map.insert("small_bytes".into(), Value::from(self.small_bytes));
        map.insert("large_bytes".into(), Value::from(self.large_bytes));
        map.insert("size_ratio".into(), Value::from(self.size_ratio));
        map.insert("small_median_ns".into(), Value::from(self.small_median_ns));
        map.insert("large_median_ns".into(), Value::from(self.large_median_ns));
        map.insert(
            "normalized_time_ratio".into(),
            Value::from(self.normalized_time_ratio),
        );
        map.insert(
            "small_peak_bytes".into(),
            Value::from(self.small_peak_bytes),
        );
        map.insert(
            "large_peak_bytes".into(),
            Value::from(self.large_peak_bytes),
        );
        map.insert(
            "normalized_peak_ratio".into(),
            Value::from(self.normalized_peak_ratio),
        );
        map.insert(
            "small_allocations".into(),
            Value::from(self.small_allocations),
        );
        map.insert(
            "large_allocations".into(),
            Value::from(self.large_allocations),
        );
        map.insert(
            "normalized_allocs_ratio".into(),
            Value::from(self.normalized_allocs_ratio),
        );
        map.insert("passed".into(), Value::from(self.passed));
        serde_json::to_string(&Value::Object(map))
            .map_err(|e| format!("failed to serialize scale record: {e}"))
    }

    /// Parse and strictly validate one `scale.jsonl` row.
    ///
    /// # Errors
    ///
    /// Returns an error if the row violates schema v1 constraints.
    pub(crate) fn parse_json_line(line: &str) -> Result<Self, String> {
        let value: Value =
            serde_json::from_str(line).map_err(|e| format!("invalid scale JSONL row: {e}"))?;
        let obj = value
            .as_object()
            .ok_or_else(|| "scale row must be a JSON object".to_owned())?;
        let schema_version = req_u64(obj, "schema_version")?;
        if schema_version != SCHEMA_VERSION {
            return Err(format!("unsupported scale schema_version {schema_version}"));
        }
        let passed = obj
            .get("passed")
            .and_then(Value::as_bool)
            .ok_or_else(|| "missing bool field `passed`".to_owned())?;
        Ok(Self {
            schema_version,
            family: req_nonempty_str(obj, "family")?,
            small_scenario: req_nonempty_str(obj, "small_scenario")?,
            large_scenario: req_nonempty_str(obj, "large_scenario")?,
            small_bytes: req_u64(obj, "small_bytes")?,
            large_bytes: req_u64(obj, "large_bytes")?,
            size_ratio: req_pos_f64(obj, "size_ratio")?,
            small_median_ns: req_pos_f64(obj, "small_median_ns")?,
            large_median_ns: req_pos_f64(obj, "large_median_ns")?,
            normalized_time_ratio: req_pos_f64(obj, "normalized_time_ratio")?,
            small_peak_bytes: req_u64(obj, "small_peak_bytes")?,
            large_peak_bytes: req_u64(obj, "large_peak_bytes")?,
            normalized_peak_ratio: req_nonneg_f64(obj, "normalized_peak_ratio")?,
            small_allocations: req_u64(obj, "small_allocations")?,
            large_allocations: req_u64(obj, "large_allocations")?,
            normalized_allocs_ratio: req_nonneg_f64(obj, "normalized_allocs_ratio")?,
            passed,
        })
    }
}

/// Read and validate all [`SpeedRecord`] rows in `path`.
///
/// # Errors
///
/// Returns an error if the file cannot be read or any row is invalid.
pub(crate) fn read_speed_jsonl(path: &Path) -> Result<Vec<SpeedRecord>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let rows: Result<Vec<_>, _> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(SpeedRecord::parse_json_line)
        .collect();
    let rows = rows?;
    if rows.is_empty() {
        return Err(format!("{} contains no speed records", path.display()));
    }
    Ok(rows)
}

/// Read and validate all [`MemoryRecord`] rows in `path`.
///
/// # Errors
///
/// Returns an error if the file cannot be read or any row is invalid.
pub(crate) fn read_memory_jsonl(path: &Path) -> Result<Vec<MemoryRecord>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let rows: Result<Vec<_>, _> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(MemoryRecord::parse_json_line)
        .collect();
    let rows = rows?;
    if rows.is_empty() {
        return Err(format!("{} contains no memory records", path.display()));
    }
    Ok(rows)
}

/// Read and validate all [`CallgrindRecord`] rows in `path`.
///
/// # Errors
///
/// Returns an error if the file cannot be read or any row is invalid.
pub(crate) fn read_callgrind_jsonl(path: &Path) -> Result<Vec<CallgrindRecord>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let rows: Result<Vec<_>, _> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(CallgrindRecord::parse_json_line)
        .collect();
    let rows = rows?;
    if rows.is_empty() {
        return Err(format!("{} contains no callgrind records", path.display()));
    }
    Ok(rows)
}

/// Read and validate all [`ScaleRecord`] rows in `path`.
///
/// # Errors
///
/// Returns an error if the file cannot be read or any row is invalid.
pub(crate) fn read_scale_jsonl(path: &Path) -> Result<Vec<ScaleRecord>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let rows: Result<Vec<_>, _> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(ScaleRecord::parse_json_line)
        .collect();
    let rows = rows?;
    if rows.is_empty() {
        return Err(format!("{} contains no scale records", path.display()));
    }
    Ok(rows)
}

fn git_head(dir: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", &dir.to_string_lossy(), "rev-parse", "HEAD"])
        .output()
        .ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

fn req_nonempty_str(obj: &Map<String, Value>, key: &str) -> Result<String, String> {
    let s = obj
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field `{key}`"))?;
    if s.trim().is_empty() {
        return Err(format!("field `{key}` must not be empty"));
    }
    Ok(s.to_owned())
}

fn req_sha256_or_unknown(obj: &Map<String, Value>, key: &str) -> Result<String, String> {
    let s = req_nonempty_str(obj, key)?;
    if s == "unknown" {
        return Ok(s);
    }
    if s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Ok(s);
    }
    Err(format!(
        "field `{key}` must be a 64-char lowercase hex SHA-256 or \"unknown\", got {s:?}"
    ))
}

fn req_u64(obj: &Map<String, Value>, key: &str) -> Result<u64, String> {
    obj.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing unsigned integer field `{key}`"))
}

fn opt_u64(obj: &Map<String, Value>, key: &str) -> Result<Option<u64>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("invalid optional unsigned integer field `{key}`")),
    }
}

fn req_nonneg_f64(obj: &Map<String, Value>, key: &str) -> Result<f64, String> {
    let v = obj
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("missing numeric field `{key}`"))?;
    if !v.is_finite() || v < 0.0 {
        return Err(format!("field `{key}` must be finite and >= 0, got {v}"));
    }
    Ok(v)
}

fn req_pos_f64(obj: &Map<String, Value>, key: &str) -> Result<f64, String> {
    let v = req_nonneg_f64(obj, key)?;
    if v <= 0.0 {
        return Err(format!("field `{key}` must be > 0, got {v}"));
    }
    Ok(v)
}

fn opt_f64(obj: &Map<String, Value>, key: &str) -> Result<Option<f64>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|x| x.is_finite())
            .map(Some)
            .ok_or_else(|| format!("invalid optional float field `{key}`")),
    }
}
