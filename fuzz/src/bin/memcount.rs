//! Allocation budgets for the value path, the typed path, and an untyped decode into a JSON
//! tree.
//!
//! This counts live and peak allocated bytes and the allocation count while
//! parsing, emitting, and serializing one input, and fails when a count passes a
//! budget given on the command line. With `AYAML_TYPED` set, the parse is a typed decode into
//! `Value`. It lives here, outside the parser crate's
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
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::hint::black_box;
use std::io::Cursor;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

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
        // Shapes that a reader of events can't read entry by entry, and one that is all
        // directives: a root with an anchor is built whole, an anchored node is kept for the
        // aliases that may follow it, and a directive is an event with no node.
        "@anchored-flow-ones-1mb" => format!("&a [1{}]\n", ",1".repeat((MEGABYTE - 3) / 2)),
        "@anchored-block-words-1mb" => {
            let mut numbers = Numbers(0xB10C_5E90_0F57);
            String::from("&a\n")
                + &megabyte_of(|out, _| writeln!(out, "- {}", numbers.word()).expect("write"))
        }
        "@anchored-maps-50000" => {
            let mut out = String::new();
            for i in 0..50_000 {
                writeln!(out, "- &a{i} {{x: {i}, y: w}}").expect("write");
            }
            out
        }
        "@anchored-inner-flow-1mb" => format!("k: &a [1{}]\n", ",1".repeat((MEGABYTE - 7) / 2)),
        "@directives-100k" => "%A\n".repeat(100_000) + "--- x\n",
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
    let options = ayaml::compat::ParseOptions::for_max_input();
    let decode = |text: &str| -> serde_json::Value {
        ayaml::compat::from_str_seed_with(text, &options, PhantomData)
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

#[derive(Clone, Debug, Deserialize, Serialize)]
struct FrontmatterOwned {
    id: String,
    title: String,
    phase: u32,
    status: String,
    effort_days: f64,
    blocked_by: Vec<String>,
    paths: Vec<String>,
    tags: Vec<String>,
}

#[allow(
    dead_code,
    reason = "fields are populated by Serde during borrowed allocation measurements"
)]
#[derive(Clone, Debug, Deserialize)]
struct FrontmatterBorrowed<'a> {
    id: &'a str,
    title: &'a str,
    phase: u32,
    status: &'a str,
    effort_days: f64,
    #[serde(borrow)]
    blocked_by: Vec<&'a str>,
    #[serde(borrow)]
    paths: Vec<&'a str>,
    #[serde(borrow)]
    tags: Vec<&'a str>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct BoundaryOwned {
    service: String,
    entries: BTreeMap<String, String>,
}

#[allow(
    dead_code,
    reason = "fields are populated by Serde during borrowed allocation measurements"
)]
#[derive(Clone, Debug, Deserialize)]
struct BoundaryBorrowed<'a> {
    service: &'a str,
    #[serde(borrow)]
    entries: BTreeMap<&'a str, Cow<'a, str>>,
}

#[allow(
    dead_code,
    reason = "fields are populated by Serde during Spanned<T> allocation measurements"
)]
#[derive(Clone, Debug, Deserialize)]
struct FrontmatterSpanned {
    id: ayaml::Spanned<String>,
    title: ayaml::Spanned<String>,
    phase: ayaml::Spanned<u32>,
    status: ayaml::Spanned<String>,
    effort_days: ayaml::Spanned<f64>,
    blocked_by: Vec<ayaml::Spanned<String>>,
    paths: Vec<ayaml::Spanned<String>>,
    tags: Vec<ayaml::Spanned<String>>,
}

#[allow(
    dead_code,
    reason = "fields are populated by Serde during Spanned<T> allocation measurements"
)]
#[derive(Clone, Debug, Deserialize)]
struct BoundarySpanned {
    service: ayaml::Spanned<String>,
    entries: BTreeMap<String, ayaml::Spanned<String>>,
}

fn wrap_frontmatter_doc(yaml: &str) -> String {
    let mut doc = String::with_capacity(yaml.len() + 64);
    doc.push_str("---\n");
    doc.push_str(yaml);
    if !yaml.ends_with('\n') {
        doc.push('\n');
    }
    doc.push_str("---\n# Benchmark Body\n\nBody paragraph following YAML frontmatter.\n");
    doc
}

#[derive(Clone, Copy, Debug)]
struct AllocSample {
    peak_bytes: usize,
    live_bytes: usize,
    allocations: usize,
    total_allocated_bytes: usize,
}

/// Non-inlined measurement boundary targeted by `cargo xtask profile --tool callgrind|dhat`
/// when profiling a single `memcount` fixture/mode so fixture generation, SHA-256 verification,
/// and pre-measurement setup parses stay strictly outside the profiled region.
#[inline(never)]
fn __ayaml_memcount_measure<T>(op: impl FnOnce() -> T) -> T {
    black_box(op())
}

fn measure_op<T>(grows_in_place: bool, op: impl FnOnce() -> T) -> AllocSample {
    GROWS_IN_PLACE.store(grows_in_place, Ordering::Relaxed);
    let live_before = LIVE.load(Ordering::Relaxed);
    let allocs_before = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes_before = ALLOCATED_BYTES.load(Ordering::Relaxed);
    reset_peak();
    let result = __ayaml_memcount_measure(op);
    let sample = AllocSample {
        peak_bytes: PEAK.load(Ordering::Relaxed).saturating_sub(live_before),
        live_bytes: LIVE.load(Ordering::Relaxed).saturating_sub(live_before),
        allocations: ALLOCATIONS.load(Ordering::Relaxed) - allocs_before,
        total_allocated_bytes: ALLOCATED_BYTES.load(Ordering::Relaxed) - bytes_before,
    };
    black_box(&result);
    drop(result);
    GROWS_IN_PLACE.store(false, Ordering::Relaxed);
    sample
}

fn format_jsonl_row(
    scenario: &str,
    bytes: usize,
    fixture_sha256: &str,
    operation: &str,
    sample: AllocSample,
) -> String {
    let ratio = if bytes == 0 {
        0.0
    } else {
        #[allow(
            clippy::cast_precision_loss,
            reason = "peak-to-input ratio is a display/diagnostic float"
        )]
        let raw = (sample.peak_bytes as f64) / (bytes as f64);
        (raw * 1000.0).round() / 1000.0
    };
    format!(
        "{{\"schema_version\":1,\"scenario\":\"{scenario}\",\"bytes\":{bytes},\"fixture_sha256\":\"{fixture_sha256}\",\"operation\":\"{operation}\",\"peak_bytes\":{},\"live_bytes\":{},\"allocations\":{},\"total_allocated_bytes\":{},\"peak_to_input_ratio\":{ratio:.3}}}",
        sample.peak_bytes, sample.live_bytes, sample.allocations, sample.total_allocated_bytes,
    )
}

#[expect(
    dead_code,
    reason = "holds Tape/TapeDocuments live until after measure_op snapshots live_bytes"
)]
enum TapeLive<'a> {
    Single(ayaml::Tape<'a>),
    Docs(ayaml::TapeDocuments<'a>),
}

fn measure_tape_or_buf_mode(
    fixture: &ayaml_fixtures::Fixture,
    mode: &str,
) -> Result<Option<AllocSample>, String> {
    let input = &fixture.content;
    let sample = match mode {
        "tape" => measure_op(false, || {
            if fixture.multi_document {
                let docs = ayaml::parse_tape_documents(input).expect("parse_tape_documents");
                black_box(TapeLive::Docs(docs))
            } else {
                let tape = ayaml::parse_tape(input).expect("parse_tape");
                black_box(TapeLive::Single(tape))
            }
        }),
        "tape_buf" => {
            let mut buf = ayaml::ParserBuf::new();
            if fixture.multi_document {
                drop(black_box(
                    buf.parse_tape_documents(input).expect("warmup tape_docs"),
                ));
                measure_op(false, || {
                    let docs = buf.parse_tape_documents(input).expect("tape_buf docs");
                    black_box(TapeLive::Docs(docs))
                })
            } else {
                drop(black_box(buf.parse_tape(input).expect("warmup tape")));
                measure_op(false, || {
                    let tape = buf.parse_tape(input).expect("tape_buf");
                    black_box(TapeLive::Single(tape))
                })
            }
        }
        "tape_buf_recycle" => {
            let mut buf = ayaml::ParserBuf::new();
            if fixture.multi_document {
                let warm = buf.parse_tape_documents(input).expect("warmup tape_docs");
                warm.recycle_into(&mut buf);
                measure_op(false, || {
                    let docs = buf
                        .parse_tape_documents(input)
                        .expect("tape_buf_recycle docs");
                    let n = docs.len();
                    docs.recycle_into(&mut buf);
                    black_box(n)
                })
            } else {
                let warm = buf.parse_tape(input).expect("warmup tape");
                warm.recycle_into(&mut buf);
                measure_op(false, || {
                    let tape = buf.parse_tape(input).expect("tape_buf_recycle");
                    let k = tape.root().kind() as usize;
                    tape.recycle_into(&mut buf);
                    black_box(k)
                })
            }
        }
        "parse_value_buf" => {
            let mut buf = ayaml::ParserBuf::new();
            if fixture.multi_document {
                drop(black_box(buf.parse_documents(input).expect("warmup docs")));
                measure_op(false, || {
                    buf.parse_documents(input).expect("parse_value_buf docs")
                })
            } else {
                drop(black_box(buf.parse_value(input).expect("warmup val")));
                measure_op(false, || buf.parse_value(input).expect("parse_value_buf"))
            }
        }
        "from_str_buf" => {
            if fixture.multi_document {
                return Err(format!(
                    "from_str_buf requires single-document fixture (got {})",
                    fixture.id
                ));
            }
            let mut buf = ayaml::ParserBuf::new();
            match fixture.id.as_str() {
                "frontmatter-tiny" => {
                    drop(black_box(
                        buf.from_str::<FrontmatterBorrowed<'_>>(input)
                            .expect("warmup frontmatter"),
                    ));
                    measure_op(false, || {
                        buf.from_str::<FrontmatterBorrowed<'_>>(input)
                            .expect("from_str_buf frontmatter")
                    })
                }
                id if id.starts_with("boundary-") => {
                    drop(black_box(
                        buf.from_str::<BoundaryBorrowed<'_>>(input)
                            .expect("warmup boundary"),
                    ));
                    measure_op(false, || {
                        buf.from_str::<BoundaryBorrowed<'_>>(input)
                            .expect("from_str_buf boundary")
                    })
                }
                _ => {
                    drop(black_box(
                        buf.from_str::<ayaml::Value>(input).expect("warmup value"),
                    ));
                    measure_op(false, || {
                        buf.from_str::<ayaml::Value>(input)
                            .expect("from_str_buf value")
                    })
                }
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(sample))
}

fn measure_feature_mode(
    fixture: &ayaml_fixtures::Fixture,
    mode: &str,
) -> Result<Option<AllocSample>, String> {
    let input = &fixture.content;
    let sample = match mode {
        "ayaml_edit" => match fixture.id.as_str() {
            "frontmatter-tiny" => {
                let path = [ayaml::PathSegment::key("status")];
                let replacement = ayaml::Value::String("done".to_owned());
                measure_op(false, || {
                    ayaml_edit::replace_scalar(input, &path, &replacement)
                        .expect("ayaml_edit frontmatter")
                })
            }
            id if id.starts_with("boundary-") => {
                let path = [ayaml::PathSegment::key("service")];
                let replacement = ayaml::Value::String("updated-svc".to_owned());
                measure_op(false, || {
                    ayaml_edit::replace_scalar(input, &path, &replacement)
                        .expect("ayaml_edit boundary")
                })
            }
            other => {
                return Err(format!("ayaml_edit mode unsupported for fixture {other}"));
            }
        },
        "frontmatter" => {
            if fixture.multi_document {
                return Err(format!(
                    "frontmatter mode requires single-document fixture (got {})",
                    fixture.id
                ));
            }
            let wrapped = wrap_frontmatter_doc(input);
            match fixture.id.as_str() {
                "frontmatter-tiny" => measure_op(false, || {
                    ayaml::parse_frontmatter::<FrontmatterBorrowed<'_>>(&wrapped)
                        .expect("parse_frontmatter")
                        .expect("frontmatter present")
                }),
                _ => measure_op(false, || {
                    ayaml::parse_frontmatter_value(&wrapped)
                        .expect("parse_frontmatter_value")
                        .expect("frontmatter present")
                }),
            }
        }
        "merge_keys" => {
            let options = ayaml::ParseOptions::new().with_merge_keys(true);
            if fixture.multi_document {
                measure_op(false, || {
                    ayaml::parse_documents_with(input, &options).expect("merge_keys docs")
                })
            } else {
                measure_op(false, || {
                    ayaml::parse_value_with(input, &options).expect("merge_keys value")
                })
            }
        }
        "spanned_typed" => match fixture.id.as_str() {
            "frontmatter-tiny" => measure_op(false, || {
                ayaml::from_str::<FrontmatterSpanned>(input).expect("frontmatter spanned")
            }),
            id if id.starts_with("boundary-") => measure_op(false, || {
                ayaml::from_str::<BoundarySpanned>(input).expect("boundary spanned")
            }),
            _ => measure_op(false, || {
                ayaml::from_str::<BTreeMap<String, ayaml::Spanned<ayaml::Value>>>(input)
                    .expect("spanned map")
            }),
        },
        "doc_deserializer" => {
            if fixture.multi_document {
                return Err(format!(
                    "doc_deserializer requires single-document fixture (got {})",
                    fixture.id
                ));
            }
            match fixture.id.as_str() {
                "frontmatter-tiny" => measure_op(false, || {
                    let de = ayaml::DocumentDeserializer::from_str(input).expect("doc_de");
                    FrontmatterBorrowed::deserialize(de).expect("doc_de frontmatter")
                }),
                id if id.starts_with("boundary-") => measure_op(false, || {
                    let de = ayaml::DocumentDeserializer::from_str(input).expect("doc_de");
                    BoundaryBorrowed::deserialize(de).expect("doc_de boundary")
                }),
                _ => measure_op(false, || {
                    let de = ayaml::DocumentDeserializer::from_str(input).expect("doc_de");
                    ayaml::Value::deserialize(de).expect("doc_de value")
                }),
            }
        }
        "from_reader" => {
            if fixture.multi_document {
                return Err(format!(
                    "from_reader requires single-document fixture (got {})",
                    fixture.id
                ));
            }
            match fixture.id.as_str() {
                "frontmatter-tiny" => measure_op(false, || {
                    ayaml::from_reader::<_, FrontmatterOwned>(Cursor::new(input.as_bytes()))
                        .expect("from_reader frontmatter")
                }),
                id if id.starts_with("boundary-") => measure_op(false, || {
                    ayaml::from_reader::<_, BoundaryOwned>(Cursor::new(input.as_bytes()))
                        .expect("from_reader boundary")
                }),
                _ => measure_op(false, || {
                    ayaml::from_reader::<_, ayaml::Value>(Cursor::new(input.as_bytes()))
                        .expect("from_reader value")
                }),
            }
        }
        "libyaml_compat" => {
            if fixture.multi_document {
                return Err(format!(
                    "libyaml_compat requires single-document fixture (got {})",
                    fixture.id
                ));
            }
            measure_op(false, || {
                ayaml::compat::from_str::<ayaml::Value>(input).expect("libyaml_compat value")
            })
        }
        "parse_value_err" => measure_op(false, || {
            ayaml::parse_value(input).expect_err("expected parse error")
        }),
        _ => return Ok(None),
    };
    Ok(Some(sample))
}

fn measure_fixture_mode(fixture: &ayaml_fixtures::Fixture, mode: &str) -> Result<String, String> {
    if let Some(sample) = measure_tape_or_buf_mode(fixture, mode)? {
        return Ok(format_jsonl_row(
            &fixture.id,
            fixture.bytes,
            &fixture.sha256,
            mode,
            sample,
        ));
    }
    if let Some(sample) = measure_feature_mode(fixture, mode)? {
        return Ok(format_jsonl_row(
            &fixture.id,
            fixture.bytes,
            &fixture.sha256,
            mode,
            sample,
        ));
    }
    let input = &fixture.content;
    let sample = match mode {
        "scan" => {
            let limits = ayaml::Limits::default();
            measure_op(false, || {
                ayaml::internals::scan_tokens(input, &limits).expect("scan_tokens")
            })
        }
        "events" => {
            let limits = ayaml::Limits::default();
            measure_op(false, || {
                ayaml::internals::parse_events(input, &limits).expect("parse_events")
            })
        }
        "parse_value" => {
            if fixture.multi_document {
                measure_op(false, || ayaml::parse_documents(input).expect("parse_docs"))
            } else {
                measure_op(false, || ayaml::parse_value(input).expect("parse_value"))
            }
        }
        "typed_value" => {
            if fixture.multi_document {
                return Err(format!(
                    "typed_value unsupported for multi-doc {}",
                    fixture.id
                ));
            }
            measure_op(false, || {
                ayaml::from_str::<ayaml::Value>(input).expect("from_str::<Value>")
            })
        }
        "typed_struct" => match fixture.id.as_str() {
            "frontmatter-tiny" => measure_op(false, || {
                ayaml::from_str::<FrontmatterOwned>(input).expect("frontmatter owned")
            }),
            id if id.starts_with("boundary-") => measure_op(false, || {
                ayaml::from_str::<BoundaryOwned>(input).expect("boundary owned")
            }),
            _ => measure_op(false, || {
                ayaml::from_str::<BTreeMap<String, ayaml::Value>>(input).expect("typed map owned")
            }),
        },
        "typed_borrowed" => match fixture.id.as_str() {
            "frontmatter-tiny" => measure_op(false, || {
                ayaml::from_str_borrowed::<FrontmatterBorrowed<'_>>(input)
                    .expect("frontmatter borrowed")
            }),
            id if id.starts_with("boundary-") => measure_op(false, || {
                ayaml::from_str_borrowed::<BoundaryBorrowed<'_>>(input).expect("boundary borrowed")
            }),
            _ => measure_op(false, || {
                ayaml::from_str_borrowed::<BTreeMap<&str, ayaml::Value>>(input)
                    .expect("typed map borrowed")
            }),
        },
        "json_compat" => {
            let options = ayaml::compat::ParseOptions::for_max_input();
            drop(black_box(
                ayaml::compat::from_str_seed_with::<PhantomData<serde_json::Value>>(
                    "a: [1, {b: c}]\n",
                    &options,
                    PhantomData,
                )
                .expect("warmup json"),
            ));
            measure_op(true, || {
                ayaml::compat::from_str_seed_with::<PhantomData<serde_json::Value>>(
                    input,
                    &options,
                    PhantomData,
                )
                .expect("json_compat decode")
            })
        }
        "spanned_tree" => measure_op(false, || {
            ayaml::SpannedTree::parse(input).expect("SpannedTree::parse")
        }),
        "emit_value" => {
            if fixture.multi_document {
                let docs = ayaml::parse_documents(input).expect("parse_docs");
                measure_op(false, || {
                    ayaml::to_string_documents(&docs).expect("emit_docs")
                })
            } else {
                let val = ayaml::parse_value(input).expect("parse_value");
                measure_op(false, || ayaml::to_string_value(&val).expect("emit_value"))
            }
        }
        "serialize_serde" => match fixture.id.as_str() {
            "frontmatter-tiny" => {
                let typed: FrontmatterOwned = ayaml::from_str(input).expect("frontmatter");
                measure_op(false, || ayaml::to_string(&typed).expect("to_string"))
            }
            id if id.starts_with("boundary-") => {
                let typed: BoundaryOwned = ayaml::from_str(input).expect("boundary");
                measure_op(false, || ayaml::to_string(&typed).expect("to_string"))
            }
            _ => {
                let val = ayaml::parse_value(input).expect("parse_value");
                measure_op(false, || ayaml::to_string(&val).expect("to_string"))
            }
        },
        other => return Err(format!("unknown jsonl mode: {other}")),
    };
    Ok(format_jsonl_row(
        &fixture.id,
        fixture.bytes,
        &fixture.sha256,
        mode,
        sample,
    ))
}

/// Curated baseline matrix covering every operation mode across realistic domain, boundary,
/// scaling, and structural fixtures.
const BASELINE_CASES: &[(&str, &[&str])] = &[
    (
        "frontmatter-tiny",
        &[
            "parse_value",
            "typed_value",
            "typed_struct",
            "typed_borrowed",
            "json_compat",
            "spanned_tree",
            "tape",
            "tape_buf",
            "tape_buf_recycle",
            "emit_value",
            "serialize_serde",
            "ayaml_edit",
            "frontmatter",
            "spanned_typed",
            "doc_deserializer",
            "from_reader",
            "libyaml_compat",
        ],
    ),
    (
        "boundary-4kb-below",
        &[
            "parse_value",
            "typed_struct",
            "typed_borrowed",
            "serialize_serde",
            "ayaml_edit",
            "spanned_typed",
            "doc_deserializer",
            "from_reader",
            "libyaml_compat",
        ],
    ),
    (
        "boundary-4kb-above",
        &[
            "parse_value",
            "typed_struct",
            "typed_borrowed",
            "serialize_serde",
            "ayaml_edit",
            "spanned_typed",
            "doc_deserializer",
            "from_reader",
            "libyaml_compat",
        ],
    ),
    (
        "openapi-spec",
        &[
            "parse_value",
            "typed_value",
            "json_compat",
            "spanned_tree",
            "tape",
            "tape_buf",
            "tape_buf_recycle",
            "emit_value",
            "serialize_serde",
        ],
    ),
    (
        "cluster-manifests",
        &["parse_value", "spanned_tree", "emit_value"],
    ),
    (
        "ci-workflow",
        &["parse_value", "typed_value", "spanned_tree"],
    ),
    (
        "container-compose",
        &["parse_value", "json_compat", "merge_keys", "libyaml_compat"],
    ),
    (
        "lockfile-wide",
        &["parse_value", "typed_value", "emit_value"],
    ),
    ("escape-routes", &["parse_value", "spanned_tree"]),
    ("json-in-yaml", &["parse_value", "json_compat"]),
    ("unicode-multibyte", &["parse_value", "spanned_tree"]),
    (
        "scale-map-100kb",
        &["parse_value", "typed_borrowed", "json_compat"],
    ),
    (
        "depth-64",
        &[
            "parse_value",
            "typed_value",
            "tape",
            "spanned_tree",
            "libyaml_compat",
        ],
    ),
    ("aliases-1000", &["parse_value", "typed_value"]),
];

fn full_suite_modes(spec: &ayaml_fixtures::FixtureSpec) -> Vec<&'static str> {
    if !spec.expect_valid {
        return vec!["parse_value_err"];
    }
    if spec.multi_document {
        return vec![
            "parse_value",
            "spanned_tree",
            "tape",
            "tape_buf",
            "tape_buf_recycle",
            "parse_value_buf",
            "emit_value",
        ];
    }
    let mut modes = vec![
        "parse_value",
        "typed_value",
        "spanned_tree",
        "tape",
        "tape_buf",
        "tape_buf_recycle",
        "parse_value_buf",
        "emit_value",
        "serialize_serde",
        "from_reader",
    ];
    if spec.id != "nested-keys" {
        modes.push("libyaml_compat");
    }
    match spec.id {
        "frontmatter-tiny" => modes.extend([
            "typed_struct",
            "typed_borrowed",
            "from_str_buf",
            "json_compat",
            "ayaml_edit",
            "frontmatter",
            "spanned_typed",
            "doc_deserializer",
        ]),
        id if id.starts_with("boundary-") => modes.extend([
            "typed_struct",
            "typed_borrowed",
            "from_str_buf",
            "ayaml_edit",
            "spanned_typed",
            "doc_deserializer",
        ]),
        "container-compose" => modes.extend(["json_compat", "merge_keys"]),
        "openapi-spec" | "json-in-yaml" => modes.push("json_compat"),
        "scale-map-100kb" => modes.extend(["typed_borrowed", "json_compat"]),
        _ => {}
    }
    modes
}

const KNOWN_JSONL_MODES: &[&str] = &[
    "scan",
    "events",
    "parse_value",
    "typed_value",
    "typed_struct",
    "typed_borrowed",
    "json_compat",
    "spanned_tree",
    "tape",
    "tape_buf",
    "tape_buf_recycle",
    "parse_value_buf",
    "from_str_buf",
    "emit_value",
    "serialize_serde",
    "ayaml_edit",
    "frontmatter",
    "merge_keys",
    "spanned_typed",
    "doc_deserializer",
    "from_reader",
    "libyaml_compat",
    "parse_value_err",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JsonlSuite {
    Baseline,
    Full,
}

#[derive(Debug, PartialEq, Eq)]
struct JsonlCliConfig<'a> {
    fixture_id: Option<&'a str>,
    mode: Option<&'a str>,
    suite: Option<JsonlSuite>,
}

fn next_cli_value<'a>(
    flag: &str,
    inline: Option<&'a str>,
    args: &'a [String],
    idx: &mut usize,
) -> Result<&'a str, String> {
    if let Some(val) = inline {
        if val.is_empty() {
            return Err(format!("missing value for {flag}"));
        }
        return Ok(val);
    }
    *idx += 1;
    let Some(val) = args.get(*idx).map(String::as_str) else {
        return Err(format!("missing value for {flag}"));
    };
    if val.is_empty() || val.starts_with("--") {
        return Err(format!("missing value for {flag} (got {val:?})"));
    }
    Ok(val)
}

fn parse_jsonl_cli_args(args: &[String]) -> Result<JsonlCliConfig<'_>, String> {
    let mut fixture_id: Option<&str> = None;
    let mut mode: Option<&str> = None;
    let mut suite: Option<JsonlSuite> = None;
    let mut i = 0;
    while i < args.len() {
        let (flag, inline) = match args[i].split_once('=') {
            Some((k, v)) if k.starts_with('-') => (k, Some(v)),
            _ => (args[i].as_str(), None),
        };
        match flag {
            "--jsonl" => {
                if inline.is_some() {
                    return Err("--jsonl does not accept a value".to_owned());
                }
            }
            "--suite" => {
                if suite.is_some() {
                    return Err("duplicate --suite option".to_owned());
                }
                let raw = next_cli_value(flag, inline, args, &mut i)?;
                suite = match raw {
                    "baseline" => Some(JsonlSuite::Baseline),
                    "full" => Some(JsonlSuite::Full),
                    other => {
                        return Err(format!(
                            "unknown --suite value {other:?} (expected `baseline` or `full`)"
                        ));
                    }
                };
            }
            "--fixture" => {
                if fixture_id.is_some() {
                    return Err("duplicate --fixture option".to_owned());
                }
                fixture_id = Some(next_cli_value(flag, inline, args, &mut i)?);
            }
            "--mode" => {
                if mode.is_some() {
                    return Err("duplicate --mode option".to_owned());
                }
                let raw = next_cli_value(flag, inline, args, &mut i)?;
                if !KNOWN_JSONL_MODES.contains(&raw) {
                    return Err(format!("unknown --mode value {raw:?}"));
                }
                mode = Some(raw);
            }
            other => {
                return Err(format!("unknown --jsonl option: {other}"));
            }
        }
        i += 1;
    }
    if suite.is_some() && fixture_id.is_some() {
        return Err("--suite and --fixture cannot be combined".to_owned());
    }
    Ok(JsonlCliConfig {
        fixture_id,
        mode,
        suite,
    })
}

fn run_jsonl_cases(
    cases: &[(ayaml_fixtures::Fixture, Vec<&str>)],
    mode_filter: Option<&str>,
) -> usize {
    let mut emitted = 0_usize;
    for (fixture, modes) in cases {
        for &m in modes {
            if let Some(filter) = mode_filter
                && filter != m
            {
                continue;
            }
            match measure_fixture_mode(fixture, m) {
                Ok(line) => {
                    println!("{line}");
                    emitted += 1;
                }
                Err(err) => {
                    eprintln!("{err}");
                    std::process::exit(1);
                }
            }
        }
    }
    emitted
}

fn run_jsonl_cli(args: &[String]) {
    let cfg = parse_jsonl_cli_args(args).unwrap_or_else(|err| {
        eprintln!("{err}");
        std::process::exit(2);
    });

    if let Some(id) = cfg.fixture_id {
        let fixture = ayaml_fixtures::generate(id).unwrap_or_else(|| {
            eprintln!("unknown fixture id: {id}");
            std::process::exit(2);
        });
        let modes: Vec<&str> = if let Some(m) = cfg.mode {
            vec![m]
        } else if !fixture.expect_valid {
            vec!["parse_value_err"]
        } else if fixture.multi_document {
            vec!["parse_value", "spanned_tree", "emit_value"]
        } else {
            vec![
                "parse_value",
                "typed_value",
                "spanned_tree",
                "emit_value",
                "serialize_serde",
            ]
        };
        run_jsonl_cases(&[(fixture, modes)], None);
        return;
    }

    let emitted = match cfg.suite.unwrap_or(JsonlSuite::Baseline) {
        JsonlSuite::Baseline => {
            let cases: Vec<_> = BASELINE_CASES
                .iter()
                .map(|&(id, modes)| {
                    let fixture = ayaml_fixtures::generate(id)
                        .unwrap_or_else(|| panic!("unknown baseline fixture {id}"));
                    (fixture, modes.to_vec())
                })
                .collect();
            run_jsonl_cases(&cases, cfg.mode)
        }
        JsonlSuite::Full => {
            let mut total = 0_usize;
            for spec in ayaml_fixtures::catalog_specs() {
                let fixture = ayaml_fixtures::generate(spec.id)
                    .unwrap_or_else(|| panic!("unknown catalog fixture {}", spec.id));
                let modes = full_suite_modes(spec);
                total += run_jsonl_cases(&[(fixture, modes)], cfg.mode);
            }
            total
        }
    };
    if emitted == 0 {
        eprintln!("no suite cases matched --mode {:?}", cfg.mode.unwrap_or(""));
        std::process::exit(2);
    }
}

fn main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    if raw_args
        .iter()
        .any(|a| a == "--jsonl" || a.starts_with("--jsonl="))
    {
        run_jsonl_cli(&raw_args);
        return;
    }
    let mut args = raw_args.into_iter();
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
        ayaml::compat::from_str_with::<ayaml::Value>(
            &input,
            &ayaml::compat::ParseOptions::for_max_input(),
        )
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

#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout};
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    use super::{ALLOCATED_BYTES, ALLOCATIONS, ALLOCATOR, GROWS_IN_PLACE, LIVE, PEAK, reset_peak};

    static SERIALIZE_ALLOC_TESTS: Mutex<()> = Mutex::new(());

    #[test]
    fn counting_allocator_tracks_alloc_realloc_and_dealloc_in_both_modes() {
        let _guard = SERIALIZE_ALLOC_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        for grows_in_place in [false, true] {
            GROWS_IN_PLACE.store(grows_in_place, Ordering::Relaxed);
            let live_start = LIVE.load(Ordering::Relaxed);
            let allocs_start = ALLOCATIONS.load(Ordering::Relaxed);
            let bytes_start = ALLOCATED_BYTES.load(Ordering::Relaxed);
            reset_peak();

            // Direct alloc -> grow -> shrink -> dealloc across multiple alignments.
            for align in [1_usize, 8, 64] {
                let before = LIVE.load(Ordering::Relaxed);
                let initial = Layout::from_size_align(64, align).expect("valid layout");
                let ptr = unsafe { ALLOCATOR.alloc(initial) };
                assert!(!ptr.is_null(), "alloc failed for align={align}");
                assert_eq!(LIVE.load(Ordering::Relaxed), before + initial.size());
                unsafe {
                    std::ptr::write_bytes(ptr, 0xA5, initial.size());
                }

                let grown_size = 256;
                let grown = unsafe { ALLOCATOR.realloc(ptr, initial, grown_size) };
                assert!(!grown.is_null(), "grow realloc failed for align={align}");
                assert_eq!(LIVE.load(Ordering::Relaxed), before + grown_size);
                unsafe {
                    let prefix = std::slice::from_raw_parts(grown, initial.size());
                    assert!(
                        prefix.iter().all(|&b| b == 0xA5),
                        "realloc lost original prefix bytes"
                    );
                    std::ptr::write_bytes(
                        grown.add(initial.size()),
                        0x5A,
                        grown_size - initial.size(),
                    );
                }

                let grown_layout = Layout::from_size_align(grown_size, align).expect("grown");
                let shrunk_size = 32;
                let shrunk = unsafe { ALLOCATOR.realloc(grown, grown_layout, shrunk_size) };
                assert!(!shrunk.is_null(), "shrink realloc failed for align={align}");
                assert_eq!(LIVE.load(Ordering::Relaxed), before + shrunk_size);
                unsafe {
                    let kept = std::slice::from_raw_parts(shrunk, shrunk_size);
                    assert!(
                        kept.iter().all(|&b| b == 0xA5),
                        "shrink realloc corrupted surviving bytes"
                    );
                    let shrunk_layout =
                        Layout::from_size_align(shrunk_size, align).expect("shrunk");
                    ALLOCATOR.dealloc(shrunk, shrunk_layout);
                }
                assert_eq!(LIVE.load(Ordering::Relaxed), before);

                // Exercise `alloc_zeroed` as well.
                let zero_layout = Layout::from_size_align(48, align).expect("zero layout");
                let zptr = unsafe { ALLOCATOR.alloc_zeroed(zero_layout) };
                assert!(!zptr.is_null(), "alloc_zeroed failed for align={align}");
                assert_eq!(LIVE.load(Ordering::Relaxed), before + zero_layout.size());
                unsafe {
                    let slice = std::slice::from_raw_parts(zptr, zero_layout.size());
                    assert!(slice.iter().all(|&b| b == 0), "alloc_zeroed not zeroed");
                    ALLOCATOR.dealloc(zptr, zero_layout);
                }
                assert_eq!(LIVE.load(Ordering::Relaxed), before);
            }

            // Exercise the global allocator through standard Rust containers (Box + Vec growth).
            let mut items: Vec<u64> = Vec::new();
            for i in 0_u64..128 {
                items.push(i);
            }
            let sum: u64 = items.iter().sum();
            assert_eq!(sum, 127 * 128 / 2);
            drop(items);

            let live_end = LIVE.load(Ordering::Relaxed);
            let peak = PEAK.load(Ordering::Relaxed);
            let allocs_delta = ALLOCATIONS.load(Ordering::Relaxed) - allocs_start;
            let bytes_delta = ALLOCATED_BYTES.load(Ordering::Relaxed) - bytes_start;
            assert!(
                peak >= live_start + 256,
                "peak {peak} did not capture allocated bytes above {live_start}"
            );
            assert!(
                allocs_delta >= 9,
                "expected >= 9 allocations, got {allocs_delta}"
            );
            assert!(
                bytes_delta >= 3 * 256,
                "expected >= 768 allocated bytes, got {bytes_delta}"
            );
            assert_eq!(
                live_end, live_start,
                "live byte counter leaked or underflowed (mode grows_in_place={grows_in_place})"
            );
        }
        GROWS_IN_PLACE.store(false, Ordering::Relaxed);

        // Run structured JSONL checks in the same single test thread so libtest does not spawn
        // parallel allocator threads that perturb global `LIVE` counters above.
        verify_jsonl_mode_schema();
    }

    fn verify_jsonl_mode_schema() {
        let fixture = ayaml_fixtures::generate("frontmatter-tiny").expect("frontmatter-tiny");
        for mode in [
            "parse_value",
            "typed_value",
            "typed_struct",
            "typed_borrowed",
            "json_compat",
            "spanned_tree",
            "tape",
            "tape_buf",
            "tape_buf_recycle",
            "parse_value_buf",
            "from_str_buf",
            "emit_value",
            "serialize_serde",
            "ayaml_edit",
            "frontmatter",
            "merge_keys",
            "spanned_typed",
            "doc_deserializer",
            "from_reader",
            "libyaml_compat",
        ] {
            let line = super::measure_fixture_mode(&fixture, mode).expect("measure mode");
            let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid JSONL");
            assert_eq!(parsed["schema_version"], 1);
            assert_eq!(parsed["scenario"], "frontmatter-tiny");
            assert_eq!(parsed["fixture_sha256"], fixture.sha256);
            assert_eq!(parsed["operation"], mode);
            assert!(parsed["peak_bytes"].as_u64().is_some());
            assert!(parsed["live_bytes"].as_u64().is_some());
            assert!(parsed["allocations"].as_u64().is_some());
            assert!(parsed["total_allocated_bytes"].as_u64().is_some());
            assert!(parsed["peak_to_input_ratio"].as_f64().is_some());
        }

        // Error-path fixtures run `parse_value_err`.
        let err_fx = ayaml_fixtures::generate("err-depth-limit").expect("err-depth-limit");
        let err_line = super::measure_fixture_mode(&err_fx, "parse_value_err").expect("err mode");
        let parsed_err: serde_json::Value = serde_json::from_str(&err_line).expect("valid JSONL");
        assert_eq!(parsed_err["operation"], "parse_value_err");

        // `--suite full` covers all 40 catalog fixtures and is a superset of BASELINE_CASES.
        let specs = ayaml_fixtures::catalog_specs();
        assert_eq!(specs.len(), 40);
        for spec in specs {
            let modes = super::full_suite_modes(spec);
            assert!(!modes.is_empty(), "empty full-suite modes for {}", spec.id);
        }
        for &(id, baseline_modes) in super::BASELINE_CASES {
            let spec = specs
                .iter()
                .find(|s| s.id == id)
                .unwrap_or_else(|| panic!("baseline fixture {id} missing from catalog"));
            let full_modes = super::full_suite_modes(spec);
            for &m in baseline_modes {
                assert!(
                    full_modes.contains(&m),
                    "full suite missing baseline mode {m} for {id}"
                );
            }
        }

        // Strict `--jsonl` CLI parser validation.
        let sv = |xs: &[&str]| xs.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let full_args = sv(&["--jsonl", "--suite=full", "--mode=tape"]);
        let parsed = super::parse_jsonl_cli_args(&full_args).expect("valid full suite args");
        assert_eq!(parsed.suite, Some(super::JsonlSuite::Full));
        assert_eq!(parsed.mode, Some("tape"));
        assert!(super::parse_jsonl_cli_args(&sv(&["--jsonl=bogus"])).is_err());
        assert!(
            super::parse_jsonl_cli_args(&sv(&[
                "--jsonl",
                "--suite",
                "full",
                "--fixture",
                "frontmatter-tiny"
            ]))
            .is_err()
        );
        assert!(super::parse_jsonl_cli_args(&sv(&["--jsonl", "--mode", "bogus"])).is_err());
        assert!(super::parse_jsonl_cli_args(&sv(&["--jsonl", "--mode", "--suite"])).is_err());
        assert!(super::parse_jsonl_cli_args(&sv(&["--jsonl", "--suite", ""])).is_err());
        assert!(
            super::parse_jsonl_cli_args(&sv(&[
                "--jsonl", "--suite", "baseline", "--suite", "full"
            ]))
            .is_err()
        );
    }
}
