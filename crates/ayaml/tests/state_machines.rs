//! Deterministic state-machine and I/O fault-injection tests for emitters,
//! `Deserializer` cursors, and streaming typed reads (AY-133).

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::fmt;
use std::io::{self, Write};

use ayaml::{
    Deserializer, EmitOptions, ErrorCode, ErrorKind, Limits, Mapping, ParseOptions, Tag,
    TaggedValue, Value,
};
use serde::de::{self, Deserialize, DeserializeSeed, MapAccess, SeqAccess, Visitor};

// ---------------------------------------------------------------------------
// Deterministic PRNG (fixed-seed XorShift64, zero external dependencies)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Rng(u64);

impl Rng {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn gen_range(&mut self, upper: usize) -> usize {
        assert!(upper > 0);
        usize::try_from(self.next_u64() % u64::try_from(upper).expect("usize fits u64"))
            .expect("remainder fits usize")
    }
}

// ---------------------------------------------------------------------------
// Part 1: Emitter I/O fault-injection harness
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum FaultPlan {
    /// Complete every write, capping each `write()` call to `chunk_sizes[step % len]` bytes and
    /// injecting `ErrorKind::Interrupted` whenever `interrupt_every` matches.
    ShortAndInterrupted {
        chunk_sizes: &'static [usize],
        interrupt_every: Option<usize>,
    },
    /// Write at most `fail_at` bytes (in `short_step` chunks, optionally with `Interrupted`),
    /// then fail with the specified `io::ErrorKind` and message.
    FailAtByte {
        fail_at: usize,
        kind: io::ErrorKind,
        message: String,
        short_step: usize,
        interrupt_once: bool,
    },
    /// Write up to `fail_at` bytes, then return `Ok(0)` so `write_all` reports `WriteZero`.
    WriteZeroAtByte { fail_at: usize, short_step: usize },
}

#[derive(Debug)]
struct FaultWriter {
    plan: FaultPlan,
    written: Vec<u8>,
    write_calls: usize,
    interrupted_injected: usize,
    chunk_cursor: usize,
}

impl FaultWriter {
    fn new(plan: FaultPlan) -> Self {
        Self {
            plan,
            written: Vec::new(),
            write_calls: 0,
            interrupted_injected: 0,
            chunk_cursor: 0,
        }
    }
}

impl Write for FaultWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_calls += 1;
        if buf.is_empty() {
            return Ok(0);
        }
        match &mut self.plan {
            FaultPlan::ShortAndInterrupted {
                chunk_sizes,
                interrupt_every,
            } => {
                if let Some(period) = *interrupt_every
                    && self.write_calls.checked_rem(period) == Some(0)
                {
                    self.interrupted_injected += 1;
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "simulated EINTR",
                    ));
                }
                let cap = chunk_sizes[self.chunk_cursor % chunk_sizes.len()].max(1);
                self.chunk_cursor += 1;
                let n = buf.len().min(cap);
                self.written.extend_from_slice(&buf[..n]);
                Ok(n)
            }
            FaultPlan::FailAtByte {
                fail_at,
                kind,
                message,
                short_step,
                interrupt_once,
            } => {
                if *interrupt_once {
                    *interrupt_once = false;
                    self.interrupted_injected += 1;
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "simulated EINTR before fault",
                    ));
                }
                let remaining = fail_at.saturating_sub(self.written.len());
                if remaining == 0 {
                    return Err(io::Error::new(*kind, message.clone()));
                }
                let n = buf.len().min(remaining).min((*short_step).max(1));
                self.written.extend_from_slice(&buf[..n]);
                Ok(n)
            }
            FaultPlan::WriteZeroAtByte {
                fail_at,
                short_step,
            } => {
                let remaining = fail_at.saturating_sub(self.written.len());
                if remaining == 0 {
                    return Ok(0);
                }
                let n = buf.len().min(remaining).min((*short_step).max(1));
                self.written.extend_from_slice(&buf[..n]);
                Ok(n)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SamplePayload {
    name: String,
    ports: Vec<u16>,
    enabled: bool,
    labels: BTreeMap<String, String>,
    notes: String,
}

fn sample_payload() -> SamplePayload {
    let mut labels = BTreeMap::new();
    labels.insert("env".to_owned(), "prod".to_owned());
    labels.insert("quoted".to_owned(), "0123".to_owned());
    labels.insert("unicode".to_owned(), "café 🚀".to_owned());
    SamplePayload {
        name: "service-alpha".to_owned(),
        ports: vec![80, 443, 8443],
        enabled: true,
        labels,
        notes: "line one\nline two\nline three\n".to_owned(),
    }
}

fn sample_value() -> Value {
    let mut inner = Mapping::new();
    inner.insert(Value::from("host"), Value::from("example.test"));
    inner.insert(Value::from("retries"), Value::from(3_u64));
    inner.insert(
        Value::from("items"),
        Value::Sequence(vec![
            Value::from("first"),
            Value::from("second: colon"),
            Value::Null,
        ]),
    );
    Value::from(TaggedValue::new(Tag::local("cfg"), Value::Mapping(inner)))
}

const SAMPLE_EVENT_YAML: &str = "\
%TAG !e! tag:example.com,2026:
---
base: &shared
  a: 1
  b: \"hello\\nworld\"
copy: *shared
tag: !e!item [true, false, null]
...
---
second:
  - alpha
  - beta
...
";

fn assert_io_error_clean(
    err: &ayaml::Error,
    writer: &FaultWriter,
    reference: &[u8],
    fail_at: usize,
    canary: Option<&str>,
    label: &str,
) {
    assert_eq!(err.kind(), ErrorKind::Io, "{label}: kind {err:?}");
    assert_eq!(err.code(), ErrorCode::Io, "{label}: code {err:?}");
    assert!(err.source().is_some(), "{label}: missing io source");
    if let Some(secret) = canary {
        let masked = err.masked();
        assert!(
            !masked.to_string().contains(secret),
            "{label}: masked Display leaked {secret}: {masked}"
        );
        assert!(
            !format!("{masked:?}").contains(secret),
            "{label}: masked Debug leaked {secret}: {masked:?}"
        );
        assert!(masked.source().is_none(), "{label}: masked exposed source");
    }
    assert!(
        writer.written.len() <= fail_at,
        "{label}: wrote {} bytes past fail_at {fail_at}",
        writer.written.len()
    );
    assert_eq!(
        writer.written.as_slice(),
        &reference[..writer.written.len()],
        "{label}: partial prefix diverged from reference"
    );
}

#[test]
fn emitter_short_writes_and_interrupted_match_reference() {
    let payload = sample_payload();
    let value = sample_value();
    let docs = vec![
        value.clone(),
        ayaml::parse_value("doc: 2\nitems: [1, 2, 3]\n").expect("doc2"),
    ];
    let events = ayaml::parse_events(SAMPLE_EVENT_YAML)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .expect("events");

    let ref_serde = ayaml::to_string(&payload).expect("to_string").into_bytes();
    let ref_value = ayaml::to_string_value(&value)
        .expect("to_string_value")
        .into_bytes();
    let ref_docs = ayaml::to_string_documents(&docs)
        .expect("to_string_documents")
        .into_bytes();
    let ref_events = ayaml::to_string_events(&events)
        .expect("to_string_events")
        .into_bytes();

    let patterns: &[(&str, &'static [usize], Option<usize>)] = &[
        ("one_byte_no_eintr", &[1], None),
        ("one_byte_every_2_eintr", &[1], Some(2)),
        ("prime_chunks_every_3_eintr", &[1, 2, 3, 5, 7], Some(3)),
        ("small_chunks_every_5_eintr", &[4, 1, 9, 2], Some(5)),
    ];

    for &(name, chunks, interrupt_every) in patterns {
        let make = || {
            FaultWriter::new(FaultPlan::ShortAndInterrupted {
                chunk_sizes: chunks,
                interrupt_every,
            })
        };

        let mut w = make();
        ayaml::to_writer(&mut w, &payload).unwrap_or_else(|e| panic!("to_writer {name}: {e}"));
        assert_eq!(w.written, ref_serde, "to_writer {name}");
        if interrupt_every.is_some() {
            assert!(w.interrupted_injected > 0, "to_writer {name} hit EINTR");
        }

        let mut w = make();
        ayaml::to_writer_value(&mut w, &value)
            .unwrap_or_else(|e| panic!("to_writer_value {name}: {e}"));
        assert_eq!(w.written, ref_value, "to_writer_value {name}");

        let mut w = make();
        ayaml::to_writer_documents(&mut w, &docs)
            .unwrap_or_else(|e| panic!("to_writer_documents {name}: {e}"));
        assert_eq!(w.written, ref_docs, "to_writer_documents {name}");

        let mut w = make();
        ayaml::to_writer_events(&mut w, &events)
            .unwrap_or_else(|e| panic!("to_writer_events {name}: {e}"));
        assert_eq!(w.written, ref_events, "to_writer_events {name}");

        let mut w = make();
        let stream = ayaml::parse_events(SAMPLE_EVENT_YAML).expect("stream");
        ayaml::to_writer_event_stream(&mut w, stream)
            .unwrap_or_else(|e| panic!("to_writer_event_stream {name}: {e}"));
        assert_eq!(w.written, ref_events, "to_writer_event_stream {name}");
    }

    // Multi-chunk output (> 16 KiB) to exercise incremental buffer flushing inside `Output`.
    let big_items: Vec<Value> = (0..600)
        .map(|i| Value::from(format!("entry-{i:04}: payload-value-{i:04}")))
        .collect();
    let big_value = Value::Sequence(big_items);
    let ref_big = ayaml::to_string_value(&big_value)
        .expect("big ref")
        .into_bytes();
    assert!(ref_big.len() > 16_384, "expected multi-chunk output");

    let mut w = FaultWriter::new(FaultPlan::ShortAndInterrupted {
        chunk_sizes: &[13, 1, 29, 7],
        interrupt_every: Some(4),
    });
    ayaml::to_writer_value(&mut w, &big_value).expect("big value short+EINTR");
    assert_eq!(w.written, ref_big);
    assert!(w.interrupted_injected > 10);

    // Also exercise all five `*_with` writer APIs under non-default `EmitOptions`
    // (indent = 4, indented sequences, explicit markers, and flow mode).
    let custom_opts = [
        EmitOptions::new()
            .with_indent(4)
            .with_indent_sequences(true)
            .with_explicit_start(true)
            .with_explicit_end(true),
        EmitOptions::flow()
            .with_explicit_start(true)
            .with_explicit_end(true),
    ];
    for (idx, opts) in custom_opts.iter().enumerate() {
        let make = || {
            FaultWriter::new(FaultPlan::ShortAndInterrupted {
                chunk_sizes: &[1, 3, 2],
                interrupt_every: Some(3),
            })
        };

        let ref_s = ayaml::to_string_with(&payload, opts).expect("to_string_with");
        let mut w = make();
        ayaml::to_writer_with(&mut w, &payload, opts).expect("to_writer_with");
        assert_eq!(w.written, ref_s.as_bytes(), "to_writer_with opt {idx}");

        let ref_v = ayaml::to_string_value_with(&value, opts).expect("to_string_value_with");
        let mut w = make();
        ayaml::to_writer_value_with(&mut w, &value, opts).expect("to_writer_value_with");
        assert_eq!(
            w.written,
            ref_v.as_bytes(),
            "to_writer_value_with opt {idx}"
        );

        let ref_d = ayaml::to_string_documents_with(&docs, opts).expect("to_string_documents_with");
        let mut w = make();
        ayaml::to_writer_documents_with(&mut w, &docs, opts).expect("to_writer_documents_with");
        assert_eq!(
            w.written,
            ref_d.as_bytes(),
            "to_writer_documents_with opt {idx}"
        );

        let ref_e = ayaml::to_string_events_with(&events, opts).expect("to_string_events_with");
        let mut w = make();
        ayaml::to_writer_events_with(&mut w, &events, opts).expect("to_writer_events_with");
        assert_eq!(
            w.written,
            ref_e.as_bytes(),
            "to_writer_events_with opt {idx}"
        );

        let mut w = make();
        let stream = ayaml::parse_events(SAMPLE_EVENT_YAML).expect("stream");
        ayaml::to_writer_event_stream_with(&mut w, stream, opts)
            .expect("to_writer_event_stream_with");
        assert_eq!(
            w.written,
            ref_e.as_bytes(),
            "to_writer_event_stream_with opt {idx}"
        );
    }
}

#[test]
fn emitter_io_error_at_every_byte_offset() {
    let payload = sample_payload();
    let value = sample_value();
    let docs = vec![
        value.clone(),
        ayaml::parse_value("second: [1, 2]\n").expect("second"),
    ];
    let events = ayaml::parse_events(SAMPLE_EVENT_YAML)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .expect("events");

    let ref_serde = ayaml::to_string(&payload).expect("ref_serde").into_bytes();
    let ref_value = ayaml::to_string_value(&value)
        .expect("ref_value")
        .into_bytes();
    let ref_docs = ayaml::to_string_documents(&docs)
        .expect("ref_docs")
        .into_bytes();
    let ref_events = ayaml::to_string_events(&events)
        .expect("ref_events")
        .into_bytes();

    let canary = "CANARY_SECRET_IO_PIPE";

    // Test every single byte offset 0..ref.len() for all five writer entry points, alternating
    // BrokenPipe, PermissionDenied, and WriteZero (Ok(0)).
    for offset in 0..ref_serde.len() {
        let mut w = if offset % 3 == 0 {
            FaultWriter::new(FaultPlan::WriteZeroAtByte {
                fail_at: offset,
                short_step: 1,
            })
        } else {
            FaultWriter::new(FaultPlan::FailAtByte {
                fail_at: offset,
                kind: if offset % 2 == 0 {
                    io::ErrorKind::BrokenPipe
                } else {
                    io::ErrorKind::PermissionDenied
                },
                message: format!("io failure near {canary}"),
                short_step: 2,
                interrupt_once: offset % 5 == 0,
            })
        };
        let err = ayaml::to_writer(&mut w, &payload).expect_err("expected io error");
        assert_io_error_clean(
            &err,
            &w,
            &ref_serde,
            offset,
            Some(canary),
            &format!("to_writer@{offset}"),
        );
    }

    for offset in 0..ref_value.len() {
        let mut w = FaultWriter::new(FaultPlan::FailAtByte {
            fail_at: offset,
            kind: io::ErrorKind::BrokenPipe,
            message: format!("value write failed {canary}"),
            short_step: 1,
            interrupt_once: offset % 7 == 0,
        });
        let err = ayaml::to_writer_value(&mut w, &value).expect_err("expected io error");
        assert_io_error_clean(
            &err,
            &w,
            &ref_value,
            offset,
            Some(canary),
            &format!("to_writer_value@{offset}"),
        );
    }

    for offset in 0..ref_docs.len() {
        let mut w = FaultWriter::new(FaultPlan::FailAtByte {
            fail_at: offset,
            kind: io::ErrorKind::WriteZero,
            message: format!("docs write failed {canary}"),
            short_step: 3,
            interrupt_once: false,
        });
        let err = ayaml::to_writer_documents(&mut w, &docs).expect_err("expected io error");
        assert_io_error_clean(
            &err,
            &w,
            &ref_docs,
            offset,
            Some(canary),
            &format!("to_writer_documents@{offset}"),
        );
    }

    for offset in 0..ref_events.len() {
        let mut w_collected = FaultWriter::new(FaultPlan::FailAtByte {
            fail_at: offset,
            kind: io::ErrorKind::BrokenPipe,
            message: format!("events write failed {canary}"),
            short_step: 1,
            interrupt_once: offset % 4 == 0,
        });
        let err = ayaml::to_writer_events(&mut w_collected, &events).expect_err("io error");
        assert_io_error_clean(
            &err,
            &w_collected,
            &ref_events,
            offset,
            Some(canary),
            &format!("to_writer_events@{offset}"),
        );

        let mut w_stream = FaultWriter::new(FaultPlan::WriteZeroAtByte {
            fail_at: offset,
            short_step: 2,
        });
        let stream = ayaml::parse_events(SAMPLE_EVENT_YAML).expect("stream");
        let err = ayaml::to_writer_event_stream(&mut w_stream, stream).expect_err("io error");
        assert_io_error_clean(
            &err,
            &w_stream,
            &ref_events,
            offset,
            None,
            &format!("to_writer_event_stream@{offset}"),
        );
    }

    // Multi-chunk mid-flush failure (> 16 KiB): fail right before, at, and after chunk flushes.
    let big_value = Value::Sequence(
        (0..600)
            .map(|i| Value::from(format!("chunk-item-{i:04}: 0123456789abcdef")))
            .collect(),
    );
    let ref_big = ayaml::to_string_value(&big_value)
        .expect("ref_big")
        .into_bytes();
    for offset in [0, 1, 4095, 4096, 4097, 8191, 8192, 8193, ref_big.len() - 1] {
        let mut w = FaultWriter::new(FaultPlan::FailAtByte {
            fail_at: offset,
            kind: io::ErrorKind::BrokenPipe,
            message: format!("big chunk fail {canary}"),
            short_step: 257,
            interrupt_once: true,
        });
        let err = ayaml::to_writer_value(&mut w, &big_value).expect_err("big fail");
        assert_io_error_clean(
            &err,
            &w,
            &ref_big,
            offset,
            Some(canary),
            &format!("big_value@{offset}"),
        );
    }

    // Output limit interacts cleanly with short-write + Interrupted writer (stops with Limit, not Io).
    let opts = EmitOptions::new().with_limits(Limits::default().with_max_output_bytes(16));
    let mut w = FaultWriter::new(FaultPlan::ShortAndInterrupted {
        chunk_sizes: &[1],
        interrupt_every: Some(2),
    });
    let err = ayaml::to_writer_value_with(&mut w, &value, &opts).expect_err("output limit");
    assert_eq!(err.kind(), ErrorKind::Limit);
    assert_eq!(err.code(), ErrorCode::OutputSizeLimit);
    assert!(w.written.len() <= 16);

    // Mid-stream parse error fed through `to_writer_event_stream` propagates the parser error.
    let broken_stream_yaml = "---\na: 1\n...\n---\n[unterminated\n";
    let stream = ayaml::parse_events(broken_stream_yaml).expect("stream init");
    let mut out = FaultWriter::new(FaultPlan::ShortAndInterrupted {
        chunk_sizes: &[3, 1],
        interrupt_every: Some(2),
    });
    let err = ayaml::to_writer_event_stream(&mut out, stream).expect_err("mid-stream parse error");
    assert_eq!(err.kind(), ErrorKind::Parser);
}

// ---------------------------------------------------------------------------
// Part 2: Deserializer cursor deterministic state-machine simulation
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
struct BorrowedDoc<'a> {
    kind: &'a str,
    id: u32,
}

struct SumSeed {
    offset: i64,
}

impl<'de> DeserializeSeed<'de> for SumSeed {
    type Value = i64;

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<i64, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let base = value
            .as_i64()
            .or_else(|| value.get("id").and_then(Value::as_i64))
            .unwrap_or(0);
        Ok(base + self.offset)
    }
}

struct BorrowedPrefixSeed<'p> {
    prefix: &'p str,
}

impl<'de> DeserializeSeed<'de> for BorrowedPrefixSeed<'_> {
    type Value = (String, &'de str);

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        let borrowed = <&'de str>::deserialize(deserializer)?;
        Ok((format!("{}:{borrowed}", self.prefix), borrowed))
    }
}

#[test]
fn deserializer_cursor_mixed_operations_and_eof() {
    // Empty and comment-only streams must return Ok(None) across all four cursor methods repeatedly.
    for empty in ["", "   \n\t\n", "# only comments\n# second line\n"] {
        let mut cursor = Deserializer::from_str(empty).expect("empty cursor");
        for _ in 0..3 {
            assert_eq!(cursor.next::<Value>().expect("next"), None);
            assert_eq!(
                cursor.next_borrowed::<Value>().expect("next_borrowed"),
                None
            );
            assert_eq!(
                cursor.next_seed(SumSeed { offset: 10 }).expect("next_seed"),
                None
            );
            assert_eq!(
                cursor
                    .next_seed_borrowed(BorrowedPrefixSeed { prefix: "p" })
                    .expect("next_seed_borrowed"),
                None
            );
        }
    }

    // Heterogeneous multi-document stream driven through all four methods in sequence.
    let multi = "\
---
kind: alpha
id: 10
---
hello
---
42
---
kind: beta
id: 20
---
~
...
";
    let mut cursor = Deserializer::from_str(multi).expect("multi cursor");

    let d0 = cursor
        .next_borrowed::<BorrowedDoc<'_>>()
        .expect("d0")
        .expect("some d0");
    assert_eq!(
        d0,
        BorrowedDoc {
            kind: "alpha",
            id: 10
        }
    );
    // Verify `d0.kind` truly borrows into `multi`.
    let base_ptr = multi.as_ptr() as usize;
    let end_ptr = base_ptr + multi.len();
    let kind_ptr = d0.kind.as_ptr() as usize;
    assert!(
        (base_ptr..=end_ptr).contains(&kind_ptr),
        "next_borrowed should borrow simple scalar directly from input"
    );

    let (tagged, raw_slice) = cursor
        .next_seed_borrowed(BorrowedPrefixSeed { prefix: "doc1" })
        .expect("d1")
        .expect("some d1");
    assert_eq!(tagged, "doc1:hello");
    assert_eq!(raw_slice, "hello");
    assert!((base_ptr..=end_ptr).contains(&(raw_slice.as_ptr() as usize)));

    let d2 = cursor
        .next_seed(SumSeed { offset: 100 })
        .expect("d2")
        .expect("some d2");
    assert_eq!(d2, 142);

    let d3 = cursor
        .next::<BTreeMap<String, Value>>()
        .expect("d3")
        .expect("some d3");
    assert_eq!(d3.get("kind"), Some(&Value::from("beta")));
    assert_eq!(d3.get("id"), Some(&Value::from(20_u64)));

    let d4 = cursor.next_borrowed::<Option<u64>>().expect("d4");
    assert_eq!(d4, Some(None));

    // Post-EOF calls across all four methods remain cleanly Ok(None).
    for _ in 0..4 {
        assert_eq!(cursor.next::<Value>().expect("eof next"), None);
        assert_eq!(
            cursor.next_borrowed::<Value>().expect("eof next_borrowed"),
            None
        );
        assert_eq!(
            cursor.next_seed(SumSeed { offset: 1 }).expect("eof seed"),
            None
        );
        assert_eq!(
            cursor
                .next_seed_borrowed(BorrowedPrefixSeed { prefix: "eof" })
                .expect("eof seed_borrowed"),
            None
        );
    }
}

#[test]
fn deserializer_cursor_error_sealing_and_type_mismatch_recovery() {
    // 1. Mid-stream parser error seals `finished = true`: subsequent calls return Ok(None).
    let broken = "---\na: 1\n---\n[1, 2}\n---\nc: 3\n";
    let mut cursor = Deserializer::from_str(broken).expect("init");
    assert_eq!(
        cursor.next::<BTreeMap<String, u32>>().expect("doc 0"),
        Some(BTreeMap::from([("a".to_owned(), 1)]))
    );
    let err = cursor
        .next_borrowed::<Value>()
        .expect_err("doc 1 syntax error");
    assert_eq!(err.kind(), ErrorKind::Parser);
    for _ in 0..3 {
        assert_eq!(cursor.next::<Value>().expect("after parse err"), None);
        assert_eq!(
            cursor
                .next_borrowed::<Value>()
                .expect("after parse err borrowed"),
            None
        );
        assert_eq!(
            cursor
                .next_seed(SumSeed { offset: 0 })
                .expect("after parse err seed"),
            None
        );
    }

    // 2. Typed mismatch and per-document composition errors (DuplicateKey, UnknownAnchor) fail
    //    only the offending document without sealing subsequent well-formed documents.
    let typed_mismatch = "---\n1\n---\nnot_an_integer\n---\n3\n";
    let mut cursor = Deserializer::from_str(typed_mismatch).expect("init");
    assert_eq!(cursor.next::<u64>().expect("doc 0"), Some(1));
    let err = cursor.next_borrowed::<u64>().expect_err("doc 1 mismatch");
    assert_eq!(err.kind(), ErrorKind::Serde);
    assert_eq!(err.code(), ErrorCode::InvalidType);
    assert_eq!(cursor.next::<u64>().expect("doc 2 still readable"), Some(3));
    assert_eq!(cursor.next::<u64>().expect("eof"), None);

    let per_doc_errors = "---\na: 1\na: 2\n---\n*missing\n---\nok: true\n";
    let mut cursor = Deserializer::from_str(per_doc_errors).expect("init");
    let dup_err = cursor.next::<Value>().expect_err("doc 0 duplicate key");
    assert_eq!(dup_err.kind(), ErrorKind::DuplicateKey);
    let alias_err = cursor
        .next_borrowed::<Value>()
        .expect_err("doc 1 unknown anchor");
    assert_eq!(alias_err.kind(), ErrorKind::Composition);
    assert_eq!(alias_err.code(), ErrorCode::UnknownAnchor);
    let ok_doc = cursor
        .next::<BTreeMap<String, bool>>()
        .expect("doc 2 survives earlier per-doc composition errors");
    assert_eq!(ok_doc, Some(BTreeMap::from([("ok".to_owned(), true)])));
    assert_eq!(cursor.next::<Value>().expect("eof"), None);

    // 3. Shared alias budget accumulates across documents in one cursor stream.
    let aliased_stream = "\
---
a: &x 1
b: *x
---
c: &y 2
d: *y
---
e: &z 3
f: *z
";
    let opts = ParseOptions::new().with_limits(Limits::default().with_max_alias_dereferences(2));
    let mut cursor = Deserializer::from_str_with(aliased_stream, &opts).expect("cursor");
    assert!(cursor.next_borrowed::<Value>().expect("doc 0").is_some());
    assert!(cursor.next::<Value>().expect("doc 1").is_some());
    let budget_err = cursor
        .next_borrowed::<Value>()
        .expect_err("doc 2 exceeds shared stream alias dereference budget");
    assert_eq!(budget_err.kind(), ErrorKind::Limit);
    assert_eq!(budget_err.code(), ErrorCode::AliasDereferenceLimit);
}

#[test]
fn deserializer_cursor_seeded_state_machine_fuzz() {
    let inputs = [
        "",
        "---\n1\n---\n2\n---\n3\n---\n4\n",
        "---\nk: &a [1, 2]\ncopy: *a\n---\nk: \"hello\"\n---\nnull\n",
        "---\n0\n---\n[bad\n---\n9\n",
        "---\na: 1\na: 2\n---\nb: 3\n",
        "---\n*unknown\n---\nok: true\n",
    ];
    let mut rng = Rng::new(0x5EED_C0DE_1337_A133);

    for trial in 0..180 {
        let input = inputs[trial % inputs.len()];
        let expected_docs = ayaml::parse_documents(input);
        let mut cursor = Deserializer::from_str(input).expect("cursor init");
        let mut doc_idx = 0_usize;
        let mut saw_error = false;
        let steps = 4 + rng.gen_range(8);

        for _ in 0..steps {
            let op = rng.gen_range(5);
            let res: Result<Option<Value>, ayaml::Error> = match op {
                0 => cursor.next::<Value>(),
                1 => cursor.next_borrowed::<Value>(),
                2 => cursor
                    .next_seed(SumSeed { offset: 5 })
                    .map(|opt| opt.map(Value::from)),
                3 => cursor.next_seed_borrowed(std::marker::PhantomData::<Value>),
                _ => {
                    // Deliberate wrong type on ~20% of ops only on error-testing inputs;
                    // for clean streams read as Value so we can cross-check exact doc values.
                    if expected_docs.is_ok() {
                        cursor.next_borrowed::<Value>()
                    } else {
                        cursor.next::<u64>().map(|opt| opt.map(Value::from))
                    }
                }
            };

            match res {
                Ok(Some(v)) => {
                    if let Ok(ref docs) = expected_docs {
                        assert!(
                            doc_idx < docs.len(),
                            "trial {trial}: cursor produced more docs than parse_documents"
                        );
                        if op == 2 {
                            let expected_base = docs[doc_idx]
                                .as_i64()
                                .or_else(|| docs[doc_idx].get("id").and_then(Value::as_i64))
                                .unwrap_or(0);
                            assert_eq!(
                                v,
                                Value::from(expected_base + 5),
                                "trial {trial} doc {doc_idx} SumSeed mismatch"
                            );
                        } else {
                            assert_eq!(
                                v, docs[doc_idx],
                                "trial {trial} doc {doc_idx} value mismatch"
                            );
                        }
                    }
                    doc_idx += 1;
                }
                Ok(None) => {
                    if let Ok(ref docs) = expected_docs {
                        assert_eq!(
                            doc_idx,
                            docs.len(),
                            "trial {trial}: premature EOF at {doc_idx}/{}",
                            docs.len()
                        );
                    }
                    // Once EOF is observed, remaining steps must keep yielding Ok(None).
                    assert_eq!(cursor.next::<Value>().expect("sticky eof"), None);
                }
                Err(_) => {
                    saw_error = true;
                }
            }
        }

        if let Ok(ref docs) = expected_docs {
            assert!(!saw_error, "valid stream errored on trial {trial}");
            assert!(doc_idx <= docs.len());
        }
    }
}

// ---------------------------------------------------------------------------
// Part 3: Typed streaming partial consumption, early stop, and error fallback
// ---------------------------------------------------------------------------

/// A Serde visitor that reads at most `limit` sequence elements and stops early without draining
/// the remaining elements.
struct TakeNSeq(usize);

impl<'de> DeserializeSeed<'de> for TakeNSeq {
    type Value = Vec<Value>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        struct SeqTakeVisitor(usize);

        impl<'de> Visitor<'de> for SeqTakeVisitor {
            type Value = Vec<Value>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a sequence")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while out.len() < self.0 {
                    let Some(item) = seq.next_element::<Value>()? else {
                        break;
                    };
                    out.push(item);
                }
                Ok(out)
            }
        }

        deserializer.deserialize_seq(SeqTakeVisitor(self.0))
    }
}

/// A Serde visitor that reads at most `limit` mapping entries and stops early.
struct TakeNMap(usize);

impl<'de> DeserializeSeed<'de> for TakeNMap {
    type Value = Vec<(String, Value)>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        struct MapTakeVisitor(usize);

        impl<'de> Visitor<'de> for MapTakeVisitor {
            type Value = Vec<(String, Value)>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a mapping")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while out.len() < self.0 {
                    let Some((k, v)) = map.next_entry::<String, Value>()? else {
                        break;
                    };
                    out.push((k, v));
                }
                Ok(out)
            }
        }

        deserializer.deserialize_map(MapTakeVisitor(self.0))
    }
}

/// A Serde visitor that calls `next_key()` on a mapping and returns early WITHOUT calling
/// `next_value()`, leaving the pending entry value unread.
struct ReadFirstKeyOnly;

impl<'de> DeserializeSeed<'de> for ReadFirstKeyOnly {
    type Value = Option<String>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        struct KeyOnlyVisitor;

        impl<'de> Visitor<'de> for KeyOnlyVisitor {
            type Value = Option<String>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a mapping")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                map.next_key::<String>()
            }
        }

        deserializer.deserialize_map(KeyOnlyVisitor)
    }
}

/// A Serde visitor that aborts with a custom Serde error at element `fail_at`.
struct FailSeqAt(usize);

impl<'de> DeserializeSeed<'de> for FailSeqAt {
    type Value = Vec<Value>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        struct FailVisitor(usize);

        impl<'de> Visitor<'de> for FailVisitor {
            type Value = Vec<Value>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a sequence")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                let mut idx = 0;
                while let Some(item) = seq.next_element::<Value>()? {
                    if idx == self.0 {
                        return Err(de::Error::custom(format!("visitor aborted at index {idx}")));
                    }
                    out.push(item);
                    idx += 1;
                }
                Ok(out)
            }
        }

        deserializer.deserialize_seq(FailVisitor(self.0))
    }
}

/// Wrapper types so we can drive `from_str`, `from_str_borrowed`, `from_str_by_tree`, and
/// `from_str_by_events` directly with partial-consumption and early-abort visitors.
#[derive(Debug, PartialEq)]
struct FirstTwoSeq(Vec<Value>);

impl<'de> Deserialize<'de> for FirstTwoSeq {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        TakeNSeq(2).deserialize(deserializer).map(Self)
    }
}

#[derive(Debug, PartialEq)]
struct ZeroSeq(Vec<Value>);

impl<'de> Deserialize<'de> for ZeroSeq {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        TakeNSeq(0).deserialize(deserializer).map(Self)
    }
}

#[derive(Debug, PartialEq)]
struct FirstOneMap(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for FirstOneMap {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        TakeNMap(1).deserialize(deserializer).map(Self)
    }
}

#[derive(Debug, PartialEq)]
struct FirstKeyOnly(Option<String>);

impl<'de> Deserialize<'de> for FirstKeyOnly {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        ReadFirstKeyOnly.deserialize(deserializer).map(Self)
    }
}

#[derive(Debug, PartialEq)]
struct AbortAtOne(Vec<Value>);

impl<'de> Deserialize<'de> for AbortAtOne {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        FailSeqAt(1).deserialize(deserializer).map(Self)
    }
}

fn large_seq_yaml(count: usize, trailing_fault: Option<&str>) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(count * 32);
    for i in 0..count {
        let _ = writeln!(out, "- id: {i}\n  label: item-{i:04}-padding-text");
    }
    if let Some(fault) = trailing_fault {
        out.push_str(fault);
    }
    out
}

fn large_map_yaml(count: usize, trailing_fault: Option<&str>) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(count * 32);
    for i in 0..count {
        let _ = writeln!(out, "k{i:04}: [\"val-{i:04}\", {i}, true]");
    }
    if let Some(fault) = trailing_fault {
        out.push_str(fault);
    }
    out
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
struct PartialStruct {
    k0000: Vec<Value>,
    k0001: Vec<Value>,
}

#[test]
fn typed_streaming_partial_consumption_and_early_stop() {
    // 1. Exact-length consumption succeeds on both tree and streaming paths.
    let two_seq = large_seq_yaml(2, None);
    let exact_two: FirstTwoSeq = ayaml::from_str(&two_seq).expect("exact 2-element seq");
    assert_eq!(exact_two.0.len(), 2);
    assert_eq!(
        ayaml::from_str_borrowed::<FirstTwoSeq>(&two_seq).expect("borrowed exact 2"),
        exact_two
    );
    let empty_seq: ZeroSeq = ayaml::from_str("[]\n").expect("empty seq");
    assert_eq!(empty_seq.0, Vec::<Value>::new());
    let one_map = large_map_yaml(1, None);
    let exact_one: FirstOneMap = ayaml::from_str(&one_map).expect("exact 1-entry map");
    assert_eq!(exact_one.0.len(), 1);
    assert_eq!(exact_one.0[0].0, "k0000");

    // 2. Partial consumption with `IgnoredAny` (both whole-document ignore and a typed struct
    //    reading 2 known fields while streaming-skipping 1,798 unknown fields) succeeds cleanly.
    for count in [4_usize, 1800_usize] {
        let seq_yaml = large_seq_yaml(count, None);
        let ignored: de::IgnoredAny = ayaml::from_str(&seq_yaml).expect("IgnoredAny seq");
        let _ = ignored;

        let map_yaml = large_map_yaml(count, None);
        let partial: PartialStruct = ayaml::from_str(&map_yaml).expect("PartialStruct");
        assert_eq!(partial.k0000[0], Value::from("val-0000"));
        assert_eq!(partial.k0001[0], Value::from("val-0001"));
        let partial_borrowed: PartialStruct =
            ayaml::from_str_borrowed(&map_yaml).expect("borrowed PartialStruct");
        assert_eq!(partial_borrowed, partial);

        // 3. Early visitor termination leaving unread elements/entries behind is consistently
        //    rejected with `ErrorKind::Serde` / `ErrorCode::InvalidLength` on both small (tree)
        //    and large (streaming event) inputs without panics or corrupted state.
        for err in [
            ayaml::from_str::<FirstTwoSeq>(&seq_yaml).expect_err("unread seq tail"),
            ayaml::from_str_borrowed::<FirstTwoSeq>(&seq_yaml).expect_err("borrowed unread tail"),
            ayaml::from_str::<ZeroSeq>(&seq_yaml).expect_err("unread zero seq"),
            ayaml::from_str::<FirstOneMap>(&map_yaml).expect_err("unread map tail"),
            ayaml::from_str::<FirstKeyOnly>(&map_yaml).expect_err("unread key-only map tail"),
        ] {
            assert_eq!(err.kind(), ErrorKind::Serde, "early stop kind: {err:?}");
            assert_eq!(
                err.code(),
                ErrorCode::InvalidLength,
                "early stop code: {err:?}"
            );
        }
    }

    // 4. Stopping after `next_key()` without calling `next_value()` on a 1-entry mapping leaves
    //    no unread entries and drains the skipped value cleanly without panicking.
    let single_entry_key: FirstKeyOnly =
        ayaml::from_str(&one_map).expect("single entry key-only drains skipped value");
    assert_eq!(single_entry_key.0.as_deref(), Some("k0000"));

    // 5. `Deserializer::next_seed_borrowed` after an early-stop `InvalidLength` error on doc 0
    //    and partial `IgnoredAny` on doc 1 must still advance cleanly to doc 2!
    let multi = "---\n- 10\n- 20\n- 30\n---\nk0: v0\nk1: v1\n---\n99\n";
    let mut cursor = Deserializer::from_str(multi).expect("cursor");
    let err = cursor
        .next_seed_borrowed(TakeNSeq(1))
        .expect_err("doc 0 early stop");
    assert_eq!(err.code(), ErrorCode::InvalidLength);
    let skipped = cursor
        .next_borrowed::<de::IgnoredAny>()
        .expect("doc 1 ignored");
    assert!(skipped.is_some());
    // 6. Anchors and aliases inside skipped/unread tails + nested partial consumption
    //    (sequence of maps where every map skips unknown fields via `PartialStruct`).
    let mut nested_seq = String::new();
    for i in 0..400 {
        use std::fmt::Write as _;
        let _ = writeln!(
            nested_seq,
            "- k0000: &a{i} [\"v{i}\"]\n  k0001: *a{i}\n  ignored_tail: *a{i}\n  extra_{i}: [1, 2, 3]"
        );
    }
    let parsed_nested: Vec<PartialStruct> =
        ayaml::from_str(&nested_seq).expect("nested partial structs with aliases in ignored tail");
    assert_eq!(parsed_nested.len(), 400);
    assert_eq!(parsed_nested[399].k0000, vec![Value::from("v399")]);
    assert_eq!(parsed_nested[399].k0001, vec![Value::from("v399")]);

    let mut aliased_tail = large_map_yaml(1200, None);
    aliased_tail.push_str("anchored_tail: &tail_ref [10, 20]\nreused_tail: *tail_ref\n");
    let partial_with_tail_alias: PartialStruct =
        ayaml::from_str(&aliased_tail).expect("valid alias inside unread skipped tail");
    assert_eq!(partial_with_tail_alias.k0000[0], Value::from("val-0000"));
}

#[test]
fn typed_streaming_error_fallback_detects_late_document_faults() {
    // Even when a visitor stops early (after 0 or 2 elements) or aborts with a custom Serde
    // error at index 1, a LATER fault in the YAML document (duplicate key, unknown alias,
    // trailing document, syntax error) MUST still be detected and reported as the document error!
    let late_faults: &[(&str, &str, ErrorKind, ErrorCode)] = &[
        (
            "late_duplicate_key",
            "- dup: 1\n  dup: 2\n",
            ErrorKind::DuplicateKey,
            ErrorCode::DuplicateKey,
        ),
        (
            "late_unknown_alias",
            "- *missing_anchor\n",
            ErrorKind::Composition,
            ErrorCode::UnknownAnchor,
        ),
        (
            "late_trailing_doc",
            "---\nextra: true\n",
            ErrorKind::Parser,
            ErrorCode::TrailingDocuments,
        ),
        (
            "late_syntax_error",
            "- [unterminated\n",
            ErrorKind::Parser,
            ErrorCode::UnterminatedFlowCollection,
        ),
    ];

    for count in [5_usize, 1800_usize] {
        for &(label, fault, expected_kind, expected_code) in late_faults {
            let input = large_seq_yaml(count, Some(fault));

            // 1. Visitor that reads 0 elements must still fail with the late document error.
            let err0 = ayaml::from_str::<ZeroSeq>(&input)
                .expect_err(&format!("{label} count={count} ZeroSeq"));
            assert_eq!(err0.kind(), expected_kind, "{label} ZeroSeq: {err0:?}");
            assert_eq!(err0.code(), expected_code, "{label} ZeroSeq: {err0:?}");

            // 2. Visitor that reads 2 elements must still fail with the late document error.
            let err2 = ayaml::from_str::<FirstTwoSeq>(&input)
                .expect_err(&format!("{label} count={count} FirstTwoSeq"));
            assert_eq!(err2.kind(), expected_kind, "{label} FirstTwoSeq: {err2:?}");
            assert_eq!(err2.code(), expected_code, "{label} FirstTwoSeq: {err2:?}");

            // 3. Visitor that aborts with a custom Serde error at index 1 MUST fall back to
            //    reporting the document error (matching the tree reader).
            let err_abort = ayaml::from_str::<AbortAtOne>(&input)
                .expect_err(&format!("{label} count={count} AbortAtOne"));
            assert_eq!(
                err_abort.kind(),
                expected_kind,
                "{label} AbortAtOne: {err_abort:?}"
            );
            assert_eq!(
                err_abort.code(),
                expected_code,
                "{label} AbortAtOne: {err_abort:?}"
            );
        }

        // On a VALID large document, `AbortAtOne` must return the visitor's custom Serde error.
        let valid = large_seq_yaml(count, None);
        let err = ayaml::from_str::<AbortAtOne>(&valid).expect_err("valid AbortAtOne");
        assert_eq!(err.kind(), ErrorKind::Serde);
        assert_eq!(err.code(), ErrorCode::SerdeCustom);
        assert!(
            err.to_string().contains("visitor aborted at index 1"),
            "unexpected error: {err}"
        );
    }

    // Same late-fault guarantee on mappings when visitor only reads the first key (`FirstKeyOnly`).
    for count in [5_usize, 1800_usize] {
        let dup_map = large_map_yaml(count, Some("k0000: duplicate\n"));
        let err = ayaml::from_str::<FirstKeyOnly>(&dup_map).expect_err("late map duplicate key");
        assert_eq!(err.kind(), ErrorKind::DuplicateKey);
        assert_eq!(err.code(), ErrorCode::DuplicateKey);
    }
}

#[cfg(feature = "bench-internals")]
#[test]
fn typed_streaming_seeded_visitor_matches_tree_reader() {
    let mut rng = Rng::new(0xA133_57A7_E001);
    let limits = Limits::default();

    for trial in 0..120 {
        let is_map = rng.gen_range(2) == 0;
        let len = 1 + rng.gen_range(12);
        let fault_kind = rng.gen_range(5);
        let fault = match (is_map, fault_kind) {
            (_, 0 | 1) => None,
            (false, 2) => Some("- dup: 1\n  dup: 2\n"),
            (true, 2) => Some("k0000: duplicate\n"),
            (false, 3) => Some("- *dangling\n"),
            (true, 3) => Some("late: *dangling\n"),
            (_, _) => Some("---\nsecond: doc\n"),
        };
        let input = if is_map {
            large_map_yaml(len, fault)
        } else {
            large_seq_yaml(len, fault)
        };

        let compat = rng.gen_range(2) == 1;
        let drop_tags = compat && rng.gen_range(2) == 1;

        macro_rules! check_equiv {
            ($ty:ty, $name:expr) => {{
                let tree = ayaml::internals::from_str_by_tree_with::<$ty>(
                    &input, &limits, compat, drop_tags,
                );
                let events = ayaml::internals::from_str_by_events_with::<$ty>(
                    &input, &limits, compat, drop_tags,
                );
                match (tree, events) {
                    (Ok(l), Ok(r)) => assert_eq!(l, r, "trial {trial} {}", $name),
                    (Err(l), Err(r)) => {
                        assert_eq!(l.kind(), r.kind(), "trial {trial} {} kind", $name);
                        assert_eq!(l.code(), r.code(), "trial {trial} {} code", $name);
                        assert_eq!(l.span(), r.span(), "trial {trial} {} span", $name);
                        assert_eq!(
                            l.location(),
                            r.location(),
                            "trial {trial} {} location",
                            $name
                        );
                    }
                    (l, r) => panic!("trial {trial} {}: tree={l:?} vs events={r:?}", $name),
                }
            }};
        }

        if is_map {
            check_equiv!(FirstOneMap, "FirstOneMap");
            check_equiv!(FirstKeyOnly, "FirstKeyOnly");
        } else {
            check_equiv!(ZeroSeq, "ZeroSeq");
            check_equiv!(FirstTwoSeq, "FirstTwoSeq");
            check_equiv!(AbortAtOne, "AbortAtOne");
        }
    }
}
