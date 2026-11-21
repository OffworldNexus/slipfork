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

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use slipfork_world::control::{self, InitFrame, PROTOCOL_VERSION, Request};
use slipfork_world::namespaces::NamespaceIdentity;
use slipfork_world::{ControlError, PodSpec};

const REMEDY_ARGS: &str =
    "run pods through `slipfork`; it passes --namespaces, --pods, and --crun explicitly";
const REMEDY_PROTOCOL: &str =
    "re-run with the `slipfork` and `slipfork-world-init` binaries from this build";
const REMEDY_CRUN: &str =
    "install crun (e.g. `apt install crun`) and confirm `crun --version` runs, then retry";
const REMEDY_USERNS: &str = "install util-linux and check that unprivileged user namespaces are enabled (`sysctl kernel.unprivileged_userns_clone=1`); then confirm `unshare --user --map-current-user -- true` succeeds for this user";
const REMEDY_RUNTIME: &str =
    "point `$XDG_RUNTIME_DIR` at a writable tmpfs owned by this user (`slipfork daemon install`)";
const REMEDY_POD: &str = "use a lowercase pod id and absolute in-container paths";

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
struct Fault {
    action: String,
    remedy: &'static str,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl Fault {
    fn new(action: impl Into<String>, remedy: &'static str) -> Self {
        Self {
            action: action.into(),
            remedy,
            source: None,
        }
    }

    fn cause(
        action: impl Into<String>,
        remedy: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            action: action.into(),
            remedy,
            source: Some(Box::new(source)),
        }
    }

    /// Single-line form for the NDJSON reply; `Display` adds the remedy on a second line.
    fn summary(&self) -> String {
        format!("{} failed", self.action)
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
    let mut world = InitWorld {
        crun: args.crun,
        pods_root: canonical(&args.pods, "pod runtime root resolution", REMEDY_RUNTIME)?,
        pods: BTreeMap::new(),
    };
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    loop {
        reap_zombies();
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
        let request = control::decode_request(line)
            .map_err(|source| Fault::cause("control request decoding", REMEDY_PROTOCOL, source))?;
        match request {
            Request::Launch { id, pod, .. } => match world.launch(&pod) {
                Ok(pid) => write_frame(
                    &InitFrame::Reply {
                        v: PROTOCOL_VERSION,
                        id,
                        ok: Some(serde_json::json!({ "pid": pid })),
                        error: None,
                    },
                    "control reply write",
                )?,
                Err(fault) => {
                    eprintln!("slipfork-world-init: {fault}");
                    write_frame(
                        &InitFrame::Reply {
                            v: PROTOCOL_VERSION,
                            id,
                            ok: None,
                            error: Some(ControlError {
                                code: "launch-failed".to_owned(),
                                message: fault.summary(),
                            }),
                        },
                        "control reply write",
                    )?
                }
            },
            Request::StopAll { id, .. } => {
                world.stop_all();
                write_frame(
                    &InitFrame::Reply {
                        v: PROTOCOL_VERSION,
                        id,
                        ok: Some(serde_json::json!({ "stopped": true })),
                        error: None,
                    },
                    "control reply write",
                )?;
            }
        }
    }
    // Lifeline EOF: stop everything and leave the namespaces with a clean exit.
    world.stop_all();
    write_frame(
        &InitFrame::Stopped {
            v: PROTOCOL_VERSION,
        },
        "stopped handshake write",
    )?;
    Ok(())
}

struct InitWorld {
    crun: PathBuf,
    pods_root: PathBuf,
    pods: BTreeMap<String, Container>,
}

struct Container {
    bundle: PathBuf,
    pid: i32,
}

impl InitWorld {
    fn launch(&mut self, pod: &PodSpec) -> Result<i32, Fault> {
        let action = format!("pod `{}` launch with crun", pod.id);
        pod.validate()
            .map_err(|source| Fault::cause(action.clone(), REMEDY_POD, source))?;
        let bundle = self.pods_root.join(&pod.id);
        let bundle = canonical(&bundle, action.clone(), REMEDY_RUNTIME)?;
        if !bundle.starts_with(&self.pods_root) {
            return Err(Fault::new(
                action,
                "keep the pod runtime root on a tmpfs owned by this user",
            ));
        }
        let pid_file = bundle.join("init.pid");
        let _ = fs::remove_file(&pid_file);
        let status = Command::new(&self.crun)
            .args(["run", "--cgroup-manager=disabled", "--detach", "--bundle"])
            .arg(&bundle)
            .arg("--pid-file")
            .arg(&pid_file)
            .arg(&pod.id)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0)
            .status()
            .map_err(|source| Fault::cause(action.clone(), REMEDY_CRUN, source))?;
        if !status.success() {
            return Err(Fault::new(
                format!(
                    "pod `{}` launch with crun (`crun run` exited with {status})",
                    pod.id
                ),
                REMEDY_CRUN,
            ));
        }
        let pid = fs::read_to_string(&pid_file)
            .map_err(|source| Fault::cause(action.clone(), REMEDY_CRUN, source))?
            .trim()
            .parse::<i32>()
            .map_err(|source| Fault::cause(action, REMEDY_CRUN, source))?;
        self.pods.insert(pod.id.clone(), Container { bundle, pid });
        Ok(pid)
    }

    fn stop_all(&mut self) {
        for (id, container) in std::mem::take(&mut self.pods) {
            let _ = Command::new(&self.crun)
                .args(["kill", "--all", &id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = Command::new(&self.crun)
                .args(["delete", "--force", &id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            kill_container(container.pid);
            let _ = fs::remove_dir_all(&container.bundle);
        }
        reap_zombies();
    }
}

/// Kills the container's whole group only when it really leads one.
fn kill_container(pid: i32) {
    if pid <= 0 {
        return;
    }
    // SAFETY: `kill(2)`/`getpgid(2)` never dereference memory.
    unsafe {
        if libc::getpgid(pid) == pid {
            libc::kill(-pid, libc::SIGKILL);
        } else {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

fn reap_zombies() {
    loop {
        let mut status = 0;
        // SAFETY: `waitpid` writes one `c_int` through the valid pointer.
        let reaped = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if reaped <= 0 {
            return;
        }
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

const TICK: Duration = Duration::from_millis(200);

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

fn write_frame(frame: &InitFrame, action: &'static str) -> Result<(), Fault> {
    let bytes =
        control::encode(frame).map_err(|source| Fault::cause(action, REMEDY_PROTOCOL, source))?;
    let mut stdout = io::stdout();
    stdout
        .write_all(&bytes)
        .and_then(|()| stdout.flush())
        .map_err(|source| Fault::cause(action, REMEDY_PROTOCOL, source))
}

fn canonical(
    path: &Path,
    action: impl Into<String>,
    remedy: &'static str,
) -> Result<PathBuf, Fault> {
    fs::canonicalize(path).map_err(|source| Fault::cause(action, remedy, source))
}

fn parse_args() -> Result<Args, Fault> {
    let mut namespaces = None;
    let mut pods = None;
    let mut crun = None;
    let mut args = std::env::args_os().skip(1);
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
