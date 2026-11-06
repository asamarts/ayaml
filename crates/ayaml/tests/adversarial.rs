//! Adversarial inputs on every public entry point.
//!
//! Each test drives every public path that reads YAML, so that no entry
//! point bypasses a limit that another enforces.
//!
//! A test whose input could grow without bound if a limit regressed runs its body in a child
//! process, through [`in_child`]. On Linux the child has an address-space limit, so running
//! out of memory aborts that child and fails that test, and the machine stays usable. Every
//! system has the deadline. A stack overflow also fails one test, not the whole binary.

use std::hash::Hash;
use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ayaml::{ErrorKind, Limits, MAX_DEPTH, MAX_DEPTH_CAP, Value};
use serde::de::IgnoredAny;

/// Set in a child process, where a test runs its body instead of starting another child.
const CHILD: &str = "AYAML_ADVERSARIAL_CHILD";

/// The address space a child may use, in KiB.
const ADDRESS_SPACE_KIB: u64 = 1 << 20;

/// The most address space a child may have used when its body ends, in KiB. This keeps normal
/// use well below the limit, and it fails a regression too small to reach the limit.
const NORMAL_PEAK_KIB: u64 = ADDRESS_SPACE_KIB / 2;

/// How long a child may run. A regression that spins without allocating ends here.
const CHILD_DEADLINE: Duration = Duration::from_secs(300);

/// Set the limits, then become the test binary, which is `$0`. `$1` is the limit in KiB. A
/// lower limit that is already set stays, because raising it could fail. Core dumps are off,
/// because a child that hits the limit aborts with up to `$1` of memory.
const LIMIT_THEN_RUN: &str = r#"
[ "$(ulimit -v)" != unlimited ] && [ "$(ulimit -v)" -le "$1" ] || ulimit -v "$1" || exit 125
ulimit -c 0
shift
exec "$0" "$@"
"#;

/// How one child ended.
struct ChildRun {
    /// `None` if the deadline passed and the child was killed.
    status: Option<ExitStatus>,
    output: String,
    elapsed: Duration,
}

impl ChildRun {
    /// libtest exits 0 when `--exact` matches nothing, so the test has to be seen passing.
    fn passed(&self) -> bool {
        self.status.is_some_and(|status| status.success()) && self.output.contains("1 passed")
    }
}

fn read_all(mut pipe: impl Read + Send + 'static) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        // A read error only shortens the output that a failure reports.
        let _ = pipe.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// Run one test of this binary in a child process and wait for it, up to `deadline`.
fn run_child(test: &str, deadline: Duration) -> ChildRun {
    let binary = std::env::current_exe().expect("the test binary's path");
    let mut command = if cfg!(target_os = "linux") {
        let mut command = Command::new("sh");
        command
            .args(["-c", LIMIT_THEN_RUN])
            .arg(binary)
            .arg(ADDRESS_SPACE_KIB.to_string());
        command
    } else {
        Command::new(binary)
    };
    let started = Instant::now();
    let mut child = command
        .args([test, "--exact", "--nocapture", "--include-ignored"])
        .env(CHILD, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the child");
    let stdout = read_all(child.stdout.take().expect("piped stdout"));
    let stderr = read_all(child.stderr.take().expect("piped stderr"));
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait for the child") {
            break Some(status);
        }
        if started.elapsed() > deadline {
            child.kill().expect("kill the child");
            child.wait().expect("wait for the killed child");
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let elapsed = started.elapsed();
    let output = stdout.join().expect("stdout reader") + &stderr.join().expect("stderr reader");
    ChildRun {
        status,
        output,
        elapsed,
    }
}

/// The first number after `label` in a file under `/proc/self`.
fn proc_self_number(file: &str, label: &str) -> Option<u64> {
    std::fs::read_to_string(format!("/proc/self/{file}"))
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix(label))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Fail unless this process really has its address-space limit. Linux only.
fn assert_limited() {
    if cfg!(target_os = "linux") {
        let limit = proc_self_number("limits", "Max address space");
        assert!(
            limit.is_some_and(|bytes| bytes <= ADDRESS_SPACE_KIB * 1024),
            "no address-space limit in this child: {limit:?}"
        );
    }
}

/// Run a test's body in a child process, or directly if this is the child. `test` has to be
/// the name of the calling test, because the child reruns it by name.
fn in_child(test: &str, body: impl FnOnce()) {
    if std::env::var_os(CHILD).is_none() {
        let run = run_child(test, CHILD_DEADLINE);
        assert!(
            run.passed(),
            "{test}: ended with {:?} after {:?}\n{}",
            run.status,
            run.elapsed,
            run.output
        );
        return;
    }
    assert_limited();
    body();
    if cfg!(target_os = "linux") {
        let peak = proc_self_number("status", "VmPeak:").expect("VmPeak");
        println!("{test}: peak address space {peak} KiB");
        assert!(
            peak <= NORMAL_PEAK_KIB,
            "{test} used {peak} KiB of address space, over the {NORMAL_PEAK_KIB} KiB expected"
        );
    }
}

/// Keep allocating and touching memory, as a runaway expansion would, with every real limit
/// still on. It stops only when the allocator can't get more.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "only for a child of runaway_allocation_fails_quickly_and_cleanly"]
fn keep_allocating() {
    if std::env::var_os(CHILD).is_some() {
        assert_limited();
        let mut blocks = Vec::new();
        loop {
            blocks.push(vec![1_u8; 1 << 20]);
            std::hint::black_box(&blocks);
        }
    }
}

/// The limit stops a runaway child with Rust's allocation failure, well before the deadline.
#[cfg(target_os = "linux")]
#[test]
fn runaway_allocation_fails_quickly_and_cleanly() {
    let run = run_child("keep_allocating", CHILD_DEADLINE);
    assert!(!run.passed(), "{}", run.output);
    assert!(run.status.is_some(), "hit the deadline: {}", run.output);
    assert!(
        run.output.contains("memory allocation of"),
        "{:?}: {}",
        run.status,
        run.output
    );
    assert!(run.elapsed < Duration::from_secs(60), "{:?}", run.elapsed);
}

#[test]
#[ignore = "only for a child of a_failing_child_fails_its_test"]
fn fail_in_a_child() {
    assert!(
        std::env::var_os(CHILD).is_none(),
        "deliberate failure in a child"
    );
}

/// A failure in the child fails the test that started it.
#[test]
#[should_panic(expected = "deliberate failure in a child")]
fn a_failing_child_fails_its_test() {
    in_child("fail_in_a_child", || unreachable!("this isn't the child"));
}

#[test]
#[ignore = "only for a child of a_child_past_its_deadline_is_killed"]
fn never_finish() {
    if std::env::var_os(CHILD).is_some() {
        std::thread::sleep(Duration::from_secs(3_600));
    }
}

#[test]
fn a_child_past_its_deadline_is_killed() {
    let run = run_child("never_finish", Duration::from_secs(1));
    assert!(run.status.is_none(), "{:?}: {}", run.status, run.output);
    assert!(run.elapsed < Duration::from_secs(60), "{:?}", run.elapsed);
}

/// A misspelled name runs nothing and exits 0, which must not count as passing.
#[test]
fn a_child_that_runs_no_test_does_not_pass() {
    let run = run_child("no_such_test", CHILD_DEADLINE);
    assert!(
        run.status.is_some_and(|status| status.success()),
        "{:?}: {}",
        run.status,
        run.output
    );
    assert!(!run.passed(), "{}", run.output);
}

/// A billion-laughs bomb: nine levels of nine aliases each, 468 bytes
/// that expand to 9^9 scalars.
fn alias_bomb() -> String {
    let mut bomb = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x]\n");
    for level in 1..9 {
        let previous = format!("*a{}", level - 1);
        let row = [previous.as_str(); 9].join(", ");
        bomb.push_str(&format!("a{level}: &a{level} [{row}]\n"));
    }
    bomb
}

fn ok<T>(result: Result<T, ayaml::Error>) -> Result<(), ayaml::Error> {
    result.map(drop)
}

/// Every public entry point that composes or decodes `input`, with the
/// default limits, paired with its name.
fn entry_points(input: &str) -> Vec<(&'static str, Result<(), ayaml::Error>)> {
    let cursor = |next: fn(&mut ayaml::Deserializer<'_>) -> Result<(), ayaml::Error>| {
        ayaml::Deserializer::from_str(input).and_then(|mut cursor| next(&mut cursor))
    };
    let results = vec![
        ("parse_value", ok(ayaml::parse_value(input))),
        ("parse_documents", ok(ayaml::parse_documents(input))),
        ("from_str", ok(ayaml::from_str::<Value>(input))),
        (
            "from_slice",
            ok(ayaml::from_slice::<Value>(input.as_bytes())),
        ),
        (
            "from_str_borrowed",
            ok(ayaml::from_str_borrowed::<Value>(input)),
        ),
        (
            "from_str_borrowed (IgnoredAny)",
            ok(ayaml::from_str_borrowed::<IgnoredAny>(input)),
        ),
        (
            "from_slice_borrowed",
            ok(ayaml::from_slice_borrowed::<Value>(input.as_bytes())),
        ),
        (
            "Deserializer::next",
            cursor(|cursor| cursor.next::<Value>().map(drop)),
        ),
        (
            "Deserializer::next_borrowed",
            cursor(|cursor| cursor.next_borrowed::<IgnoredAny>().map(drop)),
        ),
        (
            "Deserializer::next_seed",
            cursor(|cursor| {
                cursor
                    .next_seed(std::marker::PhantomData::<Value>)
                    .map(drop)
            }),
        ),
        (
            "Deserializer::next_seed_borrowed",
            cursor(|cursor| {
                cursor
                    .next_seed_borrowed(std::marker::PhantomData::<IgnoredAny>)
                    .map(drop)
            }),
        ),
    ];
    #[cfg(feature = "libyaml-compat")]
    let results = {
        let mut results = results;
        results.extend([
            (
                "compat::from_str",
                ok(ayaml::compat::from_str::<Value>(input)),
            ),
            (
                "compat::from_str_seed",
                ok(ayaml::compat::from_str_seed(
                    input,
                    std::marker::PhantomData::<IgnoredAny>,
                )),
            ),
            (
                "compat::deserialize_any",
                ok(ayaml::compat::deserialize_any(input, IgnoredAny)),
            ),
        ]);
        results
    };
    results
}

/// Every entry point enforces the alias limits.
#[test]
fn alias_bomb_is_rejected_on_every_entry_point() {
    in_child("alias_bomb_is_rejected_on_every_entry_point", || {
        let bomb = alias_bomb();
        assert!(bomb.len() < 512, "{} bytes", bomb.len());
        for (name, result) in entry_points(&bomb) {
            let error = result.expect_err(name);
            assert_eq!(error.kind(), ErrorKind::Limit, "{name}: {error}");
        }
    });
}

/// The dereference count is enforced on every entry point, even
/// when the expansion is small.
#[test]
fn alias_dereference_limit_is_enforced_on_every_entry_point() {
    in_child(
        "alias_dereference_limit_is_enforced_on_every_entry_point",
        || {
            let mut input = String::from("a: &a 1\nb: [");
            input.push_str(&vec!["*a"; 100_001].join(", "));
            input.push_str("]\n");
            for (name, result) in entry_points(&input) {
                let error = result.expect_err(name);
                assert!(
                    error.kind() == ErrorKind::Limit || error.to_string().contains("alias"),
                    "{name}: {error}"
                );
            }
        },
    );
}

/// The limits that accept an input on one entry point accept it on all.
#[test]
fn ordinary_aliases_are_accepted_on_every_entry_point() {
    let input = "base: &b {x: [1, 2]}\nuse: *b\nmore: [*b, *b]\n";
    for (name, result) in entry_points(input) {
        result.unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

#[test]
fn compat_profile_keeps_its_single_document_limit() {
    assert_eq!(Limits::compat().max_documents, 1);
}

/// `a0: &a0 [0]`, then `aN: &aN [*aN-1]`: each alias nests the previous
/// chain one level deeper.
fn alias_chain(links: usize) -> String {
    let mut chain = String::from("a0: &a0 [0]\n");
    for link in 1..links {
        chain.push_str(&format!("a{link}: &a{link} [*a{}]\n", link - 1));
    }
    chain
}

/// An alias's nesting counts toward `max_depth` where the alias sits: 127
/// links are accepted and 128 rejected.
#[test]
fn alias_nesting_counts_toward_the_depth_limit_on_every_entry_point() {
    in_child(
        "alias_nesting_counts_toward_the_depth_limit_on_every_entry_point",
        || {
            for (name, result) in entry_points(&alias_chain(127)) {
                result.unwrap_or_else(|error| panic!("{name}: {error}"));
            }
            for links in [128, 8_000] {
                for (name, result) in entry_points(&alias_chain(links)) {
                    let error = result.expect_err(name);
                    // Compat reports its own message for the depth limit.
                    let message = error.to_string();
                    assert!(
                        (error.kind() == ErrorKind::Limit && message.contains("depth"))
                            || message.contains("recursion limit exceeded"),
                        "{name}: {error}"
                    );
                }
            }
        },
    );
}

/// Anchors cost nothing until an alias uses them. A 4 MiB scalar under
/// 100 nested anchors used to be copied once per anchor.
#[test]
fn nested_anchors_are_not_copied() {
    in_child("nested_anchors_are_not_copied", || {
        let mut input = String::new();
        for level in 0..100 {
            input.push_str(&format!("&a{level} ["));
        }
        input.push_str(&"x".repeat(4 << 20));
        input.push_str(&"]".repeat(100));
        let mut value = &ayaml::parse_value(&input).expect("parse");
        for _ in 0..100 {
            value = &value[0];
        }
        assert_eq!(value.as_str().map(str::len), Some(4 << 20));
        ayaml::from_str_borrowed::<IgnoredAny>(&input).expect("typed");
    });
}

/// The nesting shapes the depth tests use, each with a different recursion
/// profile in Serde (sequences, mappings) or the parser (block, flow,
/// compact, aliases).
const SHAPES: [&str; 6] = [
    "flow sequence",
    "flow mapping",
    "block sequence",
    "block mapping",
    "compact sequence",
    "alias chain",
];

/// `depth` nested collections of one shape around a scalar.
fn nested(shape: &str, depth: usize) -> String {
    let lines = |entry: &str| {
        let mut text: String = (0..depth)
            .map(|level| format!("{}{entry}\n", " ".repeat(level)))
            .collect();
        text.push_str(&format!("{}0\n", " ".repeat(depth)));
        text
    };
    match shape {
        "flow sequence" => format!("{}0{}", "[".repeat(depth), "]".repeat(depth)),
        "flow mapping" => format!("{}0{}", "{a: ".repeat(depth), "}".repeat(depth)),
        "block sequence" => lines("-"),
        "block mapping" => lines("a:"),
        "compact sequence" => format!("{}0\n", "- ".repeat(depth)),
        // `a0: &a0 {k: 0}`, then `aN: &aN {k: *aN-1}`: the root mapping
        // and N + 1 nested mappings, the deepest Serde recursion per level.
        "alias chain" => {
            let mut chain = String::from("a0: &a0 {k: 0}\n");
            for link in 1..depth - 1 {
                chain.push_str(&format!("a{link}: &a{link} {{k: *a{}}}\n", link - 1));
            }
            chain
        }
        _ => unreachable!("{shape}"),
    }
}

/// Run `body` on a thread with a 2 MiB stack, the default for spawned and
/// test threads. A stack overflow aborts the test binary.
fn on_small_stack(body: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(body)
        .expect("spawn")
        .join()
        .expect("no panic");
}

/// Every public entry point that takes explicit limits and reads YAML.
fn limited_entry_points(
    input: &str,
    limits: &Limits,
) -> Vec<(&'static str, Result<(), ayaml::Error>)> {
    let cursor = |next: fn(&mut ayaml::Deserializer<'_>) -> Result<(), ayaml::Error>| {
        ayaml::Deserializer::from_str_with_limits(input, limits)
            .and_then(|mut cursor| next(&mut cursor))
    };
    vec![
        (
            "parse_events_with_limits",
            ayaml::parse_events_with_limits(input, limits)
                .and_then(|stream| stream.collect::<Result<Vec<_>, _>>())
                .map(drop),
        ),
        (
            "parse_value_with_limits",
            ok(ayaml::parse_value_with_limits(input, limits)),
        ),
        (
            "parse_documents_with_limits",
            ok(ayaml::parse_documents_with_limits(input, limits)),
        ),
        (
            "from_str_borrowed_with_limits",
            ok(ayaml::from_str_borrowed_with_limits::<Value>(input, limits)),
        ),
        (
            "from_str_borrowed_with_limits (serde_json)",
            ok(ayaml::from_str_borrowed_with_limits::<serde_json::Value>(
                input, limits,
            )),
        ),
        (
            "from_str_borrowed_with_limits (IgnoredAny)",
            ok(ayaml::from_str_borrowed_with_limits::<IgnoredAny>(
                input, limits,
            )),
        ),
        (
            "Deserializer::next",
            cursor(|cursor| cursor.next::<Value>().map(drop)),
        ),
        (
            "Deserializer::next_borrowed",
            cursor(|cursor| cursor.next_borrowed::<serde_json::Value>().map(drop)),
        ),
        (
            "Deserializer::next_seed",
            cursor(|cursor| {
                cursor
                    .next_seed(std::marker::PhantomData::<Value>)
                    .map(drop)
            }),
        ),
        (
            "Deserializer::next_seed_borrowed",
            cursor(|cursor| {
                cursor
                    .next_seed_borrowed(std::marker::PhantomData::<IgnoredAny>)
                    .map(drop)
            }),
        ),
    ]
}

/// Every public path that takes an already-parsed value or event stream:
/// decoding, emitting, merging, and the `Value` trait implementations.
fn value_paths(input: &str, limits: &Limits) -> Vec<(&'static str, Result<(), ayaml::Error>)> {
    let value = ayaml::parse_value_with_limits(input, limits).expect("parse");
    let events = ayaml::parse_events_with_limits(input, limits)
        .and_then(|stream| stream.collect::<Result<Vec<_>, _>>())
        .expect("events");
    let mut merged = value.clone();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    let debug = format!("{value:?}");
    vec![
        ("from_value", ok(ayaml::from_value::<Value>(value.clone()))),
        (
            "from_value (serde_json)",
            ok(ayaml::from_value::<serde_json::Value>(value.clone())),
        ),
        ("to_value", ok(ayaml::to_value(&value))),
        (
            "to_string_value_with_limits",
            ok(ayaml::to_string_value_with_limits(&value, limits)),
        ),
        (
            "to_string_with_limits",
            ok(ayaml::to_string_with_limits(&value, limits)),
        ),
        (
            "to_string_documents_with_limits",
            ok(ayaml::to_string_documents_with_limits(
                std::slice::from_ref(&value),
                limits,
            )),
        ),
        (
            "to_string_events_with_limits",
            ok(ayaml::to_string_events_with_limits(&events, limits)),
        ),
        (
            "to_writer_event_stream_with_limits",
            ayaml::parse_events_with_limits(input, limits).and_then(|stream| {
                ayaml::to_writer_event_stream_with_limits(&mut Vec::new(), stream, limits)
            }),
        ),
        ("apply_merge_keys", merged.apply_merge_keys()),
        ("clone, eq, hash, debug", {
            assert_eq!(merged, value);
            assert!(!debug.is_empty());
            Ok(())
        }),
    ]
}

/// Expect the depth limit from an entry point reading `depth`-deep
/// `shape`. The event stream leaves aliases unexpanded, so an alias chain
/// stays shallow there.
fn assert_depth_error(shape: &str, name: &str, result: Result<(), ayaml::Error>) {
    if shape == "alias chain" && name == "parse_events_with_limits" {
        result.unwrap_or_else(|error| panic!("{shape}, {name}: {error}"));
        return;
    }
    let error = result.expect_err(name);
    let message = error.to_string();
    // Compat reports its own message for the depth limit.
    assert!(
        (error.kind() == ErrorKind::Limit && message.contains("depth"))
            || message.contains("recursion limit exceeded"),
        "{shape}, {name}: {error}"
    );
}

/// a `max_depth` above the cap is a configuration error on every path
/// that takes limits; the cap itself is accepted.
#[test]
fn max_depth_above_the_cap_is_rejected_on_every_entry_point() {
    let over = Limits::default().with_max_depth(MAX_DEPTH_CAP + 1);
    let at_cap = Limits::default().with_max_depth(MAX_DEPTH_CAP);
    let input = "a: [1, {b: 2}]\n";
    let value = ayaml::parse_value(input).expect("parse");
    let events = ayaml::parse_events(input)
        .and_then(|stream| stream.collect::<Result<Vec<_>, _>>())
        .expect("events");
    let emitters = |limits: &Limits| -> Vec<(&'static str, Result<(), ayaml::Error>)> {
        vec![
            (
                "to_string_value_with_limits",
                ok(ayaml::to_string_value_with_limits(&value, limits)),
            ),
            (
                "to_string_with_limits",
                ok(ayaml::to_string_with_limits(&value, limits)),
            ),
            (
                "to_string_documents_with_limits",
                ok(ayaml::to_string_documents_with_limits(
                    std::slice::from_ref(&value),
                    limits,
                )),
            ),
            (
                "to_string_events_with_limits",
                ok(ayaml::to_string_events_with_limits(&events, limits)),
            ),
            (
                "to_writer_event_stream_with_limits",
                ayaml::parse_events_with_limits(input, &Limits::default()).and_then(|stream| {
                    ayaml::to_writer_event_stream_with_limits(&mut Vec::new(), stream, limits)
                }),
            ),
        ]
    };
    for (name, result) in limited_entry_points(input, &over)
        .into_iter()
        .chain(emitters(&over))
    {
        let error = result.expect_err(name);
        assert_eq!(error.kind(), ErrorKind::Limit, "{name}: {error}");
        let expected = format!("max_depth {}", MAX_DEPTH_CAP + 1);
        assert!(error.to_string().contains(&expected), "{name}: {error}");
    }
    for (name, result) in limited_entry_points(input, &at_cap)
        .into_iter()
        .chain(emitters(&at_cap))
    {
        result.unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

/// At the cap, every path fits a 2 MiB thread, and one level
/// more is a depth error, not an abort. CI runs this file in debug and
/// release builds.
#[test]
fn every_entry_point_fits_a_small_stack_at_the_depth_cap() {
    in_child(
        "every_entry_point_fits_a_small_stack_at_the_depth_cap",
        || {
            on_small_stack(|| {
                let limits = Limits::default().with_max_depth(MAX_DEPTH_CAP);
                for shape in SHAPES {
                    let input = nested(shape, MAX_DEPTH_CAP);
                    for (name, result) in limited_entry_points(&input, &limits)
                        .into_iter()
                        .chain(value_paths(&input, &limits))
                    {
                        result.unwrap_or_else(|error| panic!("{shape}, {name}: {error}"));
                    }
                    for (name, result) in
                        limited_entry_points(&nested(shape, MAX_DEPTH_CAP + 1), &limits)
                    {
                        assert_depth_error(shape, name, result);
                    }
                }
            });
        },
    );
}

/// The entry points without a limits parameter, compat included, use the
/// default depth and fit a 2 MiB thread at it.
#[test]
fn default_entry_points_fit_a_small_stack_at_the_default_depth() {
    in_child(
        "default_entry_points_fit_a_small_stack_at_the_default_depth",
        || {
            on_small_stack(|| {
                for shape in SHAPES {
                    for (name, result) in entry_points(&nested(shape, MAX_DEPTH)) {
                        result.unwrap_or_else(|error| panic!("{shape}, {name}: {error}"));
                    }
                    for (name, result) in entry_points(&nested(shape, MAX_DEPTH + 1)) {
                        assert_depth_error(shape, name, result);
                    }
                }
            });
        },
    );
}

/// The parser keeps its own stack, so nesting far past any depth limit
/// is a limit error on a 2 MiB thread, never an abort.
#[test]
fn deep_nesting_is_a_limit_error_on_a_small_stack() {
    in_child("deep_nesting_is_a_limit_error_on_a_small_stack", || {
        on_small_stack(|| {
            let limits = Limits::default().with_max_depth(MAX_DEPTH_CAP);
            for shape in SHAPES {
                // Far past the limit: every path stops at its depth check.
                let depth = if shape.starts_with("block") {
                    1_000
                } else {
                    10_000
                };
                let input = nested(shape, depth);
                for (name, result) in limited_entry_points(&input, &limits) {
                    assert_depth_error(shape, name, result);
                }
                for (name, result) in entry_points(&input) {
                    assert_depth_error(shape, name, result);
                }
            }
        });
    });
}

/// `entries` entries of one wide shape: a single collection, or a run of
/// entries without nested nodes, that a grammar loop must not turn into one
/// native frame per entry.
fn wide(shape: &str, entries: usize) -> String {
    let each = |entry: &dyn Fn(usize) -> String| (0..entries).map(entry).collect::<String>();
    match shape {
        "empty explicit keys" => "?\n".repeat(entries),
        "explicit pairs" => each(&|index| format!("? k{index}\n: v\n")),
        "block keys" => each(&|index| format!("k{index}:\n")),
        "block pairs" => each(&|index| format!("k{index}: v\n")),
        "block sequence" => "-\n".repeat(entries),
        "block scalars" => "- a\n".repeat(entries),
        "block properties" => each(&|index| format!("- &a{index} x\n")),
        "empty flow pairs" => format!("{{{}}}", ":, ".repeat(entries)),
        "flow keys" => format!("{{{}}}", each(&|index| format!("k{index}, "))),
        "flow explicit keys" => format!("{{{}}}", each(&|index| format!("? k{index}, "))),
        "flow explicit pairs" => format!("{{{}}}", each(&|index| format!("? k{index} : v, "))),
        "flow sequence" => format!("[{}]", "a, ".repeat(entries)),
        "flow pairs in a sequence" => format!("[{}]", each(&|index| format!("k{index}: v, "))),
        "explicit pairs in a sequence" => {
            format!("[{}]", each(&|index| format!("? k{index} : v, ")))
        }
        "empty keys in a sequence" => format!("[{}]", ": v, ".repeat(entries)),
        _ => unreachable!("{shape}"),
    }
}

/// Wide collections fit a 2 MiB thread on every entry point and through
/// the event writer. Whether a value path accepts them (repeated null keys
/// are duplicates) does not matter here, only that nothing aborts.
#[test]
fn wide_collections_fit_a_small_stack() {
    in_child("wide_collections_fit_a_small_stack", || {
        on_small_stack(|| {
            let limits = Limits::default();
            for shape in [
                "empty explicit keys",
                "explicit pairs",
                "block keys",
                "block pairs",
                "block sequence",
                "block scalars",
                "block properties",
                "empty flow pairs",
                "flow keys",
                "flow explicit keys",
                "flow explicit pairs",
                "flow sequence",
                "flow pairs in a sequence",
                "explicit pairs in a sequence",
                "empty keys in a sequence",
            ] {
                // Per-entry recursion, which this guards against, aborted below
                // 5,000 entries in a release build and 1,000 in a debug build.
                // CI runs this file in both builds.
                let entries = if cfg!(debug_assertions) {
                    3_000
                } else {
                    10_000
                };
                let input = wide(shape, entries);
                let events = ayaml::parse_events_with_limits(&input, &limits)
                    .and_then(|stream| stream.collect::<Result<Vec<_>, _>>())
                    .unwrap_or_else(|error| panic!("{shape}: {error}"));
                ayaml::to_string_events_with_limits(&events, &limits)
                    .unwrap_or_else(|error| panic!("{shape}, to_string_events: {error}"));
                for (name, result) in limited_entry_points(&input, &limits)
                    .into_iter()
                    .chain(entry_points(&input))
                {
                    if let Err(error) = result {
                        // Owned decoding reports duplicates through Serde.
                        let duplicate = error.kind() == ErrorKind::DuplicateKey
                            || error.to_string().contains("duplicate");
                        assert!(duplicate, "{shape}, {name}: {error}");
                    }
                }
            }
        });
    });
}
