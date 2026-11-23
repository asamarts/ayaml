//! Modular Criterion benchmark suite for `ayaml` over `ayaml-fixtures`.
//!
//! Groups cover:
//! - `domains`: all 11 realistic YAML domain families + bulk small-file batch + multi-doc streaming
//! - `layers`: scanner, event stream, composer, `parse_value`, `from_str`, `from_str_borrowed`,
//!   `to_string_events`, `to_string_value`, and Serde `to_string`
//! - `boundary`: 4 KiB typed tree-versus-stream strategy transition (owned and borrowed)
//! - `scaling`: 10 KiB / 100 KiB / 1 MiB ladders across mappings, sequences, and `OpenAPI`
//! - `emit`: block, flow, event, multi-document, and Serde serialization styles
//! - `spans`: `SpannedTree::parse` and `LineIndex` position/offset conversions
//! - `structural`: depth, alias reuse, large scalar, flow collections, short block entries, nested keys
//! - `errors`: early/late syntax, duplicate key, depth limit, invalid escape, and late typed decode
//!
//! A mandatory pre-timing semantic validation step verifies every benchmarked path before timing.
#![allow(
    missing_docs,
    reason = "criterion generates the benchmark entry points"
)]

use std::collections::BTreeMap;
use std::hint::black_box;
use std::sync::Once;

use ayaml::{EmitOptions, Limits, LineIndex, SpannedTree, Value};
use ayaml_bench::{
    BOUNDARY_IDS, BoundaryBorrowed, BoundaryOwned, DOMAIN_IDS, ERROR_IDS, FrontmatterBorrowed,
    FrontmatterOwned, SCALING_IDS, STRUCTURAL_IDS, must_fixture, probe_line_index,
    stream_cluster_borrowed, stream_cluster_owned, validate_all_scenarios,
};
use ayaml_fixtures::bulk_automation_files;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

static VALIDATED: Once = Once::new();

fn ensure_validated() {
    VALIDATED.call_once(|| {
        validate_all_scenarios().unwrap_or_else(|err| {
            panic!("pre-timing semantic validation failed:\n{err}");
        });
    });
}

fn bench_domains(c: &mut Criterion) {
    ensure_validated();
    let mut group = c.benchmark_group("domains");
    for &id in DOMAIN_IDS {
        let fixture = must_fixture(id);
        group.throughput(Throughput::Bytes(fixture.bytes as u64));
        group.bench_with_input(
            BenchmarkId::new("parse_documents", id),
            &fixture.content,
            |b, text| {
                b.iter(|| black_box(ayaml::parse_documents(black_box(text)).expect("parse")));
            },
        );
    }

    let cluster = must_fixture("cluster-manifests");
    group.throughput(Throughput::Bytes(cluster.bytes as u64));
    group.bench_function("stream_owned/cluster-manifests", |b| {
        b.iter(|| black_box(stream_cluster_owned(black_box(&cluster.content)).expect("stream")));
    });
    group.bench_function("stream_borrowed/cluster-manifests", |b| {
        b.iter(|| black_box(stream_cluster_borrowed(black_box(&cluster.content)).expect("stream")));
    });

    let bulk = bulk_automation_files(32);
    let total_bulk_bytes: usize = bulk.iter().map(|f| f.bytes).sum();
    group.throughput(Throughput::Bytes(total_bulk_bytes as u64));
    group.bench_function("bulk_32/automation-playbooks", |b| {
        b.iter(|| {
            let mut count = 0_usize;
            for file in &bulk {
                let v = ayaml::parse_value(black_box(&file.content)).expect("bulk parse");
                count += usize::from(v.is_sequence());
            }
            black_box(count)
        });
    });
    group.finish();
}

fn bench_layers(c: &mut Criterion) {
    ensure_validated();
    let limits = Limits::default();
    let mut group = c.benchmark_group("layers");

    let openapi = must_fixture("openapi-spec");
    let events = ayaml::internals::parse_events(&openapi.content, &limits).expect("events");
    let value = ayaml::parse_value(&openapi.content).expect("value");
    group.throughput(Throughput::Bytes(openapi.bytes as u64));

    group.bench_function("scan/openapi-spec", |b| {
        b.iter(|| {
            black_box(
                ayaml::internals::scan_tokens(black_box(&openapi.content), &limits).expect("scan"),
            )
        });
    });
    group.bench_function("events/openapi-spec", |b| {
        b.iter(|| {
            black_box(
                ayaml::internals::parse_events(black_box(&openapi.content), &limits)
                    .expect("events"),
            )
        });
    });
    group.bench_function("compose/openapi-spec", |b| {
        b.iter(|| {
            black_box(
                ayaml::internals::compose_events(&events, openapi.bytes, &limits).expect("compose"),
            )
        });
    });
    group.bench_function("parse_value/openapi-spec", |b| {
        b.iter(|| black_box(ayaml::parse_value(black_box(&openapi.content)).expect("value")));
    });
    group.bench_function("from_str_value/openapi-spec", |b| {
        b.iter(|| {
            black_box(ayaml::from_str::<Value>(black_box(&openapi.content)).expect("from_str"))
        });
    });
    group.bench_function("emit_events/openapi-spec", |b| {
        b.iter(|| black_box(ayaml::to_string_events(&events).expect("emit events")));
    });
    group.bench_function("emit_value/openapi-spec", |b| {
        b.iter(|| black_box(ayaml::to_string_value(&value).expect("emit value")));
    });

    let front = must_fixture("frontmatter-tiny");
    let fm_owned: FrontmatterOwned = ayaml::from_str(&front.content).expect("frontmatter");
    group.throughput(Throughput::Bytes(front.bytes as u64));
    group.bench_function("from_str_typed_owned/frontmatter-tiny", |b| {
        b.iter(|| {
            black_box(
                ayaml::from_str::<FrontmatterOwned>(black_box(&front.content)).expect("owned"),
            )
        });
    });
    group.bench_function("from_str_typed_borrowed/frontmatter-tiny", |b| {
        b.iter(|| {
            black_box(
                ayaml::from_str_borrowed::<FrontmatterBorrowed<'_>>(black_box(&front.content))
                    .expect("borrowed"),
            )
        });
    });
    group.bench_function("to_string_serde/frontmatter-tiny", |b| {
        b.iter(|| black_box(ayaml::to_string(black_box(&fm_owned)).expect("serde to_string")));
    });
    group.finish();
}

fn bench_boundary(c: &mut Criterion) {
    ensure_validated();
    let mut group = c.benchmark_group("boundary_4kb");
    for &id in BOUNDARY_IDS {
        let fixture = must_fixture(id);
        group.throughput(Throughput::Bytes(fixture.bytes as u64));
        group.bench_with_input(
            BenchmarkId::new("from_str_owned", id),
            &fixture.content,
            |b, text| {
                b.iter(|| {
                    black_box(ayaml::from_str::<BoundaryOwned>(black_box(text)).expect("owned"))
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("from_str_borrowed", id),
            &fixture.content,
            |b, text| {
                b.iter(|| {
                    black_box(
                        ayaml::from_str_borrowed::<BoundaryBorrowed<'_>>(black_box(text))
                            .expect("borrowed"),
                    )
                });
            },
        );
    }
    group.finish();
}

fn bench_scaling(c: &mut Criterion) {
    ensure_validated();
    let mut group = c.benchmark_group("scaling");
    for &id in SCALING_IDS {
        let fixture = must_fixture(id);
        group.throughput(Throughput::Bytes(fixture.bytes as u64));
        group.bench_with_input(
            BenchmarkId::new("parse_value", id),
            &fixture.content,
            |b, text| {
                b.iter(|| black_box(ayaml::parse_value(black_box(text)).expect("parse")));
            },
        );
    }
    group.finish();
}

fn bench_emit(c: &mut Criterion) {
    ensure_validated();
    let mut group = c.benchmark_group("emit");
    let openapi = must_fixture("openapi-spec");
    let openapi_val = ayaml::parse_value(&openapi.content).expect("openapi");
    let openapi_events =
        ayaml::internals::parse_events(&openapi.content, &Limits::default()).expect("events");
    let flow = EmitOptions::flow();
    group.throughput(Throughput::Bytes(openapi.bytes as u64));
    group.bench_function("block_value/openapi-spec", |b| {
        b.iter(|| black_box(ayaml::to_string_value(black_box(&openapi_val)).expect("block")));
    });
    group.bench_function("flow_value/openapi-spec", |b| {
        b.iter(|| {
            black_box(ayaml::to_string_value_with(black_box(&openapi_val), &flow).expect("flow"))
        });
    });
    group.bench_function("events/openapi-spec", |b| {
        b.iter(|| black_box(ayaml::to_string_events(black_box(&openapi_events)).expect("events")));
    });

    let cluster = must_fixture("cluster-manifests");
    let cluster_docs = ayaml::parse_documents(&cluster.content).expect("cluster");
    group.throughput(Throughput::Bytes(cluster.bytes as u64));
    group.bench_function("documents/cluster-manifests", |b| {
        b.iter(|| black_box(ayaml::to_string_documents(black_box(&cluster_docs)).expect("docs")));
    });

    let boundary = must_fixture("boundary-8kb");
    let typed: BoundaryOwned = ayaml::from_str(&boundary.content).expect("boundary");
    group.throughput(Throughput::Bytes(boundary.bytes as u64));
    group.bench_function("serde_to_string/boundary-8kb", |b| {
        b.iter(|| black_box(ayaml::to_string(black_box(&typed)).expect("serde")));
    });
    group.finish();
}

fn bench_spans(c: &mut Criterion) {
    ensure_validated();
    let mut group = c.benchmark_group("spans");
    for id in ["openapi-spec", "unicode-multibyte", "ci-workflow"] {
        let fixture = must_fixture(id);
        group.throughput(Throughput::Bytes(fixture.bytes as u64));
        group.bench_with_input(
            BenchmarkId::new("spanned_tree", id),
            &fixture.content,
            |b, text| {
                b.iter(|| black_box(SpannedTree::parse(black_box(text)).expect("spanned")));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("line_index_new", id),
            &fixture.content,
            |b, text| {
                b.iter(|| black_box(LineIndex::new(black_box(text))));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("line_index_queries", id),
            &fixture.content,
            |b, text| {
                b.iter(|| black_box(probe_line_index(black_box(text))));
            },
        );
    }
    group.finish();
}

fn bench_structural(c: &mut Criterion) {
    ensure_validated();
    let mut group = c.benchmark_group("structural");
    for &id in STRUCTURAL_IDS {
        let fixture = must_fixture(id);
        group.throughput(Throughput::Bytes(fixture.bytes as u64));
        group.bench_with_input(
            BenchmarkId::new("parse_value", id),
            &fixture.content,
            |b, text| {
                b.iter(|| black_box(ayaml::parse_value(black_box(text)).expect("parse")));
            },
        );
    }
    group.finish();
}

fn bench_errors(c: &mut Criterion) {
    ensure_validated();
    let mut group = c.benchmark_group("errors");
    for &id in ERROR_IDS {
        let fixture = must_fixture(id);
        group.throughput(Throughput::Bytes(fixture.bytes as u64));
        group.bench_with_input(
            BenchmarkId::new("parse_value_err", id),
            &fixture.content,
            |b, text| {
                b.iter(|| {
                    black_box(ayaml::parse_value(black_box(text)).expect_err("expected error"))
                });
            },
        );
    }
    let late_typed = must_fixture("typed-mismatch-late");
    group.throughput(Throughput::Bytes(late_typed.bytes as u64));
    group.bench_function("from_str_err/typed-mismatch-late", |b| {
        b.iter(|| {
            black_box(
                ayaml::from_str::<BTreeMap<String, u64>>(black_box(&late_typed.content))
                    .expect_err("expected typed error"),
            )
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_domains,
    bench_layers,
    bench_boundary,
    bench_scaling,
    bench_emit,
    bench_spans,
    bench_structural,
    bench_errors
);
criterion_main!(benches);
