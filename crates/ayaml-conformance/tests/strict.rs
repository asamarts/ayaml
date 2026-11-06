//! A strict run has to fail when what it reads isn't what it should be.
//!
//! The counts themselves and the digest are tested beside their code. These run the program on
//! copies of the pinned tree, so that each way of going wrong is seen to change its exit status.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn pinned_fixtures() -> PathBuf {
    repository().join("tests/fixtures")
}

fn pinned_suite() -> PathBuf {
    repository().join("tests/data/yaml-test-suite/src")
}

/// An empty directory for one test.
fn scratch(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    if path.exists() {
        fs::remove_dir_all(&path).expect("clear the scratch directory");
    }
    fs::create_dir_all(&path).expect("make the scratch directory");
    path
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("make a directory");
    for entry in fs::read_dir(from).expect("read a directory") {
        let entry = entry.expect("read an entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy a file");
        }
    }
}

/// A copy of the pinned suite and its `REVISION` file, and its `src` directory.
fn suite_copy(name: &str) -> PathBuf {
    let root = scratch(name);
    let src = root.join("src");
    copy_tree(&pinned_suite(), &src);
    fs::copy(pinned_suite().join("../REVISION"), root.join("REVISION")).expect("copy REVISION");
    src
}

fn runner() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ayaml-conformance"));
    command
        .current_dir(repository())
        .env_remove("AYAML_STRICT_SUITE")
        .env_remove("AYAML_SUITE_SHOW");
    command
}

fn run(strict: Option<&str>, fixtures: &Path, suite: &Path) -> Output {
    let mut command = runner();
    command.arg(fixtures).arg(suite);
    if let Some(value) = strict {
        command.env("AYAML_STRICT_SUITE", value);
    }
    command.output().expect("run the conformance program")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// `output` failed with `status`, saying `message`.
#[track_caller]
fn assert_fails(output: &Output, status: i32, message: &str) {
    let text = stderr(output);
    assert_eq!(output.status.code(), Some(status), "{text}");
    assert!(text.contains(message), "wanted {message:?} in: {text}");
}

#[test]
fn the_pinned_tree_passes() {
    let output = run(Some("1"), &pinned_fixtures(), &pinned_suite());
    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn a_directory_that_is_not_there_fails_in_every_mode() {
    let root = scratch("not-there");
    let missing = root.join("missing");
    for strict in [None, Some("0"), Some("1")] {
        let output = run(strict, &missing, &pinned_suite());
        assert_fails(&output, 1, "missing");
        let output = run(strict, &pinned_fixtures(), &missing);
        assert_fails(&output, 1, "missing");
    }
}

#[test]
fn an_empty_suite_fails_strict() {
    let src = suite_copy("empty-suite");
    fs::remove_dir_all(&src).expect("remove the cases");
    fs::create_dir(&src).expect("make the empty directory");
    let output = run(Some("1"), &pinned_fixtures(), &src);
    assert_fails(&output, 1, "0 files, but REVISION records");
}

#[test]
fn a_changed_suite_file_fails_strict() {
    let src = suite_copy("changed-file");
    let path = src.join("229Q.yaml");
    let mut text = fs::read(&path).expect("read a case");
    text.push(b'\n');
    fs::write(&path, text).expect("change the case");
    let output = run(Some("1"), &pinned_fixtures(), &src);
    assert_fails(&output, 1, "content_sha256 is");
}

#[test]
fn a_removed_suite_file_fails_strict() {
    let src = suite_copy("removed-file");
    fs::remove_file(src.join("229Q.yaml")).expect("remove a case");
    let output = run(Some("1"), &pinned_fixtures(), &src);
    assert_fails(&output, 1, "350 files, but REVISION records 351");
}

#[test]
fn an_added_suite_file_fails_strict() {
    let src = suite_copy("added-file");
    fs::write(src.join("notes.txt"), "not a case\n").expect("add a file");
    let output = run(Some("1"), &pinned_fixtures(), &src);
    assert_fails(&output, 1, "352 files, but REVISION records 351");
}

#[test]
fn a_suite_without_its_revision_fails_strict() {
    let src = suite_copy("no-revision");
    fs::remove_file(src.join("../REVISION")).expect("remove REVISION");
    let output = run(Some("1"), &pinned_fixtures(), &src);
    assert_fails(&output, 1, "REVISION");
}

#[test]
fn a_revision_without_a_digest_fails_strict() {
    let src = suite_copy("no-digest");
    let path = src.join("../REVISION");
    let revision = fs::read_to_string(&path).expect("read REVISION");
    let kept: Vec<&str> = revision
        .lines()
        .filter(|line| !line.starts_with("content_sha256="))
        .collect();
    fs::write(&path, kept.join("\n")).expect("write REVISION");
    let output = run(Some("1"), &pinned_fixtures(), &src);
    assert_fails(&output, 1, "exactly one value for content_sha256");
}

#[test]
fn a_missing_fixture_fails_only_strict() {
    let fixtures = scratch("missing-fixture").join("fixtures");
    copy_tree(&pinned_fixtures(), &fixtures);
    fs::remove_file(fixtures.join("compat/toolchain-pins.yml")).expect("remove a fixture");
    assert_fails(
        &run(Some("1"), &fixtures, &pinned_suite()),
        1,
        "saw 4 fixtures, expected 5",
    );
    // `0` means the same as leaving the flag unset, and neither is strict.
    for strict in [None, Some("0")] {
        let output = run(strict, &fixtures, &pinned_suite());
        assert!(output.status.success(), "{}", stderr(&output));
    }
}

#[test]
fn a_flag_with_any_other_value_stops_the_run() {
    for value in ["", "2", "true", "no"] {
        let output = run(Some(value), &pinned_fixtures(), &pinned_suite());
        assert_fails(&output, 2, "AYAML_STRICT_SUITE must be 0 or 1");
    }
    let output = runner()
        .env("AYAML_SUITE_SHOW", "many")
        .output()
        .expect("run the conformance program");
    assert_fails(&output, 2, "AYAML_SUITE_SHOW must be a whole number");
}

#[test]
fn a_fourth_argument_stops_the_run() {
    let output = runner()
        .args(["a", "b", "c", "d"])
        .output()
        .expect("run the conformance program");
    assert_fails(&output, 2, "unexpected argument: d");
}
