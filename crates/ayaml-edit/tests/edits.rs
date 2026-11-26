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
    // Run every shared format-contract vector in tests/vectors/edit-order.json when running inside
    // the repository; skip only the external vector file when unpacked from a published `.crate`.
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let vectors_path = manifest_dir.join("../../tests/vectors/edit-order.json");
    if vectors_path.is_file() {
        let vectors = std::fs::read_to_string(&vectors_path)
            .unwrap_or_else(|err| panic!("reading {}: {err}", vectors_path.display()));
        let parsed = parse_value(&vectors).expect("edit-order.json parses");
        let cases = parsed["cases"].as_sequence().expect("cases sequence");
        assert_ne!(cases.len(), 0);
        for case in cases {
            let name = case["name"].as_str().expect("name");
            let source = case["source"].as_str().expect("source");
            let expected = case["expected"].as_str().expect("expected");
            let edits: Vec<TextEdit> = case["edits"]
                .as_sequence()
                .expect("edits")
                .iter()
                .map(|e| {
                    let start =
                        usize::try_from(e["range"]["start"].as_u64().expect("start")).unwrap();
                    let end = usize::try_from(e["range"]["end"].as_u64().expect("end")).unwrap();
                    let repl = e["replacement"].as_str().expect("replacement");
                    TextEdit::new(Span::new(start, end), repl)
                })
                .collect();
            let actual = apply_edits(source, &edits)
                .unwrap_or_else(|err| panic!("vector {name:?} failed: {err}"));
            assert_eq!(actual, expected, "vector {name:?}");
        }
    } else {
        assert!(
            manifest_dir.join("Cargo.toml.orig").is_file(),
            "{} is missing outside an unpacked package",
            vectors_path.display()
        );
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

#[test]
fn batch_verification_handles_pareto_mixed_ops_large_batches_limits_and_masked_errors() {
    use ayaml::{Limits, ParseOptions};
    use ayaml_edit::{EditOptions, apply_batch_with, replace_scalar_with};
    use std::error::Error as _;
    use std::fmt::Write as _;

    // 1. Pareto-distinct DP case: two paths reach the same alignment point with
    //    `(removals=1, replaces=2)` vs `(removals=2, replaces=0)`, followed by two scalar
    //    replacements. The old single-tuple lexicographic DP discarded `(2, 0)` and falsely
    //    returned `VerificationFailed`; exact Pareto tracking accepts.
    let pareto_doc = "\
items:
  - A
  - X
  - B
  - Y
  - C
  - old1
  - old2
";
    let edited = apply_batch(
        pareto_doc,
        &[
            EditOp::RemoveEntry {
                path: vec![key("items"), index(1)],
            },
            EditOp::RemoveEntry {
                path: vec![key("items"), index(3)],
            },
            EditOp::ReplaceScalar {
                path: vec![key("items"), index(5)],
                value: Value::from("new1"),
            },
            EditOp::ReplaceScalar {
                path: vec![key("items"), index(6)],
                value: Value::from("new2"),
            },
        ],
    )
    .expect("Pareto-incomparable mixed remove/replace batch must verify");
    assert_eq!(
        edited.output(),
        "items:\n  - A\n  - B\n  - C\n  - new1\n  - new2\n"
    );

    // 2. Large-batch path where removals and scalar replacements are mixed in every order
    //    (remove-before-replace, replace-before-remove, and repeated-scalar adjacent replaces).
    if !cfg!(miri) {
        let mut big = String::new();
        for i in 0..1_200 {
            let _ = writeln!(big, "k{i:04}: v{i}");
        }
        let mut big_ops = Vec::new();
        big_ops.push(EditOp::RemoveEntry {
            path: vec![key("k0000")],
        });
        big_ops.push(EditOp::ReplaceScalar {
            path: vec![key("k0001")],
            value: Value::from("replaced_after_remove"),
        });
        big_ops.push(EditOp::ReplaceScalar {
            path: vec![key("k0002")],
            value: Value::from("replaced_before_tail_remove"),
        });
        for i in 970..1_190 {
            big_ops.push(EditOp::RemoveEntry {
                path: vec![key(&format!("k{i:04}"))],
            });
        }
        let big_edited =
            apply_batch(&big, &big_ops).expect("large mixed replace/remove batch must verify");
        let reparsed = parse_value(big_edited.output()).expect("reparsed large batch");
        assert!(reparsed.as_mapping().unwrap().get("k0000").is_none());
        assert_eq!(reparsed["k0001"], Value::from("replaced_after_remove"));
        assert_eq!(
            reparsed["k0002"],
            Value::from("replaced_before_tail_remove")
        );
        assert_eq!(reparsed["k1199"], Value::from("v1199"));
        assert!(reparsed.as_mapping().unwrap().get("k0970").is_none());
        assert_eq!(reparsed.as_mapping().unwrap().len(), 979);

        // Adjacent repeated-scalar sequence replacements before a large removed tail.
        let mut rep_seq = String::from("items:\n  - false\n  - false\n  - true\n  - true\n");
        for i in 0..500 {
            let _ = writeln!(rep_seq, "  - item{i}");
        }
        let mut seq_ops = vec![
            EditOp::ReplaceScalar {
                path: vec![key("items"), index(0)],
                value: Value::from(true),
            },
            EditOp::ReplaceScalar {
                path: vec![key("items"), index(1)],
                value: Value::from(true),
            },
        ];
        for i in 300..500 {
            seq_ops.push(EditOp::RemoveEntry {
                path: vec![key("items"), index(i)],
            });
        }
        let seq_edited = apply_batch(&rep_seq, &seq_ops)
            .expect("repeated-scalar replaces + large tail removal must verify");
        let seq_parsed = parse_value(seq_edited.output()).expect("reparsed seq");
        assert_eq!(seq_parsed["items"][0], Value::from(true));
        assert_eq!(seq_parsed["items"][1], Value::from(true));
        assert_eq!(seq_parsed["items"].as_sequence().unwrap().len(), 304);
    }

    // 3. Pre-allocation `max_input_bytes` enforcement rejects both a single oversized replacement
    //    (even when escaped characters push it over the limit) and a multi-op batch whose
    //    combined output exceeds `max_input_bytes`.
    let small_input = "a: x\nb: y\n";
    let tight_opts = EditOptions::new().with_parse_options(
        ParseOptions::new().with_limits(Limits::new().with_max_input_bytes(32)),
    );
    let limit_err = replace_scalar_with(
        small_input,
        &[key("a")],
        &Value::from("0123456789abcdefghijklmnopqrstuvwxyz"),
        &tight_opts,
    )
    .expect_err("oversized replacement must fail max_input_bytes");
    assert_eq!(limit_err.kind(), EditErrorKind::VerificationFailed);
    let masked_limit = limit_err.masked();
    assert_eq!(masked_limit.kind(), EditErrorKind::VerificationFailed);
    assert_eq!(
        masked_limit
            .source_error()
            .and_then(|err| err.limit())
            .map(ayaml::LimitExceeded::kind),
        Some(ayaml::LimitKind::InputBytes)
    );
    assert!(masked_limit.source().is_none());
    assert!(!masked_limit.to_string().contains("0123456789"));

    // Short string with heavy escaping (`\n` -> `\\n`) that fits raw `text.len() <= 32` but
    // exceeds `max_input_bytes = 32` once formatted.
    let escaped_err = replace_scalar_with(
        small_input,
        &[key("a")],
        &Value::from("\n".repeat(20)),
        &tight_opts,
    )
    .expect_err("escaped replacement exceeding max_input_bytes must fail fast");
    assert_eq!(escaped_err.kind(), EditErrorKind::VerificationFailed);
    assert_eq!(
        escaped_err
            .masked()
            .source_error()
            .and_then(|err| err.limit())
            .map(ayaml::LimitExceeded::kind),
        Some(ayaml::LimitKind::InputBytes)
    );

    // Multi-op batch where each individual replacement fits `max_input_bytes`, but their combined
    // spliced output exceeds `max_input_bytes` (caught before `String::with_capacity`).
    let batch_limit_err = apply_batch_with(
        small_input,
        &[
            EditOp::ReplaceScalar {
                path: vec![key("a")],
                value: Value::from("aaaaaaaaaaaaaaa"),
            },
            EditOp::ReplaceScalar {
                path: vec![key("b")],
                value: Value::from("bbbbbbbbbbbbbbb"),
            },
        ],
        &tight_opts,
    )
    .expect_err("combined multi-op batch exceeding max_input_bytes must fail before allocation");
    assert_eq!(batch_limit_err.kind(), EditErrorKind::VerificationFailed);
    let batch_limit = batch_limit_err
        .masked()
        .source_error()
        .and_then(|err| err.limit())
        .expect("limit metadata on batch max_input_bytes failure");
    assert_eq!(batch_limit.kind(), ayaml::LimitKind::InputBytes);
    assert_eq!(batch_limit.limit(), 32);
    assert_eq!(batch_limit.observed(), 38);

    // 4. `EditError::masked()` redacts secret keys/values from Parse errors and exposes no
    //    unmasked `std::error::Error::source`.
    let secret_dup = "super_secret_key: hunter2\nsuper_secret_key: other_secret\n";
    let parse_err =
        apply_batch_with(secret_dup, &[], &EditOptions::new()).expect_err("duplicate key parse");
    assert_eq!(parse_err.kind(), EditErrorKind::Parse);
    assert!(parse_err.to_string().contains("super_secret_key"));
    let masked = parse_err.masked();
    assert_eq!(masked.kind(), EditErrorKind::Parse);
    assert!(masked.source().is_none());
    assert!(masked.source_error().is_some());
    let masked_display = masked.to_string();
    let masked_debug = format!("{masked:?}");
    assert!(
        !masked_display.contains("super_secret_key") && !masked_display.contains("hunter2"),
        "masked Display leaked document text: {masked_display}"
    );
    assert!(
        !masked_debug.contains("super_secret_key") && !masked_debug.contains("hunter2"),
        "masked Debug leaked document text: {masked_debug}"
    );
}

fn proptest_config() -> ProptestConfig {
    let default_cases = if cfg!(miri) { 2 } else { 128 };
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default_cases);
    ProptestConfig {
        cases,
        ..ProptestConfig::default()
    }
}

proptest! {
    #![proptest_config(proptest_config())]

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
