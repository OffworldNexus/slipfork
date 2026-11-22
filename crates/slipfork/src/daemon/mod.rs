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
mod ingress;
mod lifecycle;
mod registry;
mod rpc;
mod view;
mod wire;

pub use client::{DaemonClient, DaemonError};

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::world::HostEnv;
use frames::{FrameReader, write_frame};
use lifecycle::core::Core;
use serde_json::{Value, json};
use slipfork_vcs::VcsWorkspace;
use slipfork_world::control::{FrameLines, InitFrame, StreamFrame};

/// A typed control-plane failure rendered as an `error` frame with a bare message.
#[derive(Debug, Clone)]
struct Failure {
    code: &'static str,
    message: String,
}

impl Failure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
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
    socket: PathBuf,
    state_dir: PathBuf,
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
        let Some(lock) = lock_daemon(&host.runtime_dir).map_err(|error| {
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
            socket,
            state_dir,
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
                let core = self.core()?;
                Ok(Some(json!({
                    "pong": true,
                    "pid": std::process::id(),
                    "worlds": core.world_count(),
                })))
            }
            "ps" => {
                let reply = self.core()?.ps()?;
                let value = serde_json::to_value(reply)
                    .map_err(|error| Failure::new("ps-encode", error.to_string()))?;
                Ok(Some(value))
            }
            "down" => self.core()?.down(&request.params, emit).map(Some),
            "restart" => self.core()?.restart(&request.params, emit).map(Some),
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
        if let Some(reply) = self.core()?.live_up_reply(params) {
            emit(json!({
                "event": "ready",
                "url": reply["url"],
                "world": reply["world"],
                "elapsed_ms": 0,
            }));
            return Ok(Some(reply));
        }
        let mut session = self.core()?.begin_up(params)?;
        session.run(self, emit).map(Some)
    }

    /// Snapshots the fork, then boots it through the same `up` path and answers its coordinates.
    fn dispatch_fork(
        &self,
        params: &Value,
        emit: &mut dyn FnMut(Value),
    ) -> Result<Option<Value>, Failure> {
        let target = self.core()?.inspect_fork(params)?;
        let (raw_name, fork, checkpoint_ms) = target.prepare_outside_locks()?;
        let (mut session, prepared) =
            self.core()?
                .begin_fork(target, &raw_name, fork, checkpoint_ms, emit)?;
        let fork_name = prepared.fork.clone();
        let mut annotate = |mut event: Value| {
            if event.get("event").and_then(Value::as_str) == Some("ready") {
                event["fork"] = Value::String(fork_name.clone());
            }
            emit(event);
        };
        if let Err(failure) = session.run(self, &mut annotate) {
            // A failed branch switch or boot must not leave a half-fork that blocks a retry.
            if let Ok(core) = self.core() {
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
            "checkpoint_ms": prepared.checkpoint_ms,
        })))
    }

    /// Relays one `shell`: a real PTY master for a terminal, or the `exec` pipe path otherwise.
    fn handle_shell(
        &self,
        request: &wire::Request,
        reader: &mut FrameReader,
        writer: &mut UnixStream,
    ) -> Result<Value, Failure> {
        let tty = request
            .params
            .get("tty")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !tty {
            let params = self.core()?.shell_exec_params(&request.params)?;
            let exec = wire::Request {
                v: request.v,
                id: request.id.clone(),
                method: "exec".to_owned(),
                params,
            };
            return self.handle_exec(&exec, reader, writer);
        }
        let lifecycle::stream::PtyLink { mut socket, master } =
            self.core()?.open_pty(&request.params)?;
        slipfork_world::fdpass::send_fd(writer, b"pty", Some(master.as_fd())).map_err(|error| {
            Failure::new("client-gone", format!("send the PTY descriptor: {error}"))
        })?;
        drop(master);
        let resizes = socket.try_clone().map_err(|error| {
            Failure::new(
                "world-unavailable",
                format!("clone the PTY socket: {error}"),
            )
        })?;
        std::thread::scope(|scope| {
            let handle = scope.spawn(move || frames::pump_resizes(reader, resizes));
            let result = (|| -> Result<Value, Failure> {
                let mut lines = FrameLines::new();
                loop {
                    if frames::peer_closed(writer) {
                        // Dropping `socket` below hangs up on the world; its supervisor sees HUP
                        // and reaps the whole pod process group.
                        return Err(Failure::new(
                            "client-gone",
                            "the client closed during the PTY relay",
                        ));
                    }
                    let mut pollfd = libc::pollfd {
                        fd: socket.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: one valid `pollfd` lives across the call; 100ms bounds the wait so
                    // a vanished client is noticed promptly.
                    let ready = unsafe { libc::poll(&mut pollfd, 1, 100) };
                    if ready < 0 {
                        let error = std::io::Error::last_os_error();
                        if error.kind() == std::io::ErrorKind::Interrupted {
                            continue;
                        }
                        return Err(Failure::new(
                            "world-stream",
                            format!("poll the PTY reply: {error}"),
                        ));
                    }
                    if ready == 0 {
                        continue;
                    }
                    let mut chunk = [0_u8; 4096];
                    let read = socket.read(&mut chunk).map_err(|error| {
                        Failure::new("world-stream", format!("read the PTY reply: {error}"))
                    })?;
                    if read == 0 {
                        return Err(Failure::new(
                            "world-stream",
                            "the world closed the PTY control socket",
                        ));
                    }
                    lines.extend(&chunk[..read]);
                    let Some(raw) = lines.next_line() else {
                        continue;
                    };
                    let text = std::str::from_utf8(&raw).map_err(|error| {
                        Failure::new("world-stream", format!("non-UTF-8 PTY reply: {error}"))
                    })?;
                    match slipfork_world::control::decode_init_frame(text.trim()) {
                        Ok(InitFrame::Reply { ok, error, .. }) => {
                            let value = lifecycle::stream::finish(ok, error)?;
                            let code = value.get("exit_code").and_then(Value::as_i64).unwrap_or(-1);
                            return Ok(json!({ "exit": code }));
                        }
                        Ok(_) => {}
                        Err(error) => {
                            return Err(Failure::new("world-stream", error.to_string()));
                        }
                    }
                }
            })();
            // The terminal is over; end the resize reader so the scope can join.
            let _ = writer.shutdown(std::net::Shutdown::Read);
            let _ = handle.join();
            result
        })
    }

    /// Relays one `exec`: world frames go out as `event`s, client `input` frames go in.
    fn handle_exec(
        &self,
        request: &wire::Request,
        reader: &mut FrameReader,
        writer: &mut UnixStream,
    ) -> Result<Value, Failure> {
        let (mut client, stdin) = self.core()?.open_exec(&request.params)?;
        let id = request.id.clone();
        let pump = if stdin {
            Some(
                client
                    .input_writer()
                    .map_err(|error| Failure::new("world-unavailable", error.to_string()))?,
            )
        } else {
            None
        };
        std::thread::scope(|scope| {
            let handle = pump.map(|input| scope.spawn(move || frames::pump_input(reader, input)));
            let result = (|| -> Result<Value, Failure> {
                loop {
                    match client.poll() {
                        Ok(Some(StreamFrame::Output(event))) => {
                            let frame = wire::event(
                                &id,
                                json!({
                                    "kind": event.kind.as_str(),
                                    "pod": event.pod,
                                    "stream": event.stream.as_str(),
                                    "line": event.line,
                                }),
                            );
                            if write_frame(writer, &frame).is_err() {
                                return Err(Failure::new(
                                    "client-gone",
                                    "the client closed during `exec`",
                                ));
                            }
                        }
                        Ok(Some(StreamFrame::Reply { ok, error })) => {
                            let value = lifecycle::stream::finish(ok, error)?;
                            let code = value.get("exit_code").and_then(Value::as_i64).unwrap_or(-1);
                            return Ok(json!({ "exit": code }));
                        }
                        Ok(None) => thread::sleep(std::time::Duration::from_millis(40)),
                        Err(error) => {
                            return Err(Failure::new("world-stream", error.to_string()));
                        }
                    }
                }
            })();
            let _ = writer.shutdown(std::net::Shutdown::Read);
            if let Some(handle) = handle {
                let _ = handle.join();
            }
            result
        })
    }

    fn stop_and_unlink(&self) {
        if let Ok(mut core) = self.core.lock() {
            core.stop_all();
            let _ = fs::remove_file(&self.socket);
        }
    }
}

fn lock_daemon(runtime_dir: &Path) -> io::Result<Option<File>> {
    fs::create_dir_all(runtime_dir)?;
    let path = runtime_dir.join("slipfork-daemon.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    // SAFETY: `flock` only locks the live descriptor; no memory is shared.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if locked == 0 {
        return Ok(Some(file));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(None)
    } else {
        Err(error)
    }
}
