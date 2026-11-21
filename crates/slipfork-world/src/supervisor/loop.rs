//! The supervisor's request/response loop: pod launches, waits, restart, and teardown.

use std::fs;
use std::io::Write;
use std::thread;
use std::time::{Duration, Instant};

use crate::control::{self, Request};
use crate::error::{Error, Result};
use crate::pod::{PodSpec, write_bundle};

use super::WorldSupervisor;
use super::{boot, exchange};

impl WorldSupervisor {
    /// Runs one setup/hook pod to completion, streaming every stdout/stderr line live.
    ///
    /// The init owns the non-detached `crun run`, so a failing command's own diagnostics reach
    /// `on_line` instead of being swallowed by a detached container. The deadline is passed to the
    /// init (which kills and reaps a silent step) and the host waits a grace period longer, so a
    /// timeout arrives as the init's typed `run-timeout` reply rather than a bare local timeout.
    pub fn run_once(
        &mut self,
        spec: &PodSpec,
        on_line: &mut dyn FnMut(control::Stream, &str),
    ) -> Result<i32> {
        write_bundle(spec, &self.host.pod_host(), &self.pods.pod_dir(&spec.id))?;
        let Some(live) = self.live.as_mut() else {
            return Err(Error::Init {
                message: "world is not running".to_owned(),
            });
        };
        let timeout = self.host.boot_timeout;
        live.next_request += 1;
        let id = format!("req-{}", live.next_request);
        let request = Request::RunOnce {
            v: control::PROTOCOL_VERSION,
            id: id.clone(),
            pod: spec.clone(),
            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        };
        let bytes = control::encode(&request)?;
        live.stdin
            .write_all(&bytes)
            .and_then(|()| live.stdin.flush())
            .map_err(|source| Error::io("write", self.pods.path(), source))?;
        boot::await_run_once(live, &id, timeout + Duration::from_secs(5), on_line)
    }

    /// Asks the init inside the world which pods it still tracks.
    ///
    /// Host-side `crun list` cannot inspect containers created in the world's user namespace
    /// (`kill: Operation not permitted`), so this is the only trustworthy in-world oracle. A
    /// stopped world refuses the question instead of pretending it has zero pods.
    pub fn pods(&mut self) -> Result<Vec<super::PodStatus>> {
        let Some(live) = self.live.as_mut() else {
            return Err(Error::Init {
                message: "world is not running".to_owned(),
            });
        };
        live.next_request += 1;
        let id = format!("req-{}", live.next_request);
        let request = Request::List {
            v: control::PROTOCOL_VERSION,
            id: id.clone(),
        };
        let bytes = control::encode(&request)?;
        live.stdin
            .write_all(&bytes)
            .and_then(|()| live.stdin.flush())
            .map_err(|source| Error::io("write", self.pods.path(), source))?;
        let reply = boot::await_reply(live, &id, self.host.boot_timeout)?;
        super::pod_statuses(&reply)
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
    pub fn wait(&mut self, pid: u32, timeout: Duration) -> Result<Option<i32>> {
        let Some(live) = self.live.as_mut() else {
            return Err(Error::Init {
                message: "world is not running".to_owned(),
            });
        };
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
        if ok.get("running").and_then(serde_json::Value::as_bool) == Some(true) {
            return Ok(None);
        }
        ok.get("exit_code")
            .and_then(serde_json::Value::as_i64)
            .and_then(|code| i32::try_from(code).ok())
            .map(Some)
            .ok_or_else(|| Error::Protocol {
                message: "wait reply carried neither an exit code nor `running`".to_owned(),
            })
    }

    /// Restarts one pod (or every pod) from its existing bundle and reports the count.
    pub fn restart(&mut self, service: Option<&str>) -> Result<usize> {
        let Some(live) = self.live.as_mut() else {
            return Err(Error::Init {
                message: "world is not running".to_owned(),
            });
        };
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
