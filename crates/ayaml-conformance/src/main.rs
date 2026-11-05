//! Conformance runner for ayaml.
//!
//! Usage: `ayaml-conformance [FIXTURES] [SUITE_SRC] [DEVIATIONS]`
//!
//! It checks the synthetic compatibility fixtures and every suite case. For
//! every case in the pinned YAML Test Suite it runs these checks:
//!
//! - `class`: ayaml accepts exactly the inputs the suite accepts;
//! - `tree`: the public event stream matches the suite's `tree`;
//! - `tree-eager`: the eager event parser behind the value and typed paths
//!   matches it too;
//! - `json`: the composed documents match the suite's `json`;
//! - `emit-events`: re-emitting the events and parsing the output gives the
//!   same events, ignoring presentation (collection style, document markers,
//!   and the choice among non-plain scalar styles);
//! - `emit-stream`: the lazy event writer, fed by the lazy parser, writes
//!   exactly what the collected-event writer writes;
//! - `emit-value`: re-emitting the composed documents and parsing the output
//!   gives equal values.
//!
//! By default the runner uses the spec-following parser. Built with
//! `--features compat`, it uses the libyaml-compatible parser instead:
//! the `class`, `tree`, and `json` checks then run through `ayaml::compat`, and
//! the mode-independent emitter checks are skipped.
//!
//! Known deviations are listed in `tests/data/yaml-test-suite-deviations.txt`
//! as `<case>#<index> <default|compat|all> <check> <reason>`. With
//! `AYAML_STRICT_SUITE=1`, any unlisted deviation fails the run, and so does
//! any listed one that no longer occurs. `AYAML_SUITE_SHOW=<n>` limits how
//! many deviations are printed (default 40).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use ayaml::{CollectionStyle, Event, Limits, ScalarStyle, Tag};

#[cfg(not(feature = "compat"))]
const MODE: &str = "default";
#[cfg(feature = "compat")]
const MODE: &str = "compat";

const CHECKS: [&str; 7] = [
    "class",
    "tree",
    "tree-eager",
    "json",
    "emit-events",
    "emit-stream",
    "emit-value",
];

fn walk(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk(&path, out);
        } else if path
            .extension()
            .is_some_and(|ext| ext == "yml" || ext == "yaml")
        {
            out.push(path);
        }
    }
}

fn is_expected_error(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains("expected-error") || name.contains("malformed"))
}

/// Translate the suite's visible placeholders into the characters they stand
/// for. Each of `———»`, `——»`, `—»`, and `»` is one tab; `↵` marks a line
/// break that is already present; `∎` at the end means "no final newline".
fn suite_input(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '—' => {
                while chars.peek() == Some(&'—') {
                    chars.next();
                }
                if chars.peek() == Some(&'»') {
                    chars.next();
                }
                out.push('\t');
            }
            '»' => out.push('\t'),
            '␣' => out.push(' '),
            '←' => out.push('\r'),
            '⇔' => out.push('\u{feff}'),
            '↵' => {}
            other => out.push(other),
        }
    }
    if out.ends_with("∎\n") {
        out.truncate(out.len() - "∎\n".len());
    } else if out.ends_with('∎') {
        out.pop();
    }
    out
}

fn suite_limits() -> Limits {
    Limits::default().with_max_documents(1_000)
}

// ---------------------------------------------------------------------------
// Event trees
// ---------------------------------------------------------------------------

fn tag_text(tag: &Tag) -> String {
    format!("<{}{}>", tag.handle(), tag.suffix())
}

fn properties(anchor: Option<&ayaml::Anchor<'_>>, tag: Option<&Tag>) -> String {
    let mut out = String::new();
    if let Some(anchor) = anchor {
        out.push_str(" &");
        out.push_str(anchor.as_str());
    }
    if let Some(tag) = tag {
        out.push(' ');
        out.push_str(&tag_text(tag));
    }
    out
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{8}' => out.push_str("\\b"),
            '\0' => out.push_str("\\0"),
            '\u{7}' => out.push_str("\\a"),
            '\u{b}' => out.push_str("\\v"),
            '\u{c}' => out.push_str("\\f"),
            '\u{1b}' => out.push_str("\\e"),
            '\u{feff}' => out.push_str("\\uFEFF"),
            '\u{85}' => out.push_str("\\N"),
            '\u{a0}' => out.push_str("\\_"),
            '\u{2028}' => out.push_str("\\L"),
            '\u{2029}' => out.push_str("\\P"),
            other => out.push(other),
        }
    }
    out
}

/// How much presentation detail a tree keeps.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Detail {
    /// Exactly the suite's tree format.
    Suite,
    /// Content only: no collection styles or document markers. A scalar is
    /// written as `"` when its style cannot change its value (it is tagged,
    /// non-plain, or plain text that resolves to a string) and as `:`
    /// otherwise, so quoting `-1` or an empty plain scalar is a difference
    /// but quoting `a b` is not.
    Content,
}

/// Whether an untagged plain scalar with this text resolves to a string.
fn plain_is_string(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    if value.chars().any(char::is_whitespace) {
        return true;
    }
    !matches!(
        ayaml::parse_value(value),
        Ok(ayaml::Value::Null | ayaml::Value::Bool(_) | ayaml::Value::Number(_))
    )
}

fn tree(events: &[Event<'_>], detail: Detail) -> Vec<String> {
    let full = detail == Detail::Suite;
    let mut out = Vec::with_capacity(events.len());
    for event in events {
        out.push(match event {
            Event::StreamStart { .. } => "+STR".to_owned(),
            Event::StreamEnd { .. } => "-STR".to_owned(),
            Event::DocumentStart { explicit, .. } => if *explicit && full {
                "+DOC ---"
            } else {
                "+DOC"
            }
            .to_owned(),
            Event::DocumentEnd { explicit, .. } => if *explicit && full {
                "-DOC ..."
            } else {
                "-DOC"
            }
            .to_owned(),
            Event::MappingStart {
                tag, anchor, style, ..
            } => format!(
                "+MAP{}{}",
                if *style == CollectionStyle::Flow && full {
                    " {}"
                } else {
                    ""
                },
                properties(anchor.as_ref(), tag.as_ref())
            ),
            Event::MappingEnd { .. } => "-MAP".to_owned(),
            Event::SequenceStart {
                tag, anchor, style, ..
            } => format!(
                "+SEQ{}{}",
                if *style == CollectionStyle::Flow && full {
                    " []"
                } else {
                    ""
                },
                properties(anchor.as_ref(), tag.as_ref())
            ),
            Event::SequenceEnd { .. } => "-SEQ".to_owned(),
            Event::Alias { anchor, .. } => format!("=ALI *{}", anchor.as_str()),
            Event::Directive { .. } => continue,
            Event::Scalar {
                value,
                style,
                tag,
                anchor,
                ..
            } => {
                let indicator = match (style, full) {
                    (ScalarStyle::Plain, false) if tag.is_some() || plain_is_string(value) => '"',
                    (ScalarStyle::Plain, _) => ':',
                    (_, false) => '"',
                    (ScalarStyle::SingleQuoted, true) => '\'',
                    (ScalarStyle::DoubleQuoted, true) => '"',
                    (ScalarStyle::Literal, true) => '|',
                    (ScalarStyle::Folded, true) => '>',
                    (_, true) => '"',
                };
                format!(
                    "=VAL{} {indicator}{}",
                    properties(anchor.as_ref(), tag.as_ref()),
                    escape(value)
                )
            }
            _ => continue,
        });
    }
    out
}

fn expected_tree(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.trim_start().replace('␣', " "))
        .filter(|line| !line.is_empty())
        .collect()
}

fn first_difference(actual: &[String], expected: &[String]) -> String {
    let index = actual
        .iter()
        .zip(expected)
        .position(|(a, e)| a != e)
        .unwrap_or(actual.len().min(expected.len()));
    format!(
        "line {index}: got `{}`, want `{}`",
        actual.get(index).map_or("<end>", String::as_str),
        expected.get(index).map_or("<end>", String::as_str)
    )
}

// ---------------------------------------------------------------------------
// Mode-specific parser entry points
// ---------------------------------------------------------------------------

#[cfg(not(feature = "compat"))]
fn events(input: &str) -> Result<Vec<Event<'_>>, ayaml::Error> {
    ayaml::parse_events_with_limits(input, &suite_limits())?.collect()
}

#[cfg(feature = "compat")]
fn events(input: &str) -> Result<Vec<Event<'_>>, ayaml::Error> {
    ayaml::compat::parse_events_with_limits(input, &suite_limits())
}

#[cfg(not(feature = "compat"))]
fn eager_events(input: &str) -> Option<Result<Vec<Event<'_>>, ayaml::Error>> {
    Some(ayaml::internals::parse_events(input, &suite_limits()))
}

#[cfg(feature = "compat")]
fn eager_events(_input: &str) -> Option<Result<Vec<Event<'_>>, ayaml::Error>> {
    // In compat mode `events` already is the eager parser.
    None
}

#[cfg(not(feature = "compat"))]
fn documents(input: &str) -> Option<Result<Vec<ayaml::Value>, ayaml::Error>> {
    Some(ayaml::parse_documents_with_limits(input, &suite_limits()))
}

#[cfg(feature = "compat")]
fn documents(input: &str) -> Option<Result<Vec<ayaml::Value>, ayaml::Error>> {
    // The compat surface is single-document; multi-document cases are skipped.
    let count = events(input)
        .ok()?
        .iter()
        .filter(|event| matches!(event, Event::DocumentStart { .. }))
        .count();
    if count > 1 {
        return None;
    }
    Some(
        ayaml::compat::from_str::<ayaml::Value>(input)
            .map(|value| if count == 0 { Vec::new() } else { vec![value] }),
    )
}

// ---------------------------------------------------------------------------
// JSON comparison
// ---------------------------------------------------------------------------

fn json_documents(text: &str) -> Result<Vec<serde_json::Value>, String> {
    serde_json::Deserializer::from_str(text)
        .into_iter::<serde_json::Value>()
        .collect::<Result<_, _>>()
        .map_err(|error| format!("suite json does not parse: {error}"))
}

fn json_equal(actual: &serde_json::Value, expected: &serde_json::Value) -> bool {
    use serde_json::Value as J;
    match (actual, expected) {
        (J::Number(a), J::Number(e)) => match (a.as_i64(), e.as_i64()) {
            (Some(a), Some(e)) => a == e,
            _ => a.as_f64() == e.as_f64(),
        },
        (J::Array(a), J::Array(e)) => {
            a.len() == e.len() && a.iter().zip(e).all(|(a, e)| json_equal(a, e))
        }
        (J::Object(a), J::Object(e)) => {
            a.len() == e.len()
                && a.iter()
                    .all(|(key, value)| e.get(key).is_some_and(|other| json_equal(value, other)))
        }
        _ => actual == expected,
    }
}

/// `value` without its tags. The suite's JSON holds only the payloads, and a
/// tagged `Value` serializes as a one-entry map from the tag.
fn untagged(value: &ayaml::Value) -> ayaml::Value {
    match value {
        ayaml::Value::Tagged(tagged) => untagged(&tagged.value),
        ayaml::Value::Sequence(items) => {
            ayaml::Value::Sequence(items.iter().map(untagged).collect())
        }
        ayaml::Value::Mapping(mapping) => {
            let mut result = ayaml::Mapping::new();
            for (key, value) in mapping.iter() {
                result.insert(untagged(key), untagged(value));
            }
            ayaml::Value::Mapping(result)
        }
        other => other.clone(),
    }
}

fn check_json(input: &str, expected: &str) -> Option<Result<(), String>> {
    let documents = documents(input)?;
    Some((|| {
        let expected = json_documents(expected)?;
        let actual = documents.map_err(|error| format!("rejected: {error}"))?;
        if actual.len() != expected.len() {
            return Err(format!(
                "{} documents, suite json has {}",
                actual.len(),
                expected.len()
            ));
        }
        for (index, (value, want)) in actual.iter().zip(&expected).enumerate() {
            let got = serde_json::to_value(untagged(value))
                .map_err(|error| format!("document {index} has no JSON form: {error}"))?;
            if !json_equal(&got, want) {
                return Err(format!("document {index}: got {got}, want {want}"));
            }
        }
        Ok(())
    })())
}

// ---------------------------------------------------------------------------
// Emitter round trips (default mode only)
// ---------------------------------------------------------------------------

fn check_emit_events(events: &[Event<'_>]) -> Result<(), String> {
    let limits = suite_limits();
    let output = ayaml::to_string_events_with_limits(events, &limits)
        .map_err(|error| format!("emit failed: {error}"))?;
    let reparsed = ayaml::parse_events_with_limits(&output, &limits)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .map_err(|error| format!("output does not parse: {error}; output {output:?}"))?;
    let before = tree(events, Detail::Content);
    let after = tree(&reparsed, Detail::Content);
    if before == after {
        Ok(())
    } else {
        Err(format!(
            "{}; output {output:?}",
            first_difference(&after, &before)
        ))
    }
}

fn check_emit_stream(input: &str, events: &[Event<'_>]) -> Result<(), String> {
    let limits = suite_limits();
    let collected = ayaml::to_string_events_with_limits(events, &limits)
        .map_err(|error| format!("emit failed: {error}"))?;
    let stream = ayaml::parse_events_with_limits(input, &limits)
        .map_err(|error| format!("lazy parse failed: {error}"))?;
    let mut lazy = Vec::new();
    ayaml::to_writer_event_stream_with_limits(&mut lazy, stream, &limits)
        .map_err(|error| format!("lazy emit failed: {error}"))?;
    if lazy == collected.as_bytes() {
        Ok(())
    } else {
        Err(format!(
            "lazy output {:?} differs from collected output {collected:?}",
            String::from_utf8_lossy(&lazy)
        ))
    }
}

fn check_emit_value(documents: &[ayaml::Value]) -> Result<(), String> {
    let limits = suite_limits();
    let output = ayaml::to_string_documents_with_limits(documents, &limits)
        .map_err(|error| format!("emit failed: {error}"))?;
    let reparsed = ayaml::parse_documents_with_limits(&output, &limits)
        .map_err(|error| format!("output does not parse: {error}; output {output:?}"))?;
    if reparsed == documents {
        Ok(())
    } else {
        Err(format!("reparsed value differs; output {output:?}"))
    }
}

// ---------------------------------------------------------------------------
// Deviations file
// ---------------------------------------------------------------------------

type Key = (String, String);

fn load_deviations(path: &Path) -> Result<BTreeMap<Key, String>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        }
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let mut out = BTreeMap::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut rest = line;
        let mut fields = [""; 3];
        for slot in &mut fields {
            let trimmed = rest.trim_start();
            let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
            *slot = &trimmed[..end];
            rest = &trimmed[end..];
        }
        let [case, mode, check] = fields;
        if check.is_empty() {
            return Err(format!(
                "{}:{}: malformed entry",
                path.display(),
                number + 1
            ));
        }
        let reason = rest.trim();
        if !matches!(mode, "default" | "compat" | "all") || !CHECKS.contains(&check) {
            return Err(format!(
                "{}:{}: unknown mode or check",
                path.display(),
                number + 1
            ));
        }
        if reason.is_empty() {
            return Err(format!("{}:{}: missing reason", path.display(), number + 1));
        }
        if mode == "all" || mode == MODE {
            out.insert((case.to_owned(), check.to_owned()), reason.to_owned());
        }
    }
    Ok(out)
}

struct Report {
    show: usize,
    shown: usize,
    observed: BTreeMap<Key, String>,
    runs: BTreeMap<&'static str, (usize, usize)>,
}

impl Report {
    fn record(&mut self, case: &str, check: &'static str, outcome: Result<(), String>) {
        let entry = self.runs.entry(check).or_default();
        entry.0 += 1;
        if let Err(detail) = outcome {
            entry.1 += 1;
            self.observed
                .insert((case.to_owned(), check.to_owned()), detail);
        }
    }
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

fn check_fixtures(root: &Path) -> (u32, u32) {
    let mut paths = Vec::new();
    walk(root, &mut paths);
    let (mut fixtures, mut failures) = (0, 0);
    for path in &paths {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("FAIL read {}: {error}", path.display());
                failures += 1;
                continue;
            }
        };
        fixtures += 1;
        let expect_error = is_expected_error(path);
        #[cfg(not(feature = "compat"))]
        let result = ayaml::parse_value(&text).map(|_| ());
        #[cfg(feature = "compat")]
        let result = ayaml::compat::from_str::<ayaml::Value>(&text).map(|_| ());
        match (expect_error, result) {
            (true, Ok(())) => {
                eprintln!("FAIL {}: expected-error fixture parsed", path.display());
                failures += 1;
            }
            (false, Err(error)) => {
                eprintln!("FAIL {}: {error}", path.display());
                failures += 1;
            }
            _ => {}
        }
    }
    (fixtures, failures)
}

fn field<'a>(case: &'a ayaml::Value, name: &str) -> Option<&'a str> {
    case.get(name).and_then(ayaml::Value::as_str)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let fixture_root = args
        .next()
        .map_or_else(|| PathBuf::from("tests/fixtures"), PathBuf::from);
    let suite_root = args.next().map_or_else(
        || PathBuf::from("tests/data/yaml-test-suite/src"),
        PathBuf::from,
    );
    let deviations_path = args.next().map_or_else(
        || PathBuf::from("tests/data/yaml-test-suite-deviations.txt"),
        PathBuf::from,
    );
    let strict = std::env::var_os("AYAML_STRICT_SUITE").is_some();

    let (fixtures, mut failures) = check_fixtures(&fixture_root);

    let listed = match load_deviations(&deviations_path) {
        Ok(listed) => listed,
        Err(error) => {
            eprintln!("FAIL {error}");
            std::process::exit(1);
        }
    };
    let mut report = Report {
        show: std::env::var("AYAML_SUITE_SHOW")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(40),
        shown: 0,
        observed: BTreeMap::new(),
        runs: BTreeMap::new(),
    };

    let mut suite_paths = Vec::new();
    walk(&suite_root, &mut suite_paths);
    let (mut cases, mut expected_errors) = (0_u32, 0_u32);
    let mut known_cases = BTreeSet::new();
    for path in &suite_paths {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("FAIL read suite {}: {error}", path.display());
                failures += 1;
                continue;
            }
        };
        let metadata = match ayaml::parse_value(&text) {
            Ok(metadata) => metadata,
            Err(error) => {
                eprintln!("FAIL suite {}: {error}", path.display());
                failures += 1;
                continue;
            }
        };
        let Some(entries) = metadata.as_sequence() else {
            eprintln!("FAIL suite {}: metadata is not a sequence", path.display());
            failures += 1;
            continue;
        };
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Later cases in a file inherit `yaml`, `tree`, and `json` from
        // earlier ones unless they override them.
        let (mut yaml, mut tree_text, mut json_text) = (None, None, None);
        for (index, case) in entries.iter().enumerate() {
            if let Some(value) = field(case, "yaml") {
                yaml = Some(value.to_owned());
            }
            if let Some(value) = field(case, "tree") {
                tree_text = Some(value.to_owned());
            }
            if case.get("json").is_some() {
                json_text = field(case, "json").map(str::to_owned);
            }
            if case.get("skip").and_then(ayaml::Value::as_bool) == Some(true) {
                continue;
            }
            let Some(raw) = yaml.as_deref() else {
                eprintln!("FAIL suite {}#{index}: missing yaml field", path.display());
                failures += 1;
                continue;
            };
            let id = format!("{stem}#{index}");
            known_cases.insert(id.clone());
            let input = suite_input(raw);
            let expect_error = case.get("fail").and_then(ayaml::Value::as_bool) == Some(true)
                || field(case, "tags")
                    .unwrap_or("")
                    .split_whitespace()
                    .any(|tag| tag == "error");
            cases += 1;
            let parsed = events(&input);
            let class = match (&parsed, expect_error) {
                (Ok(_), true) => Err("parsed, but the suite expects an error".to_owned()),
                (Err(error), false) => Err(format!("rejected: {error}")),
                _ => Ok(()),
            };
            report.record(&id, "class", class);
            if expect_error {
                expected_errors += 1;
                continue;
            }
            let Ok(parsed) = parsed else { continue };
            if let Some(text) = &tree_text {
                let want = expected_tree(text);
                let got = tree(&parsed, Detail::Suite);
                report.record(
                    &id,
                    "tree",
                    if got == want {
                        Ok(())
                    } else {
                        Err(first_difference(&got, &want))
                    },
                );
                if let Some(eager) = eager_events(&input) {
                    let outcome = match eager {
                        Ok(eager) => {
                            let got = tree(&eager, Detail::Suite);
                            if got == want {
                                Ok(())
                            } else {
                                Err(first_difference(&got, &want))
                            }
                        }
                        Err(error) => Err(format!("rejected: {error}")),
                    };
                    report.record(&id, "tree-eager", outcome);
                }
            }
            if let Some(text) = &json_text
                && let Some(outcome) = check_json(&input, text)
            {
                report.record(&id, "json", outcome);
            }
            if MODE == "default" {
                report.record(&id, "emit-events", check_emit_events(&parsed));
                report.record(&id, "emit-stream", check_emit_stream(&input, &parsed));
                if let Some(Ok(values)) = documents(&input) {
                    report.record(&id, "emit-value", check_emit_value(&values));
                }
            }
        }
    }

    // Compare what happened with what the deviations file expects.
    let mut unexpected = 0_usize;
    for ((case, check), detail) in &report.observed {
        if listed.contains_key(&(case.clone(), check.clone())) {
            continue;
        }
        unexpected += 1;
        if report.shown < report.show {
            report.shown += 1;
            eprintln!("DEVIATION {case} {MODE} {check}: {detail}");
        }
    }
    let mut stale = 0_usize;
    for ((case, check), reason) in &listed {
        let ran = known_cases.contains(case);
        if !ran || !report.observed.contains_key(&(case.clone(), check.clone())) {
            stale += 1;
            eprintln!("STALE {case} {MODE} {check}: listed ({reason}) but does not deviate");
        }
    }

    let summary: Vec<String> = CHECKS
        .iter()
        .filter_map(|check| {
            report
                .runs
                .get(check)
                .map(|(run, failed)| format!("{check}={}/{run}", run - failed))
        })
        .collect();
    println!(
        "mode={MODE} fixtures={fixtures} failures={failures} suite_cases={cases} expected_errors={expected_errors} {} listed={} unexpected={unexpected} stale={stale}",
        summary.join(" "),
        listed.len()
    );
    if failures != 0 || (strict && (unexpected != 0 || stale != 0)) {
        std::process::exit(1);
    }
}
