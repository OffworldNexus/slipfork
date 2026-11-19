//! Persistent per-world PID 1 (`H-011`).
//!
//! Spawned inside `unshare --user --map-user=$UID --map-group=$GID --keep-caps --mount --net
//! --ipc --pid --fork --kill-child=SIGKILL --mount-proc`, so every world owns a **stable** init
//! for its whole lifetime. The init owns no root filesystem: it is a pure namespace and socket
//! broker for the world's `crun` containers, which each carry their own OCI rootfs.
//!
//! It answers two namespace-scoped capabilities:
//!
//! 1. `netns.sock` — a `0600` pathname `AF_UNIX` socket in the world control directory. A caller
//!    sends `port: u16` (big endian) and receives the connected in-world `127.0.0.1:<port>` stream
//!    as an `SCM_RIGHTS` descriptor (`linux_cow.rs` and `btrfs.rs` are the two audited `unsafe`
//!    modules in the workspace). Pathname `AF_UNIX` sockets live in the filesystem, so they cross
//!    `CLONE_NEWNET` natively.
//! 2. `$BASE_URL` bridge — when the world spec carries a [`slipfork_world::LocalIngressBridge`],
//!    bind `127.0.0.1:<port>` (including `:80`, allowed by in-userns `CAP_NET_BIND_SERVICE` and
//!    `net.ipv4.ip_unprivileged_port_start=0`) and splice every accepted stream to `slipforkd`'s
//!    `0600` `AF_UNIX` HTTP ingress.
//!
//! It also publishes `namespaces.json` — the world-guest `user`/`network`/`ipc` join paths a
//! `crun` bundle names — and reaps every orphan in the world PID namespace. PID 1 of a child PID
//! namespace carries `SIGNAL_UNKILLABLE` (`pid_namespaces(7)`), so clean shutdown is driven by a
//! supervisor lifeline on `stdin`: when the engine closes the write end, this PID 1 observes
//! `EOF` and exits `0`, and the kernel tears the whole namespace down.

#![forbid(unsafe_code)]

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use slipfork_engine::linux_cow::{STATUS_OK, send_fd};
use slipfork_engine::world_init::{WorldInitConfig, WorldInitIngress};
use slipfork_world::PodNamespaces;

/// Environment every helper subprocess gets: a fixed search path, nothing inherited.
const HELPER_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// Bound on the request read and the in-world connect, so one stuck caller cannot wedge the world.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

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
    mount_private_tmpfs(&config.tmpfs_dirs)?;

    let dialer = bind_netns_socket(&config.netns_sock)?;
    let ingress = match &config.local_ingress {
        Some(ingress) => Some((bind_ingress(ingress)?, ingress.clone())),
        None => None,
    };
    publish_namespaces(&config.namespaces_file)?;
    let ready = fs::File::create(config.ready_path())
        .map_err(|err| format!("cannot create world readiness marker: {err}"))?;

    thread::spawn(move || serve_dials(dialer));
    if let Some((bridge, ingress)) = ingress {
        thread::spawn(move || serve_ingress(bridge, ingress.host_ingress_sock));
    }
    // PID 1 owns every orphaned descendant of the world. Reap them so a container process the
    // engine killed actually leaves `/proc` instead of lingering as a zombie.
    thread::spawn(reap_orphans);

    let mut ready = ready;
    ready
        .write_all(b"ready\n")
        .and_then(|()| ready.sync_all())
        .map_err(|err| format!("cannot write world readiness marker: {err}"))?;
    drop(ready);

    wait_for_supervisor_eof();
    Ok(())
}

/// Publish the namespace join paths every `crun` bundle in this world names.
///
/// The init is PID 1 of the world PID namespace, so `/proc/1/ns/*` is its own namespace set and is
/// exactly what a runtime resolving a bundle from inside the world can join.
fn publish_namespaces(path: &Path) -> Result<(), String> {
    let namespaces = PodNamespaces::new(
        Path::new("/proc/1/ns/user"),
        Path::new("/proc/1/ns/net"),
        Path::new("/proc/1/ns/ipc"),
    )
    .map_err(|err| format!("cannot build the world namespace publication: {err}"))?;
    let bytes = serde_json::to_vec(&namespaces)
        .map_err(|err| format!("cannot serialize world namespaces: {err}"))?;
    fs::write(path, bytes)
        .map_err(|err| format!("cannot write world namespaces {}: {err}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|err| format!("cannot chmod {} to 0600: {err}", path.display()))
}

/// Block until the engine closes its supervisor lifeline pipe on `stdin` (`destroy_world`, engine
/// `Drop`, or `slipforkd` exit/crash). Returning from PID 1 makes the kernel terminate every
/// remaining process in the world PID namespace (`pid_namespaces(7)`).
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

/// Reap adopted orphans for the whole lifetime of the world (`pid 1` of a child PID namespace).
fn reap_orphans() {
    loop {
        match slipfork_engine::linux_cow::reap_exited_child_nonblocking() {
            Ok(Some(_)) => {}
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            // `ECHILD` (no children right now, the normal steady state) or another terminal error:
            // back off rather than spinning; an adopted orphan wakes the next probe.
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
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

/// Cover the world's OCI bundle and `crun` state directories with private, memory-only tmpfs.
///
/// `mode=0700,size=16m,nosuid,nodev` keeps every resolved secret inside this world's mount
/// namespace: the host sees empty `0700` mountpoints, and the kernel reclaims both trees when
/// PID 1 exits, so nothing secret ever reaches the on-disk state directory.
fn mount_private_tmpfs(dirs: &[PathBuf]) -> Result<(), String> {
    for dir in dirs {
        helper(
            "mount",
            &[
                "-t".to_owned(),
                "tmpfs".to_owned(),
                "-o".to_owned(),
                "mode=0700,size=16m,nosuid,nodev".to_owned(),
                "tmpfs".to_owned(),
                dir.display().to_string(),
            ],
        )?;
    }
    Ok(())
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
        let Ok(mut stream) = stream else { continue };
        let host_ingress_sock = host_ingress_sock.clone();
        thread::spawn(move || {
            let Ok(mut host) = UnixStream::connect(&host_ingress_sock) else {
                return;
            };
            let _ = slipfork_wire::relay::pipe(&mut stream, &mut host);
        });
    }
}
