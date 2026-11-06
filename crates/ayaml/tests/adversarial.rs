//! Adversarial inputs on every public entry point.
//!
//! Each test drives every public path that reads YAML, so that no entry
//! point bypasses a limit that another enforces.
//!
//! A test that reads YAML runs its body in a child process, through [`in_child!`]. On Linux
//! the child has an address-space limit, so running out of memory aborts that child and fails
//! that test, and the machine stays usable. Every system has the deadline, which the child
//! keeps too, so it ends even if the test binary dies first. A stack overflow also fails one
//! test, not the whole binary.

use std::hash::Hash;
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ayaml::{ErrorKind, Limits, MAX_DEPTH, MAX_DEPTH_CAP, Value};
use serde::de::IgnoredAny;

/// Set in a child process, where a test runs its body instead of starting another child. The
/// value is how many seconds the child may run before it ends itself.
const CHILD: &str = "AYAML_ADVERSARIAL_CHILD";

/// The address space a child may use, in KiB.
const ADDRESS_SPACE_KIB: u64 = 1 << 20;

/// The most memory a child may have held when its body ends, in KiB. The tests here peak below
/// 90 MiB, so this catches a regression too small to reach the address-space limit.
const NORMAL_PEAK_KIB: u64 = ADDRESS_SPACE_KIB / 4;

/// How long a child may run. A regression that spins without allocating ends here.
const CHILD_DEADLINE: Duration = Duration::from_secs(300);

/// How long past its deadline a child ends itself. Its parent normally kills it first, but a
/// parent that has died can't.
const CHILD_GRACE: Duration = Duration::from_secs(30);

/// How much of each output stream of a child to keep, in bytes. The parent has no limit of
/// its own, so a child that floods its output must not grow the parent.
const OUTPUT_KEPT_BYTES: u64 = 1 << 20;

/// Set the limits, then become the test binary, which is `$0`. `$1` is the limit in KiB. A
/// lower limit that is already set stays, because raising it could fail. Core dumps are off,
/// because a child that hits the limit aborts with up to `$1` of memory.
const LIMIT_THEN_RUN: &str = r#"
[ "$(ulimit -v)" != unlimited ] && [ "$(ulimit -v)" -le "$1" ] || ulimit -v "$1" || exit 125
ulimit -c 0
shift
exec "$0" "$@"
"#;

/// Run the calling test's body in a child process, or directly if this is the child. The child
/// reruns the test by name, which comes from the function this is used in. A test that named
/// another test would pass without running its body.
macro_rules! in_child {
    ($body:expr $(,)?) => {{
        fn here() {}
        let path = std::any::type_name_of_val(&here);
        let test = path
            .strip_suffix("::here")
            .and_then(|path| path.split_once("::"))
            .map_or(path, |(_, test)| test);
        in_child_named(test, $body);
    }};
}

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
        self.status.is_some_and(|status| status.success())
            && self.output.contains("test result: ok. 1 passed;")
    }
}

/// Read a child's output stream to its end, keeping only the start of it. Reading on keeps a
/// flooding child from stalling on a full pipe, where it would look like a hang.
fn read_all(mut pipe: impl Read + Send + 'static) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        // A read error only shortens the output that a failure reports.
        let _ = pipe
            .by_ref()
            .take(OUTPUT_KEPT_BYTES)
            .read_to_end(&mut bytes);
        let _ = std::io::copy(&mut pipe, &mut std::io::sink());
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// Run one test of this binary in a child process and wait for it, up to `deadline`.
fn run_child(test: &str, deadline: Duration) -> ChildRun {
    run_child_ending_itself(test, deadline, deadline + CHILD_GRACE)
}

/// Like [`run_child`], with the time after which the child ends itself.
fn run_child_ending_itself(test: &str, deadline: Duration, own_deadline: Duration) -> ChildRun {
    let started = Instant::now();
    let mut child = child_command(test, own_deadline)
        .spawn()
        .expect("start the child");
    let stdout = read_all(child.stdout.take().expect("piped stdout"));
    let stderr = read_all(child.stderr.take().expect("piped stderr"));
    let status = wait_or_kill(&mut child, started, deadline);
    let elapsed = started.elapsed();
    let output = stdout.join().expect("stdout reader") + &stderr.join().expect("stderr reader");
    ChildRun {
        status,
        output,
        elapsed,
    }
}

/// The command that runs one test of this binary as a child, under its limits.
fn child_command(test: &str, own_deadline: Duration) -> Command {
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
    command
        .args([test, "--exact", "--nocapture", "--include-ignored"])
        .env(CHILD, own_deadline.as_secs().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Wait for a child that started at `started`, and kill it once `deadline` has passed.
fn wait_or_kill(child: &mut Child, started: Instant, deadline: Duration) -> Option<ExitStatus> {
    loop {
        if let Some(status) = child.try_wait().expect("wait for the child") {
            return Some(status);
        }
        if started.elapsed() > deadline {
            child.kill().expect("kill the child");
            child.wait().expect("wait for the killed child");
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
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

/// Whether this process is a child. In a child, this also starts the thread that ends the
/// process at its own deadline, so call it once.
fn is_child() -> bool {
    let Some(seconds) = std::env::var_os(CHILD) else {
        return false;
    };
    let seconds: u64 = seconds
        .to_str()
        .and_then(|seconds| seconds.parse().ok())
        .expect("a child's deadline, in seconds");
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(seconds));
        // Not `eprintln!`: if the parent has died, stderr is a closed pipe, and that macro
        // would panic here instead of ending the process.
        let _ = writeln!(
            std::io::stderr(),
            "this child passed its own deadline of {seconds} s"
        );
        std::process::abort();
    });
    true
}

/// Run `test`'s body in a child process, or directly if this is the child. Tests use
/// [`in_child!`], which can't name the wrong test.
fn in_child_named(test: &str, body: impl FnOnce()) {
    if !is_child() {
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
        let peak = proc_self_number("status", "VmHWM:").expect("VmHWM");
        println!("{test}: peak memory {peak} KiB");
        assert!(
            peak <= NORMAL_PEAK_KIB,
            "{test} used {peak} KiB of memory, over the {NORMAL_PEAK_KIB} KiB expected"
        );
    }
}

/// Keep allocating and touching memory, as a runaway expansion would, with every real limit
/// still on. It stops only when the allocator can't get more.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "only for a child of runaway_allocation_fails_quickly_and_cleanly"]
fn keep_allocating() {
    if is_child() {
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
    in_child_named("fail_in_a_child", || unreachable!("this isn't the child"));
}

#[test]
#[ignore = "only for a child of a_child_past_its_deadline_is_killed"]
fn never_finish() {
    if is_child() {
        std::thread::sleep(Duration::from_secs(3_600));
    }
}

#[test]
fn a_child_past_its_deadline_is_killed() {
    let run = run_child("never_finish", Duration::from_secs(1));
    assert!(run.status.is_none(), "{:?}: {}", run.status, run.output);
    assert!(run.elapsed < Duration::from_secs(60), "{:?}", run.elapsed);
}

/// A child's parent may die without killing it, so the child also ends itself.
#[test]
fn a_child_nobody_kills_ends_itself() {
    let run = run_child_ending_itself("never_finish", CHILD_DEADLINE, Duration::from_secs(1));
    assert!(!run.passed(), "{}", run.output);
    assert!(run.status.is_some(), "hit the deadline: {}", run.output);
    assert!(
        run.output.contains("passed its own deadline"),
        "{:?}: {}",
        run.status,
        run.output
    );
    assert!(run.elapsed < Duration::from_secs(60), "{:?}", run.elapsed);
}

/// A parent's death closes its child's output, which must not stop the child from ending.
/// Only stderr closes here, because libtest stops by itself when it can't write to stdout.
#[test]
fn a_child_whose_stderr_is_closed_ends_itself() {
    let started = Instant::now();
    let mut child = child_command("never_finish", Duration::from_secs(1))
        .spawn()
        .expect("start the child");
    let stdout = read_all(child.stdout.take().expect("piped stdout"));
    drop(child.stderr.take());
    let status = wait_or_kill(&mut child, started, Duration::from_secs(60));
    let output = stdout.join().expect("stdout reader");
    assert!(
        status.is_some_and(|status| !status.success()),
        "{status:?}: {output}"
    );
}

/// A child that floods its output grows the parent only up to the cap.
#[test]
fn a_childs_output_is_capped() {
    let flood = std::io::repeat(b'x').take(3 * OUTPUT_KEPT_BYTES);
    let kept = read_all(flood).join().expect("reader");
    assert_eq!(kept.len() as u64, OUTPUT_KEPT_BYTES);
}

/// Hold more memory than a normal test, far from any real limit and from the address-space
/// limit.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "only for a child of a_child_over_its_normal_peak_fails"]
fn hold_too_much_memory() {
    if std::env::var_os(CHILD).is_some() {
        in_child!(|| {
            let kib = usize::try_from(NORMAL_PEAK_KIB).expect("fits") + 16 * 1024;
            let held = vec![1_u8; kib * 1024];
            std::hint::black_box(&held);
        });
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_child_over_its_normal_peak_fails() {
    let run = run_child("hold_too_much_memory", CHILD_DEADLINE);
    assert!(!run.passed(), "{}", run.output);
    assert!(run.output.contains("KiB expected"), "{}", run.output);
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
    in_child!(|| {
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
    in_child!(|| {
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
    },);
}

/// The limits that accept an input on one entry point accept it on all.
#[test]
fn ordinary_aliases_are_accepted_on_every_entry_point() {
    in_child!(|| {
        let input = "base: &b {x: [1, 2]}\nuse: *b\nmore: [*b, *b]\n";
        for (name, result) in entry_points(input) {
            result.unwrap_or_else(|error| panic!("{name}: {error}"));
        }
    });
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
    in_child!(|| {
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
    },);
}

/// Anchors cost nothing until an alias uses them. A 4 MiB scalar under
/// 100 nested anchors used to be copied once per anchor.
#[test]
fn nested_anchors_are_not_copied() {
    in_child!(|| {
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

/// Inputs that repeat one unit. Each of them once took time quadratic in its length, because
/// the scanner or the line index read a line again for each unit.
fn repeated_inputs(length: usize) -> Vec<String> {
    let all = |unit: &str| unit.repeat(length / unit.len());
    let half = |unit: &str| unit.repeat(length / 2 / unit.len());
    vec![
        // Byte order marks in a document prefix.
        format!("a\n{}", all("\u{feff}#\n")),
        format!("a\n{}", all("\u{feff}\n")),
        format!("a\n{}---\nb\n", all("\u{feff}#\n")),
        format!("|\n a\n{}", all("\u{feff}#\n")),
        // A run of them on one line, before a marker, and before a scalar with many lines.
        format!("a\n{}---\nb\n", all("\u{feff}")),
        format!("{}k: a{}", half("\u{feff}"), half("\n b")),
        format!("a\n...\n{}k: a{}", half("\u{feff}"), half("\n b")),
        format!("{}- a{}", half("\u{feff}"), half("\n b")),
        // Quoted scalars on one line, and on a line with a long indent.
        format!("k: [{}]\n", all("'a',")),
        format!("k: [{}]\n", all("\"a\",")),
        format!("[{}]\n", all("'a',")),
        format!("k: [\n{}{}]\n", half(" "), half("'a',")),
        // Runs of tabs, and many runs on one line.
        format!("k: [\n{}]\n", all("\t")),
        format!("[\n{}]\n", all("\t")),
        format!("{}x\n", all("\t")),
        format!("{}#\nx\n", all("\t")),
        format!("k: [{}a]\n", all("a\t,\t")),
        format!("k: [{}]\n", all("'a'\t,")),
        all("[]\t"),
        all("{}\t"),
        all("'a'\t"),
        all("!t\t"),
        format!("k: {}", all("[]\t")),
        // Flow collections on one line.
        format!("[{}]\n", all("[],")),
        format!("[{}]\n", all("{},")),
        format!("k: [{}]\n", all("[],")),
        // Non-ASCII text on a long line, and before one.
        format!("[{}]\n", all("é,")),
        format!("# é\n[{}]\n", all("a,")),
        format!("{{\"name\":\"é\",\"items\":[{}]}}\n", all("{\"a\":1},")),
        // Marks before a long line.
        format!("{}[{}]\n", half("\u{feff}"), half("a,")),
        // A long prefix before a plain scalar with many lines.
        format!("{}a{}", half(" "), half("\n b")),
        format!("{}: a{}", half("x"), half("\n  b")),
    ]
}

#[test]
fn scanning_takes_time_linear_in_the_input() {
    in_child!(|| {
        // On 8 times the length, a linear scan takes about 8 times as long and a quadratic one
        // about 64 times, on any host. The limit sits between them, with room for a host's noise
        // on either side. A scan this quick isn't compared, because the difference would be noise.
        // Some quadratic scans take only seconds on this length, so a limit on the time alone
        // misses them. The length stays within the normal peak of memory.
        const LENGTH: usize = 1 << 19;
        const QUICK: Duration = Duration::from_millis(100);
        const LARGEST_GROWTH: u32 = 24;
        const SLOWEST: Duration = Duration::from_secs(20);
        // On a busy host one run can take 10 times its share, so a scan has to look quadratic
        // through many runs before it counts. A real quadratic scan spends this budget and fails.
        const TRIES: usize = 12;
        const RETRY_BUDGET: Duration = Duration::from_secs(40);
        type Read = fn(&str) -> Result<(), ayaml::Error>;
        let timed = |read: &Read, input: &str| {
            let started = Instant::now();
            // Some of these are errors in one mode or the other. What counts is the time.
            let _ = read(input);
            started.elapsed()
        };
        let readers: Vec<(&str, Read)> = vec![
            ("parse_documents", |input| ok(ayaml::parse_documents(input))),
            #[cfg(feature = "libyaml-compat")]
            ("compat", |input| {
                ok(ayaml::compat::from_str::<ayaml::Value>(input))
            }),
        ];
        let shorter = repeated_inputs(LENGTH / 8);
        for (short, long) in shorter.iter().zip(repeated_inputs(LENGTH)) {
            let name: String = long.chars().take(30).collect();
            for (reader, read) in &readers {
                let mut short_time = Duration::MAX;
                let mut long_time = Duration::MAX;
                let started = Instant::now();
                // Each best time drops any pause the host took, so it takes a real scan to fail.
                for _ in 0..TRIES {
                    short_time = short_time.min(timed(read, short));
                    long_time = long_time.min(timed(read, &long));
                    if long_time < QUICK
                        || long_time < short_time * LARGEST_GROWTH
                        || started.elapsed() > RETRY_BUDGET
                    {
                        break;
                    }
                }
                assert!(long_time < SLOWEST, "{reader}: {name:?}: {long_time:?}");
                assert!(
                    long_time < QUICK || long_time < short_time * LARGEST_GROWTH,
                    "{reader}: {name:?}: {short_time:?}, then {long_time:?} on 8 times the length",
                );
            }
        }
    });
}

/// The nesting shapes the depth tests use, each with a different recursion
/// profile in Serde (sequences, mappings) or the parser (block, flow,
/// compact, aliases).
#[test]
fn a_long_tag_prefix_cannot_multiply_memory() {
    in_child!(|| {
        // One long prefix and many short tags that each hold a copy of it. Without the limit,
        // this reaches gigabytes from about 1 MiB of input, well past this child's peak.
        const PREFIX: usize = 1 << 20;
        const TAGS: usize = 4_000;
        let long = "e".repeat(PREFIX);
        let inputs = [
            format!("%TAG !e! tag:{long}:\n---\n{}", "- !e!x a\n".repeat(TAGS)),
            format!("%TAG !! tag:{long}:\n---\n{}", "- !!x a\n".repeat(TAGS)),
            format!("%TAG ! tag:{long}:\n---\n{}", "- !x a\n".repeat(TAGS)),
            // A handle that expands to a core name splits at the core prefix, which copies too.
            format!(
                "%TAG !e! tag:yaml.org,2002:{long}:\n---\n{}",
                "- !e!x a\n".repeat(TAGS)
            ),
        ];
        for input in &inputs {
            for (entry_point, result) in entry_points(input) {
                let error = result.expect_err(entry_point);
                let limit = error.limit().map(ayaml::LimitExceeded::kind);
                assert_eq!(
                    limit,
                    Some(ayaml::LimitKind::TagExpansion),
                    "{entry_point}: {error}"
                );
            }
        }
    });
}

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
    in_child!(|| {
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
    });
}

/// At the cap, every path fits a 2 MiB thread, and one level
/// more is a depth error, not an abort. CI runs this file in debug and
/// release builds.
#[test]
fn every_entry_point_fits_a_small_stack_at_the_depth_cap() {
    in_child!(|| {
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
    },);
}

/// The entry points without a limits parameter, compat included, use the
/// default depth and fit a 2 MiB thread at it.
#[test]
fn default_entry_points_fit_a_small_stack_at_the_default_depth() {
    in_child!(|| {
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
    },);
}

/// The parser keeps its own stack, so nesting far past any depth limit
/// is a limit error on a 2 MiB thread, never an abort.
#[test]
fn deep_nesting_is_a_limit_error_on_a_small_stack() {
    in_child!(|| {
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
    in_child!(|| {
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
