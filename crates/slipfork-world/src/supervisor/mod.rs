//! Host-side supervision of one world: `unshare` launcher, PID 1 handshake, pod launches, reap.
//!
//! The launcher runs `unshare --user --map-current-user --net --ipc --pid --fork --mount-proc
//! --kill-child=SIGKILL` around `slipfork-world-init`. The supervisor never `setns`es (that fails
//! for an unprivileged multithreaded netns); it talks NDJSON over the init's stdin and kills the
//! whole process group on teardown. The init owns the single `<pods>/control.sock` plane that
//! `slipfork down`/`restart` speak to, so no second socket exists.

mod boot;

use std::fs;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout};
use std::thread;
use std::time::{Duration, Instant};

use crate::control::{self, Request};
use crate::error::{Error, Result};
use crate::layout::WorldName;
use crate::namespaces::NamespaceTable;
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
        let id = format!("req-{}", live.next_request);
        let request = Request::Launch {
            v: control::PROTOCOL_VERSION,
            id,
            pod: spec.clone(),
        };
        let ok = exchange(live, &request, self.host.boot_timeout)?;
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

    /// Waits up to `timeout` for one launched pod and returns its exit code.
    pub fn wait(&mut self, pid: u32, timeout: Duration) -> Result<i32> {
        let live = self.live.as_mut().ok_or_else(|| Error::Init {
            message: "world is not running".to_owned(),
        })?;
        live.next_request += 1;
        let id = format!("req-{}", live.next_request);
        let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
        let request = Request::Wait {
            v: control::PROTOCOL_VERSION,
            id,
            pid: i32::try_from(pid).map_err(|_| Error::Protocol {
                message: format!("pod pid {pid} does not fit an i32"),
            })?,
            timeout_ms,
        };
        let ok = exchange(live, &request, timeout + Duration::from_secs(5))?;
        ok.get("exit_code")
            .and_then(serde_json::Value::as_i64)
            .and_then(|code| i32::try_from(code).ok())
            .ok_or_else(|| Error::Protocol {
                message: "wait reply carried no exit code".to_owned(),
            })
    }

    /// Restarts one pod (or every pod) from its existing bundle and reports the count.
    pub fn restart(&mut self, service: Option<&str>) -> Result<usize> {
        let live = self.live.as_mut().ok_or_else(|| Error::Init {
            message: "world is not running".to_owned(),
        })?;
        live.next_request += 1;
        let id = format!("req-{}", live.next_request);
        let request = Request::Restart {
            v: control::PROTOCOL_VERSION,
            id,
            service: service.map(str::to_owned),
        };
        let ok = exchange(live, &request, self.host.boot_timeout)?;
        usize::try_from(
            ok.get("restarted")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| Error::Protocol {
                    message: "restart reply carried no count".to_owned(),
                })?,
        )
        .map_err(|_| Error::Protocol {
            message: "restart reply count overflowed".to_owned(),
        })
    }

    /// Blocks until the world init exits (any `down`, crash, or lifeline EOF).
    pub fn await_exit(&mut self) -> Result<()> {
        let unshare = self.host.unshare.clone();
        let Some(live) = self.live.as_mut() else {
            return Ok(());
        };
        loop {
            if live
                .launcher
                .try_wait()
                .map_err(|source| Error::io("wait", &unshare, source))?
                .is_some()
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }
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
        let outcome = boot::wait_for_empty_group(live.pgid, Duration::from_secs(5));
        if outcome.is_err() {
            // Reap survivors, including pods that left the launcher group, while we can still
            // enumerate them; the original `LeftoverProcesses` failure stays observable.
            if let Ok(survivors) = boot::group_members(live.pgid) {
                for pid in survivors {
                    if let Ok(pid) = i32::try_from(pid) {
                        crate::kill_group(pid);
                    }
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
