//! Boundary, scaling, structural, and error-path YAML fixture generators.

use std::fmt::Write as _;

use crate::domains;
use crate::prng::Rng;

/// Generate a block mapping of `kN: value_N` lines until byte length reaches at least `target_bytes`.
pub(crate) fn block_mapping_sized(seed: u64, target_bytes: usize) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(target_bytes + 64);
    for idx in 0_usize.. {
        if out.len() >= target_bytes {
            break;
        }
        let _ = writeln!(out, "key_{idx:06}: {}_{}", rng.word(), rng.below(100_000));
    }
    out
}

/// Generate a block sequence of `- item` lines until byte length reaches at least `target_bytes`.
pub(crate) fn block_sequence_sized(seed: u64, target_bytes: usize) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(target_bytes + 64);
    for idx in 0_usize.. {
        if out.len() >= target_bytes {
            break;
        }
        let _ = writeln!(out, "- {}_item_{idx:06}", rng.word());
    }
    out
}

/// Generate a document targeting an exact byte window around the 4 KiB typed strategy boundary.
pub(crate) fn boundary_mapping(seed: u64, target_bytes: usize) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(target_bytes + 32);
    out.push_str("service: boundary-probe\nentries:\n");
    for idx in 0_usize.. {
        let line = format!(
            "  k_{idx:04}: \"{}_{:04}\"\n",
            rng.word(),
            rng.below(10_000)
        );
        if out.len() + line.len() > target_bytes {
            break;
        }
        out.push_str(&line);
    }
    out
}

/// Nested flow sequences `[ [ ... 0 ... ] ]` of exact `depth`.
pub(crate) fn nested_flow_depth(depth: usize) -> String {
    let mut out = String::with_capacity(depth * 2 + 2);
    for _ in 0..depth {
        out.push('[');
    }
    out.push('0');
    for _ in 0..depth {
        out.push(']');
    }
    out.push('\n');
    out
}

/// Document with one anchored mapping reused `count` times via aliases.
pub(crate) fn aliases_repeated(count: usize) -> String {
    let mut out = String::with_capacity(count * 10 + 64);
    out.push_str("base: &base {x: 1, y: 2, label: shared}\nitems:\n");
    for _ in 0..count {
        out.push_str("  - *base\n");
    }
    out
}

/// Single large plain scalar (`bytes` of `'x'` followed by newline).
pub(crate) fn large_scalar(bytes: usize) -> String {
    let mut out = String::with_capacity(bytes + 1);
    for _ in 0..bytes {
        out.push('x');
    }
    out.push('\n');
    out
}

/// Single-line flow mapping sized to approximately `target_bytes`.
pub(crate) fn flow_mapping_sized(target_bytes: usize) -> String {
    let mut out = String::with_capacity(target_bytes + 32);
    out.push('{');
    for idx in 0_usize.. {
        if out.len() >= target_bytes {
            break;
        }
        if idx > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "k{idx}: {idx}");
    }
    out.push_str("}\n");
    out
}

/// Multi-line flow sequence of integers sized to approximately `target_bytes`.
pub(crate) fn flow_sequence_sized(seed: u64, target_bytes: usize) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(target_bytes + 32);
    out.push('[');
    for idx in 0_usize.. {
        if out.len() >= target_bytes {
            break;
        }
        let sep = if idx % 20 == 0 { "\n  " } else { " " };
        let _ = write!(out, "{sep}{},", rng.below(100_000));
    }
    out.push_str("\n]\n");
    out
}

/// Short block sequence entries (`- a\n`) repeated `count` times.
pub(crate) fn short_sequence(count: usize) -> String {
    "- a\n".repeat(count)
}

/// Short block mapping entries (`kN: vN\n`) repeated `count` times.
pub(crate) fn short_mapping(count: usize) -> String {
    let mut out = String::with_capacity(count * 16);
    for idx in 0..count {
        let _ = writeln!(out, "k{idx}: v{idx}");
    }
    out
}

/// Short explicit key/value mapping entries (`? kN\n: vN\n`) repeated `count` times.
pub(crate) fn short_explicit_mapping(count: usize) -> String {
    let mut out = String::with_capacity(count * 18);
    for idx in 0..count {
        let _ = writeln!(out, "? k{idx}\n: v{idx}");
    }
    out
}

/// Deeply nested complex mapping keys (`depth` outer levels wrapping a `width`-entry mapping key).
pub(crate) fn nested_complex_keys(depth: usize, width: usize) -> String {
    let mut inner = String::with_capacity(width * 12 + depth * 12 + 4);
    inner.push('{');
    for idx in 0..width {
        if idx > 0 {
            inner.push_str(", ");
        }
        let _ = write!(inner, "k{idx}: {idx}");
    }
    inner.push('}');
    let mut out = String::with_capacity(inner.len() + depth * 10 + 1);
    for _ in 0..depth {
        out.push_str("{? ");
    }
    out.push_str(&inner);
    for _ in 0..depth {
        out.push_str(": null}");
    }
    out.push('\n');
    out
}

/// Valid 64 KiB YAML mapping of integers with a trailing string value for late typed-decode errors.
pub(crate) fn typed_mismatch_late(seed: u64) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(64 * 1024 + 64);
    for idx in 0_usize.. {
        if out.len() >= 64 * 1024 {
            break;
        }
        let _ = writeln!(out, "k_{idx:06}: {}", rng.below(100_000));
    }
    out.push_str("k_mismatch: \"not-an-integer\"\n");
    out
}

/// `OpenAPI` scaling generator hitting target approximate size class.
pub(crate) fn openapi_scaled(seed: u64, path_count: usize) -> String {
    domains::openapi_spec(seed, path_count)
}

/// Error-path fixture: malformed flow syntax near the start of a ~64 KiB document.
pub(crate) fn err_syntax_early(seed: u64) -> String {
    let mut out = String::from("broken: [1, 2}\n");
    out.push_str(&block_mapping_sized(seed, 64 * 1024));
    out
}

/// Error-path fixture: malformed flow syntax at the very end of a ~64 KiB document.
pub(crate) fn err_syntax_late(seed: u64) -> String {
    let mut out = block_mapping_sized(seed, 64 * 1024);
    out.push_str("broken_tail: [1, {a: 2}\n");
    out
}

/// Error-path fixture: duplicate mapping key at the end of a wide mapping.
pub(crate) fn err_duplicate_key_late(seed: u64) -> String {
    let mut out = block_mapping_sized(seed, 64 * 1024);
    out.push_str("key_000000: duplicate_value\n");
    out
}

/// Error-path fixture: collection depth 129 exceeding `ayaml::MAX_DEPTH`.
pub(crate) fn err_depth_limit() -> String {
    nested_flow_depth(129)
}

/// Error-path fixture: invalid double-quoted escape at the end of an escape-heavy document.
pub(crate) fn err_invalid_escape_late(seed: u64) -> String {
    let mut out = domains::escape_routes(seed, 80);
    out.push_str("        - name: \"bad_escape_\\q\"\n");
    out
}
