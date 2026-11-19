//! `LineIndex` against the position vectors that the format contract shares between parsers.
//!
//! The vectors are vendored under `tests/vectors/` at the repository's root (see `SOURCE.md`
//! there), so this runs in the repository and not from the published crate, which holds only
//! the crate's own directory.

use std::path::{Path, PathBuf};

use ayaml::{ColumnEncoding, LineIndex, Position, PositionError};
use serde_json::Value;

const ENCODINGS: [(&str, ColumnEncoding); 3] = [
    ("utf8", ColumnEncoding::Utf8),
    ("scalar", ColumnEncoding::Scalar),
    ("utf16", ColumnEncoding::Utf16),
];

/// The vectors' directory, or `None` in the published package, which holds the crate alone and
/// says so with the manifest that `cargo package` leaves beside its own. Anywhere else the
/// vectors have to be there: a test that found none would pass having checked nothing.
fn vectors() -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let directory = manifest.join("../../tests/vectors");
    if directory.join("positions.json").is_file() {
        return Some(directory);
    }
    assert!(
        manifest.join("Cargo.toml.orig").is_file(),
        "{} holds no position vectors, and is no unpacked package",
        directory.display()
    );
    None
}

fn number(value: &Value, name: &str) -> usize {
    let number = value
        .as_u64()
        .unwrap_or_else(|| panic!("{name} is an unsigned integer"));
    usize::try_from(number).expect("a coordinate fits")
}

/// What a vector expects: a zero-based position, or the reason there is none.
fn expected(value: &Value) -> Result<Position, PositionError> {
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(match error {
            "inside-crlf" => PositionError::InsideCrLf,
            "invalid-utf8-boundary" => PositionError::InvalidUtf8Boundary,
            "out-of-bounds" => PositionError::OutOfBounds,
            "split-surrogate" => PositionError::SplitSurrogate,
            other => panic!("unknown position error {other}"),
        });
    }
    Ok(Position::new(
        number(&value["line"], "line"),
        number(&value["column"], "column"),
    ))
}

/// Every position case, in every column encoding, gives what the vectors say.
///
/// A source that opens with a byte-order mark has two answers in the vectors: `expected`, where
/// the mark's bytes are columns, and `without_bom`, where they are not. The contract makes the
/// second the default, and it is the only form this index has, so those cases are checked
/// against it. A position converts back to its offset, but for an offset inside the mark, whose
/// position is the first visible column and so converts to the byte after the mark.
#[test]
fn the_line_index_gives_the_shared_position_vectors() {
    let Some(directory) = vectors() else {
        return;
    };
    let text = std::fs::read_to_string(directory.join("positions.json")).expect("positions.json");
    let vectors: Value = serde_json::from_str(&text).expect("the vectors are JSON");
    assert_eq!(vectors["version"].as_u64(), Some(1), "the vectors' version");
    let (mut positions, mut errors, mut marked) = (0, 0, 0);
    for case in vectors["cases"].as_array().expect("an array of cases") {
        let name = case["name"].as_str().expect("a case's name");
        let source = case["source"].as_str().expect("a case's source");
        let offset = number(&case["offset"], "offset");
        let index = LineIndex::new(source);
        let answers = case.get("without_bom").unwrap_or_else(|| &case["expected"]);
        marked += usize::from(case.get("without_bom").is_some());
        assert_eq!(
            case.get("without_bom").is_some(),
            source.starts_with('\u{feff}'),
            "{name}: a source has a second answer exactly when a mark opens it"
        );
        for (field, encoding) in ENCODINGS {
            let expected = expected(&answers[field]);
            assert_eq!(
                index.to_position(offset, encoding),
                expected,
                "{name} / {field}"
            );
            match expected {
                Ok(position) => {
                    let visible =
                        offset.max(source.len() - source.trim_start_matches('\u{feff}').len());
                    assert_eq!(
                        index.to_offset(position, encoding),
                        Ok(visible),
                        "{name} / {field} / back to the offset"
                    );
                    positions += 1;
                }
                Err(_) => errors += 1,
            }
        }
    }
    // The set as vendored: ten cases in three encodings, eight answers that are errors, and
    // three sources behind a mark. A count keeps the test from passing on a file that lost
    // its cases.
    assert_eq!((positions + errors, errors, marked), (30, 8, 3));
}

/// The vectors that nothing here runs yet are vendored too, so the next reader of this
/// directory finds the set whole. `tools/test_ci_checks.py` holds each file's digest.
#[test]
fn the_vendored_set_is_whole() {
    let Some(directory) = vectors() else {
        return;
    };
    for name in [
        "README.md",
        "SOURCE.md",
        "edit-order.json",
        "github-escaping.json",
        "positions.json",
        "schema/check-report.v1.json",
        "schema/diff-report.v1.json",
    ] {
        assert!(directory.join(name).is_file(), "{name} is missing");
    }
}
