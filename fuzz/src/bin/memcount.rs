//! Allocation budgets for the value path.
//!
//! This counts live and peak allocated bytes and the allocation count while
//! parsing, emitting, and serializing one input, and fails when a count passes a
//! budget given on the command line. It lives here, outside the parser crate's
//! `forbid(unsafe_code)` graph, because a counting allocator needs `unsafe`.
//!
//! Usage: `memcount PATH|@NAME [PARSE_PEAK EMIT_PEAK PARSE_ALLOCS EMIT_ALLOCS
//! SERIALIZE_PEAK SERIALIZE_ALLOCS]`.
#![allow(
    unsafe_code,
    reason = "the counting global allocator is the crate-local unsafe exception"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            account(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) };
    }
}

fn reset_peak() {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
}

fn account(size: usize) {
    let live = LIVE.fetch_add(size, Ordering::Relaxed) + size;
    let mut peak = PEAK.load(Ordering::Relaxed);
    while live > peak {
        match PEAK.compare_exchange_weak(peak, live, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(current) => peak = current,
        }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Entries per short-block workload. Keep in step with
/// `SHORT_ENTRIES` in `crates/ayaml-bench/benches/throughput.rs`.
const SHORT_ENTRIES: usize = 20_000;

/// A sequence of short plain-scalar entries: `- a`.
fn short_sequence_input(count: usize) -> String {
    let mut value = String::with_capacity(count * 4);
    for _ in 0..count {
        value.push_str("- a\n");
    }
    value
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

fn generated_input(name: &str) -> Option<String> {
    match name {
        // Short-block shapes. The benchmark file generates the same text; keep
        // these in step with `crates/ayaml-bench/benches/throughput.rs`.
        "@short-seq-20000" => Some(short_sequence_input(SHORT_ENTRIES)),
        "@short-explicit-20000" => Some(short_explicit_input(SHORT_ENTRIES)),
        "@short-map-20000" => Some(short_mapping_input(SHORT_ENTRIES)),
        "@tiny" => Some("a\n".to_owned()),
        "@large-scalar" => Some(format!("{}\n", "x".repeat(1_000_000))),
        "@flow-depth-64" => {
            let mut value = "0".to_owned();
            for _ in 0..64 {
                value = format!("[{value}]");
            }
            Some(value)
        }
        "@depth-128" => {
            let mut value = "0".to_owned();
            for _ in 0..128 {
                value = format!("[{value}]");
            }
            Some(value)
        }
        "@aliases-1000" => {
            let mut value = String::from("base: &base {x: 1}\nitems:\n");
            for _ in 0..1000 {
                value.push_str("  - *base\n");
            }
            Some(value)
        }
        "@mapping-1000" => {
            let mut value = String::new();
            for index in 0..1_000 {
                let _ = writeln!(value, "key{index}: {index}");
            }
            Some(value)
        }
        "@mapping-10000" => {
            let mut value = String::new();
            for index in 0..10_000 {
                let _ = writeln!(value, "key{index}: {index}");
            }
            Some(value)
        }
        _ => None,
    }
}

/// Read one budget. A value that isn't a number fails, so a typo can't turn a
/// budget off.
fn budget(args: &mut impl Iterator<Item = String>) -> Option<usize> {
    args.next().map(|value| {
        value
            .parse()
            .expect("a budget must be a whole number of bytes or allocations")
    })
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .unwrap_or_else(|| "tests/fixtures/compat/toolchain-pins.yml".to_owned());
    let parse_limit = budget(&mut args);
    let emit_limit = budget(&mut args);
    let parse_allocation_limit = budget(&mut args);
    let emit_allocation_limit = budget(&mut args);
    let serialize_limit = budget(&mut args);
    let serialize_allocation_limit = budget(&mut args);
    let input = generated_input(&path)
        .unwrap_or_else(|| std::fs::read_to_string(&path).expect("read input"));

    let live_before_parse = LIVE.load(Ordering::Relaxed);
    let allocations_before_parse = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes_before_parse = ALLOCATED_BYTES.load(Ordering::Relaxed);
    reset_peak();
    let value = ayaml::parse_value(&input).expect("ayaml parse");
    let parse_peak = PEAK
        .load(Ordering::Relaxed)
        .saturating_sub(live_before_parse);
    let parse_live = LIVE
        .load(Ordering::Relaxed)
        .saturating_sub(live_before_parse);
    let parse_allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations_before_parse;
    let parse_allocated = ALLOCATED_BYTES.load(Ordering::Relaxed) - bytes_before_parse;
    black_box(&value);

    let live_before_emit = LIVE.load(Ordering::Relaxed);
    let allocations_before_emit = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes_before_emit = ALLOCATED_BYTES.load(Ordering::Relaxed);
    reset_peak();
    let rendered = ayaml::to_string_value(&value).expect("ayaml emit");
    let emit_peak = PEAK
        .load(Ordering::Relaxed)
        .saturating_sub(live_before_emit);
    let emit_allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations_before_emit;
    let emit_allocated = ALLOCATED_BYTES.load(Ordering::Relaxed) - bytes_before_emit;
    black_box(&rendered);

    let live_before_serialize = LIVE.load(Ordering::Relaxed);
    let allocations_before_serialize = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes_before_serialize = ALLOCATED_BYTES.load(Ordering::Relaxed);
    reset_peak();
    let serialized = ayaml::to_value(&value).expect("ayaml serialize");
    let serialize_peak = PEAK
        .load(Ordering::Relaxed)
        .saturating_sub(live_before_serialize);
    let serialize_allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations_before_serialize;
    let serialize_allocated = ALLOCATED_BYTES.load(Ordering::Relaxed) - bytes_before_serialize;
    black_box(&serialized);

    println!(
        "path={path} bytes={} parse_peak={parse_peak} parse_live={parse_live} parse_allocs={parse_allocations} parse_allocated={parse_allocated} emit_peak={emit_peak} emit_allocs={emit_allocations} emit_allocated={emit_allocated} serialize_peak={serialize_peak} serialize_allocs={serialize_allocations} serialize_allocated={serialize_allocated}",
        input.len(),
    );
    if let Some(limit) = parse_limit
        && parse_peak > limit
    {
        eprintln!("parse peak allocation {parse_peak} bytes exceeds limit {limit} bytes");
        std::process::exit(1);
    }
    if let Some(limit) = emit_limit
        && emit_peak > limit
    {
        eprintln!("emit peak allocation {emit_peak} bytes exceeds limit {limit} bytes");
        std::process::exit(1);
    }
    if let Some(limit) = parse_allocation_limit
        && parse_allocations > limit
    {
        eprintln!("parse allocation count {parse_allocations} exceeds limit {limit}");
        std::process::exit(1);
    }
    if let Some(limit) = emit_allocation_limit
        && emit_allocations > limit
    {
        eprintln!("emit allocation count {emit_allocations} exceeds limit {limit}");
        std::process::exit(1);
    }
    if let Some(limit) = serialize_limit
        && serialize_peak > limit
    {
        eprintln!("serialize peak allocation {serialize_peak} bytes exceeds limit {limit} bytes");
        std::process::exit(1);
    }
    if let Some(limit) = serialize_allocation_limit
        && serialize_allocations > limit
    {
        eprintln!("serialize allocation count {serialize_allocations} exceeds limit {limit}");
        std::process::exit(1);
    }
}
