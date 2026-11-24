#![forbid(unsafe_code)]

//! Shared structure-aware generators and deterministic harness logic for the
//! `ayaml-fuzz` workspace (AY-132).
//!
//! Keeping the target bodies in this library lets `cargo test` execute every
//! harness deterministically on seed inputs without linking libFuzzer.

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::io;

use arbitrary::{Arbitrary, Unstructured};
use ayaml::{
    ColumnEncoding, EmitOptions, Error, ErrorKind, Limits, LineIndex, MAX_DEPTH_CAP, Mapping,
    ParseOptions, PathSegment, Position, PositionError, SpannedNode, SpannedTree, Value,
};

pub mod arbitrary_yaml;

use arbitrary_yaml::{FuzzCanary, FuzzDoc, FuzzEventStream, FuzzStream, FuzzValue};

/// Structured deserialization target exercising field, sequence, map, and unknown-field errors.
#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    dead_code,
    reason = "fields exercise typed Serde matching and mismatch paths"
)]
struct TypedTarget {
    a: Option<u32>,
    b: Option<Vec<String>>,
    name: Option<String>,
    count: Option<i64>,
    nested: Option<BTreeMap<String, i32>>,
}

/// Externally-tagged enum target for tree-vs-events equivalence fuzzing.
#[derive(Debug, PartialEq, serde::Deserialize)]
#[allow(dead_code, reason = "variants exercise Serde enum decoding paths")]
enum TypedEnum {
    Alpha,
    Local(Value),
    Nested {
        a: Option<u32>,
        name: Option<String>,
    },
}

/// Strict config target used by `Error::masked()` fuzzing.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code, reason = "deserialization target for unknown-field errors")]
struct StrictConfig {
    allowed: u32,
}

/// Strict enum target used by `Error::masked()` fuzzing.
#[derive(Debug, serde::Deserialize)]
#[allow(
    dead_code,
    reason = "deserialization target for unknown-variant errors"
)]
enum StrictChoice {
    Alpha,
    Beta,
}

#[derive(Debug)]
struct FailingWriter {
    message: String,
}

impl io::Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other(self.message.clone()))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Target 1 (`structured`): structure-aware YAML parsing and round-trip oracle.
///
/// # Panics
///
/// Panics if a valid emitted document stream fails to reparse identically or disagrees with the
/// document cursor.
pub fn run_structured(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let Ok(stream) = FuzzStream::arbitrary(&mut u) else {
        return;
    };
    let rendered = stream.render();
    let limits = Limits::default().with_max_documents(16);
    let parse_opts = ParseOptions::new().with_limits(limits.clone());
    let emit_opts = EmitOptions::new().with_limits(limits);

    let _unused = ayaml::parse_value_with(&rendered, &parse_opts);
    let _unused = ayaml::from_str_borrowed::<Value>(&rendered);
    let _unused = ayaml::compat::from_str::<Value>(&rendered);

    if let Ok(docs) = ayaml::parse_documents_with(&rendered, &parse_opts) {
        if let Ok(emitted) = ayaml::to_string_documents_with(&docs, &emit_opts) {
            let reparsed = ayaml::parse_documents_with(&emitted, &parse_opts)
                .expect("structured value emission must reparse");
            assert_eq!(reparsed, docs, "structured value round-trip mismatch");
        }
        if let Ok(events) = ayaml::parse_events_with(&rendered, &parse_opts)
            .and_then(Iterator::collect::<Result<Vec<_>, _>>)
            && let Ok(ev_emitted) = ayaml::to_string_events_with(&events, &emit_opts)
        {
            let ev_reparsed = ayaml::parse_documents_with(&ev_emitted, &parse_opts)
                .expect("structured event emission must reparse");
            assert_eq!(ev_reparsed, docs, "structured event round-trip mismatch");
        }
        let mut cursor = ayaml::Deserializer::from_str_with(&rendered, &parse_opts)
            .expect("cursor parses valid stream");
        for doc in &docs {
            let via_value = ayaml::from_value::<Value>(doc.clone());
            let via_cursor = cursor.next_borrowed::<Value>();
            match (via_value, via_cursor) {
                (Ok(expected), Ok(Some(actual))) => {
                    assert_eq!(actual, expected, "cursor mismatch on structured stream");
                }
                (Err(e1), Err(e2)) => {
                    assert_eq!(e1.kind(), e2.kind());
                    assert_eq!(e1.code(), e2.code());
                }
                (expected, actual) => {
                    panic!(
                        "cursor disagreed with from_value: expected={expected:?} actual={actual:?}"
                    );
                }
            }
        }
        assert_eq!(
            cursor
                .next_borrowed::<Value>()
                .expect("cursor EOF after structured stream"),
            None
        );
    }
}

fn check_span_bounds(input: &str, span: ayaml::Span) {
    assert!(span.start() <= span.end(), "inverted span {span:?}");
    assert!(span.end() <= input.len(), "span {span:?} out of bounds");
    assert!(
        input.is_char_boundary(span.start()) && input.is_char_boundary(span.end()),
        "span {span:?} splits UTF-8 boundary"
    );
}

fn check_spanned_tree(
    input: &str,
    tree: &SpannedTree<'_>,
    value: &Value,
    path: &mut Vec<PathSegment>,
    parent_span: Option<ayaml::Span>,
) {
    let Some(node) = tree.lookup(path) else {
        return;
    };
    let span = node.span();
    check_span_bounds(input, span);
    assert_eq!(tree.span_at(path), Some(span));
    assert_eq!(tree.entry_span_at(path), node.entry_span());
    assert_eq!(tree.scalar_text_at(path), node.scalar_text());

    if let Some(parent) = parent_span
        && !span.is_empty()
        && !parent.is_empty()
    {
        assert!(
            parent.start() <= span.start() && span.end() <= parent.end(),
            "child span {span:?} outside parent {parent:?} at {path:?}"
        );
    }

    if let Some(entry_span) = node.entry_span() {
        check_span_bounds(input, entry_span);
        assert!(
            entry_span.start() <= span.start() && span.end() <= entry_span.end(),
            "value span {span:?} outside entry_span {entry_span:?} at {path:?}"
        );
        let edited = format!(
            "{}{}",
            &input[..entry_span.start()],
            &input[entry_span.end()..]
        );
        let _reparsed = ayaml::parse_value(&edited).expect("deleting entry_span must stay valid");
    }

    if node.is_alias() {
        assert!(node.scalar_text().is_none());
        assert!(input[span.start()..span.end()].starts_with('*'));
        return;
    }

    match value.untag_ref() {
        Value::Sequence(items) => {
            assert!(node.is_sequence());
            for (idx, item) in items.iter().enumerate() {
                path.push(PathSegment::Index(idx));
                check_spanned_tree(input, tree, item, path, Some(span));
                path.pop();
            }
        }
        Value::Mapping(mapping) => {
            assert!(node.is_mapping());
            for (key, item) in mapping {
                path.push(PathSegment::Key(key.clone()));
                check_spanned_tree(input, tree, item, path, Some(span));
                path.pop();
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            check_scalar_node(input, node);
        }
        Value::Tagged(_) => unreachable!("untagged above"),
    }
}

fn check_scalar_node(input: &str, node: &SpannedNode<'_>) {
    assert!(node.is_scalar());
    let span = node.span();
    assert_eq!(node.scalar_text(), Some(&input[span.start()..span.end()]));
}

fn check_line_index(input: &str) {
    let index = LineIndex::new(input);
    let bytes = input.as_bytes();
    let first_col = if input.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    };
    let encodings = [
        ColumnEncoding::Utf8,
        ColumnEncoding::Scalar,
        ColumnEncoding::Utf16,
    ];
    for encoding in encodings {
        assert_eq!(
            index.to_position(input.len() + 1, encoding),
            Err(PositionError::OutOfBounds)
        );
        assert_eq!(
            index.to_offset(Position::new(index.line_count(), 0), encoding),
            Err(PositionError::OutOfBounds)
        );
        let mut prev: Option<Position> = None;
        for offset in 0..=input.len().min(512) {
            let inside_crlf = offset > 0
                && offset < input.len()
                && bytes[offset - 1] == b'\r'
                && bytes[offset] == b'\n';
            if inside_crlf {
                assert_eq!(
                    index.to_position(offset, encoding),
                    Err(PositionError::InsideCrLf)
                );
                continue;
            }
            if encoding != ColumnEncoding::Utf8
                && offset > first_col
                && !input.is_char_boundary(offset)
            {
                assert_eq!(
                    index.to_position(offset, encoding),
                    Err(PositionError::InvalidUtf8Boundary)
                );
                continue;
            }
            let pos = index
                .to_position(offset, encoding)
                .expect("valid offset must map to position");
            if offset <= first_col {
                assert_eq!(pos, Position::new(0, 0));
                assert_eq!(index.to_offset(pos, encoding), Ok(first_col));
            } else {
                assert_eq!(index.to_offset(pos, encoding), Ok(offset));
                if encoding == ColumnEncoding::Utf16
                    && offset < input.len()
                    && let Some(ch) = input[offset..].chars().next()
                    && ch.len_utf16() == 2
                {
                    assert_eq!(
                        index.to_offset(Position::new(pos.line, pos.column + 1), encoding),
                        Err(PositionError::SplitSurrogate)
                    );
                }
                if let Some(prev_pos) = prev {
                    assert!((prev_pos.line, prev_pos.column) < (pos.line, pos.column));
                    if prev_pos.line < pos.line {
                        assert_eq!(
                            index.to_offset(
                                Position::new(prev_pos.line, prev_pos.column + 1),
                                encoding
                            ),
                            Err(PositionError::OutOfBounds)
                        );
                    }
                }
                prev = Some(pos);
            }
        }
    }
}

/// Target 2 (`spanned`): `SpannedTree` span integrity and `LineIndex` bijectivity invariants.
///
/// # Panics
///
/// Panics if any `SpannedTree` span, `entry_span` deletion, or `LineIndex` round-trip invariant
/// is violated.
pub fn run_spanned(data: &[u8]) {
    if let Ok(raw) = std::str::from_utf8(data) {
        check_line_index(raw);
        let _unused = SpannedTree::parse(raw);
        let _unused = SpannedTree::parse_compat(raw);
    }

    let mut u = Unstructured::new(data);
    let Ok(doc) = FuzzDoc::arbitrary(&mut u) else {
        return;
    };
    let input = doc.render();
    check_line_index(&input);

    let _unused = SpannedTree::parse_compat(&input);
    if let (Ok(tree), Ok(value)) = (SpannedTree::parse(&input), ayaml::parse_value(&input)) {
        assert_eq!(tree.documents().len(), 1);
        let mut path = Vec::new();
        check_spanned_tree(&input, &tree, &value, &mut path, None);
    }
}

/// Target 3 (`merge_keys`): `Value::apply_merge_keys` and `apply_merge_keys_with_limits` safety,
/// limit monotonicity, and idempotence.
///
/// # Panics
///
/// Panics if `apply_merge_keys_with_limits` violates limit monotonicity or idempotence.
pub fn run_merge_keys(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let depth_lo: usize = u.int_in_range(0..=16).unwrap_or(2);
    let depth_hi: usize = u
        .int_in_range(depth_lo..=MAX_DEPTH_CAP.min(32))
        .unwrap_or(16);
    let entries_lo: usize = u.int_in_range(0..=16).unwrap_or(2);
    let entries_hi: usize = u.int_in_range(entries_lo..=64).unwrap_or(32);

    let limits_lo = Limits::default()
        .with_max_depth(depth_lo)
        .with_max_collection_entries(entries_lo);
    let limits_hi = Limits::default()
        .with_max_depth(depth_hi)
        .with_max_collection_entries(entries_hi);

    let check_value = |base: &Value| {
        let mut v_lo = base.clone();
        let mut v_hi = base.clone();
        let res_lo = v_lo.apply_merge_keys_with_limits(&limits_lo);
        let res_hi = v_hi.apply_merge_keys_with_limits(&limits_hi);
        match (res_lo, res_hi) {
            (Ok(()), Ok(())) => {
                assert_eq!(v_lo, v_hi, "raising merge limits changed merged value");
                // Idempotence: applying merge keys again to a merged value leaves it unchanged.
                let mut again = v_hi.clone();
                again
                    .apply_merge_keys_with_limits(&limits_hi)
                    .expect("second merge on already-merged value must succeed");
                assert_eq!(again, v_hi, "apply_merge_keys must be idempotent");
            }
            (Ok(()), Err(err)) => {
                panic!("tighter merge limits succeeded while looser limits failed: {err:?}");
            }
            (Err(err_lo), Ok(())) => {
                assert_eq!(
                    err_lo.kind(),
                    ErrorKind::Limit,
                    "raising merge limits turned a non-limit error into Ok: {err_lo:?}"
                );
            }
            (Err(_), Err(_)) => {}
        }
        let mut v_default = base.clone();
        let _unused = v_default.apply_merge_keys();
    };

    if let Ok(fuzz_val) = FuzzValue::arbitrary(&mut u) {
        check_value(&fuzz_val.value);
    }
    if let Ok(doc) = FuzzDoc::arbitrary(&mut u) {
        let rendered = doc.render();
        if let Ok(parsed) = ayaml::parse_value(&rendered) {
            check_value(&parsed);
        }
    }
}

fn assert_masked_clean(error: &Error, secret: &str) {
    let masked = error.masked();
    assert_eq!(masked.kind(), error.kind());
    assert_eq!(masked.code(), error.code());
    assert_eq!(masked.span(), error.span());
    assert_eq!(masked.location(), error.location());
    assert_eq!(masked.limit(), error.limit());
    assert!(masked.source().is_none(), "masked error exposed source()");
    let display = masked.to_string();
    let debug = format!("{masked:?}");
    assert!(
        !display.contains(secret),
        "masked Display leaked secret {secret:?}: {display:?}"
    );
    assert!(
        !debug.contains(secret),
        "masked Debug leaked secret {secret:?}: {debug:?}"
    );
}

/// Target 4 (`masked_errors`): `Error::masked()` canary non-leakage across failure classes.
///
/// # Panics
///
/// Panics if `Error::masked()` leaks a generated secret canary or alters error metadata.
pub fn run_masked_errors(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let Ok(case) = FuzzCanary::arbitrary(&mut u) else {
        return;
    };
    let s = &case.secret;
    let err: Option<Error> = match case.scenario % 20 {
        0 => ayaml::parse_value(&format!("k: \"{s}")).err(),
        1 => ayaml::parse_value(&format!("k: \"{s}\\q\"")).err(),
        2 => ayaml::parse_value(&format!("@{s}\n")).err(),
        3 => ayaml::parse_value(&format!("{{{s}: [1, 2}}\n")).err(),
        4 => ayaml::parse_value(&format!("a: *{s}\n")).err(),
        5 => ayaml::parse_value(&format!("{s}: 1\n{s}: 2\n")).err(),
        6 => ayaml::from_str::<BTreeMap<String, u32>>(&format!("{s}: {s}\n")).err(),
        7 => ayaml::from_str::<StrictConfig>(&format!("{s}: 1\n")).err(),
        8 => ayaml::from_str::<StrictChoice>(&format!("{s}\n")).err(),
        9 => ayaml::parse_value_with(
            &format!("k: {s}\n"),
            &ParseOptions::new().with_limits(Limits::default().with_max_scalar_bytes(4)),
        )
        .err(),
        10 => ayaml::parse_value_with(
            &format!("k: {s}\n"),
            &ParseOptions::new().with_limits(Limits::default().with_max_input_bytes(4)),
        )
        .err(),
        11 => ayaml::parse_value(&format!("m:\n  <<: {s}\n"))
            .ok()
            .and_then(|mut v| v.apply_merge_keys().err()),
        12 => ayaml::parse_value(&format!("%YAML {s}\n---\na\n")).err(),
        13 => ayaml::parse_value(&format!("!{s}!x 1\n")).err(),
        14 => ayaml::parse_value(&format!("!!int {s}\n")).err(),
        15 => {
            let mut bad = format!("k: {s}").into_bytes();
            bad.push(0xFF);
            ayaml::from_slice::<Value>(&bad).err()
        }
        16 => {
            let mut w = FailingWriter {
                message: format!("io err near {s}"),
            };
            ayaml::to_writer(&mut w, s).err()
        }
        17 => ayaml::compat::from_str::<StrictConfig>(&format!("{s}: {s}\n")).err(),
        18 => {
            let mut merged = Mapping::new();
            merged.insert(Value::from(format!("{s}_k1")), Value::from(s.clone()));
            merged.insert(Value::from(format!("{s}_k2")), Value::from(s.clone()));
            let mut outer = Mapping::new();
            outer.insert(Value::from("<<"), Value::Mapping(merged));
            let mut root = Value::Mapping(outer);
            root.apply_merge_keys_with_limits(&Limits::default().with_max_collection_entries(1))
                .err()
        }
        _ => ayaml::to_string_with(
            s,
            &EmitOptions::new().with_limits(Limits::default().with_max_output_bytes(4)),
        )
        .err(),
    };
    let error = err.unwrap_or_else(|| {
        panic!(
            "masked_errors scenario {} unexpectedly succeeded",
            case.scenario % 20
        )
    });
    assert_masked_clean(&error, s);
}

/// Target 5 (`event_emit`): arbitrary `Event` emission consistency and reparse soundness.
///
/// # Panics
///
/// Panics if event writers disagree or if emitted event YAML fails to reparse.
pub fn run_event_emit(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let Ok(stream) = FuzzEventStream::arbitrary(&mut u) else {
        return;
    };
    let limits = Limits::default().with_max_documents(16);
    let indent: usize = u.int_in_range(2..=4).unwrap_or(2);
    let emit_opts = EmitOptions::new()
        .with_limits(limits.clone())
        .with_indent(indent);
    let parse_opts = ParseOptions::new().with_limits(limits);

    let as_string = ayaml::to_string_events_with(&stream.events, &emit_opts);
    let mut writer_buf = Vec::new();
    let as_writer = ayaml::to_writer_events_with(&mut writer_buf, &stream.events, &emit_opts);

    match (as_string, as_writer) {
        (Ok(rendered), Ok(())) => {
            assert_eq!(writer_buf, rendered.as_bytes());
            // Anything the event writer accepts MUST parse back cleanly as events and stream
            // cleanly through `to_writer_event_stream_with`.
            let reparsed = ayaml::parse_events_with(&rendered, &parse_opts)
                .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                .unwrap_or_else(|err| {
                    panic!("emitted event YAML failed to reparse ({err}): {rendered:?}");
                });
            let event_stream =
                ayaml::parse_events_with(&rendered, &parse_opts).expect("reparse stream");
            let mut lazy_buf = Vec::new();
            ayaml::to_writer_event_stream_with(&mut lazy_buf, event_stream, &emit_opts)
                .unwrap_or_else(|err| {
                    panic!("lazy event-stream writer failed on reparsed YAML ({err}): {rendered:?}")
                });
            let second_collected = ayaml::to_string_events_with(&reparsed, &emit_opts)
                .expect("collected writer succeeds where lazy writer succeeds");
            assert_eq!(lazy_buf, second_collected.as_bytes());
        }
        (Err(e1), Err(e2)) => {
            assert_eq!(e1.kind(), e2.kind());
            assert_eq!(e1.code(), e2.code());
        }
        (a, b) => {
            panic!("event writers disagreed: string={a:?} writer={b:?}");
        }
    }
}

fn assert_tree_events_same<T: PartialEq + std::fmt::Debug>(
    tree: Result<T, Error>,
    events: Result<T, Error>,
    label: &str,
    input: &str,
) {
    match (tree, events) {
        (Ok(l), Ok(r)) => assert_eq!(l, r, "{label}: Ok mismatch on {input:?}"),
        (Err(l), Err(r)) => {
            assert_eq!(l.kind(), r.kind(), "{label}: kind on {input:?}");
            assert_eq!(l.code(), r.code(), "{label}: code on {input:?}");
            assert_eq!(l.span(), r.span(), "{label}: span on {input:?}");
            assert_eq!(l.location(), r.location(), "{label}: location on {input:?}");
            assert_eq!(l.limit(), r.limit(), "{label}: limit on {input:?}");
            assert_eq!(l.masked(), r.masked(), "{label}: masked on {input:?}");
        }
        (l, r) => panic!("{label}: tree={l:?} vs events={r:?} on {input:?}"),
    }
}

/// Target 6 (`typed_equiv`): structure-aware tree-versus-event-stream typed reader equivalence.
///
/// # Panics
///
/// Panics if `from_str_by_tree_with` and `from_str_by_events_with` disagree on `Ok` values or
/// error metadata.
pub fn run_typed_equiv(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let Ok(doc) = FuzzDoc::arbitrary(&mut u) else {
        return;
    };
    let compat = bool::arbitrary(&mut u).unwrap_or(false);
    let drop_tags = compat && bool::arbitrary(&mut u).unwrap_or(false);
    let use_tight = u.ratio(1, 3).unwrap_or(false);
    let limits = if use_tight {
        let ceiling: usize = u.int_in_range(0..=32).unwrap_or(4);
        match u.int_in_range(0_u8..=5).unwrap_or(0) {
            0 => Limits::default().with_max_depth(ceiling.min(MAX_DEPTH_CAP)),
            1 => Limits::default().with_max_events(ceiling),
            2 => Limits::default().with_max_collection_entries(ceiling),
            3 => Limits::default().with_max_scalar_bytes(ceiling),
            4 => Limits::default().with_max_anchors(ceiling),
            _ => Limits::default().with_max_alias_dereferences(ceiling),
        }
    } else {
        Limits::default()
    };

    let input = doc.render();
    assert_tree_events_same(
        ayaml::internals::from_str_by_tree_with::<Value>(&input, &limits, compat, drop_tags),
        ayaml::internals::from_str_by_events_with::<Value>(&input, &limits, compat, drop_tags),
        "Value",
        &input,
    );
    assert_tree_events_same(
        ayaml::internals::from_str_by_tree_with::<TypedTarget>(&input, &limits, compat, drop_tags),
        ayaml::internals::from_str_by_events_with::<TypedTarget>(
            &input, &limits, compat, drop_tags,
        ),
        "TypedTarget",
        &input,
    );
    assert_tree_events_same(
        ayaml::internals::from_str_by_tree_with::<serde_json::Value>(
            &input, &limits, compat, drop_tags,
        ),
        ayaml::internals::from_str_by_events_with::<serde_json::Value>(
            &input, &limits, compat, drop_tags,
        ),
        "serde_json::Value",
        &input,
    );
    assert_tree_events_same(
        ayaml::internals::from_str_by_tree_with::<TypedEnum>(&input, &limits, compat, drop_tags),
        ayaml::internals::from_str_by_events_with::<TypedEnum>(&input, &limits, compat, drop_tags),
        "TypedEnum",
        &input,
    );
    assert_tree_events_same(
        ayaml::internals::from_str_by_tree_with::<serde::de::IgnoredAny>(
            &input, &limits, compat, drop_tags,
        )
        .map(|_| ()),
        ayaml::internals::from_str_by_events_with::<serde::de::IgnoredAny>(
            &input, &limits, compat, drop_tags,
        )
        .map(|_| ()),
        "IgnoredAny",
        &input,
    );
}

/// Target 7 (`edit`): `ayaml_edit::replace_scalar` and `remove_entry` never panic and every
/// returned `VerifiedEdit` parses cleanly and matches `apply_edits`.
///
/// # Panics
///
/// Panics if a returned `VerifiedEdit` fails to re-parse or disagrees with `apply_edits`.
pub fn run_edit(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let Ok(doc) = FuzzDoc::arbitrary(&mut u) else {
        return;
    };
    let input = doc.render();
    let Ok(original) = ayaml::parse_value(&input) else {
        return;
    };
    let Some(mapping) = original.as_mapping() else {
        return;
    };
    for (k, _) in mapping.iter().take(4) {
        let path = [PathSegment::Key(k.clone())];
        if let Ok(verified) = ayaml_edit::replace_scalar(&input, &path, &Value::from("edited")) {
            let applied = ayaml_edit::apply_edits(&input, verified.edits())
                .expect("verified edits must apply cleanly");
            assert_eq!(applied, verified.output());
            let reparsed = ayaml::parse_value(verified.output())
                .expect("verified replace_scalar output must parse");
            assert_eq!(
                reparsed.as_mapping().and_then(|m| m.get(k)),
                Some(&Value::from("edited"))
            );
        }
        if let Ok(verified) = ayaml_edit::remove_entry(&input, &path) {
            let applied = ayaml_edit::apply_edits(&input, verified.edits())
                .expect("verified edits must apply cleanly");
            assert_eq!(applied, verified.output());
            let reparsed = ayaml::parse_value(verified.output())
                .expect("verified remove_entry output must parse");
            assert!(reparsed.as_mapping().and_then(|m| m.get(k)).is_none());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        run_event_emit, run_masked_errors, run_merge_keys, run_spanned, run_structured,
        run_typed_equiv,
    };

    fn deterministic_seeds() -> Vec<Vec<u8>> {
        let mut seeds = vec![
            Vec::new(),
            b"a: 1\nb: [true, false, null]\n".to_vec(),
            b"\xef\xbb\xbfa: &x\n  k: 1\nb:\n  <<: *x\n  m: \"caf\xc3\xa9\"\r\n".to_vec(),
            b"---\n!local [1, 2, 3]\n...\n---\n{a: 1, b: \"x\"}\n...\n".to_vec(),
        ];
        // Deterministic pseudo-random byte streams exercising every `Arbitrary` branch.
        let mut state: u64 = 0x132F_0220_CAFE_BABE;
        for len in [8_usize, 24, 48, 96, 160] {
            for _ in 0..8 {
                let mut buf = Vec::with_capacity(len);
                for _ in 0..len {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    buf.push((state & 0xFF) as u8);
                }
                seeds.push(buf);
            }
        }
        seeds
    }

    #[test]
    fn structured_harness_runs_on_seeds() {
        for seed in deterministic_seeds() {
            run_structured(&seed);
        }
    }

    #[test]
    fn spanned_harness_runs_on_seeds() {
        for seed in deterministic_seeds() {
            run_spanned(&seed);
        }
    }

    #[test]
    fn merge_keys_harness_runs_on_seeds() {
        for seed in deterministic_seeds() {
            run_merge_keys(&seed);
        }
    }

    #[test]
    fn masked_errors_harness_runs_on_seeds() {
        for seed in deterministic_seeds() {
            run_masked_errors(&seed);
        }
    }

    #[test]
    fn event_emit_harness_runs_on_seeds() {
        for seed in deterministic_seeds() {
            run_event_emit(&seed);
        }
    }

    #[test]
    fn typed_equiv_harness_runs_on_seeds() {
        for seed in deterministic_seeds() {
            run_typed_equiv(&seed);
        }
    }

    #[test]
    fn edit_harness_runs_on_seeds() {
        for seed in deterministic_seeds() {
            super::run_edit(&seed);
        }
    }
}
