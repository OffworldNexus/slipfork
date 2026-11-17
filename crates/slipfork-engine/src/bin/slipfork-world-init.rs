//! Persistent per-world PID 1 (`H-011`).
//!
//! Spawned by [`slipfork_engine::WorldEngine::clone_world`] on
//! `BtrfsEngine` inside
//! `unshare --user --map-user=$UID --map-group=$GID --keep-caps --mount --net --ipc --pid --fork
//! --kill-child=SIGKILL --mount-proc`,
//! so every world owns a **stable** `init_pid` for its whole lifetime. A daemon thread of
//! `slipforkd` can never `setns(CLONE_NEWNET)` into an unprivileged descendant user namespace
//! (`EPERM`) and `setns(CLONE_NEWUSER)` is `EINVAL` in a multithreaded process, so the world's own
//! PID 1 must answer both namespace-scoped capabilities:
//!
//! 1. `netns.sock` — a `0600` pathname `AF_UNIX` socket inside the world's control directory. A
//!    caller sends `port: u16` (big endian) and receives the connected in-world
//!    `127.0.0.1:<port>` stream as an `SCM_RIGHTS` descriptor (`linux_cow.rs` is the only `unsafe`
//!    module in the workspace). Pathname `AF_UNIX` sockets live in the filesystem, so they cross
//!    `CLONE_NEWNET` natively.
//! 2. `$BASE_URL` bridge — when the world spec carries a [`slipfork_engine::LocalIngressBridge`],
//!    bind `127.0.0.1:<port>` (including `:80`, allowed by in-userns `CAP_NET_BIND_SERVICE` and
//!    `net.ipv4.ip_unprivileged_port_start=0`) and splice every accepted stream to `slipforkd`'s
//!    `0600` `AF_UNIX` HTTP ingress.
//!
//! `--ipc` (`CLONE_NEWIPC`) gives the world a private System V shared-memory and POSIX
//! message-queue namespace, so parallel `postgres` containers never collide on `shmget` key
//! `90000` or port-derived keys (`ADR-0010` §6); every `nsenter` entry joins it with `--ipc`.
//!
//! The process also establishes the world's bind mounts once, so `nsenter --target <init_pid>`
//! entry, exec, and services all observe the same mount/net/ipc namespace. PID 1 of a child PID
//! namespace carries `SIGNAL_UNKILLABLE` (`pid_namespaces(7)`), so clean shutdown is driven by a
//! supervisor lifeline pipe on `stdin`: when `BtrfsEngine::destroy_world`, `Drop`, or `slipforkd`
//! exit closes the write end, this PID 1 observes `EOF` and exits `0`, causing the kernel to tear
//! down the entire PID namespace atomically without leaving orphaned `unshare` supervisors.

#![forbid(unsafe_code)]

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use slipfork_engine::linux_cow::{STATUS_OK, send_fd};
use slipfork_engine::world_init::{
    WorldInitConfig, WorldInitIngress, WorldInitMount, WorldInitRootfs,
};

/// Environment every helper subprocess gets: a fixed search path, nothing inherited (`PATH` is
/// needed to resolve `ip`/`mount`/`pivot_root`; no world or secret variable is ever exposed).
const HELPER_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// Bound on the request read and the in-world connect, so one stuck caller cannot wedge the world.
const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// Guest mount points every world provides regardless of the app rootfs.
const STANDARD_MOUNTPOINTS: [&str; 10] = [
    "proc",
    "dev",
    "dev/pts",
    "dev/shm",
    "sys",
    "tmp",
    "var/tmp",
    "run",
    ".old_root",
    ".slipfork",
];
/// Read-write `tmpfs` paths (`ADR-0009` §3.1 writable-path table).
const TMPFS_MOUNTPOINTS: [(&str, &str); 3] = [
    ("tmp", "mode=1777"),
    ("var/tmp", "mode=1777"),
    ("run", "mode=755"),
];

fn main() {
    let Some(config_path) = env::args_os().nth(1) else {
        eprintln!("slipfork-world-init: usage: slipfork-world-init <config.json>");
        std::process::exit(2);
    };
    if let Err(err) = run(Path::new(&config_path)) {
        eprintln!("slipfork-world-init: {err}");
        std::process::exit(1);
    }
}

fn run(config_path: &Path) -> Result<(), String> {
    let raw = fs::read_to_string(config_path)
        .map_err(|err| format!("cannot read {}: {err}", config_path.display()))?;
    let config: WorldInitConfig =
        serde_json::from_str(&raw).map_err(|err| format!("invalid world init config: {err}"))?;

    bring_up_loopback()?;
    allow_unprivileged_ports()?;

    // Both listeners must exist before `pivot_root`: a bound `AF_UNIX` socket survives the root
    // change even though its host pathname no longer resolves. The readiness marker is created
    // empty (the engine waits for `ready\n`) and written through this handle after the pivot.
    let dialer = bind_netns_socket(&config.netns_sock)?;
    let ingress = match &config.local_ingress {
        Some(ingress) => Some((bind_ingress(ingress)?, ingress.clone())),
        None => None,
    };
    let ready = fs::File::create(config.ready_path())
        .map_err(|err| format!("cannot create world readiness marker: {err}"))?;

    prepare_rootfs(&config.rootfs, &config.mounts)?;

    thread::spawn(move || serve_dials(dialer));
    if let Some((bridge, ingress)) = ingress {
        thread::spawn(move || serve_ingress(bridge, ingress.guest_ingress_sock));
    }

    let mut ready = ready;
    ready
        .write_all(b"ready\n")
        .and_then(|()| ready.sync_all())
        .map_err(|err| format!("cannot write world readiness marker: {err}"))?;
    drop(ready);

    wait_for_supervisor_eof();
    Ok(())
}

/// Block until the engine closes its supervisor lifeline pipe on `stdin` (`destroy_world`, engine
/// `Drop`, or `slipforkd` exit/crash). Returning from PID 1 makes the kernel terminate every
/// remaining process in the world PID namespace (`pid_namespaces(7)`), and the outer `unshare`
/// supervisor reaps PID 1 and exits `0`.
fn wait_for_supervisor_eof() {
    let mut stdin = std::io::stdin().lock();
    let mut scratch = [0_u8; 64];
    loop {
        match stdin.read(&mut scratch) {
            Ok(0) => return,
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// Merge the read-only `L0` rootfs with the world's mount-point tree and `pivot_root` into it.
///
/// Two lowerdirs and **zero** upperdir/workdir are deliberate: `/usr`, `/lib`, and `/etc` are
/// genuinely `EROFS`, so no world can pollute the shared cache layer (`ADR-0009` §3.1). Every
/// bind mount is established against `<merged>/<guest>` before the root change.
fn prepare_rootfs(rootfs: &WorldInitRootfs, mounts: &[WorldInitMount]) -> Result<(), String> {
    for guest in STANDARD_MOUNTPOINTS {
        create_dir(&rootfs.mountpoints.join(guest))?;
    }
    for mount in mounts {
        let target = rootfs.mountpoints.join(relative_guest(&mount.guest_path)?);
        let parent = target
            .parent()
            .ok_or_else(|| format!("mount '{}' has no parent", target.display()))?;
        create_dir(parent)?;
        if fs::metadata(&mount.host_path)
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false)
        {
            create_dir(&target)?;
        } else {
            fs::File::create(&target)
                .map_err(|err| format!("cannot create mount target {}: {err}", target.display()))?;
        }
    }

    helper("mount", &["--make-rprivate".to_owned(), "/".to_owned()])?;
    let lower = format!(
        "userxattr,lowerdir={}:{}",
        rootfs.mountpoints.display(),
        rootfs.app_rootfs.display()
    );
    helper(
        "mount",
        &[
            "-t".to_owned(),
            "overlay".to_owned(),
            "overlay".to_owned(),
            "-o".to_owned(),
            lower,
            rootfs.merged.display().to_string(),
        ],
    )?;

    let merged = |guest: &str| rootfs.merged.join(guest);
    helper(
        "mount",
        &[
            "--rbind".to_owned(),
            "/dev".to_owned(),
            merged("dev").display().to_string(),
        ],
    )?;
    helper(
        "mount",
        &[
            "--rbind".to_owned(),
            "/sys".to_owned(),
            merged("sys").display().to_string(),
        ],
    )?;
    helper(
        "mount",
        &[
            "-t".to_owned(),
            "proc".to_owned(),
            "proc".to_owned(),
            merged("proc").display().to_string(),
        ],
    )?;
    for (guest, mode) in TMPFS_MOUNTPOINTS {
        helper(
            "mount",
            &[
                "-t".to_owned(),
                "tmpfs".to_owned(),
                "-o".to_owned(),
                mode.to_owned(),
                "tmpfs".to_owned(),
                merged(guest).display().to_string(),
            ],
        )?;
    }
    helper(
        "mount",
        &[
            "-t".to_owned(),
            "tmpfs".to_owned(),
            "-o".to_owned(),
            "nosuid,noexec,nodev,mode=1777,size=65536k".to_owned(),
            "tmpfs".to_owned(),
            merged("dev/shm").display().to_string(),
        ],
    )?;

    for mount in mounts {
        let target = merged(&relative_guest(&mount.guest_path)?.display().to_string());
        helper(
            "mount",
            &[
                "--bind".to_owned(),
                mount.host_path.display().to_string(),
                target.display().to_string(),
            ],
        )?;
        if mount.read_only {
            // `remount,ro` on a bind mount of a directory reaches XFS as an unsupported
            // `idmapped` reconfigure; `remount,bind,ro` is the verified form (§7.2).
            helper(
                "mount",
                &[
                    "-o".to_owned(),
                    "remount,bind,ro".to_owned(),
                    target.display().to_string(),
                ],
            )?;
        }
    }

    helper(
        "pivot_root",
        &[
            rootfs.merged.display().to_string(),
            rootfs.merged.join(".old_root").display().to_string(),
        ],
    )?;
    helper("umount", &["-l".to_owned(), "/.old_root".to_owned()])?;
    Ok(())
}

/// Strip the leading `/` from a guest path and reject anything that could escape the merged root.
fn relative_guest(guest: &Path) -> Result<PathBuf, String> {
    let relative = guest
        .strip_prefix("/")
        .map_err(|_| format!("guest path '{}' must be absolute", guest.display()))?;
    if relative.as_os_str().is_empty()
        || relative.components().any(|component| {
            !matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        return Err(format!(
            "guest path '{}' must be a plain path below the merged root",
            guest.display()
        ));
    }
    Ok(relative.to_path_buf())
}

fn create_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|err| format!("cannot create mount point {}: {err}", path.display()))
}

fn helper(program: &str, args: &[String]) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", HELPER_PATH)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| format!("cannot run `{program}`: {err}"))?;
    if status.status.success() {
        return Ok(());
    }
    Err(format!(
        "`{program} {}` failed with {}: {}",
        args.join(" "),
        status.status,
        String::from_utf8_lossy(&status.stderr).trim()
    ))
}

fn bring_up_loopback() -> Result<(), String> {
    helper(
        "ip",
        &[
            "link".to_owned(),
            "set".to_owned(),
            "lo".to_owned(),
            "up".to_owned(),
        ],
    )
}

fn allow_unprivileged_ports() -> Result<(), String> {
    fs::write("/proc/sys/net/ipv4/ip_unprivileged_port_start", "0\n")
        .map_err(|err| format!("cannot lower ip_unprivileged_port_start in the world netns: {err}"))
}

fn bind_netns_socket(path: &Path) -> Result<UnixListener, String> {
    let listener = UnixListener::bind(path)
        .map_err(|err| format!("cannot bind world dial socket {}: {err}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|err| format!("cannot chmod {} to 0600: {err}", path.display()))?;
    Ok(listener)
}

fn bind_ingress(ingress: &WorldInitIngress) -> Result<TcpListener, String> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, ingress.port));
    TcpListener::bind(addr)
        .map_err(|err| format!("cannot bind {addr} inside the world netns: {err}"))
}

/// Answer `dial_world_tcp` requests: read `port: u16`, connect in-world, hand the stream back.
fn serve_dials(listener: UnixListener) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        thread::spawn(move || answer_dial(stream));
    }
}

fn answer_dial(mut stream: UnixStream) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let mut port_bytes = [0_u8; 2];
    if stream.read_exact(&mut port_bytes).is_err() {
        return;
    }
    let port = u16::from_be_bytes(port_bytes);
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    match TcpStream::connect_timeout(&addr, IO_TIMEOUT) {
        Ok(upstream) => {
            let _ = send_fd(&stream, STATUS_OK, Some(upstream.as_fd()));
        }
        Err(err) => {
            let errno =
                u8::try_from(err.raw_os_error().unwrap_or(libc::ECONNREFUSED)).unwrap_or(255);
            let _ = send_fd(&stream, errno, None);
        }
    }
}

/// Splice every `127.0.0.1:<port>` connection inside the world to `slipforkd`'s AF_UNIX ingress.
fn serve_ingress(listener: TcpListener, host_ingress_sock: PathBuf) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let host_ingress_sock = host_ingress_sock.clone();
        thread::spawn(move || {
            let Ok(host) = UnixStream::connect(&host_ingress_sock) else {
                return;
            };
            splice(stream, host);
        });
    }
}

/// Bidirectional copy with half-close propagation in both directions.
fn splice(mut client: TcpStream, mut host: UnixStream) {
    let (Ok(client_reader), Ok(host_writer)) = (client.try_clone(), host.try_clone()) else {
        return;
    };
    let uplink = thread::spawn(move || {
        let mut reader = client_reader;
        let mut writer = host_writer;
        forward(&mut reader, &mut writer);
        let _ = writer.shutdown(Shutdown::Write);
    });
    forward(&mut host, &mut client);
    let _ = client.shutdown(Shutdown::Write);
    let _ = uplink.join();
}

fn forward<R: Read, W: Write>(reader: &mut R, writer: &mut W) {
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                if writer.write_all(&buffer[..read]).is_err() || writer.flush().is_err() {
                    return;
                }
            }
        }
    }
}
