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

use arbitrary_yaml::{
    FuzzCanary, FuzzDoc, FuzzEventStream, FuzzStream, FuzzValue, MASKED_ERROR_SCENARIOS,
};

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

/// Target 0 (`parse`): raw-byte parsing across every public entry point without panicking.
pub fn run_parse(data: &[u8]) {
    let limits = Limits::default().with_max_documents(16);
    let parse_opts = ParseOptions::new().with_limits(limits.clone());
    let merge_opts = ParseOptions::new()
        .with_limits(limits.clone())
        .with_merge_keys(true);

    // Raw-byte entry points must handle arbitrary (including non-UTF-8) slices safely.
    let _unused = ayaml::from_slice_with::<Value>(data, &parse_opts);
    let _unused = ayaml::from_slice_borrowed::<Value>(data);
    let _unused = ayaml::from_reader_with::<_, Value>(data, &parse_opts);
    let _unused = ayaml::from_reader_with::<_, Value>(data, &merge_opts);

    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let _unused = ayaml::parse_value_with(input, &parse_opts);
    let _unused = ayaml::parse_documents_with(input, &parse_opts);
    if let Ok(stream) = ayaml::parse_events_with(input, &parse_opts) {
        let _unused = stream.collect::<Result<Vec<_>, _>>();
    }
    let _unused = ayaml::parse_tape_with(input, &parse_opts);
    let _unused = ayaml::parse_tape_documents_with(input, &parse_opts);
    let _unused = ayaml::split_frontmatter_with(input, &parse_opts);
    let _unused = ayaml::parse_frontmatter_with::<Value>(input, &parse_opts);
    let _unused = ayaml::parse_frontmatter_value_with(input, &parse_opts);
    let _unused = ayaml::parse_value_with(input, &merge_opts);
    let _unused = ayaml::from_str_with::<Value>(input, &merge_opts);
    if let Ok(doc_de) = ayaml::DocumentDeserializer::from_str_with(input, &parse_opts) {
        let _unused: Result<ayaml::Spanned<Value>, _> = serde::Deserialize::deserialize(doc_de);
    }
    let compat_opts = ayaml::compat::ParseOptions::new().with_limits(limits);
    let _unused = ayaml::compat::from_str_with::<Value>(input, &compat_opts);
    if let Ok(stream) = ayaml::compat::parse_events_with(input, &compat_opts) {
        let _unused = stream.collect::<Result<Vec<_>, _>>();
    }
}

/// Flow and Serde emission consistency checks invoked by `fuzz/fuzz_targets/emit.rs` and unit tests.
///
/// # Panics
///
/// Panics if flow emission alters documents or if `to_string_with` / `to_writer_with` disagree or
/// emit unparseable YAML.
pub fn check_emit_flow_and_serde(
    input: &str,
    documents: &[Value],
    parse_options: &ParseOptions,
    emit_options: &EmitOptions,
) {
    let is_size_limit = |error: &Error| {
        error.kind() == ErrorKind::Limit
            && error.limit().is_some_and(|l| {
                matches!(
                    l.kind(),
                    ayaml::LimitKind::InputBytes | ayaml::LimitKind::OutputBytes
                )
            })
    };
    let flow_options = emit_options.clone().with_flow(true);
    let flow_rendered = match ayaml::to_string_documents_with(documents, &flow_options) {
        Ok(rendered) => rendered,
        Err(error) if is_size_limit(&error) => return,
        Err(error) => panic!("flow value emission failed: {error}; input={input:?}"),
    };
    let flow_decoded = match ayaml::parse_documents_with(&flow_rendered, parse_options) {
        Ok(decoded) => decoded,
        Err(error) if is_size_limit(&error) => return,
        Err(error) => {
            panic!(
                "flow value output does not parse: {error}; input={input:?} rendered={flow_rendered:?}"
            );
        }
    };
    assert_eq!(
        flow_decoded, documents,
        "flow value output changes the documents; input={input:?} rendered={flow_rendered:?}"
    );

    if let [single] = documents {
        for (label, opts) in [("serde-block", emit_options), ("serde-flow", &flow_options)] {
            let as_string = ayaml::to_string_with(single, opts);
            let mut buf = Vec::new();
            let as_writer = ayaml::to_writer_with(&mut buf, single, opts);
            match (as_string, as_writer) {
                (Ok(rendered), Ok(())) => {
                    assert_eq!(
                        buf,
                        rendered.as_bytes(),
                        "{label}: to_writer_with disagreed with to_string_with; input={input:?}"
                    );
                    let reparsed = match ayaml::parse_value_with(&rendered, parse_options) {
                        Ok(val) => val,
                        Err(error) if is_size_limit(&error) => continue,
                        Err(error) => {
                            panic!(
                                "{label} output does not parse: {error}; input={input:?} rendered={rendered:?}"
                            );
                        }
                    };
                    assert_eq!(
                        reparsed, *single,
                        "{label} output changed Value; input={input:?} rendered={rendered:?}"
                    );
                }
                (Err(e1), Err(e2)) => {
                    assert_eq!(e1.kind(), e2.kind(), "{label}: error kind mismatch");
                    assert_eq!(e1.code(), e2.code(), "{label}: error code mismatch");
                }
                (a, b) => {
                    panic!(
                        "{label}: to_string_with={a:?} vs to_writer_with={b:?}; input={input:?}"
                    );
                }
            }
        }
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
    let _unused = ayaml::from_reader_with::<_, Value>(rendered.as_bytes(), &parse_opts);
    let _unused = ayaml::compat::from_str::<Value>(&rendered);
    let _unused = ayaml::split_frontmatter_with(&rendered, &parse_opts);
    let _unused = ayaml::parse_frontmatter_value_with(&rendered, &parse_opts);

    if let Ok(docs) = ayaml::parse_documents_with(&rendered, &parse_opts) {
        let mut tape_buf = ayaml::ParserBuf::new();
        let tape_docs = ayaml::parse_tape_documents_with_in(&rendered, &parse_opts, &mut tape_buf)
            .expect("parse_tape_documents_with_in must match parse_documents_with");
        assert_eq!(
            tape_docs.to_values(),
            docs,
            "TapeDocuments mismatch on structured stream"
        );
        tape_docs.recycle_into(&mut tape_buf);
        if docs.len() <= 1 {
            let tape = ayaml::parse_tape_with_in(&rendered, &parse_opts, &mut tape_buf)
                .expect("parse_tape_with_in must match single-document stream");
            let want = docs.first().cloned().unwrap_or(Value::Null);
            assert_eq!(tape.to_value(), want, "Tape::to_value mismatch");
            let from_tape: Value = serde::Deserialize::deserialize(&tape)
                .expect("Value::deserialize(&tape) must match");
            assert_eq!(from_tape, want, "Tape Serde Value mismatch");
            tape.recycle_into(&mut tape_buf);
        }
        if docs.len() <= 1
            && let Ok(doc_de) = ayaml::DocumentDeserializer::from_str_with(&rendered, &parse_opts)
        {
            let via_doc_de: Result<ayaml::Spanned<Value>, _> =
                serde::Deserialize::deserialize(doc_de);
            if let Ok(spanned_val) = via_doc_de {
                assert!(
                    spanned_val.span().slice(&rendered).is_some(),
                    "Spanned<Value> span must lie on valid UTF-8 boundaries"
                );
            }
        }
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
        if let Err(err) = ayaml::parse_value(&edited) {
            panic!("deleting entry_span must stay valid: {err}");
        }
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
        assert!(
            tree.documents().len() <= 1,
            "single-document parse_value succeeded on {} documents",
            tree.documents().len()
        );
        if tree.documents().is_empty() {
            assert_eq!(value, Value::Null);
        } else {
            let mut path = Vec::new();
            check_spanned_tree(&input, &tree, &value, &mut path, None);
        }
    }

    // Exercise opt-in merge-key expansion on SpannedTree: entry_span must always stay None.
    let merge_opts = ParseOptions::new().with_merge_keys(true);
    if let Ok(merged_tree) = SpannedTree::parse_with(&input, &merge_opts) {
        for doc in merged_tree.documents() {
            check_span_bounds(&input, doc.span());
            assert!(
                doc.entry_span().is_none(),
                "merged SpannedTree must suppress entry_span"
            );
        }
    }
}

fn check_merge_api_consistency(rendered: &str, lim: &Limits) {
    let merge_opts = ParseOptions::new()
        .with_limits(lim.clone())
        .with_merge_keys(true);
    let _unused = ayaml::parse_events_with(rendered, &merge_opts)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>);
    let as_value = ayaml::parse_value_with(rendered, &merge_opts);
    let as_tape = ayaml::parse_tape_with(rendered, &merge_opts).map(|t| t.to_value());
    let as_typed = ayaml::from_str_with::<Value>(rendered, &merge_opts);
    let as_reader = ayaml::from_reader_with::<_, Value>(rendered.as_bytes(), &merge_opts);
    let as_doc_de: Result<Value, Error> =
        ayaml::DocumentDeserializer::from_str_with(rendered, &merge_opts)
            .and_then(serde::Deserialize::deserialize);

    if let Ok(ref expected) = as_value {
        let tape_val = as_tape.expect("parse_tape_with(merge_keys) must match parse_value_with");
        assert_eq!(&tape_val, expected, "merge_keys tape vs value mismatch");
        match &as_typed {
            Ok(typed_val) => {
                assert_eq!(typed_val, expected, "merge_keys from_str vs value mismatch");
            }
            // Typed maps intentionally reject scalar keys whose rendered text collides.
            Err(err) if err.kind() == ErrorKind::DuplicateKey => {}
            Err(err) => {
                panic!("merge_keys from_str_with failed while parse_value_with succeeded: {err:?}")
            }
        }
    }

    match (&as_typed, &as_reader) {
        (Ok(a), Ok(b)) => assert_eq!(a, b, "merge_keys from_str vs from_reader mismatch"),
        (Err(e1), Err(e2)) => {
            assert_eq!(e1.kind(), e2.kind());
            assert_eq!(e1.code(), e2.code());
        }
        (a, b) => panic!("merge_keys from_str={a:?} vs from_reader={b:?}"),
    }

    match (&as_typed, &as_doc_de) {
        (Ok(a), Ok(b)) => assert_eq!(a, b, "merge_keys from_str vs DocumentDeserializer mismatch"),
        (Err(e1), Err(e2)) => {
            assert_eq!(e1.kind(), e2.kind());
            assert_eq!(e1.code(), e2.code());
        }
        (a, b) => panic!("merge_keys from_str={a:?} vs DocumentDeserializer={b:?}"),
    }

    if let Ok(mut cursor) = ayaml::Deserializer::from_str_with(rendered, &merge_opts) {
        let first = cursor.next_borrowed::<Value>();
        if let Ok(ref expected) = as_typed {
            match first.expect("cursor first doc must match from_str_with") {
                Some(doc) => assert_eq!(&doc, expected, "merge_keys cursor first doc mismatch"),
                // An empty / comment-only stream yields 0 documents on the stream cursor and
                // `Value::Null` on single-document `from_str_with`.
                None => assert_eq!(
                    expected,
                    &Value::Null,
                    "empty cursor stream disagreed with non-null from_str_with"
                ),
            }
            assert_eq!(
                cursor
                    .next_borrowed::<Value>()
                    .expect("cursor EOF after single merged doc"),
                None
            );
        } else {
            while matches!(cursor.next_borrowed::<Value>(), Ok(Some(_))) {}
        }
    }
}

/// Target 3 (`merge_keys`): `Value::apply_merge_keys` and `apply_merge_keys_with_limits` safety,
/// limit monotonicity, raw-byte safety, cross-API consistency, and idempotence.
///
/// # Panics
///
/// Panics if `apply_merge_keys_with_limits` violates limit monotonicity or idempotence, or if
/// opt-in `with_merge_keys(true)` deserialization APIs disagree.
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

    // Feed raw bytes directly (when valid UTF-8) through both Value-level and event-level merges.
    if let Ok(raw) = std::str::from_utf8(data) {
        if let Ok(parsed) = ayaml::parse_value(raw) {
            check_value(&parsed);
        }
        for lim in [&limits_lo, &limits_hi] {
            check_merge_api_consistency(raw, lim);
        }
    }

    if let Ok(fuzz_val) = FuzzValue::arbitrary(&mut u) {
        check_value(&fuzz_val.value);
    }
    if let Ok(doc) = FuzzDoc::arbitrary(&mut u) {
        let rendered = doc.render();
        if let Ok(parsed) = ayaml::parse_value(&rendered) {
            check_value(&parsed);
        }
        for lim in [&limits_lo, &limits_hi] {
            check_merge_api_consistency(&rendered, lim);
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

/// Execute one `Error::masked()` canary scenario in `0..MASKED_ERROR_SCENARIOS`.
///
/// # Panics
///
/// Panics if the scenario unexpectedly succeeds or if `Error::masked()` leaks `secret`.
pub fn run_masked_error_scenario(secret: &str, scenario: u8) {
    let s = secret;
    let idx = scenario % MASKED_ERROR_SCENARIOS;
    let err: Option<Error> = match idx {
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
            merged.insert(Value::from(format!("{s}_k1")), Value::from(s.to_owned()));
            merged.insert(Value::from(format!("{s}_k2")), Value::from(s.to_owned()));
            let mut outer = Mapping::new();
            outer.insert(Value::from("<<"), Value::Mapping(merged));
            let mut root = Value::Mapping(outer);
            root.apply_merge_keys_with_limits(&Limits::default().with_max_collection_entries(1))
                .err()
        }
        19 => ayaml::to_string_with(
            s,
            &EmitOptions::new().with_limits(Limits::default().with_max_output_bytes(4)),
        )
        .err(),
        20 => {
            let mut bad = format!("k: {s}").into_bytes();
            bad.push(0xFF);
            ayaml::from_reader::<_, Value>(bad.as_slice()).err()
        }
        21 => ayaml::parse_frontmatter::<StrictConfig>(&format!("---\n{s}: 1\n---\nbody\n")).err(),
        22 => ayaml::DocumentDeserializer::from_str(&format!("{s}: {s}\n"))
            .and_then(<StrictConfig as serde::Deserialize>::deserialize)
            .err(),
        23 => ayaml::parse_value_with(
            &format!("m:\n  <<: {s}\n"),
            &ParseOptions::new().with_merge_keys(true),
        )
        .err(),
        24 => ayaml::parse_tape(&format!("{s}: 1\n{s}: 2\n")).err(),
        25 => ayaml::parse_tape(&format!("{s}: {s}\n"))
            .and_then(|t| <StrictConfig as serde::Deserialize>::deserialize(&t))
            .err(),
        _ => ayaml::from_value::<ayaml::Spanned<String>>(Value::from(s.to_owned())).err(),
    };
    let error =
        err.unwrap_or_else(|| panic!("masked_errors scenario {idx} unexpectedly succeeded"));
    assert_masked_clean(&error, s);
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
    run_masked_error_scenario(&case.secret, case.scenario);
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
    if !compat {
        let mut buf = ayaml::ParserBuf::new();
        let opts = ParseOptions::new().with_limits(limits.clone());
        assert_tree_events_same(
            ayaml::internals::from_str_by_tree_with::<Value>(&input, &limits, false, false),
            ayaml::from_str_with_in::<Value>(&input, &opts, &mut buf),
            "ParserBuf::from_str_with_in Value",
            &input,
        );
        assert_tree_events_same(
            ayaml::parse_value_with(&input, &opts),
            ayaml::parse_tape_with_in(&input, &opts, &mut buf).map(|t| {
                let v = t.to_value();
                t.recycle_into(&mut buf);
                v
            }),
            "Tape vs parse_value",
            &input,
        );
        // Spanned<T> deserialization must agree with plain typed deserialization and produce
        // in-bounds UTF-8 spans across both `from_str_with` and `DocumentDeserializer`.
        let plain_val = ayaml::from_str_with::<Value>(&input, &opts);
        let spanned_val = ayaml::from_str_with::<ayaml::Spanned<Value>>(&input, &opts);
        let doc_de_spanned: Result<ayaml::Spanned<Value>, Error> =
            ayaml::DocumentDeserializer::from_str_with(&input, &opts)
                .and_then(serde::Deserialize::deserialize);
        if let Ok(ref sv) = spanned_val {
            check_span_bounds(&input, sv.span());
        }
        assert_tree_events_same(
            plain_val,
            spanned_val
                .as_ref()
                .map(|s| s.as_ref().clone())
                .map_err(Clone::clone),
            "Spanned<Value> vs Value",
            &input,
        );
        assert_tree_events_same(
            spanned_val,
            doc_de_spanned,
            "Spanned<Value> from_str vs DocumentDeserializer",
            &input,
        );
        let plain_typed = ayaml::from_str_with::<TypedTarget>(&input, &opts);
        let spanned_typed = ayaml::from_str_with::<ayaml::Spanned<TypedTarget>>(&input, &opts);
        if let Ok(ref st) = spanned_typed {
            check_span_bounds(&input, st.span());
        }
        assert_tree_events_same(
            plain_typed,
            spanned_typed.map(ayaml::Spanned::into_inner),
            "Spanned<TypedTarget> vs TypedTarget",
            &input,
        );

        // Nested streaming `Spanned<T>` values and `Spanned<String>` map keys (without wrapping
        // the root in `Spanned<...>`, which would fall back to `NodeDeserializer`).
        let map_tree = ayaml::internals::from_str_by_tree_with::<SpannedEntries>(
            &input, &limits, false, false,
        );
        let map_events = ayaml::internals::from_str_by_events_with::<SpannedEntries>(
            &input, &limits, false, false,
        );
        let map_doc_de: Result<SpannedEntries, Error> =
            ayaml::DocumentDeserializer::from_str_with(&input, &opts)
                .and_then(serde::Deserialize::deserialize);
        if let Ok(ref m) = map_events {
            for (k, v) in &m.0 {
                check_span_bounds(&input, k.span());
                check_span_bounds(&input, v.span());
            }
        }
        assert_tree_events_same(
            map_tree,
            map_events.clone(),
            "SpannedEntries tree vs events",
            &input,
        );
        assert_tree_events_same(
            map_events,
            map_doc_de,
            "SpannedEntries events vs DocumentDeserializer",
            &input,
        );

        let nested_tree = ayaml::internals::from_str_by_tree_with::<NestedSpannedStruct>(
            &input, &limits, false, false,
        );
        let nested_events = ayaml::internals::from_str_by_events_with::<NestedSpannedStruct>(
            &input, &limits, false, false,
        );
        if let Ok(ref ns) = nested_events {
            if let Some(ref a) = ns.a {
                check_span_bounds(&input, a.span());
            }
            if let Some(ref b) = ns.b {
                check_span_bounds(&input, b.span());
            }
            for item in &ns.items {
                check_span_bounds(&input, item.span());
            }
        }
        assert_tree_events_same(
            nested_tree,
            nested_events,
            "NestedSpannedStruct tree vs events",
            &input,
        );
    }
}

#[derive(Debug, Clone, PartialEq)]
struct SpannedEntries(Vec<(ayaml::Spanned<String>, ayaml::Spanned<Value>)>);

impl<'de> serde::Deserialize<'de> for SpannedEntries {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct EntriesVisitor;
        impl<'de> serde::de::Visitor<'de> for EntriesVisitor {
            type Value = SpannedEntries;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a mapping with spanned string keys and spanned values")
            }
            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut entries = Vec::new();
                while let Some((k, v)) =
                    map.next_entry::<ayaml::Spanned<String>, ayaml::Spanned<Value>>()?
                {
                    entries.push((k, v));
                }
                Ok(SpannedEntries(entries))
            }
        }
        deserializer.deserialize_map(EntriesVisitor)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
struct NestedSpannedStruct {
    #[serde(default)]
    a: Option<ayaml::Spanned<Value>>,
    #[serde(default)]
    b: Option<ayaml::Spanned<String>>,
    #[serde(default)]
    items: Vec<ayaml::Spanned<Value>>,
}

fn collect_edit_paths(
    value: &Value,
    prefix: &mut Vec<PathSegment>,
    out: &mut Vec<Vec<PathSegment>>,
) {
    if out.len() >= 8 {
        return;
    }
    match value.untag_ref() {
        Value::Mapping(mapping) => {
            for (k, v) in mapping.iter().take(4) {
                prefix.push(PathSegment::Key(k.clone()));
                out.push(prefix.clone());
                collect_edit_paths(v, prefix, out);
                prefix.pop();
                if out.len() >= 8 {
                    break;
                }
            }
        }
        Value::Sequence(items) => {
            for (idx, v) in items.iter().take(4).enumerate() {
                prefix.push(PathSegment::Index(idx));
                out.push(prefix.clone());
                collect_edit_paths(v, prefix, out);
                prefix.pop();
                if out.len() >= 8 {
                    break;
                }
            }
        }
        _ => {}
    }
}

fn value_at<'a>(mut value: &'a Value, path: &[PathSegment]) -> Option<&'a Value> {
    for seg in path {
        match (value, seg) {
            (Value::Mapping(m), PathSegment::Key(k)) => value = m.get(k)?,
            (Value::Sequence(items), PathSegment::Index(idx)) => value = items.get(*idx)?,
            _ => return None,
        }
    }
    Some(value)
}

fn replace_expected(value: &Value, path: &[PathSegment], replacement: &Value) -> Option<Value> {
    let Some((first, rest)) = path.split_first() else {
        return Some(replacement.clone());
    };
    let mut updated = value.clone();
    match (&mut updated, first) {
        (Value::Mapping(m), PathSegment::Key(k)) => {
            let inner = m.get_mut(k)?;
            *inner = replace_expected(inner, rest, replacement)?;
        }
        (Value::Sequence(items), PathSegment::Index(idx)) => {
            let inner = items.get_mut(*idx)?;
            *inner = replace_expected(inner, rest, replacement)?;
        }
        _ => return None,
    }
    Some(updated)
}

fn remove_expected(value: &Value, path: &[PathSegment]) -> Option<Value> {
    let (first, rest) = path.split_first()?;
    let mut updated = value.clone();
    match (&mut updated, first) {
        (Value::Mapping(m), PathSegment::Key(k)) => {
            if rest.is_empty() {
                m.shift_remove(k)?;
            } else {
                let inner = m.get_mut(k)?;
                *inner = remove_expected(inner, rest)?;
            }
        }
        (Value::Sequence(items), PathSegment::Index(idx)) => {
            if rest.is_empty() {
                if *idx >= items.len() {
                    return None;
                }
                items.remove(*idx);
            } else {
                let inner = items.get_mut(*idx)?;
                *inner = remove_expected(inner, rest)?;
            }
        }
        _ => return None,
    }
    Some(updated)
}

/// Target 7 (`edit`): `ayaml_edit::replace_scalar`, `remove_entry`, and `apply_batch` never panic
/// and every returned `VerifiedEdit` parses cleanly, matches `apply_edits`, and preserves every
/// unrelated value in the document.
///
/// # Panics
///
/// Panics if a returned `VerifiedEdit` fails to re-parse, disagrees with `apply_edits`, or alters
/// unrelated values.
pub fn run_edit(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let Ok(doc) = FuzzDoc::arbitrary(&mut u) else {
        return;
    };
    let input = doc.render();
    let Ok(original) = ayaml::parse_value(&input) else {
        return;
    };
    let replacement = match u.int_in_range(0_u8..=4).unwrap_or(0) {
        0 => Value::from("edited"),
        1 => Value::from("a, b"),
        2 => Value::from("true"),
        3 => Value::from("{id}"),
        _ => Value::from(42_i64),
    };
    let mut paths = Vec::new();
    let mut prefix = Vec::new();
    collect_edit_paths(&original, &mut prefix, &mut paths);
    for path in &paths {
        if let Ok(verified) = ayaml_edit::replace_scalar(&input, path, &replacement) {
            let applied = ayaml_edit::apply_edits(&input, verified.edits())
                .expect("verified edits must apply cleanly");
            assert_eq!(applied, verified.output());
            let reparsed = ayaml::parse_value(verified.output())
                .expect("verified replace_scalar output must parse");
            let expected = replace_expected(&original, path, &replacement)
                .expect("target path exists in original");
            assert_eq!(
                reparsed, expected,
                "replace_scalar changed unrelated values"
            );
            assert_eq!(value_at(&reparsed, path), Some(&replacement));
        }
        if let Ok(verified) = ayaml_edit::remove_entry(&input, path) {
            let applied = ayaml_edit::apply_edits(&input, verified.edits())
                .expect("verified edits must apply cleanly");
            assert_eq!(applied, verified.output());
            let reparsed = ayaml::parse_value(verified.output())
                .expect("verified remove_entry output must parse");
            let expected =
                remove_expected(&original, path).expect("target path exists in original");
            assert_eq!(reparsed, expected, "remove_entry changed unrelated values");
        }
    }
    if paths.len() >= 2 {
        let ops = [
            ayaml_edit::EditOp::ReplaceScalar {
                path: paths[0].clone(),
                value: replacement.clone(),
            },
            ayaml_edit::EditOp::RemoveEntry {
                path: paths[1].clone(),
            },
        ];
        if let Ok(verified) = ayaml_edit::apply_batch(&input, &ops) {
            let applied = ayaml_edit::apply_edits(&input, verified.edits())
                .expect("verified batch edits must apply cleanly");
            assert_eq!(applied, verified.output());
            let reparsed =
                ayaml::parse_value(verified.output()).expect("verified batch output must parse");
            if !paths[0].starts_with(&paths[1])
                && !paths[1].starts_with(&paths[0])
                && let Some(after_replace) = replace_expected(&original, &paths[0], &replacement)
                && let Some(expected) = remove_expected(&after_replace, &paths[1])
            {
                assert_eq!(
                    reparsed, expected,
                    "apply_batch semantic mismatch on {input:?}"
                );
            }
        }
    }

    // Exercise `ayaml_edit::apply_edits` directly on arbitrary fuzzer-generated `TextEdit` slices
    // (out-of-bounds, inverted, mid-UTF-8, overlapping, unsorted, and valid slices).
    let edit_count: usize = u.int_in_range(0..=6).unwrap_or(0);
    let max_off = input.len().saturating_add(4);
    let mut raw_edits = Vec::with_capacity(edit_count);
    for _ in 0..edit_count {
        let start = u.int_in_range(0..=max_off).unwrap_or(0);
        let end = u.int_in_range(0..=max_off).unwrap_or(0);
        let repl = match u.int_in_range(0_u8..=4).unwrap_or(0) {
            0 => "",
            1 => "x",
            2 => "café",
            3 => "🚀\n",
            _ => "42",
        };
        raw_edits.push(ayaml_edit::TextEdit::new(
            ayaml::Span::new(start, end),
            repl,
        ));
    }
    if let Ok(spliced) = ayaml_edit::apply_edits(&input, &raw_edits) {
        let mut expected_len = input.len();
        for edit in &raw_edits {
            expected_len = expected_len - edit.span().len() + edit.replacement().len();
        }
        assert_eq!(
            spliced.len(),
            expected_len,
            "apply_edits output byte length mismatch"
        );
    }
}

#[cfg(test)]
mod tests {
    use arbitrary::{Arbitrary, Unstructured};

    use super::{
        arbitrary_yaml::{FuzzCanary, MASKED_ERROR_SCENARIOS},
        check_emit_flow_and_serde, run_edit, run_event_emit, run_masked_error_scenario,
        run_masked_errors, run_merge_keys, run_parse, run_spanned, run_structured, run_typed_equiv,
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
            run_edit(&seed);
        }
    }

    #[test]
    fn parse_and_emit_harnesses_run_on_seeds() {
        let limits = ayaml::Limits::default().with_max_documents(16);
        let parse_opts = ayaml::ParseOptions::new().with_limits(limits.clone());
        let emit_opts = ayaml::EmitOptions::new().with_limits(limits);
        for seed in deterministic_seeds() {
            run_parse(&seed);
            if let Ok(text) = std::str::from_utf8(&seed)
                && let Ok(docs) = ayaml::parse_documents_with(text, &parse_opts)
            {
                check_emit_flow_and_serde(text, &docs, &parse_opts, &emit_opts);
            }
        }
        // Invalid UTF-8 bytes must be handled safely by raw-byte entry points.
        run_parse(b"k: \xff\xfe\n");
    }

    #[test]
    fn masked_errors_covers_every_scenario_branch() {
        let secret = "CANARY_ALL_27_BRANCHES_é_🚀";
        for scenario in 0..MASKED_ERROR_SCENARIOS {
            run_masked_error_scenario(secret, scenario);
        }

        // Verify `FuzzCanary::arbitrary` actually generates every scenario 0..MASKED_ERROR_SCENARIOS
        // so generator and dispatcher ranges can never drift out of sync again.
        let mut seen = vec![false; usize::from(MASKED_ERROR_SCENARIOS)];
        let mut state: u64 = 0xA171_BEEF_CAFE_0001;
        for _ in 0..2_000 {
            let mut buf = [0_u8; 40];
            for byte in &mut buf {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *byte = (state & 0xFF) as u8;
            }
            let mut u = Unstructured::new(&buf);
            if let Ok(canary) = FuzzCanary::arbitrary(&mut u) {
                let idx = usize::from(canary.scenario);
                assert!(
                    idx < usize::from(MASKED_ERROR_SCENARIOS),
                    "FuzzCanary scenario {idx} out of range"
                );
                seen[idx] = true;
            }
        }
        for (idx, hit) in seen.iter().enumerate() {
            assert!(
                *hit,
                "FuzzCanary::arbitrary never generated masked_errors scenario {idx}"
            );
        }
    }
}
