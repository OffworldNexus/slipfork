//! Launched containers inside the world: `crun run`, bounded waits, restart, and teardown.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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

    /// Bounded wait for one launched pod; replies with its exit code or a typed timeout.
    pub(crate) fn wait(&self, pid: i32, timeout_ms: u64) -> Result<i32, Fault> {
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
                return Ok(exit_code(status));
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
                return Err(Fault::new(
                    format!("pod pid {pid} did not exit within {timeout_ms}ms"),
                    "raise the wait timeout, or stop the service with `slipfork down`",
                ));
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
                .args(["kill", "--all", id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = Command::new(&self.crun)
                .args(["delete", "--force", id])
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
                .args(["kill", "--all", &id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = Command::new(&self.crun)
                .args(["delete", "--force", &id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            slipfork_world::kill_group(container.pid);
            let _ = fs::remove_dir_all(&container.bundle);
        }
        reap_zombies();
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
        .args(["run", "--cgroup-manager=disabled", "--detach", "--bundle"])
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
