//! The repository-level lint policy is part of the release gate (AY-072), so it is
//! checked like any other contract. This test reads the repository's own files and
//! skips when the crate is unpacked outside them, as `cargo test` from a `.crate`
//! does.

use std::path::{Path, PathBuf};

use ayaml::{Value, parse_value};

fn repository_root() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.join(".alint.yml").is_file().then_some(root)
}

fn read(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join(name))
        .unwrap_or_else(|error| panic!("reading {name}: {error}"))
}

fn rule<'a>(config: &'a Value, id: &str) -> &'a Value {
    config["rules"]
        .as_sequence()
        .expect("rules is a sequence")
        .iter()
        .find(|rule| rule["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("the {id} rule is missing"))
}

fn strings(value: &Value) -> Vec<&str> {
    value
        .as_sequence()
        .unwrap_or_else(|| panic!("{value:?} is not a sequence"))
        .iter()
        .map(|item| item.as_str().expect("a string"))
        .collect()
}

#[test]
fn the_packaged_changelog_matches_the_workspace_changelog() {
    let Some(root) = repository_root() else {
        return;
    };
    assert_eq!(
        read(&root, "CHANGELOG.md"),
        read(&root, "crates/ayaml/CHANGELOG.md")
    );
}

#[test]
fn the_repository_lint_policy_is_the_one_the_plan_requires() {
    let Some(root) = repository_root() else {
        return;
    };
    let config = parse_value(&read(&root, ".alint.yml")).expect(".alint.yml parses");

    assert_eq!(config["requires"]["alint"].as_str(), Some("0.22.0"));
    assert_eq!(config["baseline"], Value::Null, "a baseline is not allowed");

    let extends = strings(&config["extends"]);
    for ruleset in [
        "oss-baseline",
        "rust",
        "ci/github-actions",
        "hygiene/no-tracked-artifacts",
        "tooling/editorconfig",
        "agent-hygiene",
        "agent-context",
    ] {
        let expected = format!("alint://bundled/{ruleset}@v1");
        assert!(
            extends.contains(&expected.as_str()),
            "{expected} is missing"
        );
    }

    // The text-hygiene and workflow rules that the bundled sets leave advisory.
    for id in [
        "oss-final-newline",
        "oss-no-trailing-whitespace",
        "rust-sources-final-newline",
        "rust-sources-no-trailing-whitespace",
        "gha-workflow-has-name",
    ] {
        assert_eq!(rule(&config, id)["level"].as_str(), Some("error"), "{id}");
    }

    // Full-tree portable names, links, submodules, and line endings.
    for id in [
        "portable-names",
        "case-conflicts",
        "no-symlinks",
        "no-submodules",
        "line-endings",
    ] {
        assert_eq!(rule(&config, id)["level"].as_str(), Some("error"), "{id}");
    }
    assert_eq!(
        strings(&rule(&config, "case-conflicts")["paths"]["exclude"]),
        ["tests/data/yaml-test-suite/**"]
    );

    let editorconfig = rule(&config, "editorconfig-conforms");
    assert_eq!(editorconfig["level"].as_str(), Some("error"));
    assert_eq!(
        strings(&editorconfig["paths"]),
        [".aplan.yml", ".github/**"]
    );

    // The size rule covers every maintained source and script file, with no
    // path exclusion or non-growth ratchet.
    let size = rule(&config, "source-file-size");
    assert_eq!(size["kind"].as_str(), Some("file_max_lines"));
    assert_eq!(size["level"].as_str(), Some("error"));
    assert_eq!(size["max_lines"].as_u64(), Some(2000));
    assert_eq!(size["expect_matches"].as_bool(), Some(true));
    let mut include = strings(&size["paths"]["include"]);
    include.sort_unstable();
    assert_eq!(
        include,
        [
            "**/*.bash",
            "**/*.py",
            "**/*.rs",
            "**/*.sh",
            "tools/hooks/*"
        ]
    );
    assert_eq!(size["paths"]["exclude"], Value::Null);
    assert!(
        config["rules"]
            .as_sequence()
            .expect("rules is a sequence")
            .iter()
            .all(|rule| !rule["id"].as_str().unwrap_or("").ends_with("-size-ratchet")),
        "source-size ratchets remain"
    );

    let editorconfig = read(&root, ".editorconfig");
    for line in [
        "root = true",
        "charset = utf-8",
        "end_of_line = lf",
        "insert_final_newline = true",
        "trim_trailing_whitespace = true",
        "indent_size = 4",
        "indent_size = 2",
        "indent_style = tab",
    ] {
        assert!(editorconfig.contains(line), ".editorconfig lacks {line:?}");
    }

    let workflow = read(&root, ".github/workflows/ci.yml");
    assert!(
        workflow.contains("asamarts/alint@8d7a60f6642cb1b59537202ae81df7d951a078ad"),
        "the alint action is not pinned to v0.22.0"
    );
    assert!(workflow.contains("fail-on-warning: \"true\""));

    let local = read(&root, "tools/ci-local.sh");
    assert!(local.contains("alint check --fail-on-warning"));
    assert!(local.contains("alint 0.22.0"));
}

/// The nine document shapes of issue #178 each have a peak, an allocation count, and a total
/// in the budgets of the untyped decode, and the gate script reads that file.
#[test]
fn allocation_budgets_cover_the_json_decode() {
    let Some(root) = repository_root() else {
        return;
    };
    let budgets = read(&root, "tools/json-alloc-corpus.tsv");
    let mut shapes = Vec::new();
    for line in budgets.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let columns: Vec<_> = line.split('\t').collect();
        assert_eq!(columns.len(), 4, "bad allocation row: {line}");
        for column in &columns[1..] {
            let budget: u64 = column
                .parse()
                .unwrap_or_else(|error| panic!("{line}: {error}"));
            assert!(budget > 0, "{line}: a budget of nothing");
        }
        shapes.push(columns[0]);
    }
    assert_eq!(
        shapes,
        [
            "@service-37kb",
            "@service-1mb",
            "@service-4mb",
            "@block-map-1mb",
            "@flow-map-1mb",
            "@flow-ints-1mb",
            "@flow-ones-1mb",
            "@block-seq-1mb",
            "@block-words-1mb",
        ]
    );
}

/// The gate checks each of its three budget files, with no condition that could pass one over,
/// and CI runs the gate. A call that is commented out, or that stands inside an `if`, is not a
/// line of its own.
#[test]
fn the_allocation_gate_checks_every_budget_file() {
    let Some(root) = repository_root() else {
        return;
    };
    let script = read(&root, "tools/check_alloc_corpus.sh");
    for call in [
        "check_file \"$manifest\" value",
        "check_file tools/typed-alloc-corpus.tsv typed",
        "check_json_file tools/json-alloc-corpus.tsv",
    ] {
        assert!(
            script.lines().any(|line| line == call),
            "the gate has to run `{call}` on a line of its own"
        );
    }
    let typed = read(&root, "tools/typed-alloc-corpus.tsv");
    assert!(
        typed
            .lines()
            .any(|line| !line.is_empty() && !line.starts_with('#')),
        "the typed path has no budget"
    );
    let workflow = read(&root, ".github/workflows/ci.yml");
    assert!(
        workflow
            .lines()
            .any(|line| line.trim() == "run: tools/check_alloc_corpus.sh"),
        "CI has to run the allocation gate"
    );
}

/// Every allocation workload budgets total bytes as well as peak bytes and allocation counts.
#[test]
fn allocation_budgets_include_total_bytes() {
    let Some(root) = repository_root() else {
        return;
    };
    let budgets = read(&root, "tools/alloc-corpus.tsv");
    let mut rows = 0;
    for line in budgets.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let columns: Vec<_> = line.split('\t').collect();
        assert_eq!(columns.len(), 10, "bad allocation row: {line}");
        for column in &columns[7..] {
            column
                .parse::<u64>()
                .unwrap_or_else(|error| panic!("{line}: {error}"));
        }
        rows += 1;
    }
    assert!(rows > 0, "no allocation workloads found");
}

/// CI runs `tools/test_ci_checks.py` as a script, and `unittest.main()` exits when it returns, so
/// a test class defined after that call never runs. The call must be the last thing in the file.
#[test]
fn the_ci_check_tests_all_run() {
    let Some(root) = repository_root() else {
        return;
    };
    let tests = read(&root, "tools/test_ci_checks.py");
    let (_, after) = tests
        .split_once("if __name__ == \"__main__\":")
        .expect("the file runs its tests when it is a script");
    assert_eq!(
        after.trim(),
        "unittest.main()",
        "nothing may follow the call that runs the tests"
    );
    // A second call, earlier and outside that condition, would end the run where it stands.
    assert_eq!(
        tests.matches("unittest.main(").count(),
        1,
        "one call runs the tests"
    );
}

/// Every place that repeats the workspace version states the one in `Cargo.toml`, so a release
/// that sets one and misses another fails here: the README's current release and install lines,
/// the compatibility guide's install line, and the exact requirement of each crate that pins
/// the library.
#[test]
fn every_place_that_repeats_the_version_agrees_with_the_workspace() {
    let Some(root) = repository_root() else {
        return;
    };
    let manifest: Value = {
        let text = read(&root, "Cargo.toml");
        let line = text
            .lines()
            .find(|line| line.starts_with("version = "))
            .expect("the workspace version");
        parse_value(&format!("{{{}}}", line.replacen(" = ", ": ", 1))).expect("a version")
    };
    let version = manifest["version"].as_str().expect("a version string");
    assert_eq!(version.split('.').count(), 3, "{version}");

    let readme = read(&root, "README.md");
    assert!(
        readme.contains(&format!("The current release is `{version}`.")),
        "README.md names another current release than {version}"
    );
    let install = format!("ayaml = \"{version}\"");
    assert_eq!(readme.matches(&install).count(), 2, "README.md: {install}");
    assert_eq!(
        readme.matches("ayaml = \"").count(),
        2,
        "README.md has another install line"
    );

    let guide = read(&root, "docs/compatibility.md");
    let install = format!("ayaml = {{ version = \"{version}\", features = [\"libyaml-compat\"] }}");
    assert!(guide.contains(&install), "docs/compatibility.md: {install}");
    assert_eq!(
        guide.matches("ayaml = {").count(),
        1,
        "docs/compatibility.md"
    );

    for name in [
        "crates/ayaml-bench/Cargo.toml",
        "crates/ayaml-conformance/Cargo.toml",
        "fuzz/Cargo.toml",
    ] {
        let text = read(&root, name);
        let pins: Vec<&str> = text
            .lines()
            .filter(|line| line.starts_with("ayaml = {"))
            .collect();
        assert_eq!(pins.len(), 1, "{name}: {pins:?}");
        assert!(
            pins[0].contains(&format!("version = \"={version}\"")),
            "{name}: {}",
            pins[0]
        );
    }
    for name in ["Cargo.lock", "fuzz/Cargo.lock"] {
        let text = read(&root, name);
        assert!(
            text.contains(&format!("name = \"ayaml\"\nversion = \"{version}\"\n")),
            "{name} holds another version of ayaml than {version}"
        );
    }
}
