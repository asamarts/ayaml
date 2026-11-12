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
