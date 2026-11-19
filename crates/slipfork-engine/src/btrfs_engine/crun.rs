//! Daemonless `crun` control surface: the pinned static binary, the one `nsenter` → `crun` prefix,
//! the container-registration wait, and every `run`/`exec`/`kill`/`delete` invocation
//! (`ADR-0009` §§2–3).

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slipfork_world::{EngineError, ServiceLogs, StreamSource};

use crate::child_io::spawn_log_reader;

use super::BtrfsEngine;
use super::container::ContainerProcess;
use super::services::ServiceProcess;
/// One world's `crun` execution surface: the exact `nsenter` → `crun` prefix resolved once at
/// construction (world PID 1, pinned static `crun`, disabled cgroup manager, state root).
///
/// This type is the **sole** `crun` argv owner in the engine, and it is also the single writer of
/// per-service OCI bundles: a backing-service container gets an id-addressed bundle in the world
/// control dir, and a `ServicePod` gets `ctl/<world_id>/oci/<service>/config.json` from its
/// injected [`ServicePodSpec`](slipfork_world::pod::ServicePodSpec).
#[derive(Debug, Clone)]
pub struct CrunSurface {
    prefix: Vec<String>,
    /// World PID 1: the host-side entry point into the world's private mount namespace.
    init_pid: u32,
    /// World-visible path of `crun --root`, where each container registers `<id>/status`.
    root: PathBuf,
    /// Host path of the world's CoW `state` directory (`SLIPFORK_WORLD_STATE` for hooks/execs).
    state_dir: PathBuf,
}

/// Host-side path of one world-visible path through the live init's mount namespace.
///
/// The shared init mounts private tmpfs over the world's bundle and runtime-state directories, so
/// a host-side read/write of `<ctl>/oci` sees an empty `0700` mountpoint. Everything the engine
/// materializes there must be addressed through `/proc/<init_pid>/root`, which resolves in that
/// namespace; `nsenter --mount -- crun` addresses the same paths directly.
pub(crate) fn world_mount_path(init_pid: u32, path: &Path) -> PathBuf {
    Path::new("/proc")
        .join(init_pid.to_string())
        .join("root")
        .join(path.strip_prefix("/").unwrap_or(path))
}

/// The last few stderr lines the container runtime streamed before it gave up.
///
/// A runtime that exits before registering the container leaves no state to inspect, so its own
/// stderr is the only actionable diagnostic; it is reported beside the typed failure.
fn runtime_stderr_tail(log: &ServiceLogs) -> String {
    let text: String = log
        .snapshot()
        .iter()
        .filter(|chunk| chunk.source == StreamSource::Stderr)
        .map(|chunk| String::from_utf8_lossy(&chunk.bytes).into_owned())
        .collect();
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    let tail = &lines[lines.len().saturating_sub(4)..];
    format!("\n  runtime stderr: {}", tail.join(" | "))
}

/// Bound on a freshly spawned `crun run` publishing `<root>/<id>/status` before it can be exec'd.
///
/// `crun` registers the container before it execs the payload, so this only elapses when the
/// runtime itself fails to start — then the supervisor has already exited with the real reason.
const CONTAINER_REGISTER_TIMEOUT: Duration = Duration::from_secs(15);
/// Poll interval while waiting for that registration; short enough to stay invisible.
const CONTAINER_REGISTER_POLL: Duration = Duration::from_millis(20);

impl CrunSurface {
    pub(crate) fn new(
        engine: &BtrfsEngine,
        init_pid: u32,
        crun_bin: PathBuf,
        crun_root: PathBuf,
        state_dir: PathBuf,
    ) -> Self {
        // `crun` is a world binary and each container's own cwd comes from its OCI bundle.
        let mut prefix = engine.entry_prefix(init_pid, Path::new("/"));
        prefix.push(crun_bin.display().to_string());
        prefix.extend([
            "--cgroup-manager=disabled".to_owned(),
            "--root".to_owned(),
            crun_root.display().to_string(),
        ]);
        Self {
            prefix,
            init_pid,
            root: crun_root,
            state_dir,
        }
    }

    /// Host path of one world-visible path (`/proc/<init_pid>/root/<path>`).
    pub(crate) fn host_path(&self, path: &Path) -> PathBuf {
        world_mount_path(self.init_pid, path)
    }

    /// The one `nsenter` → `crun` prefix every invocation (`run`, `exec`, `kill`, `delete`) reuses.
    pub(crate) fn prefix(&self) -> Vec<String> {
        self.prefix.clone()
    }

    /// A `crun` invocation that runs inside the world's mount/net/ipc/pid namespaces.
    pub(crate) fn command(&self, env: &BTreeMap<String, String>) -> Command {
        let mut command = Command::new("nsenter");
        command
            .args(&self.prefix)
            .env_clear()
            .envs(env)
            .env("SLIPFORK_WORLD_STATE", &self.state_dir);
        command
    }

    fn control(&self, args: &[&str]) -> Command {
        let mut command = Command::new("nsenter");
        command
            .args(&self.prefix)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    fn run_control(&self, args: &[&str], action: &str, id: &str) -> Result<(), EngineError> {
        let status = self.control(args).status().map_err(|err| {
            EngineError::Execution(format!(
                "cannot run `crun {action}` for container '{id}': {err}"
            ))
        })?;
        if status.success() {
            Ok(())
        } else {
            Err(EngineError::Execution(format!(
                "`crun {action}` for container '{id}' exited with {status}"
            )))
        }
    }

    /// `crun kill` one container; the caller decides whether a refusal is fatal.
    pub(crate) fn signal(&self, id: &str, signal: &str) -> Result<(), EngineError> {
        self.run_control(&["kill", id, signal], "kill", id)
    }

    /// `crun delete --force` after the container's supervisor exited.
    pub(crate) fn delete(&self, id: &str) -> Result<(), EngineError> {
        self.run_control(&["delete", "--force", id], "delete", id)
    }

    /// Start one container from an already-written bundle and record its supervision handles.
    ///
    /// The one `crun run` implementation: `crun` is the supervised child, both pipes feed the
    /// retained log ring, and the recorded container is exactly what the stop path later signals
    /// (`crun kill`), deletes, and whose bundle directory it removes. `what` names the service in
    /// the failure diagnostic (never the argv, which may carry resolved secrets for other
    /// runtimes).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_bundle(
        &self,
        env: &BTreeMap<String, String>,
        what: &str,
        bundle_host: &Path,
        bundle_guest: &Path,
        id: &str,
        log: &ServiceLogs,
        process: &Arc<Mutex<ServiceProcess>>,
    ) -> Result<(), EngineError> {
        let mut command = self.command(env);
        command
            .args([
                "run".to_owned(),
                "-b".to_owned(),
                bundle_guest.display().to_string(),
                id.to_owned(),
            ])
            // Every component is its own process-group leader (`pgid == the launcher's host pid`),
            // so a stop, a restart, or the reaper can signal the whole tree without reaching the
            // daemon's group.
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|err| {
            EngineError::Execution(format!(
                "cannot start {what}: {err}\n  fix: run `slipfork doctor`; the pinned static \
                 `crun` and rootfs must exist in the world"
            ))
        })?;
        if let Some(stdout) = child.stdout.take() {
            spawn_log_reader(stdout, StreamSource::Stdout, log.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_reader(stderr, StreamSource::Stderr, log.clone());
        }
        if let Err(err) = self.await_registration(&mut child, id, what) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(EngineError::Execution(format!(
                "{err}{}",
                runtime_stderr_tail(log)
            )));
        }
        if let Ok(mut process) = process.lock() {
            process.supervisor = Some(child);
            process.container = Some(ContainerProcess {
                id: id.to_owned(),
                runtime: self.clone(),
                bundle_host: bundle_host.to_path_buf(),
            });
        }
        Ok(())
    }

    /// Block until `crun` registers the started container, so an immediate `exec`/`state` cannot
    /// race the runtime's own setup (`<root>/<id>/status` is what every later verb resolves).
    ///
    /// A supervisor that exits first already surfaced its reason through the streamed log; this
    /// returns the typed failure instead of leaving a `Running` row nobody can reach.
    fn await_registration(
        &self,
        child: &mut Child,
        id: &str,
        what: &str,
    ) -> Result<(), EngineError> {
        let status = self.host_path(&self.root).join(id).join("status");
        let deadline = Instant::now() + CONTAINER_REGISTER_TIMEOUT;
        loop {
            if status.is_file() {
                return Ok(());
            }
            if let Some(exit) = child
                .try_wait()
                .map_err(|err| EngineError::Execution(format!("cannot wait for {what}: {err}")))?
            {
                return Err(EngineError::Execution(format!(
                    "cannot start {what}: the container runtime exited with {exit} before \
                     registering '{id}'\n  fix: read the service log with `slipfork logs {id}`; \
                     the image must carry the bundle's argv and a writable rootfs"
                )));
            }
            if Instant::now() >= deadline {
                return Err(EngineError::Execution(format!(
                    "cannot start {what}: the container runtime did not register '{id}' within \
                     {}s\n  fix: run `slipfork doctor`; the pinned static `crun` must work inside \
                     the world",
                    CONTAINER_REGISTER_TIMEOUT.as_secs()
                )));
            }
            std::thread::sleep(CONTAINER_REGISTER_POLL);
        }
    }
}
