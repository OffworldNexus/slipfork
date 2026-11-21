//! Launched containers inside the world: `crun run`, bounded waits, restart, and teardown.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use slipfork_world::PodSpec;

use crate::{Fault, REMEDY_CRUN, REMEDY_POD, REMEDY_RUNTIME, canonical};

pub(crate) struct InitWorld {
    crun: PathBuf,
    pods_root: PathBuf,
    pods: BTreeMap<String, Container>,
}

struct Container {
    bundle: PathBuf,
    pid: i32,
}

impl InitWorld {
    pub(crate) fn new(crun: PathBuf, pods_root: PathBuf) -> Self {
        Self {
            crun,
            pods_root,
            pods: BTreeMap::new(),
        }
    }

    pub(crate) fn pods_root(&self) -> &Path {
        &self.pods_root
    }

    /// Every tracked pod with its pid and whether `/proc/<pid>` still exists in the world.
    pub(crate) fn list(&self) -> Vec<serde_json::Value> {
        self.pods
            .iter()
            .map(|(id, container)| {
                serde_json::json!({
                    "id": id,
                    "pid": container.pid,
                    "running": Path::new(&format!("/proc/{}", container.pid)).exists(),
                })
            })
            .collect()
    }

    /// Bounded wait: `Some(code)` when the pod exited, `None` while it is still running.
    pub(crate) fn wait(&self, pid: i32, timeout_ms: u64) -> Result<Option<i32>, Fault> {
        if !self.pods.values().any(|container| container.pid == pid) {
            return Err(Fault::new(
                format!("pid {pid} is not a launched pod"),
                "pass the pid returned by the launch reply, or restart the world",
            ));
        }
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let mut status = 0;
            // SAFETY: `waitpid` writes one `c_int` through the valid pointer.
            let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if reaped == pid {
                return Ok(Some(exit_code(status)));
            }
            if reaped < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) {
                    return Err(Fault::new(
                        format!("pod pid {pid} was already reaped"),
                        "query `crun list` for the container state, then `slipfork restart`",
                    ));
                }
                return Err(Fault::cause(
                    format!("wait for pod pid {pid}"),
                    REMEDY_POD,
                    error,
                ));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub(crate) fn launch(&mut self, pod: &PodSpec) -> Result<i32, Fault> {
        let action = format!("pod `{}` launch with crun", pod.id);
        pod.validate()
            .map_err(|source| Fault::cause(action.clone(), REMEDY_POD, source))?;
        let bundle = self.pods_root.join(&pod.id);
        let bundle = canonical(&bundle, action.clone(), REMEDY_RUNTIME)?;
        if !bundle.starts_with(&self.pods_root) {
            return Err(Fault::new(
                action,
                "keep the pod runtime root on a tmpfs owned by this user",
            ));
        }
        let pid = crun_run(&self.crun, &bundle, &pod.id)?;
        self.pods.insert(pod.id.clone(), Container { bundle, pid });
        Ok(pid)
    }

    /// Restarts one pod or every pod from its existing tmpfs bundle.
    pub(crate) fn restart(&mut self, service: Option<&str>) -> Result<usize, Fault> {
        let ids: Vec<String> = match service {
            Some(id) if self.pods.contains_key(id) => vec![id.to_owned()],
            Some(id) => {
                return Err(Fault::new(
                    format!("pod `{id}` is not running"),
                    "pick a service from `slipfork up` output or omit the service name",
                ));
            }
            None => self.pods.keys().cloned().collect(),
        };
        for id in &ids {
            let Some(container) = self.pods.get(id) else {
                continue;
            };
            let bundle = container.bundle.clone();
            slipfork_world::kill_group(container.pid);
            let _ = Command::new(&self.crun)
                .args(["--cgroup-manager=disabled", "kill", "--all", id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = Command::new(&self.crun)
                .args(["--cgroup-manager=disabled", "delete", "--force", id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let pid = crun_run(&self.crun, &bundle, id)?;
            if let Some(container) = self.pods.get_mut(id) {
                container.pid = pid;
            }
        }
        reap_zombies();
        Ok(ids.len())
    }

    pub(crate) fn stop_all(&mut self) {
        for (id, container) in std::mem::take(&mut self.pods) {
            let _ = Command::new(&self.crun)
                .args(["--cgroup-manager=disabled", "kill", "--all", &id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = Command::new(&self.crun)
                .args(["--cgroup-manager=disabled", "delete", "--force", &id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            slipfork_world::kill_group(container.pid);
            let _ = fs::remove_dir_all(&container.bundle);
        }
        reap_zombies();
    }
    /// Runs one bundle to completion while forwarding every stdout/stderr line.
    ///
    /// Unlike `launch` this never detaches: the container's pipes stay open, so a failing `uv sync`
    /// reports its own stderr instead of vanishing behind an exit code.
    ///
    /// `timeout_ms` is enforced here, inside PID 1. The init loop is single-threaded, so a silent
    /// step must never block it: on expiry the process group is killed, the container is deleted,
    /// zombies are reaped, and a typed timeout fault is returned. The `on_line` early return path
    /// runs the same teardown so an undeliverable reply never orphans a container.
    pub(crate) fn run_once_streaming(
        &self,
        pod: &PodSpec,
        timeout_ms: u64,
        on_line: &mut dyn FnMut(slipfork_world::Stream, &str) -> Result<(), Fault>,
    ) -> Result<i32, Fault> {
        let bundle = self.pods_root.join(&pod.id);
        if !bundle.starts_with(&self.pods_root) {
            return Err(Fault::new(
                format!("pod `{}` bundle escapes the runtime root", pod.id),
                REMEDY_CRUN,
            ));
        }
        let action = format!("pod `{}` run-once with crun", pod.id);
        let mut child = Command::new(&self.crun)
            .args(["--cgroup-manager=disabled", "run", "--bundle"])
            .arg(&bundle)
            .arg(&pod.id)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|source| Fault::cause(action.clone(), REMEDY_CRUN, source))?;
        let child_pid = child.id();
        let (sender, receiver) = mpsc::channel::<(slipfork_world::Stream, String)>();
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            let sender = sender.clone();
            readers.push(thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let _ = sender.send((slipfork_world::Stream::Stdout, line));
                }
            }));
        }
        if let Some(stderr) = child.stderr.take() {
            let sender = sender.clone();
            readers.push(thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let _ = sender.send((slipfork_world::Stream::Stderr, line));
                }
            }));
        }
        drop(sender);
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.abort(&pod.id, child_pid, &mut child, &mut readers);
                return Err(Fault::timed_out(
                    format!(
                        "pod `{}` still running after {timeout_ms}ms; killed its process group",
                        pod.id
                    ),
                    REMEDY_POD,
                ));
            }
            match receiver.recv_timeout(remaining) {
                Ok((stream, line)) => {
                    if let Err(fault) = on_line(stream, &line) {
                        self.abort(&pod.id, child_pid, &mut child, &mut readers);
                        return Err(fault);
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        for (reader, kind) in readers.into_iter().zip(["stdout", "stderr"]) {
            if reader.join().is_err() {
                self.delete(&pod.id);
                reap_zombies();
                return Err(Fault::new(
                    format!("pod `{}` {kind} reader panicked", pod.id),
                    REMEDY_CRUN,
                ));
            }
        }
        let status = child
            .wait()
            .map_err(|source| Fault::cause(action, REMEDY_CRUN, source))?;
        self.delete(&pod.id);
        reap_zombies();
        Ok(status.code().unwrap_or(128))
    }

    /// Kills the whole run-once group, deletes the container, reaps, and joins its readers.
    fn abort(
        &self,
        id: &str,
        pid: u32,
        child: &mut std::process::Child,
        readers: &mut Vec<thread::JoinHandle<()>>,
    ) {
        if let Ok(pid) = i32::try_from(pid) {
            slipfork_world::kill_group(pid);
        }
        self.delete(id);
        let _ = child.wait();
        reap_zombies();
        for reader in readers.drain(..) {
            let _ = reader.join();
        }
    }

    /// `crun kill --all` then `crun delete --force`; both are best effort teardown.
    fn delete(&self, id: &str) {
        let _ = Command::new(&self.crun)
            .args(["--cgroup-manager=disabled", "kill", "--all", id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new(&self.crun)
            .args(["--cgroup-manager=disabled", "delete", "--force", id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Normalizes a raw `waitpid(2)` status into a shell-style exit code.
fn exit_code(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        -1
    }
}

/// Runs one already-written bundle to container start and returns its host pid.
fn crun_run(crun: &Path, bundle: &Path, id: &str) -> Result<i32, Fault> {
    let action = format!("pod `{id}` launch with crun");
    let pid_file = bundle.join("init.pid");
    let _ = fs::remove_file(&pid_file);
    let status = Command::new(crun)
        // `--cgroup-manager` is a crun global option: it must precede the subcommand.
        .args(["--cgroup-manager=disabled", "run", "--detach", "--bundle"])
        .arg(bundle)
        .arg("--pid-file")
        .arg(&pid_file)
        .arg(id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .process_group(0)
        .status()
        .map_err(|source| Fault::cause(action.clone(), REMEDY_CRUN, source))?;
    if !status.success() {
        return Err(Fault::new(
            format!("pod `{id}` launch with crun (`crun run` exited with {status})"),
            REMEDY_CRUN,
        ));
    }
    fs::read_to_string(&pid_file)
        .map_err(|source| Fault::cause(action.clone(), REMEDY_CRUN, source))?
        .trim()
        .parse::<i32>()
        .map_err(|source| Fault::cause(action, REMEDY_CRUN, source))
}

/// Reaps every exited child without blocking.
pub(crate) fn reap_zombies() {
    loop {
        let mut status = 0;
        // SAFETY: `waitpid` writes one `c_int` through the valid pointer.
        let reaped = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if reaped <= 0 {
            return;
        }
    }
}
