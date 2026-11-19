//! One child process whose output is streamed line by line under a hard deadline (`M3.30.3.2`).
//!
//! `docker buildx`, `docker pull`, `docker export`, `tar`, and the `systemctl`/`apparmor_parser`
//! install legs all outlive the 200 ms a silent wait may hide. Every such call goes through
//! [`ChildCommand`]: the child leads its own process group, both pipes are drained by reader
//! threads (no 64 KiB pipe deadlock), each line reaches the caller's sink as it arrives, the last
//! `TAIL_LINES` stderr lines survive for the failure diagnostic, and a hung child is killed
//! through its pinned group identity and reaped — never left orphaned by a Ctrl-C unwind.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::linux_cow::{ProcessSignal, signal_process_group};
use crate::supervise::ProcessIdentity;

/// Stderr lines retained for the failure diagnostic.
const TAIL_LINES: usize = 20;
/// Poll interval for line delivery, exit observation, and the deadline check.
const POLL: Duration = Duration::from_millis(25);
/// How long a finished child's pipes may stay open before stragglers are killed.
const DRAIN_GRACE: Duration = Duration::from_secs(2);
/// Default ceiling for one child: generous for a cold image build, finite for a hung daemon.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// One child invocation: program, argv, environment, and its hard deadline.
#[derive(Debug, Clone)]
pub struct ChildCommand {
    program: PathBuf,
    args: Vec<String>,
    envs: Vec<(String, String)>,
    clear_env: bool,
    timeout: Duration,
}

impl ChildCommand {
    /// A child with the inherited environment, a closed stdin, and the default deadline.
    pub fn new(program: impl Into<PathBuf>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
            envs: Vec::new(),
            clear_env: false,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Pass one variable through to the child (never through argv: `ps` would leak it).
    #[must_use]
    pub fn env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.envs.push((name.into(), value.into()));
        self
    }

    /// Start from an empty environment (the install legs use their own fixed `PATH`).
    #[must_use]
    pub fn cleared_env(mut self) -> Self {
        self.clear_env = true;
        self
    }

    /// Bound the child: past `timeout` the whole process group is killed and the child reaped.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Its own group: a deadline kill addresses the group, never just the leader.
            .process_group(0);
        if self.clear_env {
            command.env_clear();
        }
        for (name, value) in &self.envs {
            command.env(name, value);
        }
        command
    }

    /// Run to completion, capturing stdout and the stderr tail; nothing is streamed.
    pub fn capture(&self) -> std::io::Result<ChildOutput> {
        self.run(None)
    }

    /// Run to completion, forwarding every line of both pipes to `on_line`.
    pub fn stream(&self, on_line: &mut dyn FnMut(&str)) -> std::io::Result<ChildOutput> {
        self.run(Some(on_line))
    }

    fn run(&self, mut on_line: Option<&mut dyn FnMut(&str)>) -> std::io::Result<ChildOutput> {
        let mut command = self.command();
        let mut group = GroupChild::spawn(&mut command)?;
        let stdout = group.child.stdout.take().expect("stdout is piped");
        let stderr = group.child.stderr.take().expect("stderr is piped");
        let (sender, receiver) = mpsc::channel::<(bool, String)>();
        let stdout_sender = sender.clone();
        std::thread::spawn(move || pump_tagged(stdout, &stdout_sender, false));
        std::thread::spawn(move || pump_tagged(stderr, &sender, true));

        let started = Instant::now();
        let mut stdout_text = String::new();
        let mut tail = TailBuffer::default();
        let mut status: Option<ExitStatus> = None;
        let mut exited_at: Option<Instant> = None;
        loop {
            let mut closed = false;
            match receiver.recv_timeout(POLL) {
                Ok((is_stderr, line)) => {
                    if is_stderr {
                        tail.push(&line);
                    } else {
                        stdout_text.push_str(&line);
                        stdout_text.push('\n');
                    }
                    if let Some(sink) = on_line.as_deref_mut() {
                        sink(&line);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => closed = true,
            }
            if status.is_none()
                && let Some(exit) = group.child.try_wait()?
            {
                status = Some(exit);
                exited_at = Some(Instant::now());
            }
            if closed {
                break;
            }
            match status {
                None if started.elapsed() > self.timeout => {
                    group.kill();
                    let _ = group.child.wait();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "`{}` did not finish within {}s; killed its process group",
                            self.program.display(),
                            self.timeout.as_secs()
                        ),
                    ));
                }
                Some(_) if exited_at.is_some_and(|at| at.elapsed() > DRAIN_GRACE) => {
                    // A surviving grandchild holds the pipe open: it is not part of this run.
                    group.kill();
                    break;
                }
                _ => {}
            }
        }
        let status = match status {
            Some(status) => status,
            None => group.child.wait()?,
        };
        Ok(ChildOutput {
            status,
            stdout: stdout_text,
            tail: tail.into_string(),
        })
    }
}

/// One finished child: status, its full stdout, and the retained stderr tail.
#[derive(Debug)]
pub struct ChildOutput {
    /// Exit status of the child.
    pub status: ExitStatus,
    /// Everything the child wrote to stdout.
    pub stdout: String,
    /// The last `TAIL_LINES` stderr lines, newline-joined.
    pub tail: String,
}

impl ChildOutput {
    /// `true` when the child exited zero.
    #[must_use]
    pub fn success(&self) -> bool {
        self.status.success()
    }
}

/// One spawned child group, killed and reaped if the flow unwinds before it was waited for.
struct GroupChild {
    child: Child,
    identity: Option<ProcessIdentity>,
}

impl GroupChild {
    fn spawn(command: &mut Command) -> std::io::Result<Self> {
        let child = command.spawn()?;
        let pid_ns = std::fs::read_link("/proc/self/ns/pid").unwrap_or_default();
        let identity = ProcessIdentity::capture_group_leader(child.id(), pid_ns);
        Ok(Self { child, identity })
    }

    /// `SIGKILL` the pinned group, then the leader itself if the group identity was unavailable.
    fn kill(&mut self) {
        if let Some(identity) = &self.identity {
            let _ = identity.signal(ProcessSignal::Kill);
        } else {
            let _ = signal_process_group(self.child.id(), ProcessSignal::Kill);
        }
        let _ = self.child.kill();
    }
}

impl Drop for GroupChild {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.kill();
            let _ = self.child.wait();
        }
    }
}

/// Bounded stderr history for the one-line failure.
#[derive(Default)]
struct TailBuffer {
    lines: VecDeque<String>,
}

impl TailBuffer {
    fn push(&mut self, line: &str) {
        if self.lines.len() == TAIL_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line.to_owned());
    }

    fn into_string(self) -> String {
        self.lines.into_iter().collect::<Vec<_>>().join("\n")
    }
}

/// Forward every `\n`- or `\r`-terminated line of `pipe`, tagged stdout/stderr.
fn pump_tagged(mut pipe: impl Read, sender: &mpsc::Sender<(bool, String)>, is_stderr: bool) {
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => break,
        };
        for byte in &chunk[..read] {
            if *byte == b'\n' || *byte == b'\r' {
                flush(&mut pending, sender, is_stderr);
            } else {
                pending.push(*byte);
            }
        }
    }
    flush(&mut pending, sender, is_stderr);
}

/// Emit one buffered line, dropping empty ones (`\r\n` produces no blank entry).
fn flush(pending: &mut Vec<u8>, sender: &mpsc::Sender<(bool, String)>, is_stderr: bool) {
    if pending.is_empty() {
        return;
    }
    let line = String::from_utf8_lossy(pending).into_owned();
    pending.clear();
    let _ = sender.send((is_stderr, line));
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::ChildCommand;

    fn shell(script: &str) -> ChildCommand {
        ChildCommand::new("/bin/sh", vec!["-c".to_owned(), script.to_owned()])
    }

    #[test]
    fn a_line_reaches_the_sink_while_the_child_still_runs() {
        let started = Instant::now();
        let mut seen = Vec::new();
        let error = shell("printf 'early\\n'; sleep 30")
            .timeout(Duration::from_millis(300))
            .stream(&mut |line| seen.push(line.to_owned()))
            .expect_err("the 300 ms deadline fires before `sleep 30`");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            seen.contains(&"early".to_owned()),
            "the sink saw the live line before the deadline: {seen:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_timeout_kills_the_grandchild_and_reaps_the_group() {
        let scratch = tempfile::tempdir().expect("disk scratch");
        let pid_file = scratch.path().join("grandchild.pid");
        let script = format!("sh -c 'sleep 30 & echo $! > {}; wait'", pid_file.display());
        let started = Instant::now();
        let error = shell(&script)
            .timeout(Duration::from_millis(400))
            .stream(&mut |_| {})
            .expect_err("the deadline kills the whole group");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .expect("the grandchild wrote its pid")
            .trim()
            .parse()
            .expect("a numeric pid");
        // The grandchild is gone: no `/proc/<pid>` remains after the group kill and reap.
        for _ in 0..50 {
            if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("grandchild {pid} survived the group kill");
    }

    #[test]
    fn carriage_returns_split_lines_and_capture_keeps_stdout() {
        let output = shell("printf 'one\\rtwo\\nthree\\n'; printf 'warn\\r\\n' >&2")
            .capture()
            .expect("spawn /bin/sh");
        assert!(output.success());
        assert_eq!(output.stdout, "one\ntwo\nthree\n");
        assert_eq!(output.tail, "warn");
    }

    #[test]
    fn a_failing_child_keeps_its_stderr_tail() {
        let output = shell("printf 'boom\\n' >&2; exit 7")
            .stream(&mut |_| {})
            .expect("spawn /bin/sh");
        assert!(!output.success());
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.tail, "boom");
    }
}
