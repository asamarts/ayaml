use super::scalars::double_quoted;
use super::{EmitterOptions, Output, TagDirectives, ValueWriter};
use crate::value::Value;
use crate::{ErrorKind, LimitKind, Limits, ParseOptions};

struct AlwaysFailWriter {
    writes: usize,
}

impl std::io::Write for AlwaysFailWriter {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        self.writes += 1;
        Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "broken pipe",
        ))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn document_writer_streams_parseable_output() {
    let values = vec![
        crate::parse_value("a: 1\n").expect("first"),
        crate::parse_value("b: 2\n").expect("second"),
    ];
    let mut output = Vec::new();
    crate::to_writer_documents(&mut output, &values).expect("write");
    let options = ParseOptions::new().with_limits(Limits::new().with_max_documents(2));
    let decoded =
        crate::parse_documents_with(std::str::from_utf8(&output).expect("utf8"), &options)
            .expect("reparse");
    assert_eq!(decoded.len(), 2);
    assert_eq!(decoded[1]["b"], Value::from(2_u64));
}

/// `ValueWriter::emit_flow`, `emit_block_scalar`, and quoted scalar streaming break immediately
/// once `self.error` is set instead of iterating and formatting remaining entries or lines
/// (AY-169).
#[test]
fn value_writer_stops_looping_immediately_after_first_error() {
    let limits = Limits::default().with_max_output_bytes(32);
    let opts = EmitterOptions::new().with_flow(true);
    let payload = "long-payload-string-forcing-limit";

    // 1. Flow sequence of 2,000 elements stops after at most 2 iterations.
    let seq = Value::Sequence((0..2_000).map(|_| Value::from(payload)).collect());
    let mut buf = Vec::new();
    let mut out = Output::direct(&mut buf, &limits);
    let mut writer = ValueWriter::new(&mut out, &limits, opts, None, TagDirectives::default());
    writer.emit_root(&seq);
    assert!(
        writer.flow_loop_steps <= 2,
        "flow sequence kept looping after error: {} steps",
        writer.flow_loop_steps
    );
    let err = writer.finish().expect_err("limit exceeded");
    assert_eq!(err.kind(), ErrorKind::Limit);
    assert_eq!(
        err.limit().map(crate::LimitExceeded::kind),
        Some(LimitKind::OutputBytes)
    );

    // 2. Flow mapping of 2,000 entries stops after at most 2 iterations.
    let mut map = crate::Mapping::with_capacity(2_000);
    for i in 0..2_000 {
        map.insert(Value::from(format!("k{i}")), Value::from(payload));
    }
    let map_val = Value::Mapping(map);
    let mut buf = Vec::new();
    let mut out = Output::direct(&mut buf, &limits);
    let mut writer = ValueWriter::new(&mut out, &limits, opts, None, TagDirectives::default());
    writer.emit_root(&map_val);
    assert!(
        writer.flow_loop_steps <= 2,
        "flow mapping kept looping after error: {} steps",
        writer.flow_loop_steps
    );
    let err = writer.finish().expect_err("limit exceeded");
    assert_eq!(err.kind(), ErrorKind::Limit);

    // 3. Multi-line block scalar stops after at most 3 lines when output limit trips.
    let multiline = Value::String("0123456789abcdef\n".repeat(2_000));
    let mut buf = Vec::new();
    let mut out = Output::direct(&mut buf, &limits);
    let mut writer = ValueWriter::new(
        &mut out,
        &limits,
        EmitterOptions::new(),
        None,
        TagDirectives::default(),
    );
    writer.emit_root(&multiline);
    assert!(
        writer.block_scalar_lines <= 3,
        "block scalar kept looping after error: {} lines",
        writer.block_scalar_lines
    );
    let err = writer.finish().expect_err("limit exceeded");
    assert_eq!(err.kind(), ErrorKind::Limit);

    // 4. Folded block scalar rejects before allocating `expand_folded` when `body.len()`
    //    already exceeds `max_output_bytes`.
    let mut buf = Vec::new();
    let mut out = Output::direct(&mut buf, &limits);
    let mut writer = ValueWriter::new(
        &mut out,
        &limits,
        EmitterOptions::new().with_scalar_style(Some(crate::ScalarStyle::Folded)),
        None,
        TagDirectives::default(),
    );
    writer.emit_root(&multiline);
    assert_eq!(
        writer.block_scalar_lines, 0,
        "folded block scalar must abort before emitting lines when body exceeds limit"
    );
    let err = writer.finish().expect_err("folded limit exceeded");
    assert_eq!(err.kind(), ErrorKind::Limit);

    // 5. Failing `io::Write` stops flow sequence and mapping loops immediately.
    let default_limits = Limits::default();
    for (label, value) in [("io_seq", &seq), ("io_map", &map_val)] {
        let mut fail_writer = AlwaysFailWriter { writes: 0 };
        let mut out = Output::direct(&mut fail_writer, &default_limits);
        let mut writer = ValueWriter::new(
            &mut out,
            &default_limits,
            opts,
            None,
            TagDirectives::default(),
        );
        writer.emit_root(value);
        assert!(
            writer.flow_loop_steps <= 1,
            "{label} kept looping after I/O error: {} steps",
            writer.flow_loop_steps
        );
        let err = writer.finish().expect_err("io error");
        assert_eq!(err.kind(), ErrorKind::Io);
        assert_eq!(
            fail_writer.writes, 1,
            "{label} invoked writer more than once"
        );
    }

    // 6. Single-quoted and double-quoted streaming emit stop immediately on output limit.
    let single_quoted = Value::String("abc'def'".repeat(2_000));
    let mut buf = Vec::new();
    let mut out = Output::direct(&mut buf, &limits);
    let mut writer = ValueWriter::new(
        &mut out,
        &limits,
        EmitterOptions::new().with_scalar_style(Some(crate::ScalarStyle::SingleQuoted)),
        None,
        TagDirectives::default(),
    );
    writer.emit_root(&single_quoted);
    assert!(
        writer.quoted_chunks <= 10,
        "single-quoted scalar kept looping after error: {} chunks",
        writer.quoted_chunks
    );
    assert_eq!(
        writer.finish().expect_err("single-quoted limit").kind(),
        ErrorKind::Limit
    );

    let double_quoted_str =
        Value::String("a\"b\\c\n\t\x7f\u{0085}\u{2028}\u{2029}\u{feff}é".repeat(500));
    let mut buf = Vec::new();
    let mut out = Output::direct(&mut buf, &limits);
    let mut writer = ValueWriter::new(
        &mut out,
        &limits,
        EmitterOptions::new().with_scalar_style(Some(crate::ScalarStyle::DoubleQuoted)),
        None,
        TagDirectives::default(),
    );
    writer.emit_root(&double_quoted_str);
    assert!(
        writer.quoted_chunks <= 20,
        "double-quoted scalar kept looping after error: {} chunks",
        writer.quoted_chunks
    );
    assert_eq!(
        writer.finish().expect_err("double-quoted limit").kind(),
        ErrorKind::Limit
    );

    // Verify streaming double-quoted output is byte-identical to `double_quoted(...)`.
    let tricky = "a\"b\\c\0\x07\x08\t\n\x0b\x0c\r\x1b\x01\x7f\u{0085}\u{2028}\u{2029}\u{feff}\u{009f}é\u{1f680}\u{10ffff}";
    let emitted = crate::to_string_value_with(
        &Value::from(tricky),
        &crate::EmitOptions::new().with_scalar_style(Some(crate::ScalarStyle::DoubleQuoted)),
    )
    .expect("emit tricky double-quoted");
    assert_eq!(emitted, format!("{}\n", double_quoted(tricky)));
}
