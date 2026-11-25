//! Integration and property tests for `ayaml-edit`.

use ayaml::{PathSegment, Span, Value, parse_value};
use ayaml_edit::{
    EditErrorKind, EditOp, TextEdit, apply_batch, apply_edits, remove_entry, replace_scalar,
};
use proptest::prelude::*;

fn key(name: &str) -> PathSegment {
    PathSegment::key(name)
}

fn index(at: usize) -> PathSegment {
    PathSegment::index(at)
}

#[test]
fn replace_scalar_preserves_bom_crlf_comments_and_neighbors() {
    let input = "\u{feff}# header\r\nservice:\r\n  port: 8080 # default\r\n  host: localhost\r\n";
    let edited = replace_scalar(
        input,
        &[key("service"), key("port")],
        &Value::from(9090_i64),
    )
    .expect("verified scalar edit");
    assert_eq!(
        edited.output(),
        "\u{feff}# header\r\nservice:\r\n  port: 9090 # default\r\n  host: localhost\r\n"
    );
    assert_eq!(edited.edits().len(), 1);
    assert_eq!(
        apply_edits(input, edited.edits()).expect("apply_edits"),
        edited.output()
    );
}

#[test]
fn replace_scalar_quotes_ambiguous_strings_and_updates_flow_values() {
    let input = "flags: [true, off]\nname: alpha\n";
    let edited = replace_scalar(input, &[key("name")], &Value::from("true"))
        .expect("ambiguous string replacement");
    assert_eq!(
        parse_value(edited.output()).unwrap()["name"],
        Value::from("true")
    );

    let flow = replace_scalar(input, &[key("flags"), index(0)], &Value::from(false))
        .expect("flow scalar replacement");
    assert_eq!(flow.output(), "flags: [false, off]\nname: alpha\n");

    // Replacement strings containing flow indicators (`,`, `[`, `]`, `{`, `}`) are quoted cleanly
    // inside flow sequences and mappings.
    for replacement in ["a, b", "arr[0]", "{id}", "a]b"] {
        let seq_edit = replace_scalar(
            "items: [old]\n",
            &[key("items"), index(0)],
            &Value::from(replacement),
        )
        .unwrap_or_else(|e| panic!("flow seq replacement {replacement:?} failed: {e}"));
        assert_eq!(
            parse_value(seq_edit.output()).unwrap()["items"][0],
            Value::from(replacement)
        );
        let map_edit = replace_scalar(
            "m: {k: old}\n",
            &[key("m"), key("k")],
            &Value::from(replacement),
        )
        .unwrap_or_else(|e| panic!("flow map replacement {replacement:?} failed: {e}"));
        assert_eq!(
            parse_value(map_edit.output()).unwrap()["m"]["k"],
            Value::from(replacement)
        );
    }
}

#[test]
fn shared_edit_order_vectors_pass_and_edge_cases_are_rejected() {
    // Run every shared format-contract vector in tests/vectors/edit-order.json.
    let vectors = include_str!("../../../tests/vectors/edit-order.json");
    let parsed = parse_value(vectors).expect("edit-order.json parses");
    let cases = parsed["cases"].as_sequence().expect("cases sequence");
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let source = case["source"].as_str().expect("source");
        let expected = case["expected"].as_str().expect("expected");
        let edits: Vec<TextEdit> = case["edits"]
            .as_sequence()
            .expect("edits")
            .iter()
            .map(|e| {
                let start = usize::try_from(e["range"]["start"].as_u64().expect("start")).unwrap();
                let end = usize::try_from(e["range"]["end"].as_u64().expect("end")).unwrap();
                let repl = e["replacement"].as_str().expect("replacement");
                TextEdit::new(Span::new(start, end), repl)
            })
            .collect();
        let actual = apply_edits(source, &edits)
            .unwrap_or_else(|err| panic!("vector {name:?} failed: {err}"));
        assert_eq!(actual, expected, "vector {name:?}");
    }

    // Compact first key with an intervening comment refuses removal instead of deleting the comment.
    let with_comment = "- a: 1\n  # comment about b\n  b: 2\n";
    assert_eq!(
        remove_entry(with_comment, &[index(0), key("a")])
            .expect_err("intervening comment must not be deleted")
            .kind(),
        EditErrorKind::Unsupported
    );

    // Empty batches still reject duplicate keys, syntax errors, and tagged documents.
    assert_eq!(
        apply_batch("a: 1\na: 2\n", &[])
            .expect_err("dup key on empty batch")
            .kind(),
        EditErrorKind::Parse
    );
    assert_eq!(
        apply_batch("a: [1\n", &[])
            .expect_err("syntax error on empty batch")
            .kind(),
        EditErrorKind::Parse
    );
    assert_eq!(
        apply_batch("!!int bad\n", &[])
            .expect_err("tagged doc on empty batch")
            .kind(),
        EditErrorKind::Unsupported
    );

    // Merge-key expansion is refused conservatively in EditOptions.
    let merge_opts = ayaml_edit::EditOptions::new()
        .with_parse_options(ayaml::ParseOptions::new().with_merge_keys(true));
    assert_eq!(
        ayaml_edit::remove_entry_with(
            "use:\n  <<: { x: 1 }\n  z: 3\n",
            &[key("use"), key("x")],
            &merge_opts
        )
        .expect_err("merge_keys rejected in ayaml-edit")
        .kind(),
        EditErrorKind::Unsupported
    );
}

#[test]
fn remove_entry_handles_block_mappings_compact_first_keys_and_sequences() {
    let input = "\u{feff}# keep\r\nitems:\r\n  - name: first\r\n    role: web\r\n  - name: second\r\n    role: worker\r\n";
    let compact = remove_entry(input, &[key("items"), index(0), key("name")])
        .expect("compact first key removal");
    assert_eq!(
        compact.output(),
        "\u{feff}# keep\r\nitems:\r\n  - role: web\r\n  - name: second\r\n    role: worker\r\n"
    );
    let item = remove_entry(input, &[key("items"), index(1)]).expect("sequence item removal");
    assert_eq!(
        item.output(),
        "\u{feff}# keep\r\nitems:\r\n  - name: first\r\n    role: web\r\n"
    );
}

#[test]
fn apply_batch_sorts_non_overlapping_edits_and_apply_edits_orders_insertions_first() {
    let input = "a: 1\nb: 2\nc: 3\n";
    let edited = apply_batch(
        input,
        &[
            EditOp::RemoveEntry {
                path: vec![key("c")],
            },
            EditOp::ReplaceScalar {
                path: vec![key("a")],
                value: Value::from(10_i64),
            },
        ],
    )
    .expect("batch edit");
    assert_eq!(edited.output(), "a: 10\nb: 2\n");
    assert_eq!(edited.edits().len(), 2);
    assert!(edited.edits()[0].span().end() <= edited.edits()[1].span().start());

    // Batch removing an earlier sequence item while replacing a later one stays index-safe.
    let seq = "items:\n  - 1\n  - 2\n  - 3\n";
    let seq_edited = apply_batch(
        seq,
        &[
            EditOp::RemoveEntry {
                path: vec![key("items"), index(0)],
            },
            EditOp::ReplaceScalar {
                path: vec![key("items"), index(2)],
                value: Value::from(30_i64),
            },
        ],
    )
    .expect("sequence batch edit");
    assert_eq!(seq_edited.output(), "items:\n  - 2\n  - 30\n");

    // Insertion before replacement at the exact same offset is accepted; reversed order is rejected.
    let ins = TextEdit::new(Span::empty(3), "x");
    let rep = TextEdit::new(Span::new(3, 4), "9");
    assert_eq!(
        apply_edits("a: 1\n", &[ins.clone(), rep.clone()]).expect("insertion first"),
        "a: x9\n"
    );
    assert_eq!(
        apply_edits("a: 1\n", &[rep, ins]).unwrap_err().kind(),
        EditErrorKind::InvalidEdit
    );
}

#[test]
fn conservative_refusals_reject_anchors_aliases_tags_block_scalars_and_multi_doc() {
    for (input, path) in [
        ("a: &x 1\nb: 2\n", vec![key("b")]),
        ("a: &x 1\nb: *x\n", vec![key("b")]),
        ("a: !custom 1\nb: 2\n", vec![key("b")]),
        ("a: |\n  hello\nb: 2\n", vec![key("b")]),
        ("a: 1\n---\nb: 2\n", vec![key("a")]),
    ] {
        assert_eq!(
            replace_scalar(input, &path, &Value::from(9_i64))
                .unwrap_err()
                .kind(),
            EditErrorKind::Unsupported,
            "expected Unsupported for replace_scalar on {input:?}"
        );
        assert_eq!(
            remove_entry(input, &path).unwrap_err().kind(),
            EditErrorKind::Unsupported,
            "expected Unsupported for remove_entry on {input:?}"
        );
    }

    // Flow entries and single-entry collections cannot be removed.
    assert_eq!(
        remove_entry("x: {a: 1, b: 2}\n", &[key("x"), key("a")])
            .unwrap_err()
            .kind(),
        EditErrorKind::Unsupported
    );
    assert_eq!(
        remove_entry("only: 1\n", &[key("only")])
            .unwrap_err()
            .kind(),
        EditErrorKind::Unsupported
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn verified_edits_always_parse_and_preserve_unrelated_values(
        v0 in "[a-z][a-z0-9_-]{0,12}",
        v1 in -10_000_i64..10_000_i64,
        v2 in any::<bool>(),
        new_str in "[a-zA-Z0-9 _:-]{0,16}",
        crlf in any::<bool>(),
        bom in any::<bool>(),
    ) {
        let nl = if crlf { "\r\n" } else { "\n" };
        let prefix = if bom { "\u{feff}" } else { "" };
        let input = format!("{prefix}# keep{nl}k0: {v0}{nl}k1: {v1}{nl}k2: {v2}{nl}");
        let original = parse_value(&input).expect("valid generated input");

        if let Ok(verified) = replace_scalar(&input, &[key("k0")], &Value::from(new_str.clone())) {
            let reparsed = parse_value(verified.output()).expect("output must parse");
            prop_assert_eq!(&reparsed["k0"], &Value::from(new_str));
            prop_assert_eq!(&reparsed["k1"], &original["k1"]);
            prop_assert_eq!(&reparsed["k2"], &original["k2"]);
            prop_assert_eq!(
                apply_edits(&input, verified.edits()).expect("edits apply"),
                verified.output()
            );
            if bom {
                let starts_bom = verified.output().starts_with('\u{feff}');
                prop_assert!(starts_bom, "BOM must be preserved");
            }
        }

        if let Ok(removed) = remove_entry(&input, &[key("k1")]) {
            let reparsed = parse_value(removed.output()).expect("output must parse");
            prop_assert_eq!(&reparsed["k0"], &original["k0"]);
            prop_assert_eq!(&reparsed["k2"], &original["k2"]);
            prop_assert!(reparsed.as_mapping().unwrap().get("k1").is_none());
            prop_assert_eq!(
                apply_edits(&input, removed.edits()).expect("edits apply"),
                removed.output()
            );
        }
    }
}
