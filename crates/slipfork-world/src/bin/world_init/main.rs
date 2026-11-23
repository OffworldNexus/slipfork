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
mod fault;
mod pod;
mod respond;
mod s3;
mod shared;
mod stream;

use std::fs;
use std::io::{self, BufRead, BufReader};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use slipfork_world::control::{self as wire, InitFrame, PROTOCOL_VERSION};
use slipfork_world::namespaces::NamespaceIdentity;

pub(crate) use fault::{
    Fault, REMEDY_ARGS, REMEDY_CRUN, REMEDY_POD, REMEDY_PROTOCOL, REMEDY_RUNTIME, REMEDY_USERNS,
    write_frame,
};
pub(crate) use respond::respond;

const TICK: Duration = Duration::from_millis(200);

fn main() {
    if let Err(fault) = run() {
        eprintln!("slipfork-world-init: {fault}");
        std::process::exit(1);
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
    let pods_root = canonical(&args.pods, "pod runtime root resolution", REMEDY_RUNTIME)?;
    // The single composition root: the world init owns one multi-thread runtime for the native
    // S3 endpoint. `slipfork-world` itself never constructs a runtime.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|source| Fault::cause("native S3 runtime setup", REMEDY_RUNTIME, source))?;
    let mut world = pod::InitWorld::new(args.crun, pods_root, runtime.handle().clone())?;
    // Bind `<pods>/control.sock` BEFORE publishing `Ready` so fast DAG roots (such as native S3
    // `ConfigureS3`, which needs no image pull or subvolume clone) can never race `bind(2)`.
    let (listener, socket_path) = control::bind(world.pods_root())?;
    write_frame(
        &InitFrame::Ready {
            v: PROTOCOL_VERSION,
            init_pid: std::process::id(),
            namespaces: identity,
        },
        "ready handshake write",
    )?;
    let listener_fd = listener.as_raw_fd();
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    loop {
        pod::reap_zombies(&world.shared());
        if control::handle(&listener, &mut world)? {
            break;
        }
        let stdin_ready = reader.buffer().contains(&b'\n') || poll_channels(listener_fd, TICK)?;
        if control::handle(&listener, &mut world)? {
            break;
        }
        if !stdin_ready {
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
    runtime.shutdown_background();
    Ok(())
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
    // SAFETY: `request` outlives both ioctls and mirrors `struct ifreq`. `as _` adapts to the
    // platform `ioctl` request width (`c_ulong` on glibc, `c_int` on musl); both fit 0x8913.
    let read = unsafe { libc::ioctl(fd, SIOCGIFFLAGS as _, &mut request) };
    if read < 0 {
        let error = io::Error::last_os_error();
        // SAFETY: `fd` is an owned socket descriptor.
        unsafe { libc::close(fd) };
        return Err(error);
    }
    request.flags |= libc::IFF_UP as libc::c_short;
    // SAFETY: `request` outlives the ioctl and mirrors `struct ifreq`; `as _` adapts the request
    // width as above.
    let written = unsafe { libc::ioctl(fd, SIOCSIFFLAGS as _, &request) };
    let outcome = if written < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    };
    // SAFETY: `fd` is owned here and closed exactly once.
    unsafe { libc::close(fd) };
    outcome
}

/// Polls both the lifeline `stdin` and `control.sock` at once, waking in 0µs on either channel
/// instead of stalling `Dial`/`ConfigureS3`/`RunOnce` behind a 200ms stdin sleep. Returns `true`
/// when `stdin` is readable (or hit EOF/HUP).
fn poll_channels(listener_fd: i32, timeout: Duration) -> Result<bool, Fault> {
    let millis = i32::try_from(timeout.as_millis().max(1)).unwrap_or(i32::MAX);
    let mut fds = [
        libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: listener_fd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: `fds` holds two valid `pollfd` entries across the call.
    let ret = unsafe { libc::poll(fds.as_mut_ptr(), 2, millis) };
    if ret < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(Fault::cause("control channel poll", REMEDY_PROTOCOL, error));
    }
    Ok(ret > 0 && fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0)
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
