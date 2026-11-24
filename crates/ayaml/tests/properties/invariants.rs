//! Cross-layer property suites for spans, line indexes, masked errors, limits,
//! and small-tree versus event-stream typed equivalence (AY-130).

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::io;

use ayaml::{
    ColumnEncoding, EmitOptions, ErrorCode, ErrorKind, Limits, LineIndex, MAX_DEPTH_CAP,
    ParseOptions, PathSegment, Position, PositionError, SpannedNode, SpannedTree, Value,
};
use proptest::prelude::*;

use super::structured::{
    mutated_structured_yaml, structured_yaml_document, structured_yaml_stream,
};

// ---------------------------------------------------------------------------
// 1. Tree vs. event-stream typed equivalence
// ---------------------------------------------------------------------------

#[cfg(feature = "bench-internals")]
#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    dead_code,
    reason = "fields exercise typed Serde matching and mismatch errors"
)]
struct TypedTarget {
    a: Option<u32>,
    b: Option<Vec<String>>,
    name: Option<String>,
    count: Option<i64>,
    nested: Option<BTreeMap<String, i32>>,
}

#[cfg(feature = "bench-internals")]
fn assert_tree_and_events_match<T: PartialEq + std::fmt::Debug>(
    tree: Result<T, ayaml::Error>,
    events: Result<T, ayaml::Error>,
    label: &str,
    input: &str,
) -> Result<(), TestCaseError> {
    match (tree, events) {
        (Ok(left), Ok(right)) => {
            prop_assert_eq!(left, right, "{}: Ok mismatch\ninput:\n{}", label, input);
        }
        (Err(left), Err(right)) => {
            prop_assert_eq!(
                left.kind(),
                right.kind(),
                "{}: kind mismatch (tree={:?}, events={:?})\ninput:\n{}",
                label,
                left,
                right,
                input
            );
            prop_assert_eq!(
                left.code(),
                right.code(),
                "{}: code mismatch (tree={:?}, events={:?})\ninput:\n{}",
                label,
                left,
                right,
                input
            );
            prop_assert_eq!(
                left.span(),
                right.span(),
                "{}: span mismatch (tree={:?}, events={:?})\ninput:\n{}",
                label,
                left,
                right,
                input
            );
            prop_assert_eq!(
                left.location(),
                right.location(),
                "{}: location mismatch (tree={:?}, events={:?})\ninput:\n{}",
                label,
                left,
                right,
                input
            );
            prop_assert_eq!(
                left.limit(),
                right.limit(),
                "{}: limit mismatch (tree={:?}, events={:?})\ninput:\n{}",
                label,
                left,
                right,
                input
            );
            prop_assert_eq!(
                left.masked(),
                right.masked(),
                "{}: masked mismatch (tree={:?}, events={:?})\ninput:\n{}",
                label,
                left,
                right,
                input
            );
        }
        (Ok(value), Err(error)) => {
            prop_assert!(
                false,
                "{}: tree Ok({:?}) vs events Err({:?})\ninput:\n{}",
                label,
                value,
                error,
                input
            );
        }
        (Err(error), Ok(value)) => {
            prop_assert!(
                false,
                "{}: tree Err({:?}) vs events Ok({:?})\ninput:\n{}",
                label,
                error,
                value,
                input
            );
        }
    }
    Ok(())
}

#[cfg(feature = "bench-internals")]
proptest! {
    /// `from_str_by_tree` and `from_str_by_events` agree on both `Ok` values and `Err` metadata
    /// (`kind`, `code`, `span`, `location`, `limit`, `masked`) across valid and mutated structured
    /// YAML, multiple targets, default/compat modes, and default or tight resource limits.
    #[test]
    fn tree_and_event_stream_typed_readers_agree(
        input in prop_oneof![structured_yaml_document(), mutated_structured_yaml()],
        compat in any::<bool>(),
        drop_global_tags in any::<bool>(),
        tight_limit in prop::option::weighted(0.35, (limit_knob(), 0_usize..48)),
    ) {
        let limits = match tight_limit {
            Some((knob, ceiling)) => limit_pair(knob, ceiling, ceiling).0,
            None => Limits::default(),
        };
        let drop_tags = compat && drop_global_tags;

        assert_tree_and_events_match(
            ayaml::internals::from_str_by_tree_with::<Value>(&input, &limits, compat, drop_tags),
            ayaml::internals::from_str_by_events_with::<Value>(&input, &limits, compat, drop_tags),
            "Value",
            &input,
        )?;

        assert_tree_and_events_match(
            ayaml::internals::from_str_by_tree_with::<TypedTarget>(
                &input, &limits, compat, drop_tags,
            ),
            ayaml::internals::from_str_by_events_with::<TypedTarget>(
                &input, &limits, compat, drop_tags,
            ),
            "TypedTarget",
            &input,
        )?;

        let tree_ignored = ayaml::internals::from_str_by_tree_with::<serde::de::IgnoredAny>(
            &input, &limits, compat, drop_tags,
        )
        .map(|_| ());
        let events_ignored = ayaml::internals::from_str_by_events_with::<serde::de::IgnoredAny>(
            &input, &limits, compat, drop_tags,
        )
        .map(|_| ());
        assert_tree_and_events_match(tree_ignored, events_ignored, "IgnoredAny", &input)?;
    }
}

// ---------------------------------------------------------------------------
// 2. SpannedTree structural invariants and verified entry-span deletions
// ---------------------------------------------------------------------------

fn check_span_bounds(input: &str, span: ayaml::Span, ctx: &str) -> Result<(), TestCaseError> {
    prop_assert!(
        span.start() <= span.end(),
        "{}: inverted span {:?} on input:\n{}",
        ctx,
        span,
        input
    );
    prop_assert!(
        span.end() <= input.len(),
        "{}: span {:?} exceeds len {} on input:\n{}",
        ctx,
        span,
        input.len(),
        input
    );
    prop_assert!(
        input.is_char_boundary(span.start()) && input.is_char_boundary(span.end()),
        "{}: span {:?} splits UTF-8 character on input:\n{}",
        ctx,
        span,
        input
    );
    Ok(())
}

fn remove_at_path(root: &Value, path: &[PathSegment]) -> Option<Value> {
    let mut cloned = root.clone();
    if remove_in_place(&mut cloned, path) {
        Some(cloned)
    } else {
        None
    }
}

fn remove_in_place(current: &mut Value, path: &[PathSegment]) -> bool {
    let untagged = current.untag_mut();
    let Some((first, rest)) = path.split_first() else {
        return false;
    };
    if rest.is_empty() {
        return match (untagged, first) {
            (Value::Sequence(items), PathSegment::Index(idx)) if *idx < items.len() => {
                items.remove(*idx);
                true
            }
            (Value::Mapping(mapping), PathSegment::Key(key)) => mapping.shift_remove(key).is_some(),
            _ => false,
        };
    }
    match (untagged, first) {
        (Value::Sequence(items), PathSegment::Index(idx)) => items
            .get_mut(*idx)
            .is_some_and(|child| remove_in_place(child, rest)),
        (Value::Mapping(mapping), PathSegment::Key(key)) => mapping
            .get_mut(key)
            .is_some_and(|child| remove_in_place(child, rest)),
        _ => false,
    }
}

fn check_spanned_paths(
    input: &str,
    tree: &SpannedTree<'_>,
    root_value: &Value,
    value: &Value,
    path: &mut Vec<PathSegment>,
    parent_span: Option<ayaml::Span>,
) -> Result<(), TestCaseError> {
    let Some(node) = tree.lookup(path) else {
        return Ok(());
    };
    let span = node.span();
    check_span_bounds(input, span, "node span")?;
    prop_assert_eq!(tree.span_at(path), Some(span));
    prop_assert_eq!(tree.entry_span_at(path), node.entry_span());
    prop_assert_eq!(tree.scalar_text_at(path), node.scalar_text());
    prop_assert_eq!(tree.scalar_encoding_at(path), node.scalar_text());

    if let Some(parent) = parent_span
        && !span.is_empty()
        && !parent.is_empty()
    {
        prop_assert!(
            parent.start() <= span.start() && span.end() <= parent.end(),
            "child span {:?} outside parent span {:?} at path {:?}\ninput:\n{}",
            span,
            parent,
            path,
            input
        );
    }

    if let Some(entry_span) = node.entry_span() {
        check_span_bounds(input, entry_span, "entry span")?;
        let edited = format!(
            "{}{}",
            &input[..entry_span.start()],
            &input[entry_span.end()..]
        );
        let reparsed = ayaml::parse_value(&edited).unwrap_or_else(|error| {
            panic!(
                "deleting entry_span {entry_span:?} at path {path:?} produced invalid YAML ({error}):\noriginal:\n{input}\nedited:\n{edited}"
            )
        });
        let expected = remove_at_path(root_value, path).unwrap_or_else(|| {
            panic!("entry_span reported for non-removable path {path:?}\ninput:\n{input}")
        });
        prop_assert_eq!(
            reparsed,
            expected,
            "deleting entry_span {:?} at path {:?} changed unrelated values:\noriginal:\n{}\nedited:\n{}",
            entry_span,
            path,
            input,
            edited
        );
    }

    if node.is_alias() {
        prop_assert!(
            node.scalar_text().is_none(),
            "alias node should not expose scalar_text at {:?}\ninput:\n{}",
            path,
            input
        );
        prop_assert!(
            input[span.start()..span.end()].starts_with('*'),
            "alias span should start with '*' at {:?}\ninput:\n{}",
            path,
            input
        );
        return Ok(());
    }

    let untagged = value.untag_ref();
    match untagged {
        Value::Sequence(items) => {
            prop_assert!(
                node.is_sequence(),
                "expected sequence node at {:?}\ninput:\n{}",
                path,
                input
            );
            for (index, item) in items.iter().enumerate() {
                path.push(PathSegment::Index(index));
                check_spanned_paths(input, tree, root_value, item, path, Some(span))?;
                path.pop();
            }
        }
        Value::Mapping(mapping) => {
            prop_assert!(
                node.is_mapping(),
                "expected mapping node at {:?}\ninput:\n{}",
                path,
                input
            );
            for (key, item) in mapping {
                path.push(PathSegment::Key(key.clone()));
                check_spanned_paths(input, tree, root_value, item, path, Some(span))?;
                path.pop();
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            check_scalar_node(input, node, path)?;
        }
        Value::Tagged(_) => unreachable!("untagged above"),
    }
    Ok(())
}

fn check_scalar_node(
    input: &str,
    node: &SpannedNode<'_>,
    path: &[PathSegment],
) -> Result<(), TestCaseError> {
    prop_assert!(
        node.is_scalar(),
        "expected scalar node at {:?}\ninput:\n{}",
        path,
        input
    );
    let text = node.scalar_text();
    prop_assert!(
        text.is_some(),
        "scalar node missing scalar_text at {:?}\ninput:\n{}",
        path,
        input
    );
    let span = node.span();
    prop_assert_eq!(
        text,
        Some(&input[span.start()..span.end()]),
        "scalar_text mismatch at {:?}\ninput:\n{}",
        path,
        input
    );
    Ok(())
}

proptest! {
    /// Every node in `SpannedTree` has valid UTF-8 in-bounds spans, non-empty children nest inside
    /// parents, scalar text matches source slices, and deleting any reported `entry_span` leaves
    /// valid YAML.
    #[test]
    fn spanned_tree_invariants_hold_on_structured_documents(input in structured_yaml_document()) {
        let tree = SpannedTree::parse(&input)
            .unwrap_or_else(|error| panic!("SpannedTree::parse failed: {error}\ninput:\n{input}"));
        let value = ayaml::parse_value(&input)
            .unwrap_or_else(|error| panic!("parse_value failed: {error}\ninput:\n{input}"));
        prop_assert_eq!(tree.documents().len(), 1, "input:\n{}", input);
        let root = tree.document(0).expect("single document");
        if !root.is_alias() {
            prop_assert!(tree.lookup(&[]).is_some(), "root lookup failed:\n{}", input);
        }
        let mut path = Vec::new();
        check_spanned_paths(&input, &tree, &value, &value, &mut path, None)?;
    }

    /// Multi-document streams match `parse_documents` counts, disable ambiguous root `lookup`
    /// when multiple documents exist, and carry valid root spans matching document kinds.
    #[test]
    fn spanned_tree_invariants_hold_on_streams(input in structured_yaml_stream()) {
        let tree = SpannedTree::parse(&input)
            .unwrap_or_else(|error| panic!("SpannedTree::parse failed: {error}\ninput:\n{input}"));
        let docs = ayaml::parse_documents(&input)
            .unwrap_or_else(|error| panic!("parse_documents failed: {error}\ninput:\n{input}"));
        prop_assert_eq!(tree.documents().len(), docs.len(), "input:\n{}", input);
        prop_assert!(tree.document(docs.len()).is_none());
        if docs.len() != 1 {
            prop_assert!(tree.lookup(&[]).is_none(), "ambiguous stream lookup succeeded");
        }
        for (index, (doc, value)) in tree.documents().iter().zip(&docs).enumerate() {
            prop_assert!(tree.document(index).is_some());
            check_span_bounds(&input, doc.span(), "stream doc root span")?;
            if doc.is_alias() {
                continue;
            }
            match value.untag_ref() {
                Value::Sequence(_) => prop_assert!(doc.is_sequence(), "doc {index}\n{input}"),
                Value::Mapping(_) => prop_assert!(doc.is_mapping(), "doc {index}\n{input}"),
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                    check_scalar_node(&input, doc, &[])?;
                }
                Value::Tagged(_) => unreachable!("untagged above"),
            }
        }
    }

    /// `SpannedTree::parse` (and `parse_compat` when enabled) never panics on mutated or arbitrary
    /// YAML inputs.
    #[test]
    fn spanned_tree_never_panics(input in prop_oneof![mutated_structured_yaml(), any::<String>()]) {
        let _ = SpannedTree::parse(&input);
        #[cfg(feature = "libyaml-compat")]
        let _ = SpannedTree::parse_compat(&input);
    }
}

// ---------------------------------------------------------------------------
// 3. LineIndex bijectivity and monotonicity across encodings, CRLF, and BOM
// ---------------------------------------------------------------------------

fn line_index_text() -> impl Strategy<Value = String> {
    let chunk = prop_oneof![
        4 => "[a-zA-Z0-9 :_-]{0,20}",
        3 => prop::sample::select(vec![
            "\n",
            "\r",
            "\r\n",
            "\r\n\r\n",
            "é",
            "日",
            "🚀",
            "a🚀b",
            "\u{0085}",
            "\u{2028}",
            "\u{2029}",
            "\u{feff}",
            "\t",
            "",
        ])
        .prop_map(str::to_owned),
    ];
    let bom_prefix = prop::sample::select(vec!["", "\u{feff}", "\u{feff}\u{feff}"]);
    (bom_prefix, prop::collection::vec(chunk, 0..24)).prop_map(|(bom, parts)| {
        let mut out = String::from(bom);
        for part in parts {
            out.push_str(&part);
        }
        out
    })
}

proptest! {
    /// `LineIndex::to_position` and `LineIndex::to_offset` are exact inverses on every valid
    /// offset past any opening BOM, strictly monotonic, reject split surrogates and out-of-bounds
    /// columns, and agree across UTF-8, Scalar, and UTF-16 encodings (including lines > 64 bytes).
    #[test]
    fn line_index_bijectivity_and_monotonicity(input in line_index_text()) {
        let index = LineIndex::new(&input);
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
            prop_assert_eq!(
                index.to_position(input.len() + 1, encoding),
                Err(PositionError::OutOfBounds)
            );
            prop_assert_eq!(
                index.to_offset(Position::new(index.line_count(), 0), encoding),
                Err(PositionError::OutOfBounds)
            );

            let mut prev_pos: Option<Position> = None;
            let mut max_col_by_line = vec![0_usize; index.line_count()];
            for offset in 0..=input.len() {
                let inside_crlf = offset > 0
                    && offset < input.len()
                    && bytes[offset - 1] == b'\r'
                    && bytes[offset] == b'\n';
                if inside_crlf {
                    prop_assert_eq!(
                        index.to_position(offset, encoding),
                        Err(PositionError::InsideCrLf),
                        "offset={} encoding={:?} input={:?}",
                        offset,
                        encoding,
                        input
                    );
                    continue;
                }
                if encoding != ColumnEncoding::Utf8
                    && offset > first_col
                    && !input.is_char_boundary(offset)
                {
                    prop_assert_eq!(
                        index.to_position(offset, encoding),
                        Err(PositionError::InvalidUtf8Boundary),
                        "offset={} encoding={:?} input={:?}",
                        offset,
                        encoding,
                        input
                    );
                    continue;
                }

                let pos = index.to_position(offset, encoding).unwrap_or_else(|error| {
                    panic!(
                        "unexpected error {error:?} at offset={offset} encoding={encoding:?} input={input:?}"
                    )
                });
                prop_assert!(pos.line < index.line_count());
                max_col_by_line[pos.line] = max_col_by_line[pos.line].max(pos.column);

                if offset <= first_col {
                    prop_assert_eq!(pos, Position::new(0, 0));
                    prop_assert_eq!(index.to_offset(pos, encoding), Ok(first_col));
                } else {
                    prop_assert_eq!(
                        index.to_offset(pos, encoding),
                        Ok(offset),
                        "round-trip mismatch at offset={} pos={:?} encoding={:?} input={:?}",
                        offset,
                        pos,
                        encoding,
                        input
                    );
                    if let Some(prev) = prev_pos {
                        prop_assert!(
                            (prev.line, prev.column) < (pos.line, pos.column),
                            "not strictly monotonic: prev={:?} cur={:?} offset={} encoding={:?} input={:?}",
                            prev,
                            pos,
                            offset,
                            encoding,
                            input
                        );
                    }
                    prev_pos = Some(pos);
                }

                // A 4-byte character spans two UTF-16 code units; the midpoint must report SplitSurrogate.
                if encoding == ColumnEncoding::Utf16
                    && offset >= first_col
                    && input.is_char_boundary(offset)
                    && input[offset..].chars().next().is_some_and(|ch| ch.len_utf16() == 2)
                {
                    prop_assert_eq!(
                        index.to_offset(Position::new(pos.line, pos.column + 1), encoding),
                        Err(PositionError::SplitSurrogate),
                        "expected SplitSurrogate at offset={} pos={:?} input={:?}",
                        offset,
                        pos,
                        input
                    );
                }
            }

            // One column past every line's end must be OutOfBounds.
            for (line, max_col) in max_col_by_line.into_iter().enumerate() {
                prop_assert_eq!(
                    index.to_offset(Position::new(line, max_col + 1), encoding),
                    Err(PositionError::OutOfBounds),
                    "past-EOL column did not error at line={} col={} encoding={:?} input={:?}",
                    line,
                    max_col + 1,
                    encoding,
                    input
                );
            }
        }

        // Across encodings, valid character boundaries agree on line and satisfy
        // Scalar <= Utf16 <= Utf8 column counts.
        for offset in 0..=input.len() {
            let inside_crlf = offset > 0
                && offset < input.len()
                && bytes[offset - 1] == b'\r'
                && bytes[offset] == b'\n';
            if inside_crlf || (offset > first_col && !input.is_char_boundary(offset)) {
                continue;
            }
            let p_utf8 = index.to_position(offset, ColumnEncoding::Utf8).expect("utf8");
            let p_scalar = index
                .to_position(offset, ColumnEncoding::Scalar)
                .expect("scalar");
            let p_utf16 = index
                .to_position(offset, ColumnEncoding::Utf16)
                .expect("utf16");
            prop_assert_eq!(p_scalar.line, p_utf8.line);
            prop_assert_eq!(p_utf16.line, p_utf8.line);
            prop_assert!(p_scalar.column <= p_utf16.column);
            prop_assert!(p_utf16.column <= p_utf8.column);
        }
    }
}

// ---------------------------------------------------------------------------
// 4. Error::masked() secret canary non-leakage
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code, reason = "deserialization target for unknown-field errors")]
struct StrictConfig {
    allowed: u32,
}

#[derive(Debug, serde::Deserialize)]
#[allow(
    dead_code,
    reason = "deserialization target for unknown-variant errors"
)]
enum StrictChoice {
    Alpha,
    Beta,
}

fn secret_canary() -> impl Strategy<Value = String> {
    (
        "[A-Z0-9]{8,14}",
        prop::sample::select(vec!["", "_é", "_🚀"]),
    )
        .prop_map(|(body, suffix)| format!("CANARY_{body}{suffix}"))
}

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

fn assert_masked_hides_canary(
    error: &ayaml::Error,
    canary: &str,
    case: &str,
) -> Result<(), TestCaseError> {
    let masked = error.masked();
    prop_assert_eq!(
        masked.kind(),
        error.kind(),
        "{}: masked kind mismatch",
        case
    );
    prop_assert_eq!(
        masked.code(),
        error.code(),
        "{}: masked code mismatch",
        case
    );
    prop_assert_eq!(
        masked.span(),
        error.span(),
        "{}: masked span mismatch",
        case
    );
    prop_assert_eq!(
        masked.location(),
        error.location(),
        "{}: masked location mismatch",
        case
    );
    prop_assert_eq!(
        masked.limit(),
        error.limit(),
        "{}: masked limit mismatch",
        case
    );
    let display = masked.to_string();
    let debug = format!("{masked:?}");
    prop_assert!(
        !display.contains(canary),
        "{}: masked Display leaked canary {:?}: {:?}",
        case,
        canary,
        display
    );
    prop_assert!(
        !debug.contains(canary),
        "{}: masked Debug leaked canary {:?}: {:?}",
        case,
        canary,
        debug
    );
    prop_assert!(
        masked.source().is_none(),
        "{}: masked error exposed source()",
        case
    );
    Ok(())
}

proptest! {
    /// `Error::masked()` never leaks injected secret canaries across scanner, parser, alias,
    /// duplicate-key, typed Serde, merge, and limit failures.
    #[test]
    fn masked_errors_never_leak_secret_canaries(canary in secret_canary()) {
        // Scanner: unterminated double-quoted scalar and invalid escape.
        let err = ayaml::parse_value(&format!("k: \"{canary}")).expect_err("unterminated");
        assert_masked_hides_canary(&err, &canary, "unterminated quote")?;

        let err = ayaml::parse_value(&format!("k: \"{canary}\\q\"")).expect_err("bad escape");
        assert_masked_hides_canary(&err, &canary, "invalid escape")?;

        let err = ayaml::parse_value(&format!("@{canary}\n")).expect_err("reserved indicator");
        assert_masked_hides_canary(&err, &canary, "reserved indicator")?;

        // Parser: broken flow mapping.
        let err = ayaml::parse_value(&format!("{{{canary}: [1, 2}}\n")).expect_err("bad flow");
        assert_masked_hides_canary(&err, &canary, "broken flow")?;

        // Alias: undefined alias named after secret.
        let err = ayaml::parse_value(&format!("a: *{canary}\n")).expect_err("unknown alias");
        assert_masked_hides_canary(&err, &canary, "unknown alias")?;

        // Duplicate key: secret as key.
        let err = ayaml::parse_value(&format!("{canary}: 1\n{canary}: 2\n"))
            .expect_err("duplicate key");
        assert_masked_hides_canary(&err, &canary, "duplicate key")?;

        // Typed Serde: invalid type with secret key + secret value.
        let err = ayaml::from_str::<BTreeMap<String, u32>>(&format!("{canary}: {canary}\n"))
            .expect_err("invalid type");
        assert_masked_hides_canary(&err, &canary, "typed invalid type")?;

        // Typed Serde: nested path with secret segment.
        let err = ayaml::from_str::<BTreeMap<String, BTreeMap<String, u32>>>(&format!(
            "outer:\n  {canary}: {canary}\n"
        ))
        .expect_err("nested invalid type");
        assert_masked_hides_canary(&err, &canary, "nested typed invalid type")?;

        // Typed Serde: unknown field and unknown enum variant.
        let err = ayaml::from_str::<StrictConfig>(&format!("{canary}: 1\n"))
            .expect_err("unknown field");
        assert_masked_hides_canary(&err, &canary, "unknown field")?;

        let err = ayaml::from_str::<StrictChoice>(&format!("{canary}\n"))
            .expect_err("unknown variant");
        assert_masked_hides_canary(&err, &canary, "unknown variant")?;

        // Limits: scalar bytes and input bytes limits with secret payload.
        let scalar_opts =
            ParseOptions::new().with_limits(Limits::default().with_max_scalar_bytes(4));
        let err = ayaml::parse_value_with(&format!("k: {canary}\n"), &scalar_opts)
            .expect_err("scalar limit");
        assert_masked_hides_canary(&err, &canary, "scalar limit")?;

        let input_opts =
            ParseOptions::new().with_limits(Limits::default().with_max_input_bytes(4));
        let err = ayaml::parse_value_with(&format!("k: {canary}\n"), &input_opts)
            .expect_err("input limit");
        assert_masked_hides_canary(&err, &canary, "input limit")?;

        // Merge operand and merge collection-entries limit errors with secret text.
        let mut value = ayaml::parse_value(&format!("m:\n  <<: {canary}\n")).expect("parse");
        let err = value.apply_merge_keys().expect_err("invalid merge operand");
        assert_masked_hides_canary(&err, &canary, "merge operand")?;

        let mut merge_limit_val = ayaml::parse_value(&format!(
            "base: &b\n  {canary}: 1\n  other: 2\nm:\n  <<: *b\n  k: 3\n"
        ))
        .expect("parse merge");
        let err = merge_limit_val
            .apply_merge_keys_with_limits(&Limits::default().with_max_collection_entries(2))
            .expect_err("merge collection entries limit");
        assert_masked_hides_canary(&err, &canary, "merge collection entries limit")?;

        // Directives and tags containing secret text.
        let err = ayaml::parse_value(&format!("%YAML {canary}\n---\na\n"))
            .expect_err("invalid yaml directive");
        assert_masked_hides_canary(&err, &canary, "invalid directive")?;

        let err = ayaml::parse_value(&format!("!{canary}!x 1\n"))
            .expect_err("undefined tag handle");
        assert_masked_hides_canary(&err, &canary, "undefined tag handle")?;

        // Invalid UTF-8 bytes beside secret text.
        let mut bad_utf8 = format!("k: {canary}").into_bytes();
        bad_utf8.push(0xFF);
        let err = ayaml::from_slice::<Value>(&bad_utf8).expect_err("invalid utf8");
        assert_masked_hides_canary(&err, &canary, "invalid utf8")?;

        // Event emitter error on dangling alias named after secret.
        let dangling_input = format!("a: *{canary}\n");
        let dangling_events = ayaml::parse_events(&dangling_input)
            .and_then(Iterator::collect::<Result<Vec<_>, _>>)
            .expect("dangling alias parses as event");
        let err = ayaml::to_string_events(&dangling_events).expect_err("dangling alias emit");
        assert_masked_hides_canary(&err, &canary, "dangling alias emit")?;

        // Block scalar invalid header with secret text.
        let err = ayaml::parse_value(&format!("|{canary}\n  body\n"))
            .expect_err("invalid block scalar header");
        assert_masked_hides_canary(&err, &canary, "invalid block scalar header")?;

        // Core schema scalar resolution errors with secret values.
        for tag in ["!!int", "!!bool", "!!float"] {
            let err = ayaml::parse_value(&format!("{tag} {canary}\n"))
                .expect_err("resolution error");
            assert_masked_hides_canary(&err, &canary, tag)?;
        }

        // Emitter output-bytes limit error with secret payload.
        let emit_opts = EmitOptions::new().with_limits(Limits::default().with_max_output_bytes(4));
        let err = ayaml::to_string_with(&canary, &emit_opts).expect_err("output limit");
        assert_masked_hides_canary(&err, &canary, "output limit")?;

        // I/O writer error whose underlying io::Error message contains the secret canary.
        let mut writer = FailingWriter {
            message: format!("write failed near {canary}"),
        };
        let err = ayaml::to_writer(&mut writer, &canary).expect_err("io write error");
        assert_masked_hides_canary(&err, &canary, "io writer error")?;

        #[cfg(feature = "libyaml-compat")]
        {
            let err = ayaml::compat::from_str::<StrictConfig>(&format!("{canary}: {canary}\n"))
                .expect_err("compat unknown field");
            assert_masked_hides_canary(&err, &canary, "compat unknown field")?;
        }
    }
}

// ---------------------------------------------------------------------------
// 5. Limits monotonicity across readers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum LimitKnob {
    InputBytes,
    Depth,
    Events,
    Documents,
    ScalarBytes,
    CollectionEntries,
    Anchors,
    AliasDereferences,
    AliasExpansionBytes,
    AliasBytesPerInputByte,
    NodeCostBytes,
    TagExpansionBytes,
    OutputBytes,
}

fn limit_knob() -> impl Strategy<Value = LimitKnob> {
    prop::sample::select(vec![
        LimitKnob::InputBytes,
        LimitKnob::Depth,
        LimitKnob::Events,
        LimitKnob::Documents,
        LimitKnob::ScalarBytes,
        LimitKnob::CollectionEntries,
        LimitKnob::Anchors,
        LimitKnob::AliasDereferences,
        LimitKnob::AliasExpansionBytes,
        LimitKnob::AliasBytesPerInputByte,
        LimitKnob::NodeCostBytes,
        LimitKnob::TagExpansionBytes,
        LimitKnob::OutputBytes,
    ])
}

fn limit_pair(knob: LimitKnob, lo: usize, hi: usize) -> (Limits, Limits) {
    let base = Limits::default();
    match knob {
        LimitKnob::InputBytes => (
            base.clone().with_max_input_bytes(lo),
            base.with_max_input_bytes(hi),
        ),
        LimitKnob::Depth => (
            base.clone().with_max_depth(lo.min(MAX_DEPTH_CAP)),
            base.with_max_depth(hi.min(MAX_DEPTH_CAP)),
        ),
        LimitKnob::Events => (base.clone().with_max_events(lo), base.with_max_events(hi)),
        LimitKnob::Documents => (
            base.clone().with_max_documents(lo),
            base.with_max_documents(hi),
        ),
        LimitKnob::ScalarBytes => (
            base.clone().with_max_scalar_bytes(lo),
            base.with_max_scalar_bytes(hi),
        ),
        LimitKnob::CollectionEntries => (
            base.clone().with_max_collection_entries(lo),
            base.with_max_collection_entries(hi),
        ),
        LimitKnob::Anchors => (base.clone().with_max_anchors(lo), base.with_max_anchors(hi)),
        LimitKnob::AliasDereferences => (
            base.clone().with_max_alias_dereferences(lo),
            base.with_max_alias_dereferences(hi),
        ),
        LimitKnob::AliasExpansionBytes => (
            base.clone()
                .with_alias_bytes_per_input_byte(0)
                .with_max_alias_expansion_bytes(lo),
            base.with_alias_bytes_per_input_byte(0)
                .with_max_alias_expansion_bytes(hi),
        ),
        LimitKnob::AliasBytesPerInputByte => (
            base.clone()
                .with_max_alias_expansion_bytes(0)
                .with_alias_bytes_per_input_byte(lo),
            base.with_max_alias_expansion_bytes(0)
                .with_alias_bytes_per_input_byte(hi),
        ),
        // Higher node_cost_bytes charges more per copied node, so hi is stricter than lo.
        LimitKnob::NodeCostBytes => (
            base.clone()
                .with_max_alias_expansion_bytes(0)
                .with_alias_bytes_per_input_byte(16)
                .with_node_cost_bytes(hi.max(1)),
            base.with_max_alias_expansion_bytes(0)
                .with_alias_bytes_per_input_byte(16)
                .with_node_cost_bytes(lo.max(1)),
        ),
        LimitKnob::TagExpansionBytes => (
            base.clone().with_max_tag_expansion_bytes(lo),
            base.with_max_tag_expansion_bytes(hi),
        ),
        LimitKnob::OutputBytes => (
            base.clone().with_max_output_bytes(lo),
            base.with_max_output_bytes(hi),
        ),
    }
}

fn is_limit_error(error: &ayaml::Error) -> bool {
    error.kind() == ErrorKind::Limit
        || error.limit().is_some()
        || matches!(
            error.code(),
            ErrorCode::InvalidLimits
                | ErrorCode::LimitExceeded
                | ErrorCode::InputSizeLimit
                | ErrorCode::DepthLimit
                | ErrorCode::DocumentCountLimit
                | ErrorCode::ScalarSizeLimit
                | ErrorCode::CollectionEntriesLimit
                | ErrorCode::EventCountLimit
                | ErrorCode::AnchorCountLimit
                | ErrorCode::AliasDereferenceLimit
                | ErrorCode::AliasExpansionLimit
                | ErrorCode::OutputSizeLimit
                | ErrorCode::TagExpansionLimit
        )
}

fn assert_monotonic_result<T: PartialEq + std::fmt::Debug>(
    lo: Result<T, ayaml::Error>,
    hi: Result<T, ayaml::Error>,
    label: &str,
) -> Result<(), TestCaseError> {
    match (lo, hi) {
        (Ok(left), Ok(right)) => {
            prop_assert_eq!(left, right, "{}: raised limit changed parsed result", label);
        }
        (Ok(value), Err(error)) => {
            prop_assert!(
                false,
                "{}: tighter limit succeeded ({:?}) while looser limit failed ({:?})",
                label,
                value,
                error
            );
        }
        (Err(lo_err), Err(hi_err)) => {
            if !is_limit_error(&lo_err) {
                prop_assert_eq!(
                    lo_err.kind(),
                    hi_err.kind(),
                    "{}: raising a limit changed non-limit error kind ({:?} -> {:?})",
                    label,
                    lo_err,
                    hi_err
                );
                prop_assert_eq!(
                    lo_err.code(),
                    hi_err.code(),
                    "{}: raising a limit changed non-limit error code ({:?} -> {:?})",
                    label,
                    lo_err,
                    hi_err
                );
                prop_assert_eq!(
                    lo_err.span(),
                    hi_err.span(),
                    "{}: raising a limit changed non-limit error span ({:?} -> {:?})",
                    label,
                    lo_err,
                    hi_err
                );
            }
        }
        (Err(lo_err), Ok(_)) => {
            prop_assert!(
                is_limit_error(&lo_err),
                "{}: raising a limit turned non-limit error into Ok ({:?})",
                label,
                lo_err
            );
        }
    }
    Ok(())
}

proptest! {
    /// Tightening any `Limits` ceiling never turns an error into `Ok`, and raising a ceiling
    /// never turns `Ok` into an error, changes the parsed result, or alters non-limit errors.
    #[test]
    fn limits_are_monotonic(
        input in prop_oneof![
            structured_yaml_document(),
            structured_yaml_stream(),
            mutated_structured_yaml(),
        ],
        knob in limit_knob(),
        a in prop_oneof![2 => 0_usize..16, 2 => 16_usize..128, 1 => 128_usize..512],
        b in prop_oneof![2 => 0_usize..16, 2 => 16_usize..128, 1 => 128_usize..512],
    ) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let (limits_lo, limits_hi) = limit_pair(knob, lo, hi);
        let opts_lo = ParseOptions::new().with_limits(limits_lo.clone());
        let opts_hi = ParseOptions::new().with_limits(limits_hi.clone());

        let value_lo = ayaml::parse_value_with(&input, &opts_lo);
        let value_hi = ayaml::parse_value_with(&input, &opts_hi);
        let parsed_for_post = value_hi.as_ref().ok().cloned();
        assert_monotonic_result(value_lo, value_hi, "parse_value_with")?;

        if let Some(base_value) = parsed_for_post {
            let mut merged_lo = base_value.clone();
            let mut merged_hi = base_value.clone();
            assert_monotonic_result(
                merged_lo.apply_merge_keys_with_limits(&limits_lo).map(|()| merged_lo),
                merged_hi.apply_merge_keys_with_limits(&limits_hi).map(|()| merged_hi),
                "apply_merge_keys_with_limits",
            )?;

            let emit_lo = EmitOptions::new().with_limits(limits_lo);
            let emit_hi = EmitOptions::new().with_limits(limits_hi);
            assert_monotonic_result(
                ayaml::to_string_with(&base_value, &emit_lo),
                ayaml::to_string_with(&base_value, &emit_hi),
                "to_string_with",
            )?;
        }

        assert_monotonic_result(
            ayaml::parse_documents_with(&input, &opts_lo),
            ayaml::parse_documents_with(&input, &opts_hi),
            "parse_documents_with",
        )?;
        assert_monotonic_result(
            ayaml::from_str_with::<Value>(&input, &opts_lo),
            ayaml::from_str_with::<Value>(&input, &opts_hi),
            "from_str_with",
        )?;
        assert_monotonic_result(
            ayaml::Deserializer::from_str_with(&input, &opts_lo).and_then(|mut cursor| {
                let mut docs = Vec::new();
                while let Some(doc) = cursor.next_borrowed::<Value>()? {
                    docs.push(doc);
                }
                Ok(docs)
            }),
            ayaml::Deserializer::from_str_with(&input, &opts_hi).and_then(|mut cursor| {
                let mut docs = Vec::new();
                while let Some(doc) = cursor.next_borrowed::<Value>()? {
                    docs.push(doc);
                }
                Ok(docs)
            }),
            "Deserializer::next_borrowed",
        )?;
        #[cfg(feature = "libyaml-compat")]
        {
            let compat_lo = ayaml::compat::ParseOptions::new().with_limits(opts_lo.limits.clone());
            let compat_hi = ayaml::compat::ParseOptions::new().with_limits(opts_hi.limits.clone());
            assert_monotonic_result(
                ayaml::compat::from_str_with::<Value>(&input, &compat_lo),
                ayaml::compat::from_str_with::<Value>(&input, &compat_hi),
                "compat::from_str_with",
            )?;
        }
        assert_monotonic_result(
            ayaml::parse_events_with(&input, &opts_lo)
                .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                .map(|events| events.len()),
            ayaml::parse_events_with(&input, &opts_hi)
                .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                .map(|events| events.len()),
            "parse_events_with",
        )?;
        assert_monotonic_result(
            SpannedTree::parse_with(&input, &opts_lo).map(|tree| tree.documents().len()),
            SpannedTree::parse_with(&input, &opts_hi).map(|tree| tree.documents().len()),
            "SpannedTree::parse_with",
        )?;
    }
}
