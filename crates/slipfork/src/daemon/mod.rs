//! The single per-user daemon: `<runtime_dir>/daemon.sock`, NDJSON control, lifecycles, ingress.
//!
//! One `slipfork` binary is both the CLI and the daemon (`slipfork __daemon`). The daemon owns every
//! world under the invoking UID, relays boot progress as `event` frames, and reaches world pods only
//! through each world's `<pods>/control.sock` `Dial` request. No second socket exists.

mod boot;
mod client;
mod frames;
mod ingress;
mod lifecycle;
mod registry;
mod rpc;
mod wire;

pub use client::{DaemonClient, DaemonError};

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::thread;

use crate::world::HostEnv;
use frames::{FrameReader, write_frame};
use lifecycle::Core;
use serde_json::{Value, json};

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

/// `<runtime_dir>/daemon.sock`.
#[must_use]
pub fn socket_path(host: &HostEnv) -> PathBuf {
    host.runtime_dir.join("daemon.sock")
}

/// Runs the daemon in this process; returns only when startup fails.
pub fn run(host: HostEnv) -> ExitCode {
    match Daemon::start(host) {
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
}

impl Daemon {
    fn start(host: HostEnv) -> Result<Option<Self>, Startup> {
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
        let entries = registry::load(&state_dir).map_err(|error| {
            Startup::new(
                format!(
                    "cannot read `{}`: {error}",
                    registry::path(&state_dir).display()
                ),
                format!("fix or delete `{}`", registry::path(&state_dir).display()),
            )
        })?;
        let core = Core::new(host, state_dir.clone(), lock, entries);
        let retained = core.retained();
        let _ = registry::store(&state_dir, &retained);
        Ok(Some(Self {
            core: Mutex::new(core),
            socket,
            state_dir,
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
                write_frame(&mut writer, &wire::ok(&id, json!({ "stopped": true })))?;
                let _ = writer.flush();
                self.shutdown();
            }
            let outcome = self.dispatch(&request, descriptor, &mut |event| {
                let _ = write_frame(&mut writer, &wire::event(&id, event));
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
            "ping" => {
                let core = self.core()?;
                Ok(Some(json!({
                    "pong": true,
                    "pid": std::process::id(),
                    "worlds": core.world_count(),
                })))
            }
            "ps" => self.core()?.ps().map(Some),
            "down" => self.core()?.down(&request.params, emit).map(Some),
            "restart" => self.core()?.restart(&request.params, emit).map(Some),
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
        let project = PathBuf::from(
            params
                .get("project")
                .and_then(Value::as_str)
                .ok_or_else(|| Failure::new("bad-params", "`project` is required"))?,
        );
        let (key, child, log) = self.core()?.begin_up(params)?;
        boot::finish_boot(self, key, log, child, project, emit).map(Some)
    }

    fn shutdown(&self) -> ! {
        if let Ok(mut core) = self.core.lock() {
            core.stop_all();
            let _ = fs::remove_file(&self.socket);
        }
        std::process::exit(0)
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
