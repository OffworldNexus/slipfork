//! Host-side supervision of one world: `unshare` launcher, PID 1 handshake, pod launches, reap.
//!
//! The launcher runs `unshare --user --map-current-user --net --ipc --pid --fork --mount-proc
//! --kill-child=SIGKILL` around `slipfork-world-init`. The supervisor never `setns`es (that fails
//! for an unprivileged multithreaded netns); it talks NDJSON over the init's stdin and kills the
//! whole process group on teardown. The init owns the single `<pods>/control.sock` plane that
//! `slipfork down`/`restart` speak to, so no second socket exists.

mod boot;
mod r#loop;

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout};
use std::time::Duration;

use crate::control::{self, Request};
use crate::error::{Error, Result};
use crate::layout::WorldName;
use crate::namespaces::NamespaceTable;
use crate::pod::PodHost;
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
    /// Assembles the injected host description; the composition root resolves every path.
    #[must_use]
    pub fn new(
        crun: PathBuf,
        unshare: PathBuf,
        world_init: PathBuf,
        runtime_dir: PathBuf,
        uid: u32,
        gid: u32,
        resolv_conf: Option<PathBuf>,
    ) -> Self {
        Self {
            crun,
            unshare,
            world_init,
            runtime_dir,
            uid,
            gid,
            resolv_conf,
            boot_timeout: Duration::from_secs(120),
        }
    }

    fn pod_host(&self) -> PodHost {
        PodHost {
            crun: self.crun.clone(),
            resolv_conf: self.resolv_conf.clone(),
            uid: self.uid,
            gid: self.gid,
        }
    }
}

pub(super) struct LiveWorld {
    pub(super) launcher: Child,
    pub(super) stdin: ChildStdin,
    pub(super) stdout: BufReader<ChildStdout>,
    pub(super) pgid: i32,
    pub(super) init_pid: u32,
    pub(super) namespaces: NamespaceTable,
    pub(super) next_request: u64,
    pub(super) pods: Vec<String>,
}

/// One tracked pod as reported by the world init itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodStatus {
    /// Pod id (`m339pg`, …).
    pub id: String,
    /// Host-visible pid of the pod's init process.
    pub pid: i32,
    /// `false` once the process is gone (a client that finished, say).
    pub running: bool,
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
        boot::boot(host, world)
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

    /// The single `<pods>/control.sock` path a separate daemon process dials.
    #[must_use]
    pub fn dial_socket(&self) -> PathBuf {
        control::control_socket_path(self.pods.path())
            .unwrap_or_else(|_| self.pods.path().join(control::CONTROL_SOCKET))
    }

    /// Dials one world-loopback port through the init's `Dial` request.
    pub fn dial_tcp(&self, port: u16) -> Result<std::net::TcpStream> {
        control::dial_tcp(self.pods.path(), port)
    }

    /// Replays (and optionally follows) captured pod output from `<pods>/logs/<id>.log`.
    pub fn logs(
        &self,
        service: Option<&str>,
        follow: bool,
        on_output: &mut dyn FnMut(control::OutputEvent),
    ) -> Result<serde_json::Value> {
        control::logs(self.pods.path(), service, follow, on_output)
    }

    /// Runs one command inside a live pod and returns its exit code.
    pub fn exec(
        &self,
        pod: &str,
        argv: &[String],
        on_output: &mut dyn FnMut(control::OutputEvent),
    ) -> Result<i32> {
        control::exec(self.pods.path(), pod, argv, on_output)
    }
}

/// Parses one `List` reply into typed rows; the supervisor and the `down` CLI client share it so
/// the wire shape is decoded in exactly one place.
pub fn pod_statuses(reply: &serde_json::Value) -> Result<Vec<PodStatus>> {
    let pods = reply
        .get("pods")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| crate::error::Error::Protocol {
            message: "list reply carried no `pods` array".to_owned(),
        })?;
    pods.iter()
        .map(|pod| {
            let id = pod
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| crate::error::Error::Protocol {
                    message: "list entry carried no `id`".to_owned(),
                })?;
            let pid = pod
                .get("pid")
                .and_then(serde_json::Value::as_i64)
                .and_then(|pid| i32::try_from(pid).ok())
                .ok_or_else(|| crate::error::Error::Protocol {
                    message: "list entry carried no `pid`".to_owned(),
                })?;
            let running = pod
                .get("running")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            Ok(PodStatus {
                id: id.to_owned(),
                pid,
                running,
            })
        })
        .collect()
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

/// Writes one request frame and waits for the matching typed reply.
fn exchange(
    live: &mut LiveWorld,
    request: &Request,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let bytes = control::encode(request)?;
    live.stdin
        .write_all(&bytes)
        .and_then(|()| live.stdin.flush())
        .map_err(|source| Error::io("write", Path::new("slipfork-world-init stdin"), source))?;
    boot::await_reply(live, request.id(), timeout)
}
