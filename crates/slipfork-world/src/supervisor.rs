//! Host-side supervision of one world: `unshare` launcher, PID 1 handshake, pod launches, reap.
//!
//! The launcher runs `unshare --user --map-current-user --net --ipc --pid --fork --mount-proc
//! --kill-child=SIGKILL` around `slipfork-world-init`. The supervisor never `setns`es (that fails
//! for an unprivileged multithreaded netns); it talks NDJSON over the init's stdin and kills the
//! whole process group on teardown.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::control::{self, InitFrame, Request};
use crate::error::{Error, Result};
use crate::layout::WorldName;
use crate::namespaces::{NamespaceIdentity, NamespaceTable};
use crate::pod::{PodHost, PodSpec, write_bundle};
use crate::runtime_dir::RuntimePods;

/// Host values the supervisor needs; the composition root injects every one of them.
#[derive(Debug, Clone)]
pub struct WorldHost {
    /// `crun` executable.
    pub crun: PathBuf,
    /// `unshare` executable.
    pub unshare: PathBuf,
    /// `slipfork-world-init` executable (second binary of this crate).
    pub world_init: PathBuf,
    /// `$XDG_RUNTIME_DIR` (must be a tmpfs mount).
    pub runtime_dir: PathBuf,
    /// Invoking uid; identity-mapped inside the world user namespace.
    pub uid: u32,
    /// Invoking gid; identity-mapped inside the world user namespace.
    pub gid: u32,
    /// Optional host `resolv.conf` bound read-only into every pod.
    pub resolv_conf: Option<PathBuf>,
    /// Upper bound for the handshake, pod replies, and graceful teardown.
    pub boot_timeout: Duration,
}

impl WorldHost {
    fn pod_host(&self) -> PodHost {
        PodHost {
            crun: self.crun.clone(),
            resolv_conf: self.resolv_conf.clone(),
            uid: self.uid,
            gid: self.gid,
        }
    }
}

struct LiveWorld {
    launcher: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    pgid: i32,
    init_pid: u32,
    namespaces: NamespaceTable,
    next_request: u64,
    pods: Vec<String>,
}

/// One booted world namespace plus its init control channel.
pub struct WorldSupervisor {
    host: WorldHost,
    world: WorldName,
    pods: RuntimePods,
    live: Option<LiveWorld>,
}

impl WorldSupervisor {
    /// Boots the world: tmpfs pod root, `unshare` launcher, verified ready handshake.
    pub fn boot(host: WorldHost, world: &WorldName) -> Result<Self> {
        for (tool, path) in [
            ("crun", &host.crun),
            ("unshare", &host.unshare),
            ("slipfork-world-init", &host.world_init),
        ] {
            if !path.is_file() {
                return Err(Error::MissingTool {
                    tool,
                    path: path.clone(),
                });
            }
        }
        let pods = RuntimePods::prepare(&host.runtime_dir, world)?;
        let inside_file = pods.path().join("namespaces.inside.json");
        let mut command = Command::new(&host.unshare);
        command
            .args([
                "--user",
                "--map-current-user",
                "--net",
                "--ipc",
                "--pid",
                "--fork",
                "--mount-proc",
                "--kill-child=SIGKILL",
                "--",
            ])
            .arg(&host.world_init)
            .arg("--namespaces")
            .arg(&inside_file)
            .arg("--pods")
            .arg(pods.path())
            .arg("--crun")
            .arg(&host.crun)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .process_group(0);
        let mut launcher = command.spawn().map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::MissingTool {
                    tool: "unshare",
                    path: host.unshare.clone(),
                }
            } else {
                Error::io("spawn", &host.unshare, source)
            }
        })?;
        let pgid = i32::try_from(launcher.id()).unwrap_or(i32::MAX);
        let stdin = launcher.stdin.take().ok_or_else(|| Error::Init {
            message: "unshare gave no stdin pipe".to_owned(),
        })?;
        let stdout = BufReader::new(launcher.stdout.take().ok_or_else(|| Error::Init {
            message: "unshare gave no stdout pipe".to_owned(),
        })?);
        let mut live = LiveWorld {
            launcher,
            stdin,
            stdout,
            pgid,
            init_pid: 0,
            // Placeholder until the handshake resolves the real init pid.
            namespaces: NamespaceTable::from_proc(std::process::id())?,
            next_request: 0,
            pods: Vec::new(),
        };
        let ready = await_ready(&mut live, host.boot_timeout);
        let (init_pid, identity) = match ready {
            Ok(ready) => ready,
            Err(err) => {
                let _ = live.launcher.kill();
                let _ = live.launcher.wait();
                return Err(err);
            }
        };
        let namespaces = NamespaceTable::from_proc(init_pid)?;
        namespaces.verify(&identity)?;
        namespaces.write_json(&pods.path().join("namespaces.json"))?;
        live.init_pid = init_pid;
        live.namespaces = namespaces;
        Ok(Self {
            host,
            world: world.clone(),
            pods,
            live: Some(live),
        })
    }

    /// The validated world name.
    #[must_use]
    pub fn world(&self) -> &WorldName {
        &self.world
    }

    /// Host PID of `slipfork-world-init`, verified by namespace inode identity.
    #[must_use]
    pub fn init_pid(&self) -> Option<u32> {
        self.live.as_ref().map(|live| live.init_pid)
    }

    /// Canonical `/proc/<init-pid>/ns/*` table for the live world.
    #[must_use]
    pub fn namespaces(&self) -> Option<&NamespaceTable> {
        self.live.as_ref().map(|live| &live.namespaces)
    }

    /// The world's tmpfs pod root.
    #[must_use]
    pub fn pods_dir(&self) -> &Path {
        self.pods.path()
    }

    /// Writes one bundle on tmpfs and asks the init to run `crun`; returns the container pid.
    pub fn launch(&mut self, spec: &PodSpec) -> Result<u32> {
        let live = self.live.as_mut().ok_or_else(|| Error::Init {
            message: "world is not running".to_owned(),
        })?;
        if live.pods.iter().any(|id| id == &spec.id) {
            return Err(Error::InvalidPod {
                id: spec.id.clone(),
                reason: "a pod with this id is already running".to_owned(),
            });
        }
        write_bundle(spec, &self.host.pod_host(), &self.pods.pod_dir(&spec.id))?;
        live.next_request += 1;
        let request_id = format!("req-{}", live.next_request);
        let request = Request::Launch {
            v: control::PROTOCOL_VERSION,
            id: request_id.clone(),
            pod: spec.clone(),
        };
        let bytes = control::encode(&request)?;
        live.stdin
            .write_all(&bytes)
            .and_then(|()| live.stdin.flush())
            .map_err(|source| Error::io("write", Path::new("slipfork-world-init stdin"), source))?;
        let ok = await_reply(live, &request_id, self.host.boot_timeout)?;
        let pid = ok
            .get("pid")
            .and_then(serde_json::Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| Error::Protocol {
                message: "launch reply carried no pid".to_owned(),
            })?;
        live.pods.push(spec.id.clone());
        Ok(pid)
    }

    /// Stops every pod through the init, then waits, kills the group, and proves it empty.
    pub fn stop(&mut self) -> Result<()> {
        let Some(mut live) = self.live.take() else {
            return Ok(());
        };
        live.next_request += 1;
        let request = Request::StopAll {
            v: control::PROTOCOL_VERSION,
            id: format!("req-{}", live.next_request),
        };
        if let Ok(bytes) = control::encode(&request) {
            let _ = live.stdin.write_all(&bytes);
            let _ = live.stdin.flush();
        }
        // EOF lifeline: even if the request frame is lost, the init stops every pod and exits.
        drop(live.stdin);
        let deadline = Instant::now() + self.host.boot_timeout;
        let mut exited = false;
        while Instant::now() < deadline {
            if live
                .launcher
                .try_wait()
                .map_err(|source| Error::io("wait", &self.host.unshare, source))?
                .is_some()
            {
                exited = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        if !exited {
            // The launcher still owns the group id, so `-pgid` cannot be a recycled group.
            // SAFETY: `kill(2)` with a negative pid never dereferences memory.
            unsafe {
                libc::kill(-live.pgid, libc::SIGKILL);
            }
            let _ = live.launcher.wait();
        }
        let outcome = wait_for_empty_group(live.pgid, Duration::from_secs(5));
        if outcome.is_err() {
            // Reap survivors, including pods that left the launcher group, while we can still
            // enumerate them; the original `LeftoverProcesses` failure stays observable.
            if let Ok(survivors) = group_members(live.pgid) {
                for pid in survivors {
                    kill_survivor(pid);
                }
            }
        }
        let _ = fs::remove_dir_all(self.pods.path());
        outcome
    }
}

impl Drop for WorldSupervisor {
    fn drop(&mut self) {
        // The explicit `stop()` call returns teardown failures; `Drop` is the last-resort
        // best effort and must still make a leak visible instead of swallowing it.
        if let Err(error) = self.stop() {
            eprintln!("slipfork-world: teardown failed: {error}");
        }
    }
}

/// Kills one enumerated survivor and, when it leads its own group, that whole group.
fn kill_survivor(pid: u32) {
    let Ok(pid) = i32::try_from(pid) else {
        return;
    };
    // SAFETY: `kill(2)`/`getpgid(2)` never dereference memory.
    unsafe {
        if libc::getpgid(pid) == pid {
            libc::kill(-pid, libc::SIGKILL);
        }
        libc::kill(pid, libc::SIGKILL);
    }
}

fn await_ready(live: &mut LiveWorld, timeout: Duration) -> Result<(u32, NamespaceIdentity)> {
    loop {
        let line = read_line(&mut live.stdout, timeout)?.ok_or_else(|| Error::Init {
            message: "slipfork-world-init exited before its ready handshake".to_owned(),
        })?;
        match control::decode_init_frame(line.trim())? {
            InitFrame::Ready { namespaces, .. } => {
                let host_pid = resolve_init_pid(live.launcher.id(), timeout)?;
                return Ok((host_pid, namespaces));
            }
            InitFrame::Stopped { .. } => {
                return Err(Error::Init {
                    message: "world init stopped during boot".to_owned(),
                });
            }
            InitFrame::Reply { .. } => {}
        }
    }
}

/// Finds the launcher's child whose `pid` namespace inode matches the reported identity.
fn resolve_init_pid(launcher_pid: u32, timeout: Duration) -> Result<u32> {
    let deadline = Instant::now() + timeout;
    let children = format!("/proc/{launcher_pid}/task/{launcher_pid}/children");
    loop {
        if let Ok(text) = fs::read_to_string(&children) {
            for pid in text
                .split_whitespace()
                .filter_map(|value| value.parse::<u32>().ok())
            {
                if NamespaceTable::from_proc(pid).is_ok() {
                    return Ok(pid);
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::Init {
                message: format!("no init process below launcher {launcher_pid}"),
            });
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn await_reply(
    live: &mut LiveWorld,
    request_id: &str,
    timeout: Duration,
) -> Result<serde_json::Value> {
    loop {
        let line = read_line(&mut live.stdout, timeout)?.ok_or_else(|| Error::Init {
            message: "world init closed its control channel".to_owned(),
        })?;
        match control::decode_init_frame(line.trim())? {
            InitFrame::Reply { id, ok, error, .. } if id == request_id => {
                if let Some(error) = error {
                    // Only pod launches answer with an error frame; the remedy is the runtime's.
                    let message = if error.message.is_empty() {
                        error.code
                    } else {
                        error.message
                    };
                    return Err(Error::PodLaunch { message });
                }
                return Ok(ok.unwrap_or(serde_json::Value::Null));
            }
            InitFrame::Reply { id, .. } => {
                return Err(Error::Protocol {
                    message: format!("unexpected reply id `{id}`"),
                });
            }
            InitFrame::Stopped { .. } => {
                return Err(Error::Init {
                    message: "world init stopped while a request was pending".to_owned(),
                });
            }
            InitFrame::Ready { .. } => {}
        }
    }
}

fn read_line(reader: &mut BufReader<ChildStdout>, timeout: Duration) -> Result<Option<String>> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::Timeout {
                action: "wait for slipfork-world-init",
            });
        }
        if !poll_readable(reader.get_ref().as_raw_fd(), remaining)? {
            continue;
        }
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|source| Error::io("read", Path::new("slipfork-world-init stdout"), source))?;
        if read == 0 {
            return Ok(None);
        }
        return Ok(Some(line));
    }
}

fn poll_readable(fd: RawFd, timeout: Duration) -> Result<bool> {
    let millis = i32::try_from(timeout.as_millis().max(1)).unwrap_or(i32::MAX);
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid `pollfd` lives across the call.
    let ret = unsafe { libc::poll(&mut pollfd, 1, millis) };
    if ret < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(Error::io(
            "poll",
            Path::new("slipfork-world-init stdout"),
            error,
        ));
    }
    Ok(ret > 0)
}

/// Non-zombie processes still in `pgid`; the reap proof, never a `pgrep` guess.
fn group_members(pgid: i32) -> Result<Vec<u32>> {
    let mut members = Vec::new();
    let entries =
        fs::read_dir("/proc").map_err(|source| Error::io("read", Path::new("/proc"), source))?;
    for entry in entries {
        let entry = entry.map_err(|source| Error::io("read", Path::new("/proc"), source))?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let state = fields.next().unwrap_or_default();
        let _ppid = fields.next();
        let Some(process_group) = fields.next().and_then(|value| value.parse::<i32>().ok()) else {
            continue;
        };
        if process_group == pgid && state != "Z" {
            members.push(pid);
        }
    }
    Ok(members)
}

fn wait_for_empty_group(pgid: i32, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let members = group_members(pgid)?;
        if members.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::LeftoverProcesses {
                count: members.len(),
                pgid,
            });
        }
        thread::sleep(Duration::from_millis(20));
    }
}
