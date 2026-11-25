//! Automated 10x linear time and memory scaling checks across `ayaml-fixtures` ladders.

use std::hint::black_box;
use std::time::Instant;

use ayaml_fixtures::Fixture;

use crate::schema::{SCHEMA_VERSION, ScaleRecord};

/// Default upper bound on normalized 10x wall-clock time ratio (`(t_large / t_small) / (b_large / b_small)`).
///
/// Linear `O(n)` behavior yields ~`1.0`, while `O(n log n)` yields ~`1.25`, and quadratic `O(n^2)`
/// blows up to ~`10.0`. A bound of `1.80` accommodates cache-tier transitions (first-level,
/// second-level, last-level cache, and main memory) between 10 KiB, 100 KiB, and 1 MiB while
/// reliably catching superlinear regressions.
pub(crate) const DEFAULT_MAX_TIME_RATIO: f64 = 1.80;

/// Default upper bound on normalized 10x peak-heap ratio (`(peak_large / peak_small) / (b_large / b_small)`).
///
/// Power-of-two `Vec` and `IndexMap` doubling creates up to a ~1.6x sawtooth across an isolated
/// 10x step when the larger input lands immediately past a doubling boundary (dropping back to
/// ~0.75x on the next 10x step). A `1.80` bound tolerates power-of-two sawtooth while reliably
/// catching superlinear growth (quadratic reaches ~10.0x).
pub(crate) const DEFAULT_MAX_PEAK_RATIO: f64 = 1.80;

/// Default upper bound on normalized 10x allocation-count ratio.
pub(crate) const DEFAULT_MAX_ALLOCS_RATIO: f64 = 1.50;

/// Ordered 10x and 100x scaling ladder steps backed by `ayaml-fixtures`.
pub(crate) const SCALING_LADDERS: &[(&str, &str, &str)] = &[
    ("scale-map", "scale-map-10kb", "scale-map-100kb"),
    ("scale-map", "scale-map-100kb", "scale-map-1mb"),
    ("scale-map", "scale-map-10kb", "scale-map-1mb"),
    ("scale-seq", "scale-seq-10kb", "scale-seq-100kb"),
    ("scale-seq", "scale-seq-100kb", "scale-seq-1mb"),
    ("scale-seq", "scale-seq-10kb", "scale-seq-1mb"),
    ("scale-openapi", "scale-openapi-10kb", "scale-openapi-100kb"),
    ("scale-openapi", "scale-openapi-100kb", "scale-openapi-1mb"),
    ("scale-openapi", "scale-openapi-10kb", "scale-openapi-1mb"),
];

/// Bounds checked for each 10x scaling step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScaleLimits {
    /// Maximum allowed normalized wall-clock time ratio.
    pub(crate) time: f64,
    /// Maximum allowed normalized peak-heap ratio.
    pub(crate) peak: f64,
    /// Maximum allowed normalized allocation-count ratio.
    pub(crate) allocs: f64,
}

impl Default for ScaleLimits {
    fn default() -> Self {
        Self {
            time: DEFAULT_MAX_TIME_RATIO,
            peak: DEFAULT_MAX_PEAK_RATIO,
            allocs: DEFAULT_MAX_ALLOCS_RATIO,
        }
    }
}

/// Measured time and memory data for a single point in a scaling ladder.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ScalePoint {
    /// Fixture identifier.
    pub(crate) scenario: String,
    /// Input byte length.
    pub(crate) bytes: u64,
    /// Median parse time in nanoseconds.
    pub(crate) median_ns: f64,
    /// Peak heap bytes during `parse_value`.
    pub(crate) peak_bytes: u64,
    /// Allocation count during `parse_value`.
    pub(crate) allocations: u64,
}

/// Evaluate linear scaling between a `small` point and a `~10x` larger `large` point.
///
/// # Errors
///
/// Returns an error if input sizes, timings, or baseline counts are zero or non-finite.
pub(crate) fn evaluate_scaling_step(
    family: &str,
    small: &ScalePoint,
    large: &ScalePoint,
    limits: ScaleLimits,
) -> Result<ScaleRecord, String> {
    if small.bytes == 0 || large.bytes <= small.bytes {
        return Err(format!(
            "invalid scaling byte sizes for {family}: {} -> {}",
            small.bytes, large.bytes
        ));
    }
    if !small.median_ns.is_finite()
        || !large.median_ns.is_finite()
        || small.median_ns <= 0.0
        || large.median_ns <= 0.0
    {
        return Err(format!(
            "invalid median timings for {family}: {} -> {}",
            small.median_ns, large.median_ns
        ));
    }
    if small.peak_bytes == 0 || small.allocations == 0 {
        return Err(format!(
            "small point for {family} must have non-zero peak_bytes and allocations"
        ));
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "scaling ratios use floating-point division over byte and allocation counts"
    )]
    let size_ratio = (large.bytes as f64) / (small.bytes as f64);
    let time_ratio = large.median_ns / small.median_ns;
    #[allow(
        clippy::cast_precision_loss,
        reason = "scaling ratios use floating-point division over byte and allocation counts"
    )]
    let peak_ratio = (large.peak_bytes as f64) / (small.peak_bytes as f64);
    #[allow(
        clippy::cast_precision_loss,
        reason = "scaling ratios use floating-point division over byte and allocation counts"
    )]
    let allocs_ratio = (large.allocations as f64) / (small.allocations as f64);

    let norm_time = round3(time_ratio / size_ratio);
    let norm_peak = round3(peak_ratio / size_ratio);
    let norm_allocs = round3(allocs_ratio / size_ratio);

    let passed =
        norm_time <= limits.time && norm_peak <= limits.peak && norm_allocs <= limits.allocs;

    Ok(ScaleRecord {
        schema_version: SCHEMA_VERSION,
        family: family.to_owned(),
        small_scenario: small.scenario.clone(),
        large_scenario: large.scenario.clone(),
        small_bytes: small.bytes,
        large_bytes: large.bytes,
        size_ratio: round3(size_ratio),
        small_median_ns: round3(small.median_ns),
        large_median_ns: round3(large.median_ns),
        normalized_time_ratio: norm_time,
        small_peak_bytes: small.peak_bytes,
        large_peak_bytes: large.peak_bytes,
        normalized_peak_ratio: norm_peak,
        small_allocations: small.allocations,
        large_allocations: large.allocations,
        normalized_allocs_ratio: norm_allocs,
        passed,
    })
}

/// Measure median `ayaml::parse_value` time in nanoseconds over `samples` rounds (excluding 2 warmups).
///
/// # Errors
///
/// Returns an error if parsing fails.
pub(crate) fn measure_parse_median_ns(fixture: &Fixture, samples: usize) -> Result<f64, String> {
    let input = fixture.content.as_str();
    let sample_count = samples.max(3);
    // Warm up caches and allocator pools before sampling.
    for _ in 0..2 {
        let val = ayaml::parse_value(black_box(input))
            .map_err(|e| format!("parse failed on {}: {e}", fixture.id))?;
        drop(black_box(val));
    }

    // Batch tiny inputs so each sample exceeds clock resolution.
    let batch = if fixture.bytes <= 20_000 {
        8_usize
    } else {
        1_usize
    };
    let mut times = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        let start = Instant::now();
        for _ in 0..batch {
            let val = ayaml::parse_value(black_box(input))
                .map_err(|e| format!("parse failed on {}: {e}", fixture.id))?;
            drop(black_box(val));
        }
        let elapsed_ns = start.elapsed().as_secs_f64() * 1e9;
        #[allow(
            clippy::cast_precision_loss,
            reason = "small batch count fits cleanly in f64"
        )]
        let per_iter = elapsed_ns / (batch as f64);
        times.push(per_iter);
    }
    times.sort_by(f64::total_cmp);
    Ok(times[times.len() / 2])
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}
