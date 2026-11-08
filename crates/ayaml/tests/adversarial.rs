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

use ayaml::{ErrorKind, LimitExceeded, LimitKind, Limits, MAX_DEPTH, MAX_DEPTH_CAP, Value};
use serde::de::IgnoredAny;

/// Set in a child process, where a test runs its body instead of starting another child. The
/// value is how many seconds the child may run before it ends itself.
const CHILD: &str = "AYAML_ADVERSARIAL_CHILD";

/// The address space a child may use, in KiB.
const ADDRESS_SPACE_KIB: u64 = 1 << 20;

/// The most memory a child may have held when its body ends, in KiB. The tests here peak below
/// 210 MiB, so this catches a regression too small to reach the address-space limit.
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
    run_command(child_command(test, own_deadline), deadline)
}

/// Run a child from [`child_command`] and wait for it, up to `deadline`.
fn run_command(mut command: Command, deadline: Duration) -> ChildRun {
    let started = Instant::now();
    let mut child = command.spawn().expect("start the child");
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
                assert_depth_error_kind(name, &result.expect_err(name));
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

/// Inputs that repeat one unit. Many of them once took time quadratic in their length, because
/// the scanner or the line index read a line again for each unit. The rest guard the places where
/// a scan like that could come back.
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
        // Quoted scalars over two lines in a flow collection that a block collection holds, after
        // many comment or blank lines, or after a long property. Each asks for that owner.
        format!("k:\n{}  [{}]\n", half("#\n"), half("'a\n  ',")),
        format!("k: &{} [{}]\n", half("a"), half("\"a\n  \",")),
        format!("k:\n{}  [{}]\n", half("\n"), half("{a: 'a\n  '},")),
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
        // Many tab runs on a line that starts with many byte order marks, which each run asks
        // about.
        format!("{}k: {}", half("\u{feff}"), half("'a'\t")),
        format!("{}k: {}", half("\u{feff}"), half("[]\t")),
        // A tab after each of many block indicators on one line, each before a node that ends
        // before the line does.
        all("-\t'a' "),
        all("-\t[a] "),
        all("-\t*a "),
        all("-\t&a "),
        all("-\t] "),
        format!("{}\n", all("\t'a'\n")),
        // Blank lines, lines of spaces, and runs of spaces.
        format!("a{}b\n", all("\n")),
        format!("a{}b\n", all(" \n")),
        format!("k: a{}  b\n", all("\r\n")),
        format!("k: [{}]\n", all(" \n")),
        format!("[a{}]\n", all(" ")),
        format!("k: 'a{}b'\n", all(" \n")),
        format!("{}#\na\n", all(" \n")),
        format!("k: |\n{}  a\n", all(" \n")),
        // One scalar over many lines, and many directives, anchors, and aliases.
        format!("k: '{}b'\n", all("a\n  ")),
        format!("k: \"{}b\"\n", all("a\n  ")),
        format!("k: |\n{}", all("  a\n")),
        format!("k: >\n{}", all("  a\n")),
        format!("{}---\na\n", all("%X a\n")),
        all("- &a a\n"),
        format!("- &a a\n{}", all("- *a\n")),
        format!("- &a a\n- [{}]\n", all("*a, ")),
        // Flow collections nested around a long line.
        format!("[[[{}]]]\n", all("a,")),
        format!("k: [{{a: [{}]}}]\n", all("'a',")),
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

/// How the linear-time tests compare. One run on 8 times the length takes about as long as 8 runs
/// on the short input if the work is linear, and about 8 times as long if it's quadratic, on any
/// host. The limit on the ratio sits between those, with room for a host's noise on either side.
const LARGEST_RATIO: u32 = 3;
/// How long a batch of runs takes at least. A run on the long input repeats until its batch takes
/// this long, and the short runs repeat as often, so that a busy host interrupts both alike and a
/// quadratic scan too quick to time on its own still shows.
const QUICK: Duration = Duration::from_millis(100);
/// The most runs in one batch. A run that takes under `QUICK` divided by this isn't a scan worth
/// timing.
const MOST_REPEATS: u32 = 1 << 16;
/// On a busy host one batch can take 10 times its share, so a read has to look quadratic through
/// many batches before it counts. A real quadratic scan spends this budget and fails.
const TRIES: usize = 12;
const RETRY_BUDGET: Duration = Duration::from_secs(40);

/// A read whose time or memory a test measures. Errors don't matter there.
type Reading = fn(&str);

/// How long `repeats` runs of `read` on `input` take.
fn time_runs(read: &dyn Fn(&str), input: &str, repeats: u32) -> Duration {
    let started = Instant::now();
    for _ in 0..repeats {
        read(input);
    }
    started.elapsed()
}

/// How long one run of `read` on `long` takes, if `read` takes time linear in its input, from
/// `short`, which is an eighth of `long`, to `long`. Otherwise, the times that look quadratic.
/// Each time is the best of its tries, which drops any pause the host took.
fn linear_time(read: &dyn Fn(&str), short: &str, long: &str) -> Result<Duration, String> {
    let mut repeats = 1;
    let mut long_time = time_runs(read, long, repeats);
    while long_time < QUICK && repeats < MOST_REPEATS {
        repeats *= 2;
        long_time = time_runs(read, long, repeats);
    }
    if long_time < QUICK {
        return Ok(long_time / repeats);
    }
    let mut shorts_time = Duration::MAX;
    let started = Instant::now();
    for _ in 0..TRIES {
        shorts_time = shorts_time.min(time_runs(read, short, 8 * repeats));
        if long_time < shorts_time * LARGEST_RATIO || started.elapsed() > RETRY_BUDGET {
            break;
        }
        long_time = long_time.min(time_runs(read, long, repeats));
    }
    if long_time < shorts_time * LARGEST_RATIO {
        Ok(long_time / repeats)
    } else {
        Err(format!(
            "{shorts_time:?} for {} short runs, then {long_time:?} for {repeats} on 8 times the \
             length",
            8 * repeats
        ))
    }
}

#[test]
fn scanning_takes_time_linear_in_the_input() {
    in_child!(|| {
        // Some quadratic scans take only seconds on this length, so a limit on the time alone
        // misses them. The length stays within the normal peak of memory.
        const LENGTH: usize = 1 << 19;
        const SLOWEST: Duration = Duration::from_secs(20);
        // Some of these are errors in one mode or the other. What counts is the time.
        let readers: Vec<(&str, Reading)> = vec![
            ("parse_documents", |input| {
                let _ = ayaml::parse_documents(input);
            }),
            #[cfg(feature = "libyaml-compat")]
            ("compat", |input| {
                let _ = ayaml::compat::from_str::<ayaml::Value>(input);
            }),
        ];
        let shorter = repeated_inputs(LENGTH / 8);
        for (short, long) in shorter.iter().zip(repeated_inputs(LENGTH)) {
            let name: String = long.chars().take(30).collect();
            for (reader, read) in &readers {
                match linear_time(read, short, &long) {
                    Ok(time) => assert!(time < SLOWEST, "{reader}: {name:?}: {time:?}"),
                    Err(times) => panic!("{reader}: {name:?}: {times}"),
                }
            }
        }
    });
}

#[test]
fn many_keys_take_time_linear_in_their_number() {
    in_child!(|| {
        // Keys that all land in one hash bucket take quadratic time, and that's a few seconds here
        // at most, so only the ratio can tell.
        const KEYS: usize = 40_000;
        let readers: Vec<(&str, Reading)> = vec![
            ("parse_value", |input| {
                ayaml::parse_value(input).expect("distinct keys parse");
            }),
            ("from_str", |input| {
                ayaml::from_str::<ayaml::Value>(input).expect("distinct keys parse");
            }),
            #[cfg(feature = "libyaml-compat")]
            ("compat", |input| {
                ayaml::compat::from_str::<ayaml::Value>(input).expect("distinct keys parse");
            }),
        ];
        type Key = fn(usize) -> String;
        let shapes: [(&str, Key); 8] = [
            ("floats", |key| format!("{key}.5: x\n")),
            ("exponents", |key| format!("{key}e3: x\n")),
            ("integers", |key| format!("{key}: x\n")),
            ("strings", |key| format!("k{key}: x\n")),
            ("mappings", |key| format!("? {{k{key}: x}}\n: x\n")),
            ("tags", |key| format!("? !t{key} x\n: x\n")),
            // Each is two numbers that once wrote the same bytes, so their hashes canceled out.
            ("opposite integers", |key| {
                let (negative, positive) = (key + 1, (1_u128 << 64) - key as u128 - 1);
                format!("? {{-{negative}: 0, {positive}: 0}}\n: 0\n")
            }),
            ("wide integers", |key| {
                let wide = (2_u128 << 64) + key as u128;
                format!("? {{{key}: 0, {wide}: null}}\n: 0\n")
            }),
        ];
        for (shape, key) in shapes {
            let keys = |count: usize| (0..count).map(key).collect::<String>();
            let (short, long) = (keys(KEYS / 8), keys(KEYS));
            for (reader, read) in &readers {
                if let Err(times) = linear_time(read, &short, &long) {
                    panic!("{reader}: {shape}: {times}");
                }
            }
        }
    });
}

/// The comparison fails a quadratic scan even when one run on the long input takes less than
/// `QUICK`, and passes a linear one.
#[test]
fn the_linear_time_check_fails_a_quick_quadratic_scan() {
    in_child!(|| {
        const LENGTH: usize = 1 << 19;
        // Every pair of bytes in a 512th of the input: about a millionth of a second each on
        // the long input, so a run takes a few milliseconds.
        let quadratic: fn(&str) = |input| {
            let bytes = &input.as_bytes()[..input.len() / 512];
            let mut sum = 0_u64;
            for first in bytes {
                for second in bytes {
                    sum = sum.wrapping_add(u64::from(first ^ second));
                }
            }
            std::hint::black_box(sum);
        };
        let linear: fn(&str) = |input| {
            std::hint::black_box(input.bytes().map(u64::from).sum::<u64>());
        };
        let short = "ab".repeat(LENGTH / 16);
        let long = "ab".repeat(LENGTH / 2);
        let one_run = (0..5)
            .map(|_| time_runs(&quadratic, &long, 1))
            .min()
            .expect("runs");
        assert!(one_run < QUICK, "one run took {one_run:?}");
        assert!(
            linear_time(&quadratic, &short, &long).is_err(),
            "a quadratic scan passed"
        );
        linear_time(&linear, &short, &long).expect("a linear scan");
    });
}

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

#[test]
fn an_alias_cannot_multiply_a_long_tag() {
    in_child!(|| {
        // One node with a long tag and many aliases that each copy the tag. If an alias weren't
        // charged for it, this would reach gigabytes from about 1 MiB of input. The budget is
        // below the default so that the rejection comes well under this child's peak.
        const ALIASES: usize = 4_000;
        let long = "e".repeat(1 << 20);
        let aliases = "- *x\n".repeat(ALIASES);
        let limits = Limits::default().with_max_alias_expansion_bytes(8 << 20);
        let inputs = [
            format!("%TAG !e! tag:{long}:\n---\n- &x !e!v a\n{aliases}"),
            format!("---\n- &x !<tag:{long}:v> a\n{aliases}"),
            format!("---\n- &x !<tag:{long}:v> [a]\n{aliases}"),
            format!("---\n- &x !<tag:{long}:v> {{a: b}}\n{aliases}"),
            format!(
                "---\n- &x {{a: !<tag:{long}:v> b}}\n{}",
                "- {<<: *x}\n".repeat(ALIASES)
            ),
        ];
        for input in &inputs {
            let cursor = ayaml::Deserializer::from_str_with_limits(input, &limits)
                .and_then(|mut cursor| cursor.next::<Value>().map(drop));
            for (entry_point, result) in [
                (
                    "parse_value",
                    ok(ayaml::parse_value_with_limits(input, &limits)),
                ),
                (
                    "parse_documents",
                    ok(ayaml::parse_documents_with_limits(input, &limits)),
                ),
                (
                    "from_str_borrowed",
                    ok(ayaml::from_str_borrowed_with_limits::<Value>(
                        input, &limits,
                    )),
                ),
                ("Deserializer::next", cursor),
            ] {
                let error = result.expect_err(entry_point);
                let limit = error.limit().map(ayaml::LimitExceeded::kind);
                assert_eq!(
                    limit,
                    Some(ayaml::LimitKind::AliasExpansion),
                    "{entry_point}: {error}"
                );
            }
        }
    });
}

/// The nesting shapes the depth tests use, each with a different recursion
/// profile in Serde (sequences, mappings) or the parser (block, flow,
/// compact, aliases).
const SHAPES: [&str; 9] = [
    "tagged flow mapping",
    "flow sequence",
    "flow mapping",
    "flow pair",
    "flow key",
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
        "tagged flow mapping" => format!("{}0{}", "!t {a: ".repeat(depth), "}".repeat(depth)),
        "flow sequence" => format!("{}0{}", "[".repeat(depth), "]".repeat(depth)),
        "flow mapping" => format!("{}0{}", "{a: ".repeat(depth), "}".repeat(depth)),
        // A pair in a flow sequence is a mapping inside it, so each `[a: ` is two levels.
        "flow pair" => format!(
            "{}{}{}",
            "[a: ".repeat(depth / 2),
            if depth % 2 == 1 { "[0]" } else { "0" },
            "]".repeat(depth / 2)
        ),
        // A block mapping's key is inside the mapping, so a key of `depth - 1` nested
        // sequences makes `depth` levels.
        "flow key" => format!("{}0{}: 0\n", "[".repeat(depth - 1), "]".repeat(depth - 1)),
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

/// Every public path that takes explicit limits and writes YAML. What they write is `input`, read
/// under the default limits.
fn limited_emitters(input: &str, limits: &Limits) -> Vec<(&'static str, Result<(), ayaml::Error>)> {
    let value = ayaml::parse_value(input).expect("parse");
    let documents = std::slice::from_ref(&value);
    let events = ayaml::parse_events(input)
        .and_then(|stream| stream.collect::<Result<Vec<_>, _>>())
        .expect("events");
    let options = ayaml::EmitterOptions::default();
    let styles = ayaml::EmitterStyles::default();
    let emitter = ayaml::Emitter::new(limits, &options);
    vec![
        (
            "to_string_value_with_limits",
            ok(ayaml::to_string_value_with_limits(&value, limits)),
        ),
        (
            "to_string_value_with_limits_and_styles",
            ok(ayaml::to_string_value_with_limits_and_styles(
                &value, limits, &styles,
            )),
        ),
        (
            "to_writer_value_with_limits",
            ayaml::to_writer_value_with_limits(&mut Vec::new(), &value, limits),
        ),
        (
            "to_writer_value_with_limits_and_styles",
            ayaml::to_writer_value_with_limits_and_styles(&mut Vec::new(), &value, limits, &styles),
        ),
        (
            "to_string_with_limits",
            ok(ayaml::to_string_with_limits(&value, limits)),
        ),
        (
            "to_writer_with_limits",
            ayaml::to_writer_with_limits(&mut Vec::new(), &value, limits),
        ),
        (
            "to_string_documents_with_limits",
            ok(ayaml::to_string_documents_with_limits(documents, limits)),
        ),
        (
            "to_writer_documents_with_limits",
            ayaml::to_writer_documents_with_limits(&mut Vec::new(), documents, limits),
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
        ("Emitter::to_string", ok(emitter.to_string(&value))),
        (
            "Emitter::value_to_string",
            ok(emitter.value_to_string(&value)),
        ),
        (
            "Emitter::documents_to_string",
            ok(emitter.documents_to_string(documents)),
        ),
    ]
}

/// Every public path that takes explicit limits and reads YAML, compat's included.
fn limited_readers(input: &str, limits: &Limits) -> Vec<(&'static str, Result<(), ayaml::Error>)> {
    #[cfg_attr(not(feature = "libyaml-compat"), allow(unused_mut))]
    let mut readers = limited_entry_points(input, limits);
    #[cfg(feature = "libyaml-compat")]
    readers.push((
        "compat::parse_events_with_limits",
        ok(ayaml::compat::parse_events_with_limits(input, limits)),
    ));
    readers
}

/// Expect the depth limit from an entry point reading `depth`-deep
/// `shape`. The event stream leaves aliases unexpanded, so an alias chain
/// stays shallow there.
fn assert_depth_error(shape: &str, name: &str, result: Result<(), ayaml::Error>) {
    if stopped_where_json_cannot_follow(shape, &result) {
        return;
    }
    if shape == "alias chain" && name == "parse_events_with_limits" {
        result.unwrap_or_else(|error| panic!("{shape}, {name}: {error}"));
        return;
    }
    assert_depth_error_kind(name, &result.expect_err(name));
}

/// Compat's readers that build values report the depth limit as a libyaml-based reader does.
fn assert_depth_error_kind(name: &str, error: &ayaml::Error) {
    if name.starts_with("compat") {
        assert_eq!(error.kind(), ErrorKind::Serde, "{name}: {error}");
        assert_eq!(error.message(), "recursion limit exceeded", "{name}");
        assert_eq!(error.limit(), None, "{name}");
    } else {
        assert_eq!(error.kind(), ErrorKind::Limit, "{name}: {error}");
        assert!(error.to_string().contains("depth"), "{name}: {error}");
        assert_eq!(
            error.limit().map(|limit| limit.kind()),
            Some(ayaml::LimitKind::Depth),
            "{name}"
        );
    }
}

/// No reader keeps a line break, so a blank or comment line doesn't count against `max_events`.
/// Each one once did, so a million of them between two entries refused a document of about 1 MB.
#[test]
fn blank_and_comment_lines_do_not_count_against_max_events() {
    in_child!(|| {
        let lines = Limits::default().max_events + 1;
        for line in ["\n", "#\n", "  # c\n"] {
            let input = format!("a: 1\n{}b: 2\n", line.repeat(lines));
            for (name, result) in entry_points(&input)
                .into_iter()
                .chain(limited_readers(&input, &Limits::default()))
            {
                result.unwrap_or_else(|error| panic!("{line:?}, {name}: {error}"));
            }
        }
    });
}

/// a `max_depth` above the cap is a configuration error on every path
/// that takes limits; the cap itself is accepted.
#[test]
fn max_depth_above_the_cap_is_rejected_on_every_entry_point() {
    in_child!(|| {
        let over = Limits::default().with_max_depth(MAX_DEPTH_CAP + 1);
        let at_cap = Limits::default().with_max_depth(MAX_DEPTH_CAP);
        let input = "a: [1, {b: 2}]\n";
        for (name, result) in limited_readers(input, &over)
            .into_iter()
            .chain(limited_emitters(input, &over))
        {
            let error = result.expect_err(name);
            assert_eq!(error.kind(), ErrorKind::Limit, "{name}: {error}");
            let expected = format!("max_depth {}", MAX_DEPTH_CAP + 1);
            assert!(error.to_string().contains(&expected), "{name}: {error}");
        }
        for (name, result) in limited_readers(input, &at_cap)
            .into_iter()
            .chain(limited_emitters(input, &at_cap))
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
                    assert_fits(shape, name, result);
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

/// JSON has no form for a tag, nor for a key that isn't a string, so a reader into JSON stops at
/// the first one. All that counts for it is not overflowing the stack.
fn stopped_where_json_cannot_follow(shape: &str, result: &Result<(), ayaml::Error>) -> bool {
    let expected = match shape {
        "tagged flow mapping" => "expected any valid JSON value",
        "flow key" => "expected a string key",
        _ => return false,
    };
    result
        .as_ref()
        .is_err_and(|error| error.to_string().contains(expected))
}

fn assert_fits(shape: &str, name: &str, result: Result<(), ayaml::Error>) {
    if !stopped_where_json_cannot_follow(shape, &result) {
        result.unwrap_or_else(|error| panic!("{shape}, {name}: {error}"));
    }
}

/// The entry points without a limits parameter, compat included, use the
/// default depth and fit a 2 MiB thread at it.
#[test]
fn default_entry_points_fit_a_small_stack_at_the_default_depth() {
    in_child!(|| {
        on_small_stack(|| {
            for shape in SHAPES {
                for (name, result) in entry_points(&nested(shape, MAX_DEPTH)) {
                    assert_fits(shape, name, result);
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

/// Nine levels of nine aliases each over a list of nine empty nodes: 9^9 nodes with no text, which
/// only the cost of a node charges for.
fn empty_node_bomb() -> String {
    let mut bomb = String::from("a0: &a0\n");
    bomb.push_str(&"-\n".repeat(9));
    for level in 1..9 {
        let previous = format!("*a{}", level - 1);
        let row = [previous.as_str(); 9].join(", ");
        bomb.push_str(&format!("a{level}: &a{level} [{row}]\n"));
    }
    bomb
}

/// The benchmark harness's composer refuses a node cost of 0 as the public readers do.
#[cfg(feature = "bench-internals")]
#[test]
fn the_internal_composer_rejects_a_node_cost_of_zero() {
    let input = "a: &x [1, {b: 2}, ~]\nb: *x\n";
    let events = ayaml::internals::parse_events(input, &Limits::default()).expect("events");
    let zero = Limits::default().with_node_cost_bytes(0);
    let error = ayaml::internals::compose_events(&events, input.len(), &zero).expect_err("cost 0");
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
    assert!(error.to_string().contains("node_cost_bytes 0"), "{error}");
    let one = Limits::default().with_node_cost_bytes(1);
    ayaml::internals::compose_events(&events, input.len(), &one).expect("cost 1");
}

/// A `node_cost_bytes` of 0 would make every node an alias copies free, which turns the alias budget
/// off, so it is a configuration error on every path that takes limits. 1, the least cost, is
/// accepted, and the budget still stops a bomb of empty nodes there.
#[test]
fn a_node_cost_of_zero_is_rejected_on_every_entry_point() {
    in_child!(|| {
        let zero = Limits::default().with_node_cost_bytes(0);
        let one = Limits::default().with_node_cost_bytes(1);
        let input = "a: &x [1, {b: 2}, ~]\nb: *x\n";
        for (name, result) in limited_readers(input, &zero)
            .into_iter()
            .chain(limited_emitters(input, &zero))
        {
            let error = result.expect_err(name);
            assert_eq!(error.kind(), ErrorKind::Limit, "{name}: {error}");
            assert_eq!(error.limit(), None, "{name}");
            assert!(
                error.to_string().contains("node_cost_bytes 0"),
                "{name}: {error}"
            );
        }
        for (name, result) in limited_readers(input, &one)
            .into_iter()
            .chain(limited_emitters(input, &one))
        {
            result.unwrap_or_else(|error| panic!("{name}: {error}"));
        }
        // A node costs more memory than 1 byte, so the budget is small here: it lets through at
        // most 256 Ki nodes before it stops the bomb. The event stream doesn't expand aliases.
        let small = one
            .with_max_alias_expansion_bytes(256 << 10)
            .with_alias_bytes_per_input_byte(0);
        for (name, result) in limited_readers(&empty_node_bomb(), &small) {
            if name.ends_with("parse_events_with_limits") {
                result.unwrap_or_else(|error| panic!("{name}: {error}"));
                continue;
            }
            let error = result.expect_err(name);
            assert_eq!(
                error.limit().map(LimitExceeded::kind),
                Some(LimitKind::AliasExpansion),
                "{name}: {error}"
            );
        }
    });
}

/// Every use of a handle counts the prefix it stands for, `!` 1 byte, `!!` 18, and a `%TAG`
/// handle its prefix, over the whole stream. The non-specific `!` and a verbatim tag count
/// nothing. A stream is accepted at the limit and refused one byte below it, where the tag that
/// goes over is the error's place.
#[test]
fn tag_expansion_is_accepted_up_to_its_limit() {
    in_child!(|| {
        let core = "tag:yaml.org,2002:".len();
        let one_document =
            "%TAG !e! tag:ab:\n---\n- !!str a\n- !e!x b\n- !y c\n- ! d\n- !<tag:z> e\n";
        let two_documents = "--- !!str a\n--- !!str b\n";
        let streams = |input: &str, limits: &Limits| {
            let cursor = ayaml::Deserializer::from_str_with_limits(input, limits)
                .and_then(|mut cursor| cursor.next::<Value>().map(drop));
            vec![
                (
                    "parse_events_with_limits",
                    ayaml::parse_events_with_limits(input, limits)
                        .and_then(|stream| stream.collect::<Result<Vec<_>, _>>())
                        .map(drop),
                ),
                (
                    "parse_documents_with_limits",
                    ok(ayaml::parse_documents_with_limits(input, limits)),
                ),
                ("Deserializer::next", cursor),
            ]
        };
        type Readers = fn(&str, &Limits) -> Vec<(&'static str, Result<(), ayaml::Error>)>;
        let cases: [(&str, usize, (usize, usize), Readers); 2] = [
            (
                one_document,
                core + "tag:ab:".len() + 1,
                (5, 3),
                limited_readers,
            ),
            (two_documents, 2 * core, (2, 5), streams),
        ];
        // JSON has no form for a tag, so a reader into JSON stops at the first local one.
        let into_json = |result: &Result<(), ayaml::Error>| {
            result
                .as_ref()
                .is_err_and(|error| error.to_string().contains("expected any valid JSON value"))
        };
        for (input, cost, (line, column), readers) in cases {
            for limit in [cost, cost + 1] {
                let limits = Limits::default().with_max_tag_expansion_bytes(limit);
                for (name, result) in readers(input, &limits) {
                    if !into_json(&result) {
                        result.unwrap_or_else(|error| panic!("{name}, limit {limit}: {error}"));
                    }
                }
            }
            let limits = Limits::default().with_max_tag_expansion_bytes(cost - 1);
            for (name, result) in readers(input, &limits) {
                if into_json(&result) {
                    continue;
                }
                let error = result.expect_err(name);
                let limit = error.limit().expect("a structured limit");
                assert_eq!(limit.kind(), LimitKind::TagExpansion, "{name}: {error}");
                assert_eq!(limit.limit(), (cost - 1) as u64, "{name}");
                assert_eq!(limit.observed(), cost as u64, "{name}");
                let at = error.location().expect("a location");
                assert_eq!((at.line(), at.column()), (line, column), "{name}: {error}");
            }
        }
    });
}

/// A block scalar's size, for `max_scalar_bytes`, is its text in the input from its indicator
/// through its last line, header and indentation included, so it's never less than its value.
#[test]
fn a_block_scalar_counts_its_header_toward_the_scalar_limit() {
    in_child!(|| {
        for input in [
            "k: |\n  ab\n  c\n",
            "k: >\n  ab\n  c\n",
            "k: >-\n  ab\n\n  c\n",
            "k: |+ # kept\n  ab\n\n\n",
            "- |2\n   ab\n  c\n",
            "k: >\n  ab\n  c\nnext: 1\n",
        ] {
            let start = input.find(['|', '>']).expect("an indicator");
            let end = input.find("next").unwrap_or(input.len());
            let size = end - start;
            for limit in [size, size + 1] {
                let limits = Limits::default().with_max_scalar_bytes(limit);
                for (name, result) in limited_readers(input, &limits) {
                    result.unwrap_or_else(|error| panic!("{name}, {input:?}, {limit}: {error}"));
                }
            }
            let limits = Limits::default().with_max_scalar_bytes(size - 1);
            for (name, result) in limited_readers(input, &limits) {
                let error = result.expect_err(name);
                let limit = error.limit().expect("a structured limit");
                assert_eq!(limit.kind(), LimitKind::ScalarBytes, "{name}: {error}");
                assert_eq!(limit.observed(), size as u64, "{name}, {input:?}");
                let at = error.span().expect("a span").start().index();
                assert_eq!(at, start, "{name}, {input:?}");
            }
        }
    });
}

/// Set in a child of a memory test: the case it measures, as `case|reader`.
const MEASURED: &str = "AYAML_ADVERSARIAL_MEASURED";

/// Whether this host can measure a read's peak of memory, as only Linux resets it when asked.
const MEASURES_MEMORY: bool = cfg!(target_os = "linux");

/// How much more memory this process held at its peak during `read` than when `read` started, in
/// bytes. On Linux, which resets the peak when asked, a reset that fails fails the test.
fn peak_growth(read: impl FnOnce()) -> u64 {
    std::fs::write("/proc/self/clear_refs", "5").unwrap_or_else(|error| {
        panic!("resetting the peak through /proc/self/clear_refs: {error}")
    });
    let before = proc_self_number("status", "VmRSS:").expect("VmRSS in /proc/self/status");
    read();
    let peak = proc_self_number("status", "VmHWM:").expect("VmHWM in /proc/self/status");
    peak.saturating_sub(before) * 1024
}

/// A read whose memory a test measures, which says whether it succeeded.
type MeasuredRead = fn(&str) -> bool;

/// The readers whose memory the tests below measure: one that builds a value, one that keeps
/// nothing, and compat's.
fn measured_readers() -> Vec<(&'static str, MeasuredRead)> {
    vec![
        ("parse_value", |input| ayaml::parse_value(input).is_ok()),
        ("from_str_borrowed (IgnoredAny)", |input| {
            ayaml::from_str_borrowed::<IgnoredAny>(input).is_ok()
        }),
        #[cfg(feature = "libyaml-compat")]
        ("compat::from_str", |input| {
            ayaml::compat::from_str::<Value>(input).is_ok()
        }),
    ]
}

/// Block scalars of 4 MiB: each case's name, header, the unit its lines repeat, and the most memory
/// a read may hold at its peak, as a multiple of the input. A read holds about 5 times the input
/// for short lines and 10 for blank ones. When a scalar's lines were listed before it was built,
/// these took 25 to 42 times the input.
const BLOCK_SCALARS: [(&str, &str, &str, u64); 4] = [
    ("literal short lines", "k: |\n", " a\n", 8),
    ("folded short lines", "k: >\n", " a\n", 8),
    ("folded blank runs", "k: >\n", " a\n\n", 10),
    ("literal blank lines", "k: |\n a\n", "\n", 16),
];

/// Entries in the flow sequence whose tokens the arena holds, and the most memory a read may hold
/// for each token. A read holds about 190 bytes a token: the token, and the event it becomes.
const FLOW_ENTRIES: usize = 200_000;
const MOST_PER_TOKEN: u64 = 300;

/// Line breaks after an unclosed `[`, which fails at the end of the input, and the most memory a
/// read may hold for each. The scanner's index holds 8 bytes a line, and the error's position is
/// counted without a second index. When it was counted with one, a read held 16 bytes a line.
const OPEN_LINES: usize = 4 << 20;
const MOST_PER_LINE: u64 = 12;

/// The input a memory case reads, the most memory a read of it may hold at its peak, and whether
/// the read succeeds.
fn memory_case(case: &str) -> (String, u64, bool) {
    if case == "flow sequence" {
        let tokens = 2 * FLOW_ENTRIES + 3;
        let input = format!("[{}a]\n", "a,".repeat(FLOW_ENTRIES));
        return (input, MOST_PER_TOKEN * tokens as u64, true);
    }
    if case == "unclosed flow sequence" {
        let input = format!("[{}", "\n".repeat(OPEN_LINES));
        return (input, MOST_PER_LINE * OPEN_LINES as u64, false);
    }
    let (_, header, unit, most) = BLOCK_SCALARS
        .into_iter()
        .find(|(name, ..)| *name == case)
        .unwrap_or_else(|| panic!("no memory case {case:?}"));
    let input = format!("{header}{}", unit.repeat((4 << 20) / unit.len()));
    let bound = most * input.len() as u64;
    (input, bound, true)
}

/// Measure each reader on `case`, each in a child of its own. A process keeps memory a read gave
/// back, so a later read in the same process could reuse it and seem to need none.
fn measure_in_children(case: &str) {
    if !MEASURES_MEMORY {
        println!("{case}: not measured, since only Linux resets a process's peak of memory");
        return;
    }
    for (reader, _) in measured_readers() {
        let mut command = child_command("measure_one_read", CHILD_DEADLINE + CHILD_GRACE);
        command.env(MEASURED, format!("{case}|{reader}"));
        let run = run_command(command, CHILD_DEADLINE);
        assert!(
            run.passed(),
            "{case}, {reader}: ended with {:?} after {:?}\n{}",
            run.status,
            run.elapsed,
            run.output
        );
    }
}

/// One read that a memory test measures, in a child of that test.
#[test]
#[ignore = "only for a child of a memory test"]
fn measure_one_read() {
    let Ok(measured) = std::env::var(MEASURED) else {
        return;
    };
    in_child!(|| {
        let (case, reader) = measured.split_once('|').expect("case|reader");
        let (_, read) = measured_readers()
            .into_iter()
            .find(|(name, _)| *name == reader)
            .unwrap_or_else(|| panic!("no reader {reader:?}"));
        let (input, most, succeeds) = memory_case(case);
        let mut succeeded = !succeeds;
        let growth = peak_growth(|| succeeded = read(&input));
        assert_eq!(
            succeeded, succeeds,
            "{case}, {reader}: whether the read succeeded"
        );
        let ratio = growth as f64 / input.len() as f64;
        println!("{case}, {reader}: {growth} bytes at the peak, {ratio:.2} times the input");
        assert!(
            growth <= most,
            "{case}, {reader}: {growth} bytes at the peak, over the {most} expected"
        );
    });
}

/// A block scalar is built as it is read, so a read holds a small multiple of its input at most.
/// Each LF costs 8 bytes in the index of line starts, so blank lines, a byte each, cost most.
#[test]
fn a_block_scalar_holds_memory_in_proportion_to_its_input() {
    for (case, ..) in BLOCK_SCALARS {
        measure_in_children(case);
    }
}

/// The scanner keeps a logical line's tokens in one arena, 104 bytes each, and counts every token
/// it keeps against `max_events`. A flow collection is one logical line, so a long one holds memory in
/// proportion to its tokens, and the arena wastes at most a sixteenth of itself as it grows.
#[test]
fn tokens_hold_memory_in_proportion_to_their_number() {
    measure_in_children("flow sequence");
}

/// An error at the end of the input is placed without an index of its lines beside the scanner's.
#[test]
fn an_error_at_the_end_holds_one_index_of_lines() {
    measure_in_children("unclosed flow sequence");
}
