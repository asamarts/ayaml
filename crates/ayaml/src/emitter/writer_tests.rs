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

/// `ValueWriter` flow-collection loops, `emit_block_scalar`, and quoted scalar streaming break
/// immediately once `self.error` is set instead of iterating and formatting remaining entries or
/// lines (AY-169).
#[test]
fn value_writer_stops_looping_immediately_after_first_error() {
    let limits = Limits::default().with_max_output_bytes(32);
    let opts = EmitterOptions::new().with_flow(true);
    let payload = "long-payload-string-forcing-limit";

    let count = if cfg!(miri) { 64 } else { 2_000 };

    // 1. Flow sequence stops after at most 2 iterations.
    let seq = Value::Sequence((0..count).map(|_| Value::from(payload)).collect());
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

    // 2. Flow mapping stops after at most 2 iterations.
    let mut map = crate::Mapping::with_capacity(count);
    for i in 0..count {
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
    let multiline = Value::String("0123456789abcdef\n".repeat(count));
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

    // 4. Folded block scalar rejects before streaming lines when `body.len()` already exceeds
    //    `max_output_bytes`.
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

/// Tear down a deeply nested `Value` iteratively so dropping it on a small test thread stack does
/// not overflow Rust's recursive default `Drop`.
fn drop_value_iteratively(root: Value) {
    let mut work = vec![root];
    while let Some(node) = work.pop() {
        match node {
            Value::Sequence(items) => work.extend(items),
            Value::Mapping(map) => {
                for (k, v) in map {
                    work.push(k);
                    work.push(v);
                }
            }
            Value::Tagged(tagged) => work.push(tagged.value),
            _ => {}
        }
    }
}

/// `ValueWriter` and `TagDirectives::scan` use explicit tiered/heap stacks rather than native
/// recursion, so emitting trees far deeper than `MAX_DEPTH_CAP` (exercising small, medium, and
/// heap-overflow stack tiers) fits comfortably on a 512 KiB thread stack (AY-176).
#[test]
fn iterative_value_writer_and_tag_scan_handle_deep_nesting_on_small_stack() {
    use crate::emitter::{EmitterStyles, NodeStyle, PathSegment};
    use crate::{Mapping, Tag, TaggedValue};

    let handle = std::thread::Builder::new()
        .stack_size(512 << 10)
        .spawn(|| {
            // Under Miri, depth 136 still exceeds MAX_DEPTH_CAP (128) and exercises all three
            // TieredStack tiers (inline 8 -> medium 128 -> heap overflow) without O(DEPTH^2)
            // indentation formatting taking minutes under interpretation.
            const DEPTH: usize = if cfg!(miri) { 136 } else { 512 };
            let limits = Limits::default().with_max_depth(DEPTH);
            let tight_limits = Limits::default().with_max_depth(DEPTH - 1);

            let build_nested_seq = |depth: usize| -> Value {
                let mut v = Value::from(0_u64);
                for _ in 0..depth {
                    v = Value::Sequence(vec![v]);
                }
                v
            };

            let build_nested_map = |depth: usize| -> Value {
                let mut v = Value::from("leaf");
                for _ in 0..depth {
                    let mut m = Mapping::new();
                    m.insert(Value::from("k"), v);
                    v = Value::Mapping(m);
                }
                v
            };

            let build_nested_tagged_map = |depth: usize| -> Value {
                // Both root and leaf prefixes require `%TAG` directives (contain a space).
                let mut v = Value::Tagged(Box::new(TaggedValue::new(
                    Tag::new("custom leaf:", "item"),
                    Value::from(1_u64),
                )));
                for level in 0..depth {
                    let mut m = Mapping::new();
                    m.insert(Value::from("n"), v);
                    let wrapped = if level == depth - 1 {
                        Value::Tagged(Box::new(TaggedValue::new(
                            Tag::new("custom root:", "top"),
                            Value::Mapping(m),
                        )))
                    } else {
                        Value::Mapping(m)
                    };
                    v = wrapped;
                }
                v
            };

            let emit_direct = |value: &Value,
                               limits: &Limits,
                               opts: EmitterOptions,
                               styles: Option<&EmitterStyles>|
             -> Result<String, crate::Error> {
                let mut buf = Vec::new();
                let mut out = Output::direct(&mut buf, limits);
                let mut directives = TagDirectives::default();
                directives.scan(value, limits.max_depth, &out)?;
                if !directives.is_empty() {
                    directives.write_lines(&mut out)?;
                    out.push_str("---\n")?;
                }
                let mut writer = ValueWriter::new(&mut out, limits, opts, styles, directives);
                writer.emit_root(value);
                writer.finish()?;
                out.finish()?;
                Ok(String::from_utf8(buf).expect("utf8"))
            };

            // 1. Block and flow sequences of depth 512.
            let seq = build_nested_seq(DEPTH);
            for flow in [false, true] {
                let opts = EmitterOptions::new().with_flow(flow);
                let text = emit_direct(&seq, &limits, opts, None).expect("deep seq emits");
                assert!(text.contains('0'));
                let err = emit_direct(&seq, &tight_limits, opts, None)
                    .expect_err("depth - 1 rejects deep seq");
                assert_eq!(err.kind(), ErrorKind::Limit);
                assert_eq!(
                    err.limit().map(crate::LimitExceeded::kind),
                    Some(LimitKind::Depth)
                );
            }
            drop_value_iteratively(seq);

            // 2. Block and flow mappings of depth 512 (with active EmitterStyles path tracking).
            let map = build_nested_map(DEPTH);
            let mut styles = EmitterStyles::new();
            styles.set(
                vec![PathSegment::key("k")],
                NodeStyle::new().with_scalar_style(Some(crate::ScalarStyle::DoubleQuoted)),
            );
            for flow in [false, true] {
                let opts = EmitterOptions::new().with_flow(flow);
                let text = emit_direct(&map, &limits, opts, Some(&styles)).expect("deep map emits");
                assert!(text.contains("leaf"));
                let err = emit_direct(&map, &tight_limits, opts, Some(&styles))
                    .expect_err("depth - 1 rejects deep map");
                assert_eq!(err.kind(), ErrorKind::Limit);
                assert_eq!(
                    err.limit().map(crate::LimitExceeded::kind),
                    Some(LimitKind::Depth)
                );
            }
            drop_value_iteratively(map);

            // 3. Deeply nested tagged mapping verifying TagDirectives::scan DFS handle ordering
            //    (`!t0!` for root tag, `!t1!` for depth-512 leaf tag) across all stack tiers.
            let tagged = build_nested_tagged_map(DEPTH);
            let text = emit_direct(
                &tagged,
                &limits,
                EmitterOptions::new().with_flow(true),
                None,
            )
            .expect("deep tagged map emits");
            assert!(text.starts_with("%TAG !t0! custom%20root:\n%TAG !t1! custom%20leaf:\n---\n"));
            assert!(text.contains("!t0!"));
            assert!(text.contains("!t1!item 1"));
            drop_value_iteratively(tagged);
        })
        .expect("spawn small-stack thread");
    handle.join().expect("small-stack deep emission succeeded");
}

/// Folded block scalars, huge directive-requiring tags, and huge local tags reject incrementally
/// under tight `max_output_bytes` without allocating full expanded strings (AY-176).
#[test]
fn value_writer_enforces_output_limit_incrementally_for_folded_and_tag_directives() {
    use crate::{Tag, TaggedValue};

    // 1. Folded scalar where `body.len() <= max_output_bytes`, but indentation + extra folded
    //    blank lines push total output over `max_output_bytes`: stops early with zero full copy.
    let body = "a\n".repeat(40); // 80 bytes; expanded folded lines with indent 2 take ~200 bytes.
    let limits = Limits::default().with_max_output_bytes(100);
    let mut buf = Vec::new();
    let mut out = Output::direct(&mut buf, &limits);
    let mut writer = ValueWriter::new(
        &mut out,
        &limits,
        EmitterOptions::new().with_scalar_style(Some(crate::ScalarStyle::Folded)),
        None,
        TagDirectives::default(),
    );
    writer.emit_root(&Value::String(body));
    let err = writer.finish().expect_err("folded expansion limit");
    assert_eq!(err.kind(), ErrorKind::Limit);
    assert_eq!(
        err.limit().map(crate::LimitExceeded::kind),
        Some(LimitKind::OutputBytes)
    );
    assert!(
        writer.block_scalar_lines <= 50,
        "folded writer kept emitting past limit: {} lines",
        writer.block_scalar_lines
    );

    // 2. Huge directive-requiring tag prefix fails inside `TagDirectives::scan` before allocating.
    let huge = if cfg!(miri) { 512 } else { 100_000 };
    let huge_directive_tag = Value::Tagged(Box::new(TaggedValue::new(
        Tag::new(format!("custom {}:", "p".repeat(huge)), "s"),
        Value::from("v"),
    )));
    let tight = crate::EmitOptions::new().with_limits(Limits::default().with_max_output_bytes(64));
    let err = crate::to_string_value_with(&huge_directive_tag, &tight)
        .expect_err("oversized directive tag");
    assert_eq!(err.kind(), ErrorKind::Limit);
    assert_eq!(
        err.limit().map(crate::LimitExceeded::kind),
        Some(LimitKind::OutputBytes)
    );

    // 3. Huge local tag streams directly through `write_tag` and stops at `max_output_bytes`.
    let huge_local_tag = Value::Tagged(Box::new(TaggedValue::new(
        Tag::local("x".repeat(huge)),
        Value::from("v"),
    )));
    let err =
        crate::to_string_value_with(&huge_local_tag, &tight).expect_err("oversized local tag");
    assert_eq!(err.kind(), ErrorKind::Limit);
    assert_eq!(
        err.limit().map(crate::LimitExceeded::kind),
        Some(LimitKind::OutputBytes)
    );
}

/// Exercises `TieredStack` promotion/pop/spill transitions (`8 -> 9 -> 7 -> 9 -> 128 -> 129 -> 128`),
/// multi-branch `TagDirectives::scan` DFS handle ordering (keys before values, left before right),
/// active `EmitterStyles` error unwinding on deep explicit keys (no recursive `Value::clone`), and
/// nested-tagged flow keys (no directive mutation before `nested_tag_error`) (AY-176).
#[test]
fn tiered_stack_transitions_and_multibranch_tag_scan_preserve_dfs_order() {
    use super::{EmitterOptions, EmitterStyles, NodeStyle, Output, TagDirectives, ValueWriter};
    use crate::{ErrorCode, ErrorKind, LimitKind, Limits, Mapping, PathSegment, Tag, TaggedValue};

    let mut small = [const { None }; super::STACK_SMALL];
    let mut medium = [const { None }; super::STACK_MEDIUM];
    let mut stack = super::TieredStack::<usize>::new(&mut small, &mut []);
    for i in 0..8 {
        stack.push(i);
        assert!(stack.take_pending().is_none());
    }
    stack.push(8);
    let pending = stack.take_pending().expect("pending at 8 -> 9");
    let (small_buf, small_len) = stack.small_parts();
    let mut promoted =
        super::TieredStack::from_promoted(small_buf, &mut medium, small_len, pending);
    // Pop back below 8, re-grow past 8 and past 128 into heap overflow, then drain back to 0.
    assert_eq!(promoted.pop(), Some(8));
    assert_eq!(promoted.pop(), Some(7));
    for i in 7..132 {
        promoted.push(i);
    }
    for expected in (0..132).rev() {
        assert_eq!(promoted.pop(), Some(expected));
    }
    assert_eq!(promoted.pop(), None);

    // Multi-branch tree with sibling sequences and mappings carrying tagged keys & tagged values
    // across depths 1, 10 (promoted tier), and 132 (heap-overflow tier).
    let wrap_seq = |depth: usize, mut inner: Value| -> Value {
        for _ in 0..depth {
            inner = Value::Sequence(vec![inner]);
        }
        inner
    };
    let mut inner_map = Mapping::new();
    inner_map.insert(
        Value::Tagged(Box::new(TaggedValue::new(
            Tag::new("branch key:", "k"),
            Value::from("key"),
        ))),
        Value::Tagged(Box::new(TaggedValue::new(
            Tag::new("branch val:", "v"),
            Value::from("val"),
        ))),
    );
    let tree = Value::Sequence(vec![
        wrap_seq(
            10,
            Value::Tagged(Box::new(TaggedValue::new(
                Tag::new("first branch:", "a"),
                Value::from(1_u64),
            ))),
        ),
        wrap_seq(130, Value::Mapping(inner_map)),
        wrap_seq(
            10,
            Value::Tagged(Box::new(TaggedValue::new(
                Tag::new("last branch:", "z"),
                Value::from(2_u64),
            ))),
        ),
    ]);
    let limits = Limits::default().with_max_depth(256);
    let mut buf = Vec::new();
    let mut out = Output::direct(&mut buf, &limits);
    let mut directives = TagDirectives::default();
    directives
        .scan(&tree, limits.max_depth, &out)
        .expect("scan");
    directives.write_lines(&mut out).expect("write directives");
    out.push_str("---\n").expect("marker");
    let mut writer = ValueWriter::new(
        &mut out,
        &limits,
        EmitterOptions::new().with_flow(true),
        None,
        directives,
    );
    writer.emit_root(&tree);
    writer.finish().expect("finish");
    out.finish().expect("out finish");
    let text = String::from_utf8(buf).expect("utf8");
    assert!(
        text.starts_with(
            "%TAG !t0! first%20branch:\n\
             %TAG !t1! branch%20key:\n\
             %TAG !t2! branch%20val:\n\
             %TAG !t3! last%20branch:\n\
             ---\n"
        ),
        "unexpected DFS directive order:\n{text}"
    );
    drop_value_iteratively(tree);

    // Nested-tagged flow key fails with `UnsupportedTag` without mutating directives.
    let mut nested_tag_map = Mapping::new();
    nested_tag_map.insert(
        Value::Tagged(Box::new(TaggedValue::new(
            Tag::new("nested outer:", "a"),
            Value::Tagged(Box::new(TaggedValue::new(
                Tag::new("nested inner:", "b"),
                Value::from("x"),
            ))),
        ))),
        Value::from(1_u64),
    );
    let nested_tag_val = Value::Mapping(nested_tag_map);
    let err =
        crate::to_string_value_with(&nested_tag_val, &crate::EmitOptions::new().with_flow(true))
            .expect_err("nested tag flow key");
    assert_eq!(err.code(), ErrorCode::UnsupportedTag);

    // Block mapping with active `EmitterStyles` and deep explicit collection key fails cleanly on
    // `LimitKind::Depth` on a 512 KiB stack without running recursive `Value::clone` on error.
    let handle = std::thread::Builder::new()
        .stack_size(512 << 10)
        .spawn(|| {
            let mut deep_key = Value::from(1_u64);
            for _ in 0..512 {
                deep_key = Value::Sequence(vec![deep_key]);
            }
            let mut root_map = Mapping::new();
            root_map.insert(deep_key, Value::from("v"));
            let root = Value::Mapping(root_map);
            let mut styles = EmitterStyles::new();
            styles.set(
                vec![PathSegment::key("other")],
                NodeStyle::new().with_scalar_style(Some(crate::ScalarStyle::DoubleQuoted)),
            );
            let tight = Limits::default().with_max_depth(64);
            let mut out_buf = Vec::new();
            let mut out = Output::direct(&mut out_buf, &tight);
            let mut writer = ValueWriter::new(
                &mut out,
                &tight,
                EmitterOptions::new(),
                Some(&styles),
                TagDirectives::default(),
            );
            writer.emit_root(&root);
            let err = writer.finish().expect_err("depth limit on deep key");
            assert_eq!(err.kind(), ErrorKind::Limit);
            assert_eq!(
                err.limit().map(crate::LimitExceeded::kind),
                Some(LimitKind::Depth)
            );
            drop_value_iteratively(root);
        })
        .expect("spawn 512 KiB thread");
    handle.join().expect("deep key error path succeeded");
}
