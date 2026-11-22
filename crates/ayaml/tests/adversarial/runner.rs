use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{ADDRESS_SPACE_KIB, CHILD, CHILD_GRACE, LIMIT_THEN_RUN, OUTPUT_KEPT_BYTES};

/// How one child ended.
pub(super) struct ChildRun {
    /// `None` if the deadline passed and the child was killed.
    pub(super) status: Option<ExitStatus>,
    pub(super) output: String,
    pub(super) elapsed: Duration,
}

impl ChildRun {
    /// libtest exits 0 when `--exact` matches nothing, so the test has to be seen passing.
    pub(super) fn passed(&self) -> bool {
        self.status.is_some_and(|status| status.success())
            && self.output.contains("test result: ok. 1 passed;")
    }
}

/// Read a child's output stream to its end, keeping only the start of it. Reading on keeps a
/// flooding child from stalling on a full pipe, where it would look like a hang.
pub(super) fn read_all(mut pipe: impl Read + Send + 'static) -> JoinHandle<String> {
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
pub(super) fn run_child(test: &str, deadline: Duration) -> ChildRun {
    run_child_ending_itself(test, deadline, deadline + CHILD_GRACE)
}

/// Like [`run_child`], with the time after which the child ends itself.
pub(super) fn run_child_ending_itself(
    test: &str,
    deadline: Duration,
    own_deadline: Duration,
) -> ChildRun {
    run_command(child_command(test, own_deadline), deadline)
}

/// Run a child from [`child_command`] and wait for it, up to `deadline`.
pub(super) fn run_command(mut command: Command, deadline: Duration) -> ChildRun {
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
pub(super) fn child_command(test: &str, own_deadline: Duration) -> Command {
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
pub(super) fn wait_or_kill(
    child: &mut Child,
    started: Instant,
    deadline: Duration,
) -> Option<ExitStatus> {
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
