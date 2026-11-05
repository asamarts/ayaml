//! Time each layer of ayaml on a fixed set of inputs.
//!
//! Results depend on the host, so compare only runs from the same host, compiler,
//! and build mode. `docs/benchmarks.md` has the method.

use criterion::{Criterion, criterion_group, criterion_main};
use std::fmt::Write as _;
use std::hint::black_box;

/// A representative document with mixed collections, tags, anchors, and merge keys.
const TOOLCHAIN_PINS: &str = include_str!("../../../tests/fixtures/compat/toolchain-pins.yml");

/// Entries per short-block workload. Keep in step with `SHORT_ENTRIES` in
/// `fuzz/src/bin/memcount.rs`.
const SHORT_ENTRIES: usize = 20_000;

fn depth_input(depth: usize) -> String {
    let mut value = "0".to_owned();
    for _ in 0..depth {
        value = format!("[{value}]");
    }
    value
}

fn alias_input(count: usize) -> String {
    let mut value = String::from("base: &base {x: 1}\nitems:\n");
    for _ in 0..count {
        value.push_str("  - *base\n");
    }
    value
}

/// A sequence of short plain-scalar entries: `- a`.
fn short_sequence_input(count: usize) -> String {
    "- a\n".repeat(count)
}

/// Explicit key/value pairs with short scalar payloads: `? k0` / `: v0`.
fn short_explicit_input(count: usize) -> String {
    let mut value = String::with_capacity(count * 16);
    for index in 0..count {
        writeln!(value, "? k{index}\n: v{index}").expect("write to String");
    }
    value
}

/// Implicit mapping entries with short scalar payloads: `k0: v0`.
fn short_mapping_input(count: usize) -> String {
    let mut value = String::with_capacity(count * 16);
    for index in 0..count {
        writeln!(value, "k{index}: v{index}").expect("write to String");
    }
    value
}

fn layers(criterion: &mut Criterion) {
    let text = TOOLCHAIN_PINS;
    let limits = ayaml::Limits::default();
    let events = ayaml::internals::parse_events(text, &limits).expect("events");
    let value = ayaml::parse_value(text).expect("value");

    criterion.bench_function("scan/toolchain-pins", |bencher| {
        bencher.iter(|| black_box(ayaml::internals::scan_tokens(text, &limits).expect("scan")));
    });
    criterion.bench_function("events/toolchain-pins", |bencher| {
        bencher.iter(|| black_box(ayaml::internals::parse_events(text, &limits).expect("events")));
    });
    criterion.bench_function("compose/toolchain-pins", |bencher| {
        bencher.iter(|| {
            black_box(
                ayaml::internals::compose_events(&events, text.len(), &limits).expect("compose"),
            )
        });
    });
    criterion.bench_function("from-str/toolchain-pins", |bencher| {
        bencher.iter(|| black_box(ayaml::from_str::<ayaml::Value>(text).expect("from_str")));
    });
    criterion.bench_function("emit-events/toolchain-pins", |bencher| {
        bencher.iter(|| black_box(ayaml::to_string_events(&events).expect("emit events")));
    });
    criterion.bench_function("emit/toolchain-pins", |bencher| {
        bencher.iter(|| black_box(ayaml::to_string_value(&value).expect("emit")));
    });
}

fn workloads(criterion: &mut Criterion) {
    for (name, text) in [
        ("toolchain-pins", TOOLCHAIN_PINS.to_owned()),
        ("empty", String::new()),
        ("tiny", "a\n".to_owned()),
        ("large-scalar", format!("{}\n", "x".repeat(1_000_000))),
        ("depth-128", depth_input(128)),
        ("aliases-1000", alias_input(1_000)),
        ("short-seq-20000", short_sequence_input(SHORT_ENTRIES)),
        ("short-explicit-20000", short_explicit_input(SHORT_ENTRIES)),
        ("short-map-20000", short_mapping_input(SHORT_ENTRIES)),
    ] {
        criterion.bench_function(&format!("parse/{name}"), |bencher| {
            bencher.iter(|| black_box(ayaml::parse_documents(&text).expect("parse")));
        });
    }
}

criterion_group!(benches, layers, workloads);
criterion_main!(benches);
