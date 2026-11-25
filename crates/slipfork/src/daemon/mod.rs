//! The single per-user daemon: `<runtime_dir>/daemon.sock`, NDJSON control, lifecycles, ingress.
//!
//! One `slipfork` binary is both the CLI and the daemon (`slipfork __daemon`). The daemon owns every
//! world under the invoking UID, relays boot progress as `event` frames, and reaches world pods only
//! through each world's `<pods>/control.sock` `Dial` request. No second socket exists.

mod boot;
mod client;
mod frames;
mod handoff;
mod host;
mod identity;
pub(crate) mod image;
mod ingress;
mod lease;
mod lifecycle;
mod lock;
mod reconcile;
mod registry;
mod rpc;
mod spawn;
mod view;
mod wire;

pub use client::{DaemonClient, DaemonError};

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::world::HostEnv;
use frames::{FrameReader, write_frame};
use identity::DaemonIdentity;
use lifecycle::core::Core;
use serde_json::{Value, json};
use slipfork_vcs::VcsWorkspace;

/// A typed control-plane failure rendered as an `error` frame with a bare message.
#[derive(Debug, Clone)]
pub(crate) struct Failure {
    code: &'static str,
    message: String,
}

impl Failure {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// One daemon startup failure; the CLI prints exactly one `next:` line.
struct Startup {
    message: String,
    next: String,
}

impl Startup {
    fn new(message: impl Into<String>, next: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            next: next.into(),
        }
    }
}

/// Runs the socket-activated `:80` handoff in this process; called from the composition root.
pub use handoff::{Mode as IngressMode, run as run_handoff};

/// `<runtime_dir>/daemon.sock`.
#[must_use]
pub fn socket_path(host: &HostEnv) -> PathBuf {
    host.runtime_dir.join("daemon.sock")
}

/// Runs the daemon in this process; returns only when startup fails.
pub fn run(host: HostEnv, vcs: Arc<dyn VcsWorkspace>) -> ExitCode {
    match Daemon::start(host, vcs) {
        Ok(Some(daemon)) => match daemon.serve() {
            Ok(()) => ExitCode::SUCCESS,
            Err(startup) => {
                report(&startup);
                ExitCode::FAILURE
            }
        },
        Ok(None) => ExitCode::SUCCESS,
        Err(startup) => {
            report(&startup);
            ExitCode::FAILURE
        }
    }
}

fn report(startup: &Startup) {
    eprintln!("error: {}", startup.message);
    eprintln!("next: {}", startup.next);
}

struct Daemon {
    core: Mutex<Core>,
    /// Serializes image builds/pulls so `images/refs.json` and `built.json` never race.
    image_lock: Mutex<()>,
    socket: PathBuf,
    state_dir: PathBuf,
    /// This daemon's own binary and version, so a client can refuse a stale daemon.
    identity: DaemonIdentity,
    /// The invoking process environment, snapshotted by the composition root.
    env: BTreeMap<String, String>,
}

impl Daemon {
    fn start(host: HostEnv, vcs: Arc<dyn VcsWorkspace>) -> Result<Option<Self>, Startup> {
        fs::create_dir_all(&host.state_dir).map_err(|error| {
            Startup::new(
                format!("create `{}`: {error}", host.state_dir.display()),
                "check that the state directory is writable",
            )
        })?;
        let Some(lock) = lock::acquire(&host.runtime_dir).map_err(|error| {
            Startup::new(
                format!("lock `{}`: {error}", host.runtime_dir.display()),
                "check that XDG_RUNTIME_DIR is writable by this user",
            )
        })?
        else {
            eprintln!("slipfork daemon: another daemon already holds the runtime lock");
            return Ok(None);
        };
        // Detach from the invoking terminal; the daemon outlives the CLI that spawned it.
        // SAFETY: `setsid(2)` only moves this process into a fresh session.
        unsafe {
            libc::setsid();
        }
        let socket = socket_path(&host);
        let _ = fs::remove_file(&socket);
        let identity = DaemonIdentity::new(&host.exe);
        let state_dir = host.state_dir.clone();
        let env = host.env.clone();
        let entries = registry::load(&state_dir).map_err(|error| {
            Startup::new(
                format!(
                    "cannot read `{}`: {error}",
                    registry::path(&state_dir).display()
                ),
                format!("fix or delete `{}`", registry::path(&state_dir).display()),
            )
        })?;
        let core = Core::new(host, vcs, state_dir.clone(), lock, entries);
        let retained = core.retained();
        let _ = registry::store(&state_dir, &retained);
        Ok(Some(Self {
            core: Mutex::new(core),
            image_lock: Mutex::new(()),
            socket,
            state_dir,
            identity,
            env,
        }))
    }

    fn serve(&self) -> Result<(), Startup> {
        let _ = fs::create_dir_all(&self.state_dir);
        let listener = UnixListener::bind(&self.socket).map_err(|error| {
            Startup::new(
                format!("bind `{}`: {error}", self.socket.display()),
                "remove the stale socket and retry",
            )
        })?;
        fs::set_permissions(&self.socket, fs::Permissions::from_mode(0o600)).map_err(|error| {
            Startup::new(
                format!("chmod `{}`: {error}", self.socket.display()),
                "check ownership of the runtime directory",
            )
        })?;
        thread::scope(|scope| {
            for incoming in listener.incoming() {
                match incoming {
                    Ok(stream) => {
                        scope.spawn(move || {
                            if let Err(error) = self.handle_connection(stream) {
                                if !matches!(
                                    error.kind(),
                                    io::ErrorKind::BrokenPipe
                                        | io::ErrorKind::ConnectionReset
                                        | io::ErrorKind::UnexpectedEof
                                ) {
                                    eprintln!("slipfork daemon: connection ended: {error}");
                                }
                            }
                        });
                    }
                    Err(error) => {
                        if error.kind() != io::ErrorKind::Interrupted {
                            eprintln!("slipfork daemon: accept failed: {error}");
                        }
                    }
                }
            }
        });
        Ok(())
    }

    fn core(&self) -> Result<std::sync::MutexGuard<'_, Core>, Failure> {
        self.core
            .lock()
            .map_err(|_| Failure::new("daemon-poisoned", "daemon state lock is poisoned"))
    }

    fn handle_connection(&self, stream: UnixStream) -> io::Result<()> {
        let mut writer = stream.try_clone()?;
        let mut reader = FrameReader::new(stream);
        loop {
            let (payload, descriptor) = match reader.next()? {
                Some(frame) => frame,
                None => return Ok(()),
            };
            let request: wire::Request = match serde_json::from_slice(&payload) {
                Ok(request) => request,
                Err(error) => {
                    write_frame(
                        &mut writer,
                        &wire::error("", "bad-frame", &format!("malformed JSON request: {error}")),
                    )?;
                    continue;
                }
            };
            let id = request.id.clone();
            if request.v != wire::VERSION {
                write_frame(
                    &mut writer,
                    &wire::error(
                        &id,
                        "unsupported-version",
                        &format!("daemon protocol v{} is not v{}", request.v, wire::VERSION),
                    ),
                )?;
                continue;
            }
            if request.method == "shutdown" {
                self.stop_and_unlink();
                let _ = write_frame(&mut writer, &wire::ok(&id, json!({ "stopped": true })));
                let _ = writer.flush();
                std::process::exit(0);
            }
            if request.method == "exec" {
                let outcome = self.handle_exec(&request, &mut reader, &mut writer);
                match outcome {
                    Ok(value) => write_frame(&mut writer, &wire::ok(&id, value))?,
                    Err(failure) => write_frame(
                        &mut writer,
                        &wire::error(&id, failure.code, &failure.message),
                    )?,
                }
                return Ok(());
            }
            if request.method == "shell" {
                let outcome = self.handle_shell(&request, &mut reader, &mut writer);
                match outcome {
                    Ok(value) => write_frame(&mut writer, &wire::ok(&id, value))?,
                    Err(failure) => write_frame(
                        &mut writer,
                        &wire::error(&id, failure.code, &failure.message),
                    )?,
                }
                continue;
            }
            if request.method == "logs" {
                let plan = match self.core() {
                    Ok(mut core) => core.plan_logs_stream(&request.params),
                    Err(failure) => Err(failure),
                };
                // The core lock is released here: a followed stream must never block `up`/`down`.
                let value = {
                    let probe = writer.try_clone()?;
                    let mut emit =
                        |event: Value| write_frame(&mut writer, &wire::event(&id, event)).is_ok();
                    let mut alive = || !frames::peer_closed(&probe);
                    match plan {
                        Ok(plan) => plan.run(&mut emit, &mut alive),
                        Err(failure) => Err(failure),
                    }
                };
                match value {
                    Ok(value) => {
                        if value.get("closed").and_then(Value::as_bool) == Some(true) {
                            eprintln!("slipfork daemon: logs follower closed {{\"closed\":true}}");
                        }
                        write_frame(&mut writer, &wire::ok(&id, value))?;
                    }
                    Err(failure) => write_frame(
                        &mut writer,
                        &wire::error(&id, failure.code, &failure.message),
                    )?,
                }
                continue;
            }
            if matches!(request.method.as_str(), "image.build" | "image.pull") {
                self.handle_image(&id, &request.method, &request.params, &mut writer)?;
                continue;
            }
            let outcome = self.dispatch(&request, descriptor, &mut |event| {
                if write_frame(&mut writer, &wire::event(&id, event)).is_err() {
                    eprintln!("slipfork daemon: client closed during `{}`", request.method);
                }
            });
            match outcome {
                Ok(Some(value)) => write_frame(&mut writer, &wire::ok(&id, value))?,
                Ok(None) => {}
                Err(failure) => {
                    write_frame(
                        &mut writer,
                        &wire::error(&id, failure.code, &failure.message),
                    )?;
                }
            }
        }
    }

    fn dispatch(
        &self,
        request: &wire::Request,
        descriptor: Option<std::os::fd::OwnedFd>,
        emit: &mut dyn FnMut(Value),
    ) -> Result<Option<Value>, Failure> {
        match request.method.as_str() {
            "ingress.accept" => self.dispatch_ingress(descriptor),
            "up" => self.dispatch_up(&request.params, emit),
            "fork" => self.dispatch_fork(&request.params, emit),
            "drop" => self.core()?.drop_fork(&request.params, emit).map(Some),
            "ping" => {
                let mut reply = self.identity.ping_fields();
                reply["pong"] = json!(true);
                reply["pid"] = json!(std::process::id());
                reply["worlds"] = json!(self.core()?.world_count());
                Ok(Some(reply))
            }
            "ps" => {
                let reply = self.core()?.ps()?;
                let value = serde_json::to_value(reply)
                    .map_err(|error| Failure::new("ps-encode", error.to_string()))?;
                Ok(Some(value))
            }
            "down" => self.core()?.down(&request.params, emit).map(Some),
            "restart" => self.dispatch_restart(&request.params, emit),
            "env" => host::env(&self.state_dir, &self.env, &request.params),
            "secret.list" => host::secret_list(&self.state_dir, &request.params),
            "secret.set" => host::secret_set(&self.state_dir, &request.params),
            "secret.unset" => host::secret_unset(&self.state_dir, &request.params),
            other => Err(Failure::new(
                "unknown-method",
                format!("unknown method `{other}`"),
            )),
        }
    }

    fn dispatch_ingress(
        &self,
        descriptor: Option<std::os::fd::OwnedFd>,
    ) -> Result<Option<Value>, Failure> {
        let Some(descriptor) = descriptor else {
            return Err(Failure::new(
                "missing-descriptor",
                "`ingress.accept` must carry exactly one accepted connection descriptor",
            ));
        };
        let (registry, pods) = self.core()?.ingress_view()?;
        ingress::serve(registry, pods, descriptor)?;
        Ok(None)
    }

    fn dispatch_up(
        &self,
        params: &Value,
        emit: &mut dyn FnMut(Value),
    ) -> Result<Option<Value>, Failure> {
        // Decide under the daemon lock, then release it: heal/reconcile run typed world calls
        // that must never block `ps`/`ping`, exactly like the `logs` stream path.
        // The head walks the build context, so it is computed before taking the lock.
        let (project, world, _) = self.up_target(params)?;
        let host = { self.core()?.host().clone() };
        let desired = crate::daemon::lease::prepare(&host, &project, &world)
            .ok()
            .map(|(head, _)| head);
        let action = self.core()?.up_plan(params, desired.as_ref())?;
        match action {
            lifecycle::up::UpAction::Idle(plan) => {
                let reply = lifecycle::up::settle(*plan, emit)?;
                emit(json!({
                    "event": "ready",
                    "url": reply["url"],
                    "world": reply["world"],
                    "elapsed_ms": reply["elapsed_ms"],
                }));
                Ok(Some(reply))
            }
            lifecycle::up::UpAction::Reconcile => {
                let (project, world, no_cache) = self.up_target(params)?;
                let planned = self.preplan(&project, &world, no_cache, emit)?;
                let plan = self
                    .core()?
                    .detach_live(&rpc::key_of(&project))
                    .ok_or_else(|| {
                        Failure::new("no-world", "no world is running in this directory")
                    })?;
                lifecycle::up::reconcile(plan);
                let mut session = self.core()?.begin_up(params, planned)?;
                session.plan_already_announced();
                session.run(self, emit).map(Some)
            }
            lifecycle::up::UpAction::Boot => {
                let (project, world, no_cache) = self.up_target(params)?;
                let planned = self.preplan(&project, &world, no_cache, emit)?;
                let mut session = self.core()?.begin_up(params, planned)?;
                session.plan_already_announced();
                session.run(self, emit).map(Some)
            }
        }
    }

    /// Snapshots the fork, then boots it through the same `up` path and answers its coordinates.
    fn dispatch_fork(
        &self,
        params: &Value,
        emit: &mut dyn FnMut(Value),
    ) -> Result<Option<Value>, Failure> {
        let target = self.core()?.inspect_fork(params)?;
        let (raw_name, fork, preflight_ms) = target.prepare_outside_locks()?;
        let (mut session, prepared) =
            self.core()?
                .begin_fork(target, &raw_name, fork, preflight_ms, emit)?;
        let fork_name = prepared.fork.clone();
        let mut annotate = |mut event: Value| {
            if event.get("event").and_then(Value::as_str) == Some("ready") {
                event["fork"] = Value::String(fork_name.clone());
            }
            emit(event);
        };
        if let Err(failure) = session.run(self, &mut annotate) {
            // A booting child is only a pid in `stop_world`; reap the session's own child first
            // so no supervisor outlives the rollback even when the world was never published.
            session.stop();
            // A failed boot must not leave a half-fork that blocks a retry: stop its supervisor
            // and pods, drop the registry entry, then unlink the snapshots.
            if let Ok(mut core) = self.core() {
                core.discard_fork(&prepared);
            }
            return Err(failure);
        }
        Ok(Some(json!({
            "fork": prepared.fork,
            "world": prepared.world.clone(),
            "project": prepared.project,
            "url": format!("http://{}.localhost", prepared.world),
            "elapsed_ms": prepared.started.elapsed().as_millis() as u64,
            "snapshot_ms": prepared.snapshot_ms,
            "preflight_ms": prepared.preflight_ms,
        })))
    }

    fn stop_and_unlink(&self) {
        if let Ok(mut core) = self.core.lock() {
            core.stop_all();
            let _ = fs::remove_file(&self.socket);
        }
    }
}
