//! The daemon's serving loop: one socket, one connection per thread, every verb dispatched here.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};
use slipfork_vcs::VcsWorkspace;
use slipfork_world::{ForkLedger, StatePaths};

use super::frames::{self, FrameReader, write_frame};
use super::identity::DaemonIdentity;
use super::lifecycle::core::Core;
use super::{
    Failure, Startup, host, ingress, lifecycle, lock, rpc, socket_path, startup_ledger, wire,
};
use crate::world::HostEnv;
use crate::world::elapsed_ms;

pub(crate) struct Daemon {
    pub(super) core: Mutex<Core>,
    /// Serializes image builds/pulls so `images/refs.json` and `built.json` never race.
    pub(super) image_lock: Mutex<()>,
    pub(super) socket: PathBuf,
    pub(super) state_dir: PathBuf,
    /// This daemon's own binary and version, so a client can refuse a stale daemon.
    pub(super) identity: DaemonIdentity,
    /// The invoking process environment, snapshotted by the composition root.
    pub(super) env: BTreeMap<String, String>,
}

impl Daemon {
    pub(super) fn start(
        host: HostEnv,
        vcs: Arc<dyn VcsWorkspace>,
    ) -> Result<Option<Self>, Startup> {
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
        let ledger = ForkLedger::open_writer(StatePaths::new(state_dir.clone()), lock)
            .map_err(startup_ledger)?;
        let mut core = Core::new(host, vcs, ledger);
        core.converge().map_err(|failure| {
            Startup::new(
                failure
                    .message
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
                "fix the reported world state, then retry",
            )
        })?;
        Ok(Some(Self {
            core: Mutex::new(core),
            image_lock: Mutex::new(()),
            socket,
            state_dir,
            identity,
            env,
        }))
    }

    pub(super) fn serve(&self) -> Result<(), Startup> {
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

    pub(super) fn core(&self) -> Result<std::sync::MutexGuard<'_, Core>, Failure> {
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
            "fork" => {
                // Settle any divergence from a previous daemon before snapshotting a parent.
                self.core()?.converge()?;
                self.dispatch_fork(&request.params, emit)
            }
            "drop" => {
                // Settle any divergence before the ledger row that names the fork is deleted.
                let mut core = self.core()?;
                core.converge()?;
                core.drop_fork(&request.params, emit).map(Some)
            }
            "ping" => {
                let mut reply = self.identity.ping_fields();
                reply["pong"] = json!(true);
                reply["pid"] = json!(std::process::id());
                reply["worlds"] = json!(self.core()?.world_count());
                Ok(Some(reply))
            }
            "ps" => {
                let mut core = self.core()?;
                core.converge()?;
                let reply = core.ps()?;
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
        // Settle any divergence from a previous daemon before deciding this boot.
        self.core()?.converge()?;
        // Decide under the daemon lock, then release it: heal/reconcile run typed world calls
        // that must never block `ps`/`ping`, exactly like the `logs` stream path.
        // The head walks the build context, so it is computed before taking the lock.
        let (project, world, _) = self.up_target(params)?;
        let origin = self.core()?.origin_world(&project);
        let host = { self.core()?.host().clone() };
        let desired = crate::daemon::lease::prepare(&host, &project, &world, origin.as_ref())
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
                let mut session = self.core()?.begin_up(params, planned, None)?;
                session.plan_already_announced();
                session.run(self, emit).map(Some)
            }
            lifecycle::up::UpAction::Boot => {
                let (project, world, no_cache) = self.up_target(params)?;
                let planned = self.preplan(&project, &world, no_cache, emit)?;
                let mut session = self.core()?.begin_up(params, planned, None)?;
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
            "elapsed_ms": elapsed_ms(prepared.started),
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
