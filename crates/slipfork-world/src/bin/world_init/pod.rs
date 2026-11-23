//! Launched containers inside the world: `crun run`, captured logs, waits, restart, teardown.
//!
//! The module root owns the world's pod table and queries; [`launch`] owns `crun run` and the
//! log pumps, [`stop`] owns group kill and restart.

mod launch;
mod stop;

use std::fs;
use std::io;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use slipfork_world::PodSpec;
use slipfork_world::control::S3WorldConfig;
use tokio::runtime::Handle;

use crate::s3::S3Host;
pub(crate) use crate::shared::{PodInfo, WorldShared, reap_zombies};
use crate::{Fault, REMEDY_POD, REMEDY_RUNTIME};
pub(crate) use launch::{launch_on_shared, list_on_shared, run_once_on_shared};

pub(crate) struct InitWorld {
    shared: Arc<WorldShared>,
    /// The one runtime the native S3 endpoint is served on; never constructed in library code.
    runtime: Handle,
    s3: Option<S3Host>,
}

impl InitWorld {
    pub(crate) fn new(crun: PathBuf, pods_root: PathBuf, runtime: Handle) -> Result<Self, Fault> {
        let crun_root = pods_root.join("crun");
        // A fresh init is a new user/net/ipc/pid namespace, so no `crun` container from an earlier
        // boot of this world name can still be live; a kernel-killed init leaves its state dir
        // behind on the shared tmpfs and every relaunch then fails with `container already exists`.
        match fs::remove_dir_all(&crun_root) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Fault::cause(
                    "crun state root reset",
                    REMEDY_RUNTIME,
                    source,
                ));
            }
        }
        fs::create_dir_all(&crun_root)
            .and_then(|()| fs::set_permissions(&crun_root, fs::Permissions::from_mode(0o700)))
            .map_err(|source| Fault::cause("crun state root setup", REMEDY_RUNTIME, source))?;
        Ok(Self {
            shared: Arc::new(WorldShared::new(crun, crun_root, pods_root)),
            runtime,
            s3: None,
        })
    }

    pub(crate) fn shared(&self) -> Arc<WorldShared> {
        Arc::clone(&self.shared)
    }

    pub(crate) fn pods_root(&self) -> &Path {
        self.shared.pods_root()
    }

    /// Every tracked pod with its pid, reaping status, and exit code.
    pub(crate) fn list(&self) -> Vec<serde_json::Value> {
        list_on_shared(&self.shared)
    }

    /// Bounded wait: `Some(code)` when the pod exited, `None` while it is still running.
    pub(crate) fn wait(&self, pid: i32, timeout_ms: u64) -> Result<Option<i32>, Fault> {
        let tracked = self
            .shared
            .pids
            .lock()
            .map(|pids| pids.values().any(|candidate| *candidate == pid))
            .unwrap_or(false);
        if !tracked {
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
                return Ok(Some(crate::shared::exit_code(status)));
            }
            if reaped < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) {
                    if let Some(code) = self.shared.take_status(pid) {
                        return Ok(Some(code));
                    }
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
        launch_on_shared(&self.shared, pod)
    }

    /// Binds and serves the declared buckets on the injected runtime, replacing any previous
    /// endpoint. Returns the bound address and the bucket count.
    pub(crate) fn configure_s3(
        &mut self,
        config: S3WorldConfig,
    ) -> Result<(SocketAddr, usize), Fault> {
        self.stop_s3();
        let host = S3Host::bind(&self.runtime, config)?;
        let reply = (host.listen_addr(), host.buckets());
        self.s3 = Some(host);
        Ok(reply)
    }

    fn stop_s3(&mut self) {
        if let Some(host) = self.s3.take() {
            host.shutdown();
        }
    }

    /// Runs one bundle to completion while forwarding every stdout/stderr line.
    pub(crate) fn run_once_streaming(
        &self,
        pod: &PodSpec,
        timeout_ms: u64,
        on_line: &mut dyn FnMut(slipfork_world::Stream, &str) -> Result<(), Fault>,
    ) -> Result<i32, Fault> {
        run_once_on_shared(&self.shared, pod, timeout_ms, on_line)
    }
}
