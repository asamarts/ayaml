//! The repository-level lint policy is part of the release gate (AY-072), so it is
//! checked like any other contract. This test reads the repository's own files and
//! skips when the crate is unpacked outside them, as `cargo test` from a `.crate`
//! does.

use std::path::{Path, PathBuf};

use ayaml::{Value, parse_value};

/// The repository's root, or `None` in the published package, which holds the crate alone and
/// says so with the manifest that `cargo package` leaves beside its own. Anywhere else the root
/// has to be there: a test that found none would pass having checked nothing.
fn repository_root() -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    if root.join(".alint.yml").is_file() {
        return Some(root);
    }
    assert!(
        manifest.join("Cargo.toml.orig").is_file(),
        "{} is neither in the repository nor an unpacked package",
        manifest.display()
    );
    None
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
/// in the budgets of the untyped decode, and the gate script reads that file. So do the shapes
/// that a reader of events builds as trees, and the one that is all directives.
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
            "@anchored-flow-ones-1mb",
            "@anchored-block-words-1mb",
            "@anchored-maps-50000",
            "@directives-100k",
        ]
    );
}

/// The gate checks each of its three budget files, and CI runs the gate with no argument and
/// under no condition. What the gate does with the files is tested by running it, in the
/// `AllocationGate` tests of `tools/test_ci_checks.py`; this pins what those can't see, the
/// files it reads by default and the step that runs it.
#[test]
fn the_allocation_gate_checks_every_budget_file() {
    let Some(root) = repository_root() else {
        return;
    };
    let script = read(&root, "tools/check_alloc_corpus.sh");
    for line in [
        "manifest=\"${1:-tools/alloc-corpus.tsv}\"",
        "typed_manifest=\"${2:-tools/typed-alloc-corpus.tsv}\"",
        "json_manifest=\"${3:-tools/json-alloc-corpus.tsv}\"",
        "check_file \"$manifest\" value",
        "check_file \"$typed_manifest\" typed",
        "check_json_file \"$json_manifest\"",
    ] {
        assert!(
            script.lines().any(|found| found == line),
            "the gate has to hold `{line}` on a line of its own"
        );
    }
    let typed = read(&root, "tools/typed-alloc-corpus.tsv");
    assert!(
        typed
            .lines()
            .any(|line| !line.is_empty() && !line.starts_with('#')),
        "the typed path has no budget"
    );
    let text = read(&root, ".github/workflows/ci.yml");
    let gate = |workflow: &str| {
        steps_that_run(workflow, &|step| {
            step["run"].as_str().map(str::trim) == Some("tools/check_alloc_corpus.sh")
        })
    };
    assert_eq!(
        gate(&text),
        Ok(1),
        "CI has to run the allocation gate, once, with no argument, and under no condition"
    );
    // Each of these keeps the step in the workflow and stops it from running the gate.
    let run = "        run: tools/check_alloc_corpus.sh\n";
    assert_eq!(text.matches(run).count(), 1);
    for change in [
        format!("        shell: true {{0}}\n{run}"),
        format!("        if: false\n{run}"),
        format!("        continue-on-error: true\n{run}"),
        format!("        working-directory: docs\n{run}"),
    ] {
        let changed = text.replacen(run, &change, 1);
        assert!(gate(&changed).is_err(), "the check misses {change:?}");
    }
    let job = "  allocation:\n";
    assert_eq!(text.matches(job).count(), 1);
    for change in [
        "  allocation:\n    if: false\n",
        "  allocation:\n    continue-on-error: true\n",
        "  allocation:\n    defaults:\n      run:\n        shell: true {0}\n",
    ] {
        let changed = text.replacen(job, change, 1);
        assert!(gate(&changed).is_err(), "the check misses {change:?}");
    }
}

/// How many steps of a workflow `wanted` picks, if each of them runs whenever its job's
/// workflow does, in the default shell and directory, with the build flags that it states and
/// no others: no condition, no error passed over, and no setting of the step, its job, or the
/// workflow that changes the shell or replaces the compiler's flags.
fn steps_that_run(workflow: &str, wanted: &dyn Fn(&Value) -> bool) -> Result<usize, String> {
    let workflow = parse_value(workflow).map_err(|error| error.to_string())?;
    let jobs = workflow["jobs"].as_mapping().ok_or("no jobs")?;
    let mut found = 0;
    for (name, job) in jobs {
        let steps = job["steps"].as_sequence().ok_or("a job with no steps")?;
        for step in steps.iter().filter(|step| wanted(step)) {
            found += 1;
            let keys = step.as_mapping().ok_or("a step that is no mapping")?;
            for (key, _) in keys {
                let key = key.as_str().unwrap_or("?");
                if !["name", "run", "env"].contains(&key) {
                    return Err(format!("{name:?}: the step sets `{key}`"));
                }
            }
            for (owner, what) in [(step, "step"), (job, "job"), (&workflow, "workflow")] {
                let owner = owner.as_mapping().ok_or("not a mapping")?;
                for key in ["if", "continue-on-error", "defaults"] {
                    if owner.contains_key(key) {
                        return Err(format!("{name:?}: the {what} sets `{key}`"));
                    }
                }
                // Cargo takes these before `RUSTFLAGS`, so either would replace a step's flags.
                for flags in ["CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS"] {
                    if owner
                        .get("env")
                        .and_then(Value::as_mapping)
                        .is_some_and(|env| env.contains_key(flags))
                    {
                        return Err(format!("{name:?}: the {what} sets `{flags}`"));
                    }
                }
            }
        }
    }
    Ok(found)
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
    assert_eq!(
        stated_releases(&readme),
        [version],
        "README.md names another current release than {version}"
    );
    assert_eq!(
        dependency_versions(&readme),
        [version, version],
        "README.md's install lines"
    );
    let guide = read(&root, "docs/compatibility.md");
    assert_eq!(
        dependency_versions(&guide),
        [version],
        "docs/compatibility.md's install line"
    );
    assert!(
        guide.contains(&format!(
            "ayaml = {{ version = \"{version}\", features = [\"libyaml-compat\"] }}"
        )),
        "docs/compatibility.md's install line has the feature"
    );
    let series = version.rsplit_once('.').expect("a patch number").0;
    assert!(
        read(&root, "SECURITY.md").contains(&format!(
            "Only the latest `{series}.x` release gets security fixes."
        )),
        "SECURITY.md names another series than {series}.x as the one that gets fixes"
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

/// Each release that a text calls the current one.
fn stated_releases(text: &str) -> Vec<&str> {
    text.split("The current release is `")
        .skip(1)
        .map(|rest| rest.split('`').next().unwrap_or(""))
        .collect()
}

/// The version of each line of a text that asks for this crate as a dependency, in either
/// form: `ayaml = "1.2.3"`, or `ayaml = { version = "1.2.3", .. }`.
fn dependency_versions(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|line| line.trim_start().strip_prefix("ayaml = "))
        .map(|rest| {
            let rest = rest
                .split_once("version = ")
                .map_or(rest, |(_, after)| after);
            rest.strip_prefix('"')
                .and_then(|rest| rest.split('"').next())
                .unwrap_or(rest)
        })
        .collect()
}

/// The two helpers above find a version wherever a document can state one, so that a second,
/// stale statement fails the test below, where it once passed beside a true one.
#[test]
fn a_stale_version_beside_a_true_one_is_found() {
    let text = "The current release is `1.2.3`. And: The current release is `1.2.2`.\n\
        ayaml = \"1.2.3\"\n  ayaml = { version = \"1.2.2\", features = [\"x\"] }\n\
        ayaml = { features = [\"x\"], version = \"1.2.1\" }\nayaml = 7\nnot ayaml = \"9\"\n";
    assert_eq!(stated_releases(text), ["1.2.3", "1.2.2"]);
    assert_eq!(dependency_versions(text), ["1.2.3", "1.2.2", "1.2.1", "7"]);
    assert_eq!(stated_releases("no release is stated"), Vec::<&str>::new());
}

/// Every Rust source file under `directory`.
fn sources(directory: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).expect("a source directory") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// The lines of the crate's code that may name the map behind `Mapping`, or a name imported
/// from its crate, each with the number of times it stands in `mapping.rs`: the import, private
/// fields, and code inside functions. No other file names it at all.
const MAP_LINES: &[(&str, usize)] = &[
    ("use indexmap::{Equivalent, IndexMap};", 1),
    ("impl Equivalent<Value> for StringKey<'_> {", 1),
    ("map: IndexMap<Value, Value, GlobalHasher>,", 1),
    (
        "map: IndexMap::with_capacity_and_hasher(capacity, GlobalHasher),",
        1,
    ),
    ("indexmap::map::Entry::Vacant(entry) => {", 1),
    (
        "indexmap::map::Entry::Occupied(entry) => Err(crate::value::key_text(entry.key())),",
        1,
    ),
    ("inner: indexmap::map::Entry<'a, Value, Value>,", 1),
    (
        "if let indexmap::map::Entry::Occupied(entry) = &mut self.inner {",
        1,
    ),
    ("inner: indexmap::map::Iter<'a, Value, Value>,", 1),
    ("inner: indexmap::map::IterMut<'a, Value, Value>,", 1),
    ("inner: indexmap::map::IntoIter<Value, Value>,", 1),
];

/// The lines of a source file that name the map's crate, or a name that the file imports from
/// it, and are not among `allowed`. A comment may say what it likes.
fn lines_naming_the_map(text: &str, allowed: &[(&str, usize)]) -> Vec<String> {
    // A name bound from the crate is as good as its path, so each import adds its names: the
    // last word of every path it lists, and what an `as` renames one to.
    let mut names = vec!["indexmap".to_owned()];
    for line in text.lines().map(str::trim) {
        let Some(found) = line.find("use indexmap") else {
            continue;
        };
        if line.starts_with("//") {
            continue;
        }
        let imported = &line[found + "use indexmap".len()..];
        names.extend(
            imported
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .filter(|word| !word.is_empty() && !["as", "self", "map", "set"].contains(word))
                .map(str::to_owned),
        );
    }
    let names_it = |line: &str| {
        line.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|word| names.iter().any(|name| name == word))
    };
    let mut left: Vec<(&str, usize)> = allowed.to_vec();
    let mut found = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let code = line.trim();
        if code.starts_with("//") || !names_it(code) {
            continue;
        }
        match left
            .iter_mut()
            .find(|(allowed, count)| *allowed == code && *count > 0)
        {
            Some((_, count)) => *count -= 1,
            None => found.push(format!("{}: {code}", number + 1)),
        }
    }
    found
}

/// No type of the map behind `Mapping` is part of the crate's API, so that crate's major version
/// is none of this one's business: no public item, associated type, or trait implementation
/// names it. The crate's own fields and private code may, and each line that does is listed
/// above, so that a new one is looked at. A line that binds another name to one of its types,
/// such as an alias or a renamed import, names the crate itself, and so is one of them.
#[test]
fn the_api_names_no_type_of_the_map_behind_a_mapping() {
    let mut files = Vec::new();
    sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(files.len() > 20, "{} source files", files.len());
    let mut mapping = None;
    for file in files {
        let text = std::fs::read_to_string(&file).expect("a source file");
        if file.ends_with("src/mapping.rs") {
            mapping = Some(text);
            continue;
        }
        assert_eq!(
            lines_naming_the_map(&text, &[]),
            Vec::<String>::new(),
            "{} names the map's crate",
            file.display()
        );
    }
    let mapping = mapping.expect("src/mapping.rs");
    assert_eq!(
        lines_naming_the_map(&mapping, MAP_LINES),
        Vec::<String>::new(),
        "src/mapping.rs names the map's crate on a line that isn't listed"
    );
    // Every listed line is still there, so the list doesn't outlive what it allows.
    for (line, count) in MAP_LINES {
        assert_eq!(
            mapping
                .lines()
                .filter(|found| found.trim() == *line)
                .count(),
            *count,
            "{line}"
        );
    }

    // Each of these compiles, and each puts a type of the map's crate into the API. The check
    // has to see every one of them, on whichever line the type's name stands.
    for (leak, at) in [
        (
            "    pub fn raw(&self) -> indexmap::map::Iter<'_, Value, Value> {\n",
            "impl Mapping {\n",
        ),
        (
            "    pub fn raw(\n        &self,\n    ) -> indexmap::map::Iter<'_, Value, Value> {\n",
            "impl Mapping {\n",
        ),
        (
            "use indexmap::map::Iter as RawIter;\nimpl<'a> From<RawIter<'a, Value, Value>> for Iter<'a> {\n",
            "use indexmap::{Equivalent, IndexMap};\n",
        ),
        (
            "#[doc(hidden)] pub use indexmap;\n",
            "use indexmap::{Equivalent, IndexMap};\n",
        ),
        (
            "pub type Raw = IndexMap<Value, Value>;\n",
            "use indexmap::{Equivalent, IndexMap};\n",
        ),
        (
            "    pub map: IndexMap<Value, Value, GlobalHasher>,\n",
            "pub struct Mapping {\n",
        ),
        (
            "impl Equivalent<Value> for Value {\n",
            "use indexmap::{Equivalent, IndexMap};\n",
        ),
    ] {
        assert_eq!(mapping.matches(at).count(), 1, "{at:?}");
        let leaking = mapping.replacen(at, &format!("{at}{leak}"), 1);
        assert!(
            !lines_naming_the_map(&leaking, MAP_LINES).is_empty(),
            "the check misses {leak:?}"
        );
    }
}

/// CI runs the whole suite once more with every typed read made from events, in a build that
/// the flag below makes, under no condition. Without that step the tests of small inputs would
/// run on the tree reader alone.
#[test]
fn ci_runs_the_suite_with_every_typed_read_from_events() {
    let Some(root) = repository_root() else {
        return;
    };
    let text = read(&root, ".github/workflows/ci.yml");
    let suite = |workflow: &str| {
        steps_that_run(workflow, &|step| {
            step["env"]["RUSTFLAGS"].as_str() == Some("--cfg ayaml_typed_from_events")
                && step["run"].as_str().map(str::trim)
                    == Some("cargo test -p ayaml --locked --all-features")
        })
    };
    assert_eq!(
        suite(&text),
        Ok(1),
        "one step has to test typed reads from events at every size, under no condition"
    );
    // Each of these keeps the step and has it test the ordinary build, or nothing.
    let flags = "          RUSTFLAGS: --cfg ayaml_typed_from_events\n";
    assert_eq!(text.matches(flags).count(), 1);
    for change in [
        format!("{flags}          CARGO_ENCODED_RUSTFLAGS: \"\"\n"),
        format!("{flags}          CARGO_BUILD_RUSTFLAGS: \"\"\n"),
    ] {
        let changed = text.replacen(flags, &change, 1);
        assert!(suite(&changed).is_err(), "the check misses {change:?}");
    }
    let env = "        env:\n          RUSTFLAGS: --cfg ayaml_typed_from_events\n";
    assert_eq!(text.matches(env).count(), 1);
    for change in [
        format!("        if: false\n{env}"),
        format!("        continue-on-error: true\n{env}"),
        format!("        shell: true {{0}}\n{env}"),
    ] {
        let changed = text.replacen(env, &change, 1);
        assert!(suite(&changed).is_err(), "the check misses {change:?}");
    }
    // Without the flag, or with another command, no step is the one.
    assert_eq!(
        suite(&text.replacen(flags, "          OTHER: 1\n", 1)),
        Ok(0)
    );
}
