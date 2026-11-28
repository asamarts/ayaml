//! Process-isolated adversarial tests for `ayaml-edit` (AY-171).
//!
//! Each hostile test runs in a child process with a 1 GiB Linux virtual-memory ceiling and a
//! wall-clock deadline so any limit regression aborts only the child process and keeps the host
//! healthy.

use std::fmt::Write as _;
use std::process::Command;
use std::time::Duration;

use ayaml::{Limits, MAX_DEPTH, ParseOptions, PathSegment, Span, Value};
use ayaml_edit::{
    EditErrorKind, EditOp, EditOptions, TextEdit, apply_batch, apply_batch_with, apply_edits,
    remove_entry, remove_entry_with, replace_scalar, replace_scalar_with,
};

const CHILD: &str = "AYAML_EDIT_ADVERSARIAL_CHILD";
const ADDRESS_SPACE_KIB: u64 = 1 << 20;

const LIMIT_THEN_RUN: &str = r#"
[ "$(ulimit -v)" != unlimited ] && [ "$(ulimit -v)" -le "$1" ] || ulimit -v "$1" || exit 125
ulimit -c 0
shift
exec "$0" "$@"
"#;

macro_rules! in_child {
    ($body:expr $(,)?) => {{
        fn here() {}
        let path = std::any::type_name_of_val(&here);
        let test = path
            .strip_suffix("::here")
            .and_then(|p| p.split_once("::"))
            .map_or(path, |(_, t)| t);
        in_child_named(test, $body);
    }};
}

fn assert_limited() {
    if cfg!(target_os = "linux") {
        let limit = std::fs::read_to_string("/proc/self/limits")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix("Max address space"))?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
            });
        assert!(
            limit.is_some_and(|bytes| bytes <= ADDRESS_SPACE_KIB * 1024),
            "no address-space limit in this child: {limit:?}"
        );
    }
}

fn in_child_named(test: &str, body: impl FnOnce()) {
    if std::env::var_os(CHILD).is_some() {
        assert_limited();
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(120));
            std::process::abort();
        });
        body();
        return;
    }

    let exe = std::env::current_exe().expect("current test binary");
    let mut cmd = if cfg!(target_os = "linux") {
        let mut sh = Command::new("sh");
        sh.arg("-c")
            .arg(LIMIT_THEN_RUN)
            .arg(&exe)
            .arg(ADDRESS_SPACE_KIB.to_string());
        sh
    } else {
        Command::new(&exe)
    };
    let output = cmd
        .arg("--exact")
        .arg(test)
        .arg("--nocapture")
        .env(CHILD, "1")
        .output()
        .expect("spawn bounded adversarial child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed;"),
        "child `{test}` failed (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
}

fn alias_bomb() -> String {
    let mut bomb = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x]\n");
    for level in 1..9 {
        let prev = format!("*a{}", level - 1);
        let row = [prev.as_str(); 9].join(", ");
        let _ = writeln!(bomb, "a{level}: &a{level} [{row}]");
    }
    bomb
}

/// Hostile YAML inputs (alias bombs, deep nesting, oversized inputs, giant scalars, wide maps)
/// are rejected boundedly across `replace_scalar`, `remove_entry`, and `apply_batch`.
#[test]
fn edit_rejects_hostile_yaml_inputs_boundedly() {
    in_child!(|| {
        // 1. Billion-laughs alias bomb is rejected without expanding.
        let bomb = alias_bomb();
        let path = [PathSegment::key("a0")];
        let repl = Value::from("ok");
        for err in [
            replace_scalar(&bomb, &path, &repl).expect_err("replace alias bomb"),
            remove_entry(&bomb, &path).expect_err("remove alias bomb"),
            apply_batch(
                &bomb,
                &[EditOp::ReplaceScalar {
                    path: path.to_vec(),
                    value: repl.clone(),
                }],
            )
            .expect_err("batch alias bomb"),
        ] {
            assert!(
                matches!(
                    err.kind(),
                    EditErrorKind::Parse | EditErrorKind::Unsupported
                ),
                "unexpected error kind for alias bomb: {err:?}"
            );
        }

        // 2. Deep nesting above MAX_DEPTH and at extreme depth (5,000) is rejected cleanly.
        for depth in [MAX_DEPTH + 1, 5_000] {
            let deep = format!("{}{}\n", "[".repeat(depth), "]".repeat(depth));
            let err = replace_scalar(&deep, &[PathSegment::index(0)], &repl)
                .expect_err("deep nesting rejected");
            assert_eq!(err.kind(), EditErrorKind::Parse);
            let err =
                remove_entry(&deep, &[PathSegment::index(0)]).expect_err("deep remove rejected");
            assert_eq!(err.kind(), EditErrorKind::Parse);
        }

        // 3. Tight input-size, scalar-size, and collection-entry limits propagate cleanly.
        let tight_input = EditOptions::new().with_parse_options(
            ParseOptions::new().with_limits(Limits::new().with_max_input_bytes(64)),
        );
        let big = format!("k: \"{}\"\n", "a".repeat(4096));
        assert_eq!(
            replace_scalar_with(&big, &[PathSegment::key("k")], &repl, &tight_input)
                .expect_err("max_input_bytes")
                .kind(),
            EditErrorKind::Parse
        );
        assert_eq!(
            remove_entry_with(&big, &[PathSegment::key("k")], &tight_input)
                .expect_err("max_input_bytes remove")
                .kind(),
            EditErrorKind::Parse
        );

        let tight_scalar = EditOptions::new().with_parse_options(
            ParseOptions::new().with_limits(Limits::new().with_max_scalar_bytes(32)),
        );
        assert_eq!(
            replace_scalar_with(&big, &[PathSegment::key("k")], &repl, &tight_scalar)
                .expect_err("max_scalar_bytes")
                .kind(),
            EditErrorKind::Parse
        );

        let mut wide = String::with_capacity(8192);
        for i in 0..300 {
            let _ = writeln!(wide, "k{i}: {i}");
        }
        let tight_entries = EditOptions::new().with_parse_options(
            ParseOptions::new().with_limits(Limits::new().with_max_collection_entries(128)),
        );
        assert_eq!(
            replace_scalar_with(&wide, &[PathSegment::key("k0")], &repl, &tight_entries)
                .expect_err("max_collection_entries")
                .kind(),
            EditErrorKind::Parse
        );
    });
}

/// Hostile `apply_batch` requests (thousands of conflicting/overlapping ops, oversized scalar
/// replacements, and tight output/input limits) and hostile `apply_edits` slices fail cleanly
/// with bounded memory.
#[test]
fn edit_batch_and_apply_edits_reject_hostile_operations_boundedly() {
    in_child!(|| {
        // 1. 10,000 conflicting operations targeting the same path fail cleanly without O(N^2) blowup.
        let doc = "a: 1\nb: 2\nc: 3\n";
        let dup_ops: Vec<EditOp> = (0..10_000_i64)
            .map(|i| EditOp::ReplaceScalar {
                path: vec![PathSegment::key("a")],
                value: Value::from(i),
            })
            .collect();
        let err = apply_batch(doc, &dup_ops).expect_err("duplicate batch targets rejected");
        assert_eq!(err.kind(), EditErrorKind::InvalidEdit);

        // 2. Oversized replacement scalar exceeding configured max_input_bytes / max_scalar_bytes
        //    is rejected cleanly during verification instead of returning an unparseable document.
        let tight = EditOptions::new().with_parse_options(
            ParseOptions::new().with_limits(
                Limits::new()
                    .with_max_input_bytes(256)
                    .with_max_scalar_bytes(128),
            ),
        );
        let huge_repl = Value::from("x".repeat(4096));
        let err = apply_batch_with(
            doc,
            &[EditOp::ReplaceScalar {
                path: vec![PathSegment::key("a")],
                value: huge_repl,
            }],
            &tight,
        )
        .expect_err("oversized replacement rejected");
        assert_eq!(
            err.kind(),
            EditErrorKind::VerificationFailed,
            "unexpected error for oversized replacement: {err:?}"
        );

        // 3. Hostile `apply_edits` slices: out-of-bounds, inverted, mid-UTF-8, unsorted,
        //    overlapping, and massive duplicate-offset edit lists all fail cleanly.
        let utf8 = "k: café 🚀\n";
        let mid_e = utf8.find('é').expect("'é' present") + 1;
        assert!(!utf8.is_char_boundary(mid_e));
        for bad in [
            vec![TextEdit::new(Span::new(0, utf8.len() + 1), "x")],
            vec![TextEdit::new(Span::new(5, 2), "x")],
            // Byte `mid_e` lies inside the 2-byte `'é'` sequence.
            vec![TextEdit::new(Span::new(mid_e, mid_e + 1), "x")],
            vec![
                TextEdit::new(Span::new(3, 5), "a"),
                TextEdit::new(Span::new(0, 1), "b"),
            ],
            vec![
                TextEdit::new(Span::new(0, 4), "a"),
                TextEdit::new(Span::new(3, 6), "b"),
            ],
            vec![
                TextEdit::new(Span::new(0, 2), "a"),
                TextEdit::new(Span::empty(0), "b"),
            ],
        ] {
            let err = apply_edits(utf8, &bad).expect_err("invalid edit slice rejected");
            assert_eq!(err.kind(), EditErrorKind::InvalidEdit);
        }

        // 10,000 overlapping edits reject quickly with bounded memory.
        let overlapping: Vec<TextEdit> = (0..10_000)
            .map(|_| TextEdit::new(Span::new(0, 1), "z"))
            .collect();
        assert_eq!(
            apply_edits(utf8, &overlapping)
                .expect_err("overlapping edits")
                .kind(),
            EditErrorKind::InvalidEdit
        );
    });
}
