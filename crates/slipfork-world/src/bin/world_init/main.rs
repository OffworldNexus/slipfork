//! Child-PID-namespace PID 1 for one Slipfork world.
//!
//! It is spawned by [`slipfork_world::WorldSupervisor`] as
//! `unshare --user --map-current-user --net --ipc --pid --fork --mount-proc --kill-child=SIGKILL`,
//! so it owns the world's user/net/ipc namespaces and is PID 1 of a fresh PID namespace.
//!
//! Contract:
//! - `O_CLOEXEC` stdin is both the lifeline and the NDJSON control channel; EOF stops every pod,
//!   leaves the namespaces, and exits `0`.
//! - Namespace inode identities are written to `--namespaces <path>`; the host never trusts a PID
//!   before matching `/proc/<pid>/ns/*` against them.
//! - Pods are launched here, the only process holding `CAP_SYS_ADMIN` in the world user namespace.
//!   They inherit `net`/`ipc` and create only `pid` + `mount` + `uts` per pod.
//!
//! Only argv and its own descriptors are read; no environment variable is consulted.

mod control;
mod pod;

use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use slipfork_world::control::{self as wire, InitFrame, PROTOCOL_VERSION, Request};
use slipfork_world::namespaces::NamespaceIdentity;

pub(crate) const REMEDY_ARGS: &str =
    "run pods through `slipfork`; it passes --namespaces, --pods, and --crun explicitly";
pub(crate) const REMEDY_PROTOCOL: &str =
    "re-run with the `slipfork` and `slipfork-world-init` binaries from this build";
pub(crate) const REMEDY_CRUN: &str =
    "install crun (e.g. `apt install crun`) and confirm `crun --version` runs, then retry";
pub(crate) const REMEDY_USERNS: &str = "install util-linux and check that unprivileged user namespaces are enabled (`sysctl kernel.unprivileged_userns_clone=1`); then confirm `unshare --user --map-current-user -- true` succeeds for this user";
pub(crate) const REMEDY_RUNTIME: &str =
    "point `$XDG_RUNTIME_DIR` at a writable tmpfs owned by this user (`slipfork daemon install`)";
pub(crate) const REMEDY_POD: &str = "use a lowercase pod id and absolute in-container paths";

const TICK: Duration = Duration::from_millis(200);

fn main() {
    if let Err(fault) = run() {
        eprintln!("slipfork-world-init: {fault}");
        std::process::exit(1);
    }
}

/// One init failure: a human action line, a concrete remedy, and OS detail kept for `Debug`.
///
/// `Display` is exactly two lines (action + `next:`); the raw `io::Error` lives on `source()`
/// and `Debug` so errno text never reaches the operator without a way forward.
#[derive(Debug)]
pub(crate) struct Fault {
    action: String,
    remedy: &'static str,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
    timeout: bool,
}

impl Fault {
    pub(crate) fn new(action: impl Into<String>, remedy: &'static str) -> Self {
        Self {
            action: action.into(),
            remedy,
            source: None,
            timeout: false,
        }
    }

    pub(crate) fn cause(
        action: impl Into<String>,
        remedy: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            action: action.into(),
            remedy,
            source: Some(Box::new(source)),
            timeout: false,
        }
    }

    /// A deadline was missed after the container was already killed and reaped.
    pub(crate) fn timed_out(action: impl Into<String>, remedy: &'static str) -> Self {
        Self {
            action: action.into(),
            remedy,
            source: None,
            timeout: true,
        }
    }

    pub(crate) fn is_timeout(&self) -> bool {
        self.timeout
    }

    /// Single-line form for the NDJSON reply; `Display` adds the remedy on a second line.
    ///
    /// The underlying OS detail is part of the summary so an `EPERM`/`ENOENT` class is
    /// diagnosable from the boot log instead of hiding behind the generic action.
    pub(crate) fn summary(&self) -> String {
        match self.source.as_deref() {
            Some(source) => format!("{} failed: {source}", self.action),
            None => format!("{} failed", self.action),
        }
    }
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\nnext: {}", self.summary(), self.remedy)
    }
}

impl std::error::Error for Fault {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|error| error as &(dyn std::error::Error + 'static))
    }
}

struct Args {
    namespaces: PathBuf,
    pods: PathBuf,
    crun: PathBuf,
}

fn run() -> Result<(), Fault> {
    let args = parse_args()?;
    // Pods must never inherit the control/lifeline descriptor.
    set_cloexec(libc::STDIN_FILENO)
        .map_err(|source| Fault::cause("stdin O_CLOEXEC setup", REMEDY_PROTOCOL, source))?;
    bring_loopback_up()
        .map_err(|source| Fault::cause("127.0.0.1 loopback setup", REMEDY_USERNS, source))?;
    let identity = NamespaceIdentity::observe()
        .map_err(|source| Fault::cause("world namespace observation", REMEDY_USERNS, source))?;
    identity
        .write(&args.namespaces)
        .map_err(|source| Fault::cause("namespace identity write", REMEDY_RUNTIME, source))?;
    write_frame(
        &InitFrame::Ready {
            v: PROTOCOL_VERSION,
            init_pid: std::process::id(),
            namespaces: identity,
        },
        "ready handshake write",
    )?;
    let pods_root = canonical(&args.pods, "pod runtime root resolution", REMEDY_RUNTIME)?;
    let mut world = pod::InitWorld::new(args.crun, pods_root);
    let (listener, socket_path) = control::bind(world.pods_root())?;
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    loop {
        pod::reap_zombies();
        if control::handle(&listener, &mut world)? {
            break;
        }
        if !poll_stdin(TICK)? {
            continue;
        }
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|source| Fault::cause("control request read", REMEDY_PROTOCOL, source))?;
        if read == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let request = wire::decode_request(line)
            .map_err(|source| Fault::cause("control request decoding", REMEDY_PROTOCOL, source))?;
        let (frame, shutdown) = respond(&mut world, request, &mut |frame| {
            write_frame(&frame, "control reply write")
        })?;
        write_frame(&frame, "control reply write")?;
        if shutdown {
            break;
        }
    }
    // Lifeline EOF or `slipfork down`: stop everything and leave the namespaces cleanly.
    let _ = fs::remove_file(&socket_path);
    world.stop_all();
    write_frame(
        &InitFrame::Stopped {
            v: PROTOCOL_VERSION,
        },
        "stopped handshake write",
    )?;
    Ok(())
}

/// Builds the typed reply for one request; the boolean means "the world must exit".
///
/// `emit` writes intermediate frames (live [`InitFrame::Output`] lines) while a request runs.
pub(crate) fn respond(
    world: &mut pod::InitWorld,
    request: Request,
    emit: &mut dyn FnMut(InitFrame) -> Result<(), Fault>,
) -> Result<(InitFrame, bool), Fault> {
    let (reply, shutdown) = match request {
        Request::Launch { id, pod, .. } => match world.launch(&pod) {
            Ok(pid) => (
                InitFrame::Reply {
                    v: PROTOCOL_VERSION,
                    id,
                    ok: Some(serde_json::json!({ "pid": pid })),
                    error: None,
                },
                false,
            ),
            Err(fault) => {
                eprintln!("slipfork-world-init: {fault}");
                (failure_frame(id, "launch-failed", &fault), false)
            }
        },
        Request::StopAll { id, .. } => {
            world.stop_all();
            (stopped_frame(id), false)
        }
        Request::Shutdown { id, .. } => {
            world.stop_all();
            (stopped_frame(id), true)
        }
        Request::RunOnce {
            id,
            pod,
            timeout_ms,
            ..
        } => {
            let frame_id = id.clone();
            let mut on_line = |stream: slipfork_world::Stream, line: &str| {
                emit(InitFrame::Output {
                    v: PROTOCOL_VERSION,
                    id: frame_id.clone(),
                    stream,
                    line: line.to_owned(),
                })
            };
            match world.run_once_streaming(&pod, timeout_ms, &mut on_line) {
                Ok(code) => (
                    InitFrame::Reply {
                        v: PROTOCOL_VERSION,
                        id,
                        ok: Some(serde_json::json!({ "exit_code": code })),
                        error: None,
                    },
                    false,
                ),
                Err(fault) => {
                    eprintln!("slipfork-world-init: {fault}");
                    let code = if fault.is_timeout() {
                        "run-timeout"
                    } else {
                        "run-failed"
                    };
                    (failure_frame(id, code, &fault), false)
                }
            }
        }
        Request::Dial { id, .. } => (
            failure_frame(
                id,
                "dial-requires-socket",
                &Fault::new(
                    "dial must use the world control socket",
                    "send the dial request to `<pods>/control.sock`, not the supervisor stdin",
                ),
            ),
            false,
        ),
        Request::List { id, .. } => (
            InitFrame::Reply {
                v: PROTOCOL_VERSION,
                id,
                ok: Some(serde_json::json!({ "pods": world.list() })),
                error: None,
            },
            false,
        ),
        Request::Restart { id, service, .. } => match world.restart(service.as_deref()) {
            Ok(restarted) => (
                InitFrame::Reply {
                    v: PROTOCOL_VERSION,
                    id,
                    ok: Some(serde_json::json!({ "restarted": restarted })),
                    error: None,
                },
                false,
            ),
            Err(fault) => {
                eprintln!("slipfork-world-init: {fault}");
                (failure_frame(id, "restart-failed", &fault), false)
            }
        },
        Request::Wait {
            id,
            pid,
            timeout_ms,
            ..
        } => match world.wait(pid, timeout_ms) {
            Ok(Some(code)) => (
                InitFrame::Reply {
                    v: PROTOCOL_VERSION,
                    id,
                    ok: Some(serde_json::json!({ "exit_code": code })),
                    error: None,
                },
                false,
            ),
            Ok(None) => (
                InitFrame::Reply {
                    v: PROTOCOL_VERSION,
                    id,
                    ok: Some(serde_json::json!({ "running": true })),
                    error: None,
                },
                false,
            ),
            Err(fault) => {
                eprintln!("slipfork-world-init: {fault}");
                (failure_frame(id, "wait-failed", &fault), false)
            }
        },
    };
    Ok((reply, shutdown))
}

/// The `{"stopped": true}` reply shared by `StopAll` and `Shutdown`.
pub(crate) fn stopped_frame(id: String) -> InitFrame {
    InitFrame::Reply {
        v: PROTOCOL_VERSION,
        id,
        ok: Some(serde_json::json!({ "stopped": true })),
        error: None,
    }
}

/// A typed failure frame carrying only a one-line summary.
pub(crate) fn failure_frame(id: String, code: &str, fault: &Fault) -> InitFrame {
    InitFrame::Reply {
        v: PROTOCOL_VERSION,
        id,
        ok: None,
        error: Some(slipfork_world::ControlError {
            code: code.to_owned(),
            message: fault.summary(),
        }),
    }
}

fn bring_loopback_up() -> io::Result<()> {
    #[repr(C)]
    struct IfReq {
        name: [libc::c_char; 16],
        flags: libc::c_short,
        pad: [u8; 22],
    }
    // SAFETY: `socket(2)` returns a descriptor or `-1`; no memory is shared.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut request = IfReq {
        name: [0; 16],
        flags: 0,
        pad: [0; 22],
    };
    for (slot, byte) in request.name.iter_mut().zip(b"lo\0".iter()) {
        *slot = *byte as libc::c_char;
    }
    const SIOCGIFFLAGS: libc::c_ulong = 0x8913;
    const SIOCSIFFLAGS: libc::c_ulong = 0x8914;
    // SAFETY: `request` outlives both ioctls and mirrors `struct ifreq`.
    let read = unsafe { libc::ioctl(fd, SIOCGIFFLAGS, &mut request) };
    if read < 0 {
        let error = io::Error::last_os_error();
        // SAFETY: `fd` is an owned socket descriptor.
        unsafe { libc::close(fd) };
        return Err(error);
    }
    request.flags |= libc::IFF_UP as libc::c_short;
    // SAFETY: `request` outlives the ioctl and mirrors `struct ifreq`.
    let written = unsafe { libc::ioctl(fd, SIOCSIFFLAGS, &request) };
    let outcome = if written < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    };
    // SAFETY: `fd` is owned here and closed exactly once.
    unsafe { libc::close(fd) };
    outcome
}

fn poll_stdin(timeout: Duration) -> Result<bool, Fault> {
    let millis = i32::try_from(timeout.as_millis().max(1)).unwrap_or(i32::MAX);
    let mut pollfd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid `pollfd` lives across the call.
    let ret = unsafe { libc::poll(&mut pollfd, 1, millis) };
    if ret < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(Fault::cause("control channel poll", REMEDY_PROTOCOL, error));
    }
    Ok(ret > 0)
}

fn set_cloexec(fd: i32) -> io::Result<()> {
    // SAFETY: `fcntl` only inspects/sets descriptor flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above, with the descriptor we just inspected.
    let ret = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn write_frame(frame: &InitFrame, action: &'static str) -> Result<(), Fault> {
    let bytes =
        wire::encode(frame).map_err(|source| Fault::cause(action, REMEDY_PROTOCOL, source))?;
    let mut stdout = io::stdout();
    stdout
        .write_all(&bytes)
        .and_then(|()| stdout.flush())
        .map_err(|source| Fault::cause(action, REMEDY_PROTOCOL, source))
}

pub(crate) fn canonical(
    path: &Path,
    action: impl Into<String>,
    remedy: &'static str,
) -> Result<PathBuf, Fault> {
    fs::canonicalize(path).map_err(|source| Fault::cause(action, remedy, source))
}

/// This init's argv, read from `/proc/self/cmdline`; the supervisor injects every path.
fn argv() -> Result<Vec<std::ffi::OsString>, Fault> {
    use std::os::unix::ffi::OsStringExt;

    let bytes = fs::read("/proc/self/cmdline")
        .map_err(|source| Fault::cause("argv read", REMEDY_ARGS, source))?;
    Ok(bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .skip(1)
        .map(|part| std::ffi::OsString::from_vec(part.to_vec()))
        .collect())
}

fn parse_args() -> Result<Args, Fault> {
    let mut namespaces = None;
    let mut pods = None;
    let mut crun = None;
    let mut args = argv()?.into_iter();
    while let Some(flag) = args.next() {
        let flag = flag
            .to_str()
            .ok_or_else(|| Fault::new("argv decoding", REMEDY_ARGS))?
            .to_owned();
        let mut value = |flag: &str| {
            args.next()
                .ok_or_else(|| Fault::new(format!("`{flag}` value parsing"), REMEDY_ARGS))
                .map(PathBuf::from)
        };
        match flag.as_str() {
            "--namespaces" => namespaces = Some(value("--namespaces")?),
            "--pods" => pods = Some(value("--pods")?),
            "--crun" => crun = Some(value("--crun")?),
            other => {
                return Err(Fault::new(
                    format!("unknown argument `{other}`"),
                    REMEDY_ARGS,
                ));
            }
        }
    }
    Ok(Args {
        namespaces: namespaces.ok_or_else(|| Fault::new("`--namespaces` parsing", REMEDY_ARGS))?,
        pods: pods.ok_or_else(|| Fault::new("`--pods` parsing", REMEDY_ARGS))?,
        crun: crun.ok_or_else(|| Fault::new("`--crun` parsing", REMEDY_ARGS))?,
    })
}
