use std::collections::BTreeMap;

use serde::Deserialize;

use super::{NodeTag, OVERFLOW_SPAN_LEN, Tape, TapeKind, TapeNode};
use crate::{
    ErrorCode, ErrorKind, LimitKind, Limits, Number, ParseOptions, ParserBuf, Span, Spanned, Value,
    from_str, from_str_in, parse_documents, parse_documents_in, parse_tape, parse_tape_documents,
    parse_value, parse_value_in,
};

#[test]
fn tape_node_size_and_alignment() {
    assert_eq!(size_of::<TapeNode>(), 16);
    assert_eq!(align_of::<TapeNode>(), 8);
}

#[test]
fn scalar_and_collection_accessors_and_zero_copy_borrowing() {
    let input = concat!(
        "null_plain: null\n",
        "null_empty:\n",
        "bool_t: true\n",
        "bool_f: false\n",
        "pos: 42\n",
        "neg: -99\n",
        "wide_pos: 340282366920938463463374607431768211455\n",
        "wide_neg: -170141183460469231731687303715884105728\n",
        "flt: 3.5\n",
        "borrowed: hello\n",
        "quoted: \"world\"\n",
        "decoded: \"line1\\nline2\"\n",
        "seq: [10, 20, 30]\n",
        "tagged: !custom [1, 2]\n",
    );
    let tape = parse_tape(input).expect("parse tape");
    let value = parse_value(input).expect("parse value");
    assert_eq!(tape.to_value(), value);
    assert_eq!(tape.root().kind(), TapeKind::Mapping);
    assert_eq!(tape.root().len(), Some(14));
    assert_eq!(tape.root().is_empty(), Some(false));

    assert!(tape.get("null_plain").expect("null_plain").is_null());
    assert!(tape.get("null_empty").expect("null_empty").is_null());
    assert_eq!(tape.get("bool_t").and_then(|n| n.as_bool()), Some(true));
    assert_eq!(tape.get("bool_f").and_then(|n| n.as_bool()), Some(false));
    assert_eq!(tape.get("pos").and_then(|n| n.as_u64()), Some(42));
    assert_eq!(tape.get("pos").and_then(|n| n.as_i64()), Some(42));
    assert_eq!(tape.get("neg").and_then(|n| n.as_i64()), Some(-99));
    assert_eq!(tape.get("neg").and_then(|n| n.as_u64()), None);
    assert_eq!(
        tape.get("wide_pos").and_then(|n| n.as_u128()),
        Some(u128::MAX)
    );
    assert_eq!(
        tape.get("wide_neg").and_then(|n| n.as_i128()),
        Some(i128::MIN)
    );
    assert_eq!(
        tape.get("wide_pos").and_then(|n| n.as_number()),
        Some(Number::from_u128(u128::MAX))
    );
    assert_eq!(tape.get("flt").and_then(|n| n.as_f64()), Some(3.5));

    // Zero-copy borrowed string check (pointer lies inside `input`).
    let borrowed = tape.get("borrowed").expect("borrowed");
    let b_str = borrowed.as_borrowed_str().expect("borrowed str");
    assert_eq!(b_str, "hello");
    assert!(std::ptr::eq(
        b_str.as_ptr(),
        std::ptr::from_ref(&input.as_bytes()[input.find("hello").expect("find hello")])
    ));

    let quoted = tape.get("quoted").expect("quoted");
    assert_eq!(quoted.as_borrowed_str(), Some("world"));

    let decoded = tape.get("decoded").expect("decoded");
    assert_eq!(decoded.as_str(), Some("line1\nline2"));
    assert_eq!(decoded.as_borrowed_str(), None);

    // Sequence indexing and ExactSizeIterator.
    let seq = tape.get("seq").expect("seq");
    assert_eq!(seq.kind(), TapeKind::Sequence);
    assert_eq!(seq.len(), Some(3));
    assert_eq!(seq.get(1).and_then(|n| n.as_u64()), Some(20));
    let mut seq_iter = seq.as_sequence().expect("as_sequence");
    assert_eq!(seq_iter.len(), 3);
    assert_eq!(seq_iter.next().and_then(|n| n.as_u64()), Some(10));
    assert_eq!(seq_iter.len(), 2);

    // Tagged value.
    let tagged = tape.get("tagged").expect("tagged");
    assert_eq!(tagged.kind(), TapeKind::Tagged);
    let (tag, inner) = tagged.as_tagged().expect("as_tagged");
    assert_eq!(tag.to_string(), "!custom");
    assert_eq!(inner.kind(), TapeKind::Sequence);
    assert_eq!(inner.len(), Some(2));

    // Indexing by `Value`, `&str`, `String`, `usize`.
    assert_eq!(
        tape.get(String::from("pos")).and_then(|n| n.as_u64()),
        Some(42)
    );
    assert_eq!(
        tape.get(Value::from("pos")).and_then(|n| n.as_u64()),
        Some(42)
    );
    assert!(tape.get(0_usize).is_none());
}

#[test]
fn tape_ref_subnode_zero_copy_serde_outlives_tape() {
    #[derive(Debug, PartialEq, Deserialize)]
    struct Item<'a> {
        name: &'a str,
        count: u32,
        tag: Spanned<&'a str>,
    }

    let input = "items:\n  - name: alpha\n    count: 7\n    tag: v1\n";
    let decoded: Item<'_> = {
        let tape = parse_tape(input).expect("tape");
        let item_ref = tape
            .get("items")
            .and_then(|s| s.get(0))
            .expect("first item");
        Item::deserialize(item_ref).expect("deserialize from TapeRef")
    };
    // `tape` is dropped above, while `decoded` still holds `&'a str` slices into `input`!
    assert_eq!(decoded.name, "alpha");
    assert_eq!(decoded.count, 7);
    assert_eq!(*decoded.tag, "v1");
    assert_eq!(decoded.tag.span().slice(input), Some("v1"));
}

#[test]
fn tape_serde_matches_from_str_edge_cases() {
    #[derive(Debug, PartialEq, Deserialize)]
    enum Choice {
        Unit,
        Newtype(u32),
        Struct {
            x: i32,
        },
        #[serde(rename = "Tagged")]
        Tagged(String),
    }

    #[derive(Debug, PartialEq, Deserialize)]
    struct Edge<'a> {
        empty_str: String,
        empty_opt: Option<String>,
        explicit_null_opt: Option<String>,
        lead_zero_f64: f64,
        int_f64: f64,
        borrowed: &'a str,
        spanned_map: Spanned<BTreeMap<String, u32>>,
        unit_enum: Choice,
        newtype_enum: Choice,
        struct_enum: Choice,
    }

    let yaml = concat!(
        "empty_str:\n",
        "empty_opt:\n",
        "explicit_null_opt: null\n",
        "lead_zero_f64: 01\n",
        "int_f64: 9\n",
        "borrowed: raw_text\n",
        "spanned_map:\n",
        "  a: 1\n",
        "  b: 2\n",
        "unit_enum: Unit\n",
        "newtype_enum:\n",
        "  Newtype: 5\n",
        "struct_enum:\n",
        "  Struct:\n",
        "    x: -3\n",
    );

    let from_tape: Edge<'_> = Edge::deserialize(&parse_tape(yaml).expect("tape")).expect("de");
    let from_stream: Edge<'_> = crate::from_str_borrowed(yaml).expect("stream");
    assert_eq!(from_tape, from_stream);

    // Explicit `null` must fail when target expects `String`, matching `from_value`.
    let tape_null = parse_tape("null\n").expect("tape null");
    let err_tape = String::deserialize(&tape_null).expect_err("explicit null -> String");
    let err_val =
        crate::from_value::<String>(Value::Null).expect_err("from_value explicit null -> String");
    assert_eq!(err_tape.code(), err_val.code());

    // Radix ints into f64 must fail identically to `from_str`.
    for radix in ["0x1F\n", "0o7\n", "0b1\n"] {
        let tape = parse_tape(radix).expect("tape radix");
        let e1 = f64::deserialize(&tape).expect_err("tape radix f64");
        let e2 = from_str::<f64>(radix).expect_err("stream radix f64");
        assert_eq!(e1.code(), e2.code(), "{radix:?}");
        assert_eq!(e1.span(), e2.span(), "{radix:?}");
    }

    // Owned `Tape` `Deserializer` also works directly.
    let owned_map: BTreeMap<String, u32> =
        BTreeMap::deserialize(parse_tape("a: 1\nb: 2\n").expect("tape")).expect("owned de");
    assert_eq!(owned_map["a"], 1);
    assert_eq!(owned_map["b"], 2);
}

#[test]
fn aliases_anchors_merge_keys_and_multi_document_streams() {
    let aliased = concat!("base: &b\n", "  x: 1\n", "  y: [10, 20]\n", "copy: *b\n",);
    let tape = parse_tape(aliased).expect("aliased tape");
    let val = parse_value(aliased).expect("aliased value");
    assert_eq!(tape.to_value(), val);
    // Alias root carries use-site span (`*b`).
    let copy_ref = tape.get("copy").expect("copy");
    assert_eq!(copy_ref.span().slice(aliased), Some("*b"));

    // Opt-in merge keys.
    let merged_yaml = "base: &b\n  a: 1\n  b: 2\nchild:\n  <<: *b\n  b: 9\n";
    let opts = ParseOptions::new().with_merge_keys(true);
    let merged_tape = Tape::parse_with(merged_yaml, &opts).expect("merged tape");
    let merged_val = crate::parse_value_with(merged_yaml, &opts).expect("merged val");
    assert_eq!(merged_tape.to_value(), merged_val);
    assert_eq!(
        merged_tape
            .get("child")
            .and_then(|m| m.get("a"))
            .and_then(|n| n.as_u64()),
        Some(1)
    );
    assert_eq!(
        merged_tape
            .get("child")
            .and_then(|m| m.get("b"))
            .and_then(|n| n.as_u64()),
        Some(9)
    );

    // Multi-document stream.
    let multi = "---\na: 1\n---\n- x\n- y\n---\n";
    let docs = parse_tape_documents(multi).expect("tape docs");
    assert_eq!(docs.len(), 3);
    assert!(!docs.is_empty());
    assert_eq!(docs.to_values(), parse_documents(multi).expect("val docs"));
    assert_eq!(
        docs.get(0)
            .and_then(|d| d.get("a"))
            .and_then(|n| n.as_u64()),
        Some(1)
    );
    assert_eq!(
        docs.get(1)
            .and_then(|d| d.get(1))
            .and_then(|n| n.as_borrowed_str()),
        Some("y")
    );
    assert!(docs.get(2).expect("doc 2").is_null());
}

#[test]
fn duplicate_keys_small_large_nested_and_complex() {
    use std::fmt::Write as _;

    // Small mapping duplicate.
    let err = parse_tape("a: 1\nb: 2\na: 3\n").expect_err("small dup");
    assert_eq!(err.kind(), ErrorKind::DuplicateKey);
    assert_eq!(err.code(), ErrorCode::DuplicateKey);

    // Large mapping (> 12 keys) promotion + duplicate.
    let mut big = String::new();
    for i in 0..25 {
        let _ = writeln!(big, "k{i}: {i}");
    }
    assert!(parse_tape(&big).is_ok());
    let mut big_dup = big.clone();
    big_dup.push_str("k0: 999\n");
    let err = parse_tape(&big_dup).expect_err("large dup");
    assert_eq!(err.code(), ErrorCode::DuplicateKey);

    // Complex mapping keys with differing entry order compare equal (set semantics).
    let complex_dup = "? {a: 1, b: 2}\n: first\n? {b: 2, a: 1}\n: second\n";
    let err = parse_tape(complex_dup).expect_err("complex mapping key dup");
    assert_eq!(err.code(), ErrorCode::DuplicateKey);

    // Complex sequence keys.
    let seq_dup = "? [1, 2]\n: x\n? [1, 2]\n: y\n";
    let err = parse_tape(seq_dup).expect_err("complex seq key dup");
    assert_eq!(err.code(), ErrorCode::DuplicateKey);

    // Nested large mappings.
    let mut nested = String::new();
    for i in 0..15 {
        let _ = writeln!(nested, "o{i}: {i}");
    }
    nested.push_str("inner:\n");
    for i in 0..16 {
        let _ = writeln!(nested, "  o{i}: {i}");
    }
    nested.push_str("tail: ok\n");
    let tape = parse_tape(&nested).expect("nested large valid");
    assert_eq!(tape.get("tail").and_then(|n| n.as_str()), Some("ok"));
}

#[test]
fn parser_buf_reuse_recycle_outlier_clamping_and_panic_safety() {
    struct PanickingTarget;
    impl<'de> Deserialize<'de> for PanickingTarget {
        fn deserialize<D: serde::Deserializer<'de>>(_: D) -> Result<Self, D::Error> {
            panic!("intentional test panic inside Deserialize");
        }
    }

    let mut buf = ParserBuf::new();

    // 1. Bulk reuse across tape, value, documents, and from_str.
    for i in 0..20 {
        let doc = format!("id: {i}\nname: item_{i}\ntags: [a, b, c]\n");
        let tape = buf.parse_tape(&doc).expect("buf tape");
        assert_eq!(tape.get("id").and_then(|n| n.as_u64()), Some(i));
        let as_val = tape.to_value();
        tape.recycle_into(&mut buf);

        let val = parse_value_in(&doc, &mut buf).expect("buf value");
        assert_eq!(val, as_val);

        let docs = parse_documents_in(&doc, &mut buf).expect("buf docs");
        assert_eq!(docs, vec![as_val.clone()]);

        let typed: Value = from_str_in(&doc, &mut buf).expect("buf from_str");
        assert_eq!(typed, as_val);
    }

    // 2. Multi-doc recycle_into works cleanly.
    let multi = "---\na: 1\n---\nb: 2\n";
    let tdocs = buf.parse_tape_documents(multi).expect("buf multi");
    assert_eq!(tdocs.len(), 2);
    tdocs.recycle_into(&mut buf);

    // 3. Outlier document (> 32,768 nodes & > 64 KiB decoded) followed by retention clamping.
    #[cfg(not(miri))]
    {
        use std::fmt::Write as _;

        let mut huge = String::new();
        for i in 0..18_000 {
            let _ = writeln!(huge, "k{i}: \"val_{i}_padding_text\\n\"");
        }
        let big_tape = buf.parse_tape(&huge).expect("big tape");
        assert_eq!(big_tape.root().len(), Some(18_000));
        big_tape.recycle_into(&mut buf);
        let (tok_cap, line_cap, node_cap, decoded_cap) = buf.retained_capacities();
        assert!(tok_cap <= 4_096, "tokens cap clamped: {tok_cap}");
        assert!(line_cap <= 1_024, "lines cap clamped: {line_cap}");
        assert_eq!(node_cap, 0, "outlier nodes cap dropped: {node_cap}");
        assert_eq!(decoded_cap, 0, "outlier decoded cap dropped: {decoded_cap}");
    }

    // 4. `shrink_to_fit` releases all retained buffers.
    buf.shrink_to_fit();
    assert_eq!(buf.retained_capacities(), (0, 0, 0, 0));

    // 5. Panic safety: panicking inside a custom `Deserialize` leaves `ParserBuf` completely
    //    clean and reusable for subsequent parses.
    let panic_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(buf.from_str::<PanickingTarget>("a: 1\nb: 2\n"));
    }));
    assert!(panic_res.is_err(), "expected panic");

    // Immediately reuse `buf` across all paths without corruption.
    let recovered: BTreeMap<String, u32> =
        buf.from_str("x: 10\ny: 20\n").expect("recover from_str");
    assert_eq!(recovered["x"], 10);
    assert_eq!(recovered["y"], 20);
    let recovered_tape = buf.parse_tape("ok: true\n").expect("recover tape");
    assert_eq!(
        recovered_tape.get("ok").and_then(|n| n.as_bool()),
        Some(true)
    );
    recovered_tape.recycle_into(&mut buf);
}

#[test]
fn overflow_span_side_table_lookup() {
    // Directly exercise the >16 MiB span sentinel without allocating 16 MiB in unit tests.
    let node = TapeNode::new(u32::MAX, OVERFLOW_SPAN_LEN, NodeTag::EmptyNull, 0, 0);
    assert_eq!(node.span_len_bits(), OVERFLOW_SPAN_LEN);
    assert_eq!(node.tag(), NodeTag::EmptyNull);

    // Also verify limits check on Tape::parse_with.
    let opts = ParseOptions::new().with_limits(Limits::new().with_max_depth(2));
    let err = Tape::parse_with("[[[1]]]\n", &opts).expect_err("depth limit");
    assert_eq!(err.kind(), ErrorKind::Limit);
    assert_eq!(
        err.limit().map(crate::LimitExceeded::kind),
        Some(LimitKind::Depth)
    );

    // Empty / comment-only document yields EmptyNull root with end-of-input span.
    let empty_input = "# only a comment\n";
    let empty_tape = parse_tape(empty_input).expect("empty tape");
    assert!(empty_tape.root().is_null());
    assert_eq!(empty_tape.root().span(), Span::empty(empty_input.len()));
    let empty_docs = parse_tape_documents(empty_input).expect("empty docs");
    assert!(empty_docs.is_empty());
}

#[test]
fn audit_regression_empty_anchors_and_large_map_error_reuse() {
    use std::fmt::Write as _;

    // Empty anchored scalars at/near start of input must never underflow in anchor_to_token_text.
    for input in ["a:\n  &x\n", "# hi\n&x\n", "&x\n", "&longanchor\n"] {
        let v_stream: Value = from_str(input).expect("from_str empty anchor");
        let v_parse = parse_value(input).expect("parse_value empty anchor");
        let tape = parse_tape(input).expect("parse_tape empty anchor");
        assert_eq!(v_stream, v_parse, "{input:?}");
        assert_eq!(tape.to_value(), v_parse, "{input:?}");
        let mut buf = ParserBuf::new();
        let v_buf: Value = buf.from_str(input).expect("buf.from_str empty anchor");
        assert_eq!(v_buf, v_parse, "{input:?}");
        #[cfg(feature = "libyaml-compat")]
        {
            let v_compat: Value =
                crate::compat::from_str(input).expect("compat::from_str empty anchor");
            assert_eq!(v_compat, v_parse, "{input:?}");
        }
    }

    // Error inside a promoted large mapping (>12 keys) must reset seen_large_top depth
    // sentinel to 0 so ParserBuf reuses the warm table cleanly on subsequent large mappings.
    let mut buf = ParserBuf::new();
    let mut bad_large = String::new();
    for i in 0..16 {
        let _ = writeln!(bad_large, "k{i}: {i}");
    }
    bad_large.push_str("k0: dup\n");
    let dup_err = buf.parse_tape(&bad_large).expect_err("large map duplicate");
    assert_eq!(dup_err.code(), ErrorCode::DuplicateKey);

    let mut good_large = String::new();
    for i in 0..20 {
        let _ = writeln!(good_large, "m{i}: {i}");
    }
    for _ in 0..3 {
        let t = buf
            .parse_tape(&good_large)
            .expect("valid large after error");
        assert_eq!(t.root().len(), Some(20));
        assert_eq!(t.get("m19").and_then(|n| n.as_u64()), Some(19));
        t.recycle_into(&mut buf);
    }
}

#[test]
fn audit_regression_tape_serde_parity_and_overflow_spans() {
    #[derive(Debug, PartialEq, Deserialize)]
    struct OnlyA {
        a: u32,
    }
    #[derive(Debug, PartialEq, Deserialize)]
    enum TagEnum {
        Foo(u32),
        Unit,
        Pair(i32, i32),
    }
    #[derive(Debug, PartialEq, Deserialize)]
    struct EmptyTuple();
    #[derive(Debug, PartialEq, Deserialize)]
    struct WrapEmptyTuple {
        a: EmptyTuple,
    }
    #[derive(Debug, PartialEq, Deserialize)]
    struct OptWrap(Option<i32>);
    #[derive(Debug, PartialEq, Deserialize)]
    enum BareNewtype {
        V(OptWrap),
    }
    #[derive(Debug, PartialEq, Deserialize)]
    struct SpannedAliasDoc {
        base: Spanned<Spanned<u32>>,
        alias: Option<Spanned<u32>>,
    }
    struct BadMapVisitor;
    impl<'de> serde::de::Visitor<'de> for BadMapVisitor {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("map")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            let _: () = map.next_value()?;
            Ok(())
        }
    }

    // Unresolved core tags (`!!binary`, `!!unknown`) must be rejected identically to from_str.
    for (label, input, err_tape, err_stream) in [
        (
            "!!binary seq",
            "!!binary [1, 2]\n",
            parse_tape("!!binary [1, 2]\n")
                .expect("tape")
                .deserialize::<Vec<u8>>()
                .expect_err("tape !!binary"),
            from_str::<Vec<u8>>("!!binary [1, 2]\n").expect_err("stream !!binary"),
        ),
        (
            "!!unknown struct",
            "!!unknown {a: 1}\n",
            parse_tape("!!unknown {a: 1}\n")
                .expect("tape")
                .deserialize::<OnlyA>()
                .expect_err("tape !!unknown struct"),
            from_str::<OnlyA>("!!unknown {a: 1}\n").expect_err("stream !!unknown struct"),
        ),
        (
            "!!unknown int",
            "!!unknown 42\n",
            parse_tape("!!unknown 42\n")
                .expect("tape")
                .deserialize::<i32>()
                .expect_err("tape !!unknown int"),
            from_str::<i32>("!!unknown 42\n").expect_err("stream !!unknown int"),
        ),
    ] {
        assert_eq!(err_tape.code(), err_stream.code(), "{label} ({input:?})");
        assert!(
            err_tape.to_string().contains("tagged value"),
            "{label}: unexpected message: {err_tape}"
        );
    }

    // Custom-tagged enum mapping key `{!Custom Foo: 1}` must fail with InvalidEnumRepresentation.
    let e_tape = parse_tape("{!Custom Foo: 1}\n")
        .expect("tape")
        .deserialize::<TagEnum>()
        .expect_err("custom tagged enum key");
    let e_stream = from_str::<TagEnum>("{!Custom Foo: 1}\n").expect_err("stream tagged enum key");
    assert_eq!(e_tape.code(), ErrorCode::InvalidEnumRepresentation);
    assert_eq!(e_tape.code(), e_stream.code());

    // Tuples, 0-field tuple structs, and tuple_variant reject EmptyNull as InvalidType.
    let et_tape = parse_tape("a:\n")
        .expect("tape")
        .get("a")
        .expect("a")
        .deserialize::<EmptyTuple>()
        .expect_err("empty tuple on EmptyNull");
    let et_stream = from_str::<WrapEmptyTuple>("a:\n").expect_err("stream empty tuple");
    assert_eq!(et_tape.code(), ErrorCode::InvalidType);
    assert_eq!(et_tape.code(), et_stream.code());

    let tv_tape = parse_tape("!Pair\n")
        .expect("tape")
        .deserialize::<TagEnum>()
        .expect_err("tuple variant on EmptyNull");
    let tv_stream = from_str::<TagEnum>("!Pair\n").expect_err("stream tuple variant");
    assert_eq!(tv_tape.code(), ErrorCode::InvalidType);
    assert_eq!(tv_tape.code(), tv_stream.code());

    // Bare enum variant wrapping a newtype Option<i32> succeeds via NothingDeserializer.
    let bn_tape: BareNewtype = parse_tape("V\n")
        .expect("tape")
        .deserialize()
        .expect("bare newtype");
    let bn_stream: BareNewtype = from_str("V\n").expect("stream bare newtype");
    assert_eq!(bn_tape, bn_stream);
    assert_eq!(bn_tape, BareNewtype::V(OptWrap(None)));

    // Unit-variant error code, check_consumed InvalidLength, and collection-key `[?]` path.
    let uv_tape = parse_tape("Unit: 123\n")
        .expect("tape")
        .deserialize::<TagEnum>()
        .expect_err("non-null unit variant");
    let uv_stream = from_str::<TagEnum>("Unit: 123\n").expect_err("stream non-null unit variant");
    assert_eq!(uv_tape.code(), ErrorCode::InvalidEnumRepresentation);
    assert_eq!(uv_tape.code(), uv_stream.code());

    let tl_tape = parse_tape("[1, 2, 3]\n")
        .expect("tape")
        .deserialize::<(u32, u32)>()
        .expect_err("trailing tuple elements");
    let tl_stream = from_str::<(u32, u32)>("[1, 2, 3]\n").expect_err("stream trailing tuple");
    assert_eq!(tl_tape.code(), ErrorCode::InvalidLength);
    assert_eq!(tl_tape.code(), tl_stream.code());

    let ck_yaml = "? [1, 2]\n: bad_int\n";
    let ck_tape = parse_tape(ck_yaml)
        .expect("tape")
        .deserialize::<BTreeMap<Vec<u32>, u32>>()
        .expect_err("collection key error");
    let ck_stream =
        from_str::<BTreeMap<Vec<u32>, u32>>(ck_yaml).expect_err("stream collection key error");
    assert_eq!(ck_tape.to_string(), ck_stream.to_string());
    assert!(
        ck_tape.to_string().contains("[?]"),
        "expected `[?]` in collection-key error path: {ck_tape}"
    );

    // MapValueBeforeKey error code when a misbehaving visitor asks for value before key.
    let mv_tape = serde::Deserializer::deserialize_map(
        parse_tape("a: 1\n").expect("tape").root(),
        BadMapVisitor,
    )
    .expect_err("map value before key");
    assert_eq!(mv_tape.code(), ErrorCode::MapValueBeforeKey);
    let mv_empty = serde::Deserializer::deserialize_map(
        parse_tape("a:\n").expect("tape").get("a").expect("a"),
        BadMapVisitor,
    )
    .expect_err("empty map value before key");
    assert_eq!(mv_empty.code(), ErrorCode::MapValueBeforeKey);

    // Empty stream `""` -> String fails; `!!null ""` -> String succeeds; `!!int "0x10"` -> f64 succeeds.
    assert_eq!(
        parse_tape("")
            .expect("tape")
            .deserialize::<String>()
            .expect_err("empty stream -> String")
            .code(),
        from_str::<String>("")
            .expect_err("stream empty -> String")
            .code()
    );
    assert_eq!(
        parse_tape("!!null \"\"\n")
            .expect("tape")
            .deserialize::<String>()
            .expect("!!null empty -> String"),
        from_str::<String>("!!null \"\"\n").expect("stream !!null empty")
    );
    let q_radix_tape: f64 = parse_tape("!!int \"0x10\"\n")
        .expect("tape")
        .deserialize()
        .expect("quoted !!int 0x10 -> f64");
    let q_radix_stream: f64 = from_str("!!int \"0x10\"\n").expect("stream quoted !!int 0x10");
    assert_eq!(q_radix_tape.to_bits(), q_radix_stream.to_bits());

    // Nested Spanned<Spanned<T>> and Option<Spanned<T>> on aliases.
    let sa_yaml = "base: &b 42\nalias: *b\n";
    let sa_tape: SpannedAliasDoc = parse_tape(sa_yaml)
        .expect("tape")
        .deserialize()
        .expect("de");
    let sa_stream: SpannedAliasDoc = from_str(sa_yaml).expect("stream");
    assert_eq!(sa_tape, sa_stream);
    assert_eq!(
        sa_tape.alias.as_ref().and_then(|s| s.span().slice(sa_yaml)),
        Some("*b")
    );
}

#[test]
fn audit_regression_root_deserializer_int_indexing_and_overflow_alias() {
    // Owned `Tape` and `&Tape` Deserializer impls must forward bool and integer targets to
    // `root()` so non-core tags (`!port 80`, `!flag true`) succeed and core tags fail consistently.
    let tagged_port = parse_tape("!port 80\n").expect("tagged port");
    assert_eq!(tagged_port.deserialize::<u16>().expect("method"), 80);
    assert_eq!(u16::deserialize(&tagged_port).expect("&Tape"), 80);
    assert_eq!(u16::deserialize(tagged_port).expect("owned Tape"), 80);

    let tagged_flag = parse_tape("!flag true\n").expect("tagged flag");
    assert!(bool::deserialize(&tagged_flag).expect("&Tape bool"));
    assert!(bool::deserialize(tagged_flag).expect("owned Tape bool"));

    let unknown_core_int = parse_tape("!!unknown 42\n").expect("unknown core int");
    let err_ref = i32::deserialize(&unknown_core_int).expect_err("&Tape !!unknown");
    let err_owned = i32::deserialize(unknown_core_int).expect_err("owned Tape !!unknown");
    assert!(err_ref.to_string().contains("tagged value"));
    assert_eq!(err_ref.code(), err_owned.code());

    // `usize` indexing matches `Value::get(usize)` on mappings with integer keys, and `Debug`
    // formats scalar `TapeRef` values without `Some(...)` wrappers.
    let int_key_tape = parse_tape("{0: hello, 1: true}\n").expect("int key map");
    assert_eq!(
        int_key_tape.get(0_usize).and_then(|n| n.as_str()),
        Some("hello")
    );
    assert_eq!(
        format!("{:?}", int_key_tape.get(0_usize).expect("0")),
        "String(\"hello\")"
    );
    assert_eq!(
        format!("{:?}", int_key_tape.get(1_usize).expect("1")),
        "Bool(true)"
    );

    // Overflow span (>16 MiB) both outside AND inside an anchored collection copied via alias
    // preserves definition and use-site spans across `overflow_spans` rebasing.
    #[cfg(not(miri))]
    {
        let mut big_input = String::with_capacity(16 * 1024 * 1024 + 128);
        big_input.push_str("base: &b [\"");
        big_input.extend(std::iter::repeat_n('x', 16 * 1024 * 1024));
        big_input.push_str("\", 2]\ncopy: *b\n");
        let big_opts = ParseOptions::new().with_limits(
            Limits::new()
                .with_max_input_bytes(20 * 1024 * 1024)
                .with_max_scalar_bytes(20 * 1024 * 1024)
                .with_max_alias_expansion_bytes(40 * 1024 * 1024),
        );
        let big_tape = Tape::parse_with(&big_input, &big_opts).expect("overflow span alias tape");
        let copy_node = big_tape.get("copy").expect("copy");
        assert_eq!(copy_node.span().slice(&big_input), Some("*b"));
        let elem0 = copy_node.get(0).expect("elem 0");
        assert_eq!(elem0.as_str().map(str::len), Some(16 * 1024 * 1024));
        assert_eq!(elem0.span().len(), 16 * 1024 * 1024 + 2);
        let elem1 = copy_node.get(1).expect("elem 1");
        assert_eq!(elem1.as_u64(), Some(2));
        assert_eq!(elem1.span().slice(&big_input), Some("2"));
    }
}
