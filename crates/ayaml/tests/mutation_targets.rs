//! Targeted mutation-killing tests for spans, line/column positions,
//! `SpannedTree` slices, and emitter indentation options (AY-037).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use ayaml::{
    ColumnEncoding, EmitOptions, ErrorCode, ErrorKind, LineIndex, Location, Mapping, PathSegment,
    Position, PositionError, ScalarStyle, Span, SpannedTree, Value,
};

fn hash_of<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[test]
fn span_and_location_boundary_mutations() {
    // Half-open containment boundaries: start - 1, start, end - 1, end.
    let span = Span::new(5, 9);
    assert_eq!(span.start(), 5);
    assert_eq!(span.end(), 9);
    assert_eq!(span.len(), 4);
    assert!(!span.is_empty());
    assert!(!span.contains(4));
    assert!(span.contains(5));
    assert!(span.contains(8));
    assert!(!span.contains(9));
    assert_eq!(span.range(), 5..9);
    assert_eq!(Span::from(5..9), span);
    assert_eq!(std::ops::Range::<usize>::from(span), 5..9);

    // Empty and inverted spans have length 0 and contain no index.
    let empty = Span::empty(5);
    assert_eq!(empty.start(), 5);
    assert_eq!(empty.end(), 5);
    assert_eq!(empty.len(), 0);
    assert!(empty.is_empty());
    assert!(!empty.contains(5));
    assert_eq!(empty.slice("0123456789"), Some(""));

    let inverted = Span::new(9, 5);
    assert_eq!(inverted.len(), 0);
    assert!(inverted.is_empty());
    assert!(!inverted.contains(5));
    assert!(!inverted.contains(7));
    assert!(!inverted.contains(9));
    assert_eq!(inverted.slice("0123456789"), None);

    // Multi-byte UTF-8 slicing rejects every interior byte and out-of-bounds end.
    let utf8 = "aé€🚀";
    assert_eq!(utf8.len(), 10);
    assert_eq!(Span::new(0, 1).slice(utf8), Some("a"));
    assert_eq!(Span::new(1, 3).slice(utf8), Some("é"));
    assert_eq!(Span::new(3, 6).slice(utf8), Some("€"));
    assert_eq!(Span::new(6, 10).slice(utf8), Some("🚀"));
    for bad in [
        Span::new(1, 2),
        Span::new(2, 3),
        Span::new(3, 4),
        Span::new(4, 6),
        Span::new(6, 7),
        Span::new(6, 8),
        Span::new(6, 9),
        Span::new(7, 10),
        Span::new(0, 11),
        Span::new(11, 11),
    ] {
        assert_eq!(bad.slice(utf8), None, "expected None for {bad:?}");
    }

    // Position / Location exact arithmetic, ordering, hashing, and saturation.
    let p0 = Position::new(0, 0);
    assert_eq!(p0, Position::default());
    let l0 = Location::from(p0);
    assert_eq!((l0.line(), l0.column()), (1, 1));
    assert_eq!(Location::default(), Location::new(0, 0));

    let p_mid = Position::new(4, 9);
    let l_mid = Location::from(p_mid);
    assert_eq!((l_mid.line(), l_mid.column()), (5, 10));
    assert!(l0 < l_mid);
    assert_ne!(hash_of(&p0), hash_of(&p_mid));
    assert_ne!(hash_of(&l0), hash_of(&l_mid));

    let l_max = Location::from(Position::new(usize::MAX, usize::MAX));
    assert_eq!((l_max.line(), l_max.column()), (usize::MAX, usize::MAX));
}

#[test]
fn line_index_encoding_bom_crlf_and_block_boundary_mutations() {
    // Line counts across empty, LF, CR, CRLF, and non-break YAML 1.2 characters (NEL/LS/PS).
    assert_eq!(LineIndex::new("").line_count(), 1);
    assert_eq!(LineIndex::new("x").line_count(), 1);
    assert_eq!(LineIndex::new("x\n").line_count(), 2);
    assert_eq!(LineIndex::new("x\r").line_count(), 2);
    assert_eq!(LineIndex::new("x\r\n").line_count(), 2);
    assert_eq!(LineIndex::new("a\r\nb\nc\rd").line_count(), 4);
    assert_eq!(
        LineIndex::new("a\u{0085}b\u{2028}c\u{2029}d").line_count(),
        1
    );

    // Leading BOM skipped on line 0; second BOM on line 0 and BOM on line 1 count as columns.
    let bom_input = "\u{feff}\u{feff}a\r\n\u{feff}é🚀!\n";
    let idx = LineIndex::new(bom_input);
    assert_eq!(idx.line_count(), 3);
    for encoding in [
        ColumnEncoding::Utf8,
        ColumnEncoding::Scalar,
        ColumnEncoding::Utf16,
    ] {
        // Offsets 0..=3 inside the opening BOM all map to (0, 0) and invert to offset 3.
        for inside_bom in 0..=3 {
            assert_eq!(
                idx.to_position(inside_bom, encoding),
                Ok(Position::new(0, 0)),
                "bom offset {inside_bom} {encoding:?}"
            );
        }
        assert_eq!(idx.to_offset(Position::new(0, 0), encoding), Ok(3));
        assert_eq!(
            idx.to_position(bom_input.len() + 1, encoding),
            Err(PositionError::OutOfBounds)
        );
        assert_eq!(
            idx.to_offset(Position::new(idx.line_count(), 0), encoding),
            Err(PositionError::OutOfBounds)
        );
    }

    // Offset 6 is the second BOM end ('a' starts at byte 6).
    assert_eq!(
        idx.to_position(6, ColumnEncoding::Utf8),
        Ok(Position::new(0, 3))
    );
    assert_eq!(
        idx.to_position(6, ColumnEncoding::Scalar),
        Ok(Position::new(0, 1))
    );
    assert_eq!(
        idx.to_position(6, ColumnEncoding::Utf16),
        Ok(Position::new(0, 1))
    );

    // Inside CRLF: byte 7 is '\r', byte 8 is '\n' (between CR and LF).
    let cr_pos = bom_input.find('\r').expect("cr");
    assert_eq!(
        idx.to_position(cr_pos, ColumnEncoding::Utf8),
        Ok(Position::new(0, 4))
    );
    for encoding in [
        ColumnEncoding::Utf8,
        ColumnEncoding::Scalar,
        ColumnEncoding::Utf16,
    ] {
        assert_eq!(
            idx.to_position(cr_pos + 1, encoding),
            Err(PositionError::InsideCrLf)
        );
        assert_eq!(
            idx.to_position(cr_pos + 2, encoding),
            Ok(Position::new(1, 0))
        );
        assert_eq!(idx.to_offset(Position::new(1, 0), encoding), Ok(cr_pos + 2));
    }

    // Line 1: `\u{feff}` (3 bytes, 1 scalar, 1 utf16), `é` (2 bytes, 1, 1), `🚀` (4 bytes, 1, 2), `!`
    let bang = bom_input.find('!').expect("bang");
    assert_eq!(
        idx.to_position(bang, ColumnEncoding::Utf8),
        Ok(Position::new(1, 9))
    );
    assert_eq!(
        idx.to_position(bang, ColumnEncoding::Scalar),
        Ok(Position::new(1, 3))
    );
    assert_eq!(
        idx.to_position(bang, ColumnEncoding::Utf16),
        Ok(Position::new(1, 4))
    );
    assert_eq!(
        idx.to_offset(Position::new(1, 9), ColumnEncoding::Utf8),
        Ok(bang)
    );
    assert_eq!(
        idx.to_offset(Position::new(1, 3), ColumnEncoding::Scalar),
        Ok(bang)
    );
    assert_eq!(
        idx.to_offset(Position::new(1, 4), ColumnEncoding::Utf16),
        Ok(bang)
    );
    // Surrogate split inside `🚀` (starts at UTF-16 column 2, ends at 4 -> col 3 splits).
    assert_eq!(
        idx.to_offset(Position::new(1, 3), ColumnEncoding::Utf16),
        Err(PositionError::SplitSurrogate)
    );
    // Mid-UTF-8 byte inside `é` fails for Scalar & Utf16, succeeds for Utf8.
    let e_offset = bom_input.find('é').expect("é");
    assert_eq!(
        idx.to_position(e_offset + 1, ColumnEncoding::Utf8),
        Ok(Position::new(1, 4))
    );
    assert_eq!(
        idx.to_offset(Position::new(1, 4), ColumnEncoding::Utf8),
        Ok(e_offset + 1)
    );
    assert_eq!(
        idx.to_position(e_offset + 1, ColumnEncoding::Scalar),
        Err(PositionError::InvalidUtf8Boundary)
    );
    assert_eq!(
        idx.to_position(e_offset + 1, ColumnEncoding::Utf16),
        Err(PositionError::InvalidUtf8Boundary)
    );
    // Past end-of-line column on line 1 (ends right after `!`).
    assert_eq!(
        idx.to_offset(Position::new(1, 11), ColumnEncoding::Utf8),
        Err(PositionError::OutOfBounds)
    );
    assert_eq!(
        idx.to_offset(Position::new(1, 5), ColumnEncoding::Scalar),
        Err(PositionError::OutOfBounds)
    );
    assert_eq!(
        idx.to_offset(Position::new(1, 6), ColumnEncoding::Utf16),
        Err(PositionError::OutOfBounds)
    );

    // Block-boundary crossing test: `LineIndex::to_offset` counts 256-byte chunks before decoding.
    // Place 2-, 3-, and 4-byte UTF-8 characters straddling offsets 256 and 512.
    for prefix_len in [253_usize, 254, 255, 256, 257] {
        let mut long_line = "a".repeat(prefix_len);
        long_line.push_str("🚀é€");
        long_line.push_str(&"b".repeat(252));
        long_line.push_str("🚀z\n");
        let long_idx = LineIndex::new(&long_line);

        for (byte_offset, ch) in long_line.char_indices() {
            if ch == '\n' {
                break;
            }
            for encoding in [
                ColumnEncoding::Utf8,
                ColumnEncoding::Scalar,
                ColumnEncoding::Utf16,
            ] {
                let pos = long_idx
                    .to_position(byte_offset, encoding)
                    .expect("long line to_position");
                assert_eq!(
                    long_idx.to_offset(pos, encoding),
                    Ok(byte_offset),
                    "prefix={prefix_len} offset={byte_offset} ch={ch:?} {encoding:?}"
                );
                if encoding == ColumnEncoding::Utf16 && ch.len_utf16() == 2 {
                    assert_eq!(
                        long_idx.to_offset(Position::new(pos.line, pos.column + 1), encoding),
                        Err(PositionError::SplitSurrogate),
                        "prefix={prefix_len} split surrogate at {byte_offset}"
                    );
                }
            }
        }
    }
}

#[test]
fn spanned_tree_spans_and_entry_spans_are_exact() {
    let input = "\u{feff}service:\r\n  name: \"café 🚀\"\r\n  ports:\r\n    - 80\r\n    - 443\r\n  flow: {a: 1, b: [2, 3]}\r\n";
    let tree = SpannedTree::parse(input).expect("spanned parse");
    assert_eq!(tree.documents().len(), 1);

    let name_path = [
        PathSegment::Key(Value::from("service")),
        PathSegment::Key(Value::from("name")),
    ];
    let name_node = tree.lookup(&name_path).expect("service.name");
    assert!(name_node.is_scalar());
    assert_eq!(name_node.scalar_text(), Some("\"café 🚀\""));
    let name_span = name_node.span();
    assert_eq!(name_span.slice(input), Some("\"café 🚀\""));
    assert_eq!(tree.span_at(&name_path), Some(name_span));
    assert_eq!(tree.scalar_text_at(&name_path), Some("\"café 🚀\""));

    // Deleting `entry_span` removes only `name` and leaves `ports` and `flow` intact.
    let name_entry = name_node
        .entry_span()
        .expect("entry_span for mapping entry");
    assert!(name_entry.start() <= name_span.start() && name_span.end() <= name_entry.end());
    let without_name = format!(
        "{}{}",
        &input[..name_entry.start()],
        &input[name_entry.end()..]
    );
    let reparsed = ayaml::parse_value(&without_name).expect("reparse after entry deletion");
    assert_eq!(reparsed["service"].get("name"), None);
    assert_eq!(reparsed["service"]["ports"][0], Value::from(80_u64));
    assert_eq!(reparsed["service"]["ports"][1], Value::from(443_u64));

    // Sequence item span and entry deletion.
    let port0_path = [
        PathSegment::Key(Value::from("service")),
        PathSegment::Key(Value::from("ports")),
        PathSegment::Index(0),
    ];
    let port0 = tree.lookup(&port0_path).expect("ports[0]");
    assert_eq!(port0.scalar_text(), Some("80"));
    let port0_entry = port0.entry_span().expect("ports[0] entry_span");
    let without_port0 = format!(
        "{}{}",
        &input[..port0_entry.start()],
        &input[port0_entry.end()..]
    );
    let reparsed_ports = ayaml::parse_value(&without_port0).expect("reparse without port0");
    assert_eq!(
        reparsed_ports["service"]["ports"],
        Value::Sequence(vec![Value::from(443_u64)])
    );

    // Flow collection nested lookup.
    let flow_b1 = [
        PathSegment::Key(Value::from("service")),
        PathSegment::Key(Value::from("flow")),
        PathSegment::Key(Value::from("b")),
        PathSegment::Index(1),
    ];
    assert_eq!(tree.scalar_text_at(&flow_b1), Some("3"));
    assert!(
        tree.lookup(&[PathSegment::Key(Value::from("missing"))])
            .is_none()
    );
}

#[test]
fn emitter_indentation_options_produce_exact_layout_and_validate_bounds() {
    let mut nested = Mapping::new();
    nested.insert(Value::from("leaf"), Value::from("ok"));
    nested.insert(
        Value::from("items"),
        Value::Sequence(vec![
            Value::from("alpha"),
            Value::Sequence(vec![Value::from(1_u64), Value::from(2_u64)]),
        ]),
    );
    nested.insert(Value::from("notes"), Value::from("line1\nline2\n"));

    let mut root = Mapping::new();
    root.insert(Value::from("top"), Value::Mapping(nested));
    let value = Value::Mapping(root);

    // Every valid indent width 2..=9 under both `indent_sequences = false` and `true`.
    for indent in 2..=9_usize {
        for indent_sequences in [false, true] {
            let opts = EmitOptions::new()
                .with_indent(indent)
                .with_indent_sequences(indent_sequences);

            let rendered = ayaml::to_string_value_with(&value, &opts)
                .unwrap_or_else(|e| panic!("indent={indent} seq={indent_sequences}: {e}"));
            let reparsed = ayaml::parse_value(&rendered)
                .unwrap_or_else(|e| panic!("reparse indent={indent}: {e}\n{rendered}"));
            assert_eq!(
                reparsed, value,
                "round-trip failed for indent={indent} indent_sequences={indent_sequences}"
            );

            // Check exact leading-space indentation of nested mapping key `leaf:`.
            let leaf_line = rendered
                .lines()
                .find(|line| line.trim_start().starts_with("leaf:"))
                .expect("leaf line");
            let leaf_spaces = leaf_line.len() - leaf_line.trim_start_matches(' ').len();
            assert_eq!(
                leaf_spaces, indent,
                "expected {indent} spaces before `leaf:`, got {leaf_spaces} in:\n{rendered}"
            );

            // Check sequence indentation directly under `items:`:
            // `indent_sequences = false` places `-` at `indent`; `true` places `-` at `2 * indent`.
            let alpha_line = rendered
                .lines()
                .find(|line| {
                    line.trim_start().starts_with("- alpha")
                        || line.trim_start().starts_with("- \"alpha\"")
                })
                .expect("alpha sequence line");
            let seq_spaces = alpha_line.len() - alpha_line.trim_start_matches(' ').len();
            let expected_seq_spaces = if indent_sequences { indent * 2 } else { indent };
            assert_eq!(
                seq_spaces, expected_seq_spaces,
                "indent={indent} indent_sequences={indent_sequences}:\n{rendered}"
            );

            // `to_string_with` (Serde path) produces the same indentation width on `leaf:`.
            let serde_rendered = ayaml::to_string_with(&value, &opts).expect("to_string_with");
            assert_eq!(
                ayaml::parse_value(&serde_rendered).expect("reparse serde"),
                value
            );
            let serde_leaf = serde_rendered
                .lines()
                .find(|line| line.trim_start().starts_with("leaf:"))
                .expect("serde leaf");
            assert_eq!(
                serde_leaf.len() - serde_leaf.trim_start_matches(' ').len(),
                indent
            );

            // Literal scalar style also preserves indentation width and round-trips cleanly.
            let lit_opts = opts.with_scalar_style(Some(ScalarStyle::Literal));
            let lit_rendered =
                ayaml::to_string_value_with(&value, &lit_opts).expect("literal emit");
            assert_eq!(
                ayaml::parse_value(&lit_rendered).expect("reparse literal"),
                value
            );
        }
    }

    // Explicit document markers + flow layout override block indentation cleanly.
    let flow_opts = EmitOptions::flow()
        .with_explicit_start(true)
        .with_explicit_end(true)
        .with_indent(4);
    let flow_rendered = ayaml::to_string_value_with(&value, &flow_opts).expect("flow emit");
    assert!(flow_rendered.starts_with("---\n"));
    assert!(flow_rendered.ends_with("...\n"));
    assert_eq!(
        ayaml::parse_value(&flow_rendered).expect("reparse flow"),
        value
    );

    // Every invalid indent outside 2..=9 fails cleanly across all emitter entry points.
    let docs = vec![value.clone()];
    let sample_yaml = ayaml::to_string_value(&value).expect("default emit");
    let events = ayaml::parse_events(&sample_yaml)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .expect("events");

    for bad_indent in [0_usize, 1, 10, 100, usize::MAX] {
        let bad = EmitOptions::new().with_indent(bad_indent);
        let mut buf = Vec::new();
        let stream = ayaml::parse_events(&sample_yaml).expect("stream");
        let results = [
            ayaml::to_string_with(&value, &bad).map(drop),
            ayaml::to_string_value_with(&value, &bad).map(drop),
            ayaml::to_string_documents_with(&docs, &bad).map(drop),
            ayaml::to_string_events_with(&events, &bad).map(drop),
            ayaml::to_writer_with(&mut buf, &value, &bad),
            ayaml::to_writer_value_with(&mut buf, &value, &bad),
            ayaml::to_writer_documents_with(&mut buf, &docs, &bad),
            ayaml::to_writer_events_with(&mut buf, &events, &bad),
            ayaml::to_writer_event_stream_with(&mut buf, stream, &bad),
        ];
        for (i, result) in results.into_iter().enumerate() {
            let err = result.expect_err(&format!("bad_indent={bad_indent} entry={i}"));
            assert_eq!(
                err.kind(),
                ErrorKind::Emitter,
                "bad_indent={bad_indent} entry={i}"
            );
            assert_eq!(
                err.code(),
                ErrorCode::InvalidEmitterOptions,
                "bad_indent={bad_indent} entry={i}"
            );
        }
    }
}
