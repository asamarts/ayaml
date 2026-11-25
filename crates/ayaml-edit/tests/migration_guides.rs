//! End-to-end verification tests for `docs/editing.md` and the `ayaml-edit`
//! migration examples in `docs/migrating-from/yaml-rust2-and-saphyr.md`.

use ayaml::{Limits, ParseOptions, PathSegment, Value};
use ayaml_edit::{
    EditErrorKind, EditOp, EditOptions, apply_batch, apply_edits, remove_entry, remove_entry_with,
    replace_scalar, replace_scalar_with,
};

#[test]
fn editing_guide_replace_remove_batch_and_edits_roundtrip() {
    let input = "# Service manifest\nservice:\n  name: billing # inline note\n  port: 8080\n  legacy: true\n";

    // Single scalar replacement preserves comments, formatting, and exposes minimal TextEdits.
    let updated = replace_scalar(
        input,
        &[PathSegment::key("service"), PathSegment::key("port")],
        &Value::from(9090u64),
    )
    .expect("replace_scalar");
    assert_eq!(updated.edits().len(), 1);
    assert_eq!(updated.edits()[0].replacement(), "9090");
    assert_eq!(
        apply_edits(input, updated.edits()).expect("apply_edits"),
        updated.output()
    );
    assert!(updated.output().contains("# Service manifest"));
    assert!(updated.output().contains("name: billing # inline note"));
    assert!(updated.output().contains("port: 9090"));

    // Custom EditOptions / ParseOptions / Limits variants work identically.
    let opts = EditOptions::new()
        .with_parse_options(ParseOptions::new().with_limits(Limits::new().with_max_depth(32)));
    let removed = remove_entry_with(
        updated.output(),
        &[PathSegment::key("service"), PathSegment::key("legacy")],
        &opts,
    )
    .expect("remove_entry_with");
    let final_text = removed.into_output();
    assert!(!final_text.contains("legacy:"));
    assert!(final_text.contains("# Service manifest"));

    let re_replaced = replace_scalar_with(
        &final_text,
        &[PathSegment::key("service"), PathSegment::key("port")],
        &Value::from(443u64),
        &opts,
    )
    .expect("replace_scalar_with");
    assert!(re_replaced.output().contains("port: 443"));

    // Atomic batch applies replacement + removal together and replays identically via apply_edits.
    let batched = apply_batch(
        input,
        &[
            EditOp::ReplaceScalar {
                path: vec![PathSegment::key("service"), PathSegment::key("port")],
                value: Value::from(9090u64),
            },
            EditOp::RemoveEntry {
                path: vec![PathSegment::key("service"), PathSegment::key("legacy")],
            },
        ],
    )
    .expect("apply_batch");
    assert_eq!(
        apply_edits(input, batched.edits()).expect("batch apply_edits"),
        batched.output()
    );
    assert!(batched.output().contains("port: 9090"));
    assert!(!batched.output().contains("legacy:"));

    // Standard remove_entry also succeeds on an unadorned sibling key.
    let removed_plain = remove_entry(
        input,
        &[PathSegment::key("service"), PathSegment::key("legacy")],
    )
    .expect("remove_entry");
    assert!(!removed_plain.output().contains("legacy:"));
}

#[test]
fn editing_guide_refuses_documents_with_block_scalars_conservatively() {
    let with_block = "service:\n  port: 8080\n  notes: |\n    keep me verbatim\n";
    let err = replace_scalar(
        with_block,
        &[PathSegment::key("service"), PathSegment::key("port")],
        &Value::from(9090u64),
    )
    .unwrap_err();
    assert_eq!(err.kind(), EditErrorKind::Unsupported);
}
