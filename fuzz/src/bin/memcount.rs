//! Allocation budgets for the value path.
//!
//! This counts live and peak allocated bytes and the allocation count while
//! parsing, emitting, and serializing one input, and fails when a count passes a
//! budget given on the command line. It lives here, outside the parser crate's
//! `forbid(unsafe_code)` graph, because a counting allocator needs `unsafe`.
//!
//! Usage: `memcount PATH|@NAME [PARSE_PEAK EMIT_PEAK PARSE_ALLOCS EMIT_ALLOCS
//! SERIALIZE_PEAK SERIALIZE_ALLOCS PARSE_TOTAL EMIT_TOTAL SERIALIZE_TOTAL]`.
//!
//! With `AYAML_JSON` set it measures one thing instead: an untyped decode into a JSON tree
//! through the compatibility reader, as a consumer that reads YAML into JSON does. Usage:
//! `AYAML_JSON=1 memcount PATH|@NAME [PEAK ALLOCS TOTAL]`.
#![allow(
    unsafe_code,
    reason = "the counting global allocator is the crate-local unsafe exception"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::hint::black_box;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);
/// Count a reallocation by the bytes it adds, as an allocator that grows a block in place
/// does, and not as a new block beside the old one. The JSON measurement sets this, so that
/// its peaks are the ones a consumer's counting allocator reports.
static GROWS_IN_PLACE: AtomicBool = AtomicBool::new(false);

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

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if !GROWS_IN_PLACE.load(Ordering::Relaxed) {
            // What the trait does by default: a new block, a copy, and the old block freed, so
            // both are live at once.
            let Ok(grown) = Layout::from_size_align(size, layout.align()) else {
                return std::ptr::null_mut();
            };
            let moved = unsafe { self.alloc(grown) };
            if !moved.is_null() {
                unsafe {
                    std::ptr::copy_nonoverlapping(pointer, moved, layout.size().min(size));
                    self.dealloc(pointer, layout);
                }
            }
            return moved;
        }
        let moved = unsafe { System.realloc(pointer, layout, size) };
        if !moved.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            if size >= layout.size() {
                ALLOCATED_BYTES.fetch_add(size - layout.size(), Ordering::Relaxed);
                account(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        moved
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

/// The size that the generated shapes of issue #178 grow to, except the three service
/// descriptions.
const MEGABYTE: usize = 1_000_000;

/// A fixed sequence of numbers (xorshift64*), so a generated shape is the same in every run.
struct Numbers(u64);

impl Numbers {
    fn below(&mut self, below: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let value = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33;
        usize::try_from(value).expect("31 bits") % below
    }

    fn word(&mut self) -> &'static str {
        const WORDS: [&str; 10] = [
            "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
        ];
        WORDS[self.below(WORDS.len())]
    }
}

/// A service description with `paths` path items, each with two operations of three
/// parameters: about 920 bytes a path item, nested block mappings with short flow collections.
fn service_description(paths: usize) -> String {
    let mut numbers = Numbers(0x0A11_E47A_11E4_7A11);
    let mut out = String::from("openapi: 3.0.0\ninfo:\n  title: big\n  version: 1.0.0\npaths:\n");
    for i in 0..paths {
        writeln!(out, "  /api/v1/{}/{i}:", numbers.word()).expect("write to String");
        for verb in ["get", "post"] {
            let summary: Vec<&str> = (0..6).map(|_| numbers.word()).collect();
            writeln!(
                out,
                "    {verb}:\n      operationId: op{verb}{i}\n      summary: \"{}\"\n      \
                 tags: [{}, {}]\n      parameters:",
                summary.join(" "),
                numbers.word(),
                numbers.word()
            )
            .expect("write to String");
            for p in 0..3 {
                writeln!(
                    out,
                    "        - name: p{p}\n          in: query\n          \
                     schema: {{type: string, maxLength: {}}}",
                    1 + numbers.below(99)
                )
                .expect("write to String");
            }
            out += "      responses:\n        '200':\n          description: ok\n";
        }
    }
    out
}

/// Lines from `line`, one for each number from 0, until the text is a megabyte.
fn megabyte_of(mut line: impl FnMut(&mut String, usize)) -> String {
    let mut out = String::new();
    for i in 0.. {
        if out.len() >= MEGABYTE {
            break;
        }
        line(&mut out, i);
    }
    out
}

/// The nine shapes that issue #178 measures: three service descriptions, and six documents of
/// about a megabyte made of many short scalars.
fn short_scalar_shape(name: &str) -> Option<String> {
    Some(match name {
        "@service-37kb" => service_description(40),
        "@service-1mb" => service_description(1_200),
        "@service-4mb" => service_description(4_800),
        // A block mapping of `kN: 1` lines.
        "@block-map-1mb" => megabyte_of(|out, i| writeln!(out, "k{i}: 1").expect("write")),
        // A flow mapping of `kN: 1` entries on one line.
        "@flow-map-1mb" => {
            let mut out = String::from("{");
            for i in 0.. {
                if out.len() >= MEGABYTE {
                    break;
                }
                write!(out, "k{i}: 1, ").expect("write");
            }
            out + "}\n"
        }
        // A flow sequence of integers below 100,000, twenty to a line.
        "@flow-ints-1mb" => {
            let mut numbers = Numbers(0x5EED_0F1A_7E6E_2500);
            let mut out = String::from("[");
            for i in 0.. {
                if out.len() >= MEGABYTE {
                    break;
                }
                let separator = if i % 20 == 0 { "\n  " } else { " " };
                write!(out, "{separator}{},", numbers.below(100_000)).expect("write");
            }
            out + "\n]\n"
        }
        // A flow sequence of `1` on one line.
        "@flow-ones-1mb" => format!("[1{}]\n", ",1".repeat((MEGABYTE - 3) / 2)),
        // A block sequence of `value N` lines.
        "@block-seq-1mb" => megabyte_of(|out, i| writeln!(out, "- value {i}").expect("write")),
        // A block sequence of one short word a line.
        "@block-words-1mb" => {
            let mut numbers = Numbers(0xB10C_5E90_0F57);
            megabyte_of(|out, _| writeln!(out, "- {}", numbers.word()).expect("write"))
        }
        _ => return None,
    })
}

fn generated_input(name: &str) -> Option<String> {
    if let Some(shape) = short_scalar_shape(name) {
        return Some(shape);
    }
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

/// Decode `input` into a JSON tree through the compatibility reader, with the count limits
/// lifted as [`ayaml::Limits::for_max_input`] lifts them, and check what the decode allocates
/// against the budgets. A reallocation counts by the bytes it adds.
fn measure_json(
    path: &str,
    input: &str,
    peak_limit: Option<usize>,
    allocation_limit: Option<usize>,
    total_limit: Option<usize>,
) {
    let limits = ayaml::Limits::for_max_input();
    let decode = |text: &str| -> serde_json::Value {
        ayaml::compat::from_str_seed_with_limits(text, &limits, PhantomData)
            .expect("ayaml decodes the document")
    };
    // Whatever a decode allocates once in a process, before the counts.
    drop(black_box(decode("a: [1, {b: c}]\n")));
    GROWS_IN_PLACE.store(true, Ordering::Relaxed);
    let live_before = LIVE.load(Ordering::Relaxed);
    let allocations_before = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes_before = ALLOCATED_BYTES.load(Ordering::Relaxed);
    reset_peak();
    let tree = decode(input);
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(live_before);
    let kept = LIVE.load(Ordering::Relaxed).saturating_sub(live_before);
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations_before;
    let allocated = ALLOCATED_BYTES.load(Ordering::Relaxed) - bytes_before;
    GROWS_IN_PLACE.store(false, Ordering::Relaxed);
    black_box(&tree);
    println!(
        "path={path} bytes={} json_peak={peak} json_kept={kept} json_allocs={allocations} json_allocated={allocated}",
        input.len(),
    );
    for (what, measured, limit) in [
        ("peak allocation in bytes", peak, peak_limit),
        ("allocation count", allocations, allocation_limit),
        ("total allocation in bytes", allocated, total_limit),
    ] {
        if let Some(limit) = limit
            && measured > limit
        {
            eprintln!("json {what}, {measured}, exceeds limit {limit}");
            std::process::exit(1);
        }
    }
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
    let parse_total_limit = budget(&mut args);
    let emit_total_limit = budget(&mut args);
    let serialize_total_limit = budget(&mut args);
    let input = generated_input(&path)
        .unwrap_or_else(|| std::fs::read_to_string(&path).expect("read input"));
    if std::env::var_os("AYAML_JSON").is_some() {
        // In this mode the three budgets are the peak, the allocations, and the total bytes.
        measure_json(
            &path,
            &input,
            parse_limit,
            emit_limit,
            parse_allocation_limit,
        );
        return;
    }

    let typed = std::env::var_os("AYAML_TYPED").is_some();
    let live_before_parse = LIVE.load(Ordering::Relaxed);
    let allocations_before_parse = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes_before_parse = ALLOCATED_BYTES.load(Ordering::Relaxed);
    reset_peak();
    let value = if typed {
        ayaml::compat::from_str_with_limits::<ayaml::Value>(&input, &ayaml::Limits::for_max_input())
            .expect("ayaml typed parse")
    } else {
        ayaml::parse_value(&input).expect("ayaml parse")
    };
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
    if let Some(limit) = parse_total_limit
        && parse_allocated > limit
    {
        eprintln!("parse total allocation {parse_allocated} bytes exceeds limit {limit} bytes");
        std::process::exit(1);
    }
    if let Some(limit) = emit_total_limit
        && emit_allocated > limit
    {
        eprintln!("emit total allocation {emit_allocated} bytes exceeds limit {limit} bytes");
        std::process::exit(1);
    }
    if let Some(limit) = serialize_total_limit
        && serialize_allocated > limit
    {
        eprintln!(
            "serialize total allocation {serialize_allocated} bytes exceeds limit {limit} bytes"
        );
        std::process::exit(1);
    }
}
