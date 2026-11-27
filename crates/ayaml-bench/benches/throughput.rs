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
    FrontmatterOwned, SCALING_IDS, STRUCTURAL_IDS, must_fixture, probe_line_index_queries,
    stream_cluster_borrowed, stream_cluster_owned, validate_all_scenarios,
};
use ayaml_fixtures::bulk_automation_files;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

static VALIDATED: Once = Once::new();

fn ensure_validated() {
    if std::env::var_os("AYAML_BENCH_SKIP_VALIDATE").is_some() {
        return;
    }
    VALIDATED.call_once(|| {
        validate_all_scenarios().unwrap_or_else(|err| {
            panic!("pre-timing semantic validation failed:\n{err}");
        });
    });
}

fn group_selected(group: &str) -> bool {
    if std::env::var_os("AYAML_DET_LIST").is_some() {
        return true;
    }
    let Ok(filter) = std::env::var("AYAML_DET_FILTER") else {
        return true;
    };
    filter.starts_with(group) && filter.as_bytes().get(group.len()) == Some(&b'/')
}

fn item_selected(full_id: &str) -> bool {
    if std::env::var_os("AYAML_DET_LIST").is_some() {
        return true;
    }
    let Ok(filter) = std::env::var("AYAML_DET_FILTER") else {
        return true;
    };
    filter == full_id
}

fn bench_domains(c: &mut Criterion) {
    ensure_validated();
    if !group_selected("domains") {
        return;
    }
    let mut group = c.benchmark_group("domains");
    for &id in DOMAIN_IDS {
        if !item_selected(&format!("domains/parse_documents/{id}")) {
            continue;
        }
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

    if item_selected("domains/stream_owned/cluster-manifests")
        || item_selected("domains/stream_borrowed/cluster-manifests")
    {
        let cluster = must_fixture("cluster-manifests");
        group.throughput(Throughput::Bytes(cluster.bytes as u64));
        if item_selected("domains/stream_owned/cluster-manifests") {
            group.bench_function("stream_owned/cluster-manifests", |b| {
                b.iter(|| {
                    black_box(stream_cluster_owned(black_box(&cluster.content)).expect("stream"))
                });
            });
        }
        if item_selected("domains/stream_borrowed/cluster-manifests") {
            group.bench_function("stream_borrowed/cluster-manifests", |b| {
                b.iter(|| {
                    black_box(stream_cluster_borrowed(black_box(&cluster.content)).expect("stream"))
                });
            });
        }
    }

    if item_selected("domains/bulk_32/automation-playbooks")
        || item_selected("domains/bulk_32_value_buf/automation-playbooks")
        || item_selected("domains/bulk_32_tape_buf/automation-playbooks")
    {
        let bulk = bulk_automation_files(32);
        let total_bulk_bytes: usize = bulk.iter().map(|f| f.bytes).sum();
        group.throughput(Throughput::Bytes(total_bulk_bytes as u64));
        if item_selected("domains/bulk_32/automation-playbooks") {
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
        }
        if item_selected("domains/bulk_32_value_buf/automation-playbooks") {
            let mut buf = ayaml::ParserBuf::new();
            group.bench_function("bulk_32_value_buf/automation-playbooks", |b| {
                b.iter(|| {
                    let mut count = 0_usize;
                    for file in &bulk {
                        let v = buf
                            .parse_value(black_box(&file.content))
                            .expect("bulk value buf");
                        count += usize::from(v.is_sequence());
                    }
                    black_box(count)
                });
            });
        }
        if item_selected("domains/bulk_32_tape_buf/automation-playbooks") {
            let mut buf = ayaml::ParserBuf::new();
            group.bench_function("bulk_32_tape_buf/automation-playbooks", |b| {
                b.iter(|| {
                    let mut count = 0_usize;
                    for file in &bulk {
                        let tape = buf
                            .parse_tape(black_box(&file.content))
                            .expect("bulk tape buf");
                        count += usize::from(tape.root().is_sequence());
                        tape.recycle_into(&mut buf);
                    }
                    black_box(count)
                });
            });
        }
    }
    group.finish();
}

fn bench_layers(c: &mut Criterion) {
    ensure_validated();
    if !group_selected("layers") {
        return;
    }
    let limits = Limits::default();
    let mut group = c.benchmark_group("layers");

    let need_openapi = [
        "layers/scan/openapi-spec",
        "layers/events/openapi-spec",
        "layers/compose/openapi-spec",
        "layers/parse_value/openapi-spec",
        "layers/parse_tape/openapi-spec",
        "layers/parse_tape_buf/openapi-spec",
        "layers/from_str_value/openapi-spec",
        "layers/emit_events/openapi-spec",
        "layers/emit_value/openapi-spec",
    ]
    .into_iter()
    .any(item_selected);

    if need_openapi {
        let openapi = must_fixture("openapi-spec");
        group.throughput(Throughput::Bytes(openapi.bytes as u64));

        if item_selected("layers/scan/openapi-spec") {
            group.bench_function("scan/openapi-spec", |b| {
                b.iter(|| {
                    black_box(
                        ayaml::internals::scan_tokens(black_box(&openapi.content), &limits)
                            .expect("scan"),
                    )
                });
            });
        }
        if item_selected("layers/events/openapi-spec") {
            group.bench_function("events/openapi-spec", |b| {
                b.iter(|| {
                    black_box(
                        ayaml::internals::parse_events(black_box(&openapi.content), &limits)
                            .expect("events"),
                    )
                });
            });
        }
        if item_selected("layers/compose/openapi-spec")
            || item_selected("layers/emit_events/openapi-spec")
        {
            let events = ayaml::internals::parse_events(&openapi.content, &limits).expect("events");
            if item_selected("layers/compose/openapi-spec") {
                group.bench_function("compose/openapi-spec", |b| {
                    b.iter(|| {
                        black_box(
                            ayaml::internals::compose_events(&events, openapi.bytes, &limits)
                                .expect("compose"),
                        )
                    });
                });
            }
            if item_selected("layers/emit_events/openapi-spec") {
                group.bench_function("emit_events/openapi-spec", |b| {
                    b.iter(|| black_box(ayaml::to_string_events(&events).expect("emit events")));
                });
            }
        }
        if item_selected("layers/parse_value/openapi-spec") {
            group.bench_function("parse_value/openapi-spec", |b| {
                b.iter(|| {
                    black_box(ayaml::parse_value(black_box(&openapi.content)).expect("value"))
                });
            });
        }
        if item_selected("layers/parse_tape/openapi-spec") {
            group.bench_function("parse_tape/openapi-spec", |b| {
                b.iter(|| black_box(ayaml::parse_tape(black_box(&openapi.content)).expect("tape")));
            });
        }
        if item_selected("layers/parse_tape_buf/openapi-spec") {
            let mut buf = ayaml::ParserBuf::new();
            group.bench_function("parse_tape_buf/openapi-spec", |b| {
                b.iter(|| {
                    let tape = buf
                        .parse_tape(black_box(&openapi.content))
                        .expect("tape buf");
                    let k = black_box(tape.root().kind());
                    tape.recycle_into(&mut buf);
                    k
                });
            });
        }
        if item_selected("layers/from_str_value/openapi-spec") {
            group.bench_function("from_str_value/openapi-spec", |b| {
                b.iter(|| {
                    black_box(
                        ayaml::from_str::<Value>(black_box(&openapi.content)).expect("from_str"),
                    )
                });
            });
        }
        if item_selected("layers/emit_value/openapi-spec") {
            let value = ayaml::parse_value(&openapi.content).expect("value");
            group.bench_function("emit_value/openapi-spec", |b| {
                b.iter(|| black_box(ayaml::to_string_value(&value).expect("emit value")));
            });
        }
    }

    let need_front = [
        "layers/from_str_typed_owned/frontmatter-tiny",
        "layers/from_str_typed_borrowed/frontmatter-tiny",
        "layers/from_str_buf_borrowed/frontmatter-tiny",
        "layers/to_string_serde/frontmatter-tiny",
    ]
    .into_iter()
    .any(item_selected);

    if need_front {
        let front = must_fixture("frontmatter-tiny");
        group.throughput(Throughput::Bytes(front.bytes as u64));
        if item_selected("layers/from_str_typed_owned/frontmatter-tiny") {
            group.bench_function("from_str_typed_owned/frontmatter-tiny", |b| {
                b.iter(|| {
                    black_box(
                        ayaml::from_str::<FrontmatterOwned>(black_box(&front.content))
                            .expect("owned"),
                    )
                });
            });
        }
        if item_selected("layers/from_str_typed_borrowed/frontmatter-tiny") {
            group.bench_function("from_str_typed_borrowed/frontmatter-tiny", |b| {
                b.iter(|| {
                    black_box(
                        ayaml::from_str_borrowed::<FrontmatterBorrowed<'_>>(black_box(
                            &front.content,
                        ))
                        .expect("borrowed"),
                    )
                });
            });
        }
        if item_selected("layers/from_str_buf_borrowed/frontmatter-tiny") {
            let mut buf = ayaml::ParserBuf::new();
            group.bench_function("from_str_buf_borrowed/frontmatter-tiny", |b| {
                b.iter(|| {
                    black_box(
                        buf.from_str::<FrontmatterBorrowed<'_>>(black_box(&front.content))
                            .expect("buf borrowed"),
                    )
                });
            });
        }
        if item_selected("layers/to_string_serde/frontmatter-tiny") {
            let fm_owned: FrontmatterOwned = ayaml::from_str(&front.content).expect("frontmatter");
            group.bench_function("to_string_serde/frontmatter-tiny", |b| {
                b.iter(|| {
                    black_box(ayaml::to_string(black_box(&fm_owned)).expect("serde to_string"))
                });
            });
        }
    }
    group.finish();
}

fn bench_boundary(c: &mut Criterion) {
    ensure_validated();
    if !group_selected("boundary_4kb") {
        return;
    }
    let mut group = c.benchmark_group("boundary_4kb");
    for &id in BOUNDARY_IDS {
        let want_owned = item_selected(&format!("boundary_4kb/from_str_owned/{id}"));
        let want_borrowed = item_selected(&format!("boundary_4kb/from_str_borrowed/{id}"));
        if !want_owned && !want_borrowed {
            continue;
        }
        let fixture = must_fixture(id);
        group.throughput(Throughput::Bytes(fixture.bytes as u64));
        if want_owned {
            group.bench_with_input(
                BenchmarkId::new("from_str_owned", id),
                &fixture.content,
                |b, text| {
                    b.iter(|| {
                        black_box(ayaml::from_str::<BoundaryOwned>(black_box(text)).expect("owned"))
                    });
                },
            );
        }
        if want_borrowed {
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
    }
    group.finish();
}

fn bench_scaling(c: &mut Criterion) {
    ensure_validated();
    if !group_selected("scaling") {
        return;
    }
    let mut group = c.benchmark_group("scaling");
    for &id in SCALING_IDS {
        if !item_selected(&format!("scaling/parse_value/{id}")) {
            continue;
        }
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
    if !group_selected("emit") {
        return;
    }
    let mut group = c.benchmark_group("emit");
    let want_block = item_selected("emit/block_value/openapi-spec");
    let want_flow = item_selected("emit/flow_value/openapi-spec");
    let want_events = item_selected("emit/events/openapi-spec");
    if want_block || want_flow || want_events {
        let openapi = must_fixture("openapi-spec");
        group.throughput(Throughput::Bytes(openapi.bytes as u64));
        if want_block || want_flow {
            let openapi_val = ayaml::parse_value(&openapi.content).expect("openapi");
            let flow = EmitOptions::flow();
            if want_block {
                group.bench_function("block_value/openapi-spec", |b| {
                    b.iter(|| {
                        black_box(ayaml::to_string_value(black_box(&openapi_val)).expect("block"))
                    });
                });
            }
            if want_flow {
                group.bench_function("flow_value/openapi-spec", |b| {
                    b.iter(|| {
                        black_box(
                            ayaml::to_string_value_with(black_box(&openapi_val), &flow)
                                .expect("flow"),
                        )
                    });
                });
            }
        }
        if want_events {
            let openapi_events =
                ayaml::internals::parse_events(&openapi.content, &Limits::default())
                    .expect("events");
            group.bench_function("events/openapi-spec", |b| {
                b.iter(|| {
                    black_box(ayaml::to_string_events(black_box(&openapi_events)).expect("events"))
                });
            });
        }
    }

    if item_selected("emit/documents/cluster-manifests") {
        let cluster = must_fixture("cluster-manifests");
        let cluster_docs = ayaml::parse_documents(&cluster.content).expect("cluster");
        group.throughput(Throughput::Bytes(cluster.bytes as u64));
        group.bench_function("documents/cluster-manifests", |b| {
            b.iter(|| {
                black_box(ayaml::to_string_documents(black_box(&cluster_docs)).expect("docs"))
            });
        });
    }

    if item_selected("emit/serde_to_string/boundary-8kb") {
        let boundary = must_fixture("boundary-8kb");
        let typed: BoundaryOwned = ayaml::from_str(&boundary.content).expect("boundary");
        group.throughput(Throughput::Bytes(boundary.bytes as u64));
        group.bench_function("serde_to_string/boundary-8kb", |b| {
            b.iter(|| black_box(ayaml::to_string(black_box(&typed)).expect("serde")));
        });
    }
    group.finish();
}

fn bench_spans(c: &mut Criterion) {
    ensure_validated();
    if !group_selected("spans") {
        return;
    }
    let mut group = c.benchmark_group("spans");
    for id in ["openapi-spec", "unicode-multibyte", "ci-workflow"] {
        let want_spanned = item_selected(&format!("spans/spanned_tree/{id}"));
        let want_idx = item_selected(&format!("spans/line_index_new/{id}"));
        let want_queries = item_selected(&format!("spans/line_index_queries/{id}"));
        if !want_spanned && !want_idx && !want_queries {
            continue;
        }
        let fixture = must_fixture(id);
        group.throughput(Throughput::Bytes(fixture.bytes as u64));
        if want_spanned {
            group.bench_with_input(
                BenchmarkId::new("spanned_tree", id),
                &fixture.content,
                |b, text| {
                    b.iter(|| black_box(SpannedTree::parse(black_box(text)).expect("spanned")));
                },
            );
        }
        if want_idx {
            group.bench_with_input(
                BenchmarkId::new("line_index_new", id),
                &fixture.content,
                |b, text| {
                    b.iter(|| black_box(LineIndex::new(black_box(text))));
                },
            );
        }
        if want_queries {
            group.bench_with_input(
                BenchmarkId::new("line_index_queries", id),
                &fixture.content,
                |b, text| {
                    let index = LineIndex::new(text);
                    b.iter(|| {
                        black_box(probe_line_index_queries(black_box(text), black_box(&index)))
                    });
                },
            );
        }
    }
    group.finish();
}

fn bench_structural(c: &mut Criterion) {
    ensure_validated();
    if !group_selected("structural") {
        return;
    }
    let mut group = c.benchmark_group("structural");
    for &id in STRUCTURAL_IDS {
        if !item_selected(&format!("structural/parse_value/{id}")) {
            continue;
        }
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
    if !group_selected("errors") {
        return;
    }
    let mut group = c.benchmark_group("errors");
    for &id in ERROR_IDS {
        if !item_selected(&format!("errors/parse_value_err/{id}")) {
            continue;
        }
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
    if item_selected("errors/from_str_err/typed-mismatch-late") {
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
    }
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
