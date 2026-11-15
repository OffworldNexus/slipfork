//! Persistent per-world PID 1 (`H-011`).
//!
//! Spawned by [`slipfork_engine::WorldEngine::clone_world`] on
//! `ReflinkNetnsEngine` inside
//! `unshare --map-user=$UID --map-group=$GID --keep-caps --mount --net --pid --fork --mount-proc`,
//! so every world owns a **stable** `init_pid` for its whole lifetime. A daemon thread of
//! `slipforkd` can never `setns(CLONE_NEWNET)` into an unprivileged descendant user namespace
//! (`EPERM`) and `setns(CLONE_NEWUSER)` is `EINVAL` in a multithreaded process, so the world's own
//! PID 1 must answer both namespace-scoped capabilities:
//!
//! 1. `netns.sock` — a `0600` pathname `AF_UNIX` socket inside the world directory. A caller sends
//!    `port: u16` (big endian) and receives the connected in-world `127.0.0.1:<port>` stream as an
//!    `SCM_RIGHTS` descriptor (`linux_cow.rs` is the only `unsafe` module in the workspace).
//!    Pathname `AF_UNIX` sockets live in the filesystem, so they cross `CLONE_NEWNET` natively.
//! 2. `$BASE_URL` bridge — when the world spec carries a [`slipfork_engine::LocalIngressBridge`],
//!    bind `127.0.0.1:<port>` (including `:80`, allowed by in-userns `CAP_NET_BIND_SERVICE` and
//!    `net.ipv4.ip_unprivileged_port_start=0`) and splice every accepted stream to `slipforkd`'s
//!    `0600` `AF_UNIX` HTTP ingress.
//!
//! The process also establishes the world's bind mounts once, so `nsenter --target <init_pid>`
//! entry, exec, and services all observe the same mount/net namespace. `SIGKILL` to this PID 1
//! tears the whole PID namespace down atomically; `ReflinkNetnsEngine::destroy_world` reaps it.

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
use slipfork_engine::world_init::{WorldInitConfig, WorldInitIngress, WorldInitMount};

/// Environment every helper subprocess gets: a fixed search path, nothing inherited (`PATH` is
/// needed to resolve `ip`/`mount`; no world or secret variable is ever exposed to them).
const HELPER_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
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
    bind_mounts(&config.mounts)?;

    let dialer = bind_netns_socket(&config.netns_sock)?;
    thread::spawn(move || serve_dials(dialer));

    if let Some(ingress) = config.local_ingress.clone() {
        let bridge = bind_ingress(&ingress)?;
        thread::spawn(move || serve_ingress(bridge, ingress.host_ingress_sock));
    }

    fs::write(config.ready_path(), b"ready\n")
        .map_err(|err| format!("cannot write world readiness marker: {err}"))?;

    loop {
        thread::park();
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

/// Establish every `WorldSpec` mount once, so later `nsenter` entries share them.
fn bind_mounts(mounts: &[WorldInitMount]) -> Result<(), String> {
    for mount in mounts {
        helper(
            "mount",
            &[
                "--bind".to_owned(),
                mount.host_path.display().to_string(),
                mount.guest_path.display().to_string(),
            ],
        )?;
        if mount.read_only {
            helper(
                "mount",
                &[
                    "-o".to_owned(),
                    "remount,bind,ro".to_owned(),
                    mount.guest_path.display().to_string(),
                ],
            )?;
        }
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
