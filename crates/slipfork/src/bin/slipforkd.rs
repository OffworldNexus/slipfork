//! `slipforkd`: the sole stateful Rust Composition Root (`H-010` #1, `ADR-0008` §4–§5).
//!
//! This binary is the only place that resolves `HOME`/`$XDG_DATA_HOME`, the developer's uid/gid,
//! and the sibling `slipfork-world-init` helper, then hands those values to
//! [`slipfork::daemon::compose_daemon`], which wires every stateful adapter in-process: the single
//! `BtrfsEngine` over `Arc::new(KernelBtrfsDriver)` (failing closed with
//! `EngineError::BtrfsRequired` before any world directory exists), `FileVaultProvider` layered
//! over a paired `OsppSecretProvider`, `ResourcePlan::resolve`, `GitRepoRootResolver` over the one
//! `SlipforkConfig` `ProjectSlugOverride`, `RepositoryWorkspaces` (Git or Jujutsu by `.jj`),
//! `DiskSeedDumpStore`, and `WorldRegistry`. It serves the HTTP/WS
//! ingress (`LISTEN_FDS` or `--listen`, plus `0600` `<state_dir>/http-ingress.sock`) and the typed
//! `DaemonControl` socket (`0600` `<runtime_dir>/daemon.sock`).
//!
//! It never guesses `$XDG_RUNTIME_DIR` or the working directory: both roots are explicit
//! `--state-dir`/`--runtime-dir` arguments, and every `DaemonControl` request carries its own
//! explicit `cwd`.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;

use clap::Parser;
use slipfork::browser::Browser;
use slipfork::daemon::{DaemonWiring, compose_daemon, host_caches, resolve_boot_endpoint};
use slipfork::layout::{Layout, ensure_private_dir};
use slipfork::runtime::{runtime_assets, target_arch};
use slipfork_edge::{DaemonControlServer, HttpIngress, ListenConfig, resolve_listener_set};
use slipfork_engine::btrfs::KernelBtrfsDriver;
use slipfork_engine::current_uid_gid;
use slipfork_wire::control::IngressInfo;
use slipfork_world::InheritedVar;

/// Unified unprivileged user daemon.
#[derive(Debug, Parser)]
#[command(
    name = "slipforkd",
    version,
    about = "Slipfork daemon: unprivileged HTTP/WS ingress, CoW worlds, and the daemon control socket"
)]
struct DaemonCli {
    /// Explicit CoW state directory (snapshots, world clones, generated world homes).
    #[arg(long)]
    state_dir: PathBuf,

    /// Explicit runtime directory holding the `0600` `daemon.sock`.
    #[arg(long)]
    runtime_dir: PathBuf,

    /// Bind the HTTP ingress on this address (ignored when `LISTEN_FDS` is present).
    #[arg(long)]
    listen: Option<SocketAddr>,

    /// Also bind the planned static forward listener on `127.0.0.1:<port>`.
    #[arg(long)]
    forward_port: Option<u16>,

    /// Explicit path of the `slipfork-world-init` helper (defaults to the sibling executable).
    #[arg(long)]
    world_init: Option<PathBuf>,

    /// Explicit path of the pinned static `crun` binary staged into every world rootfs
    /// (defaults to `<state_dir>/cache/runtime/<pinned file>`).
    #[arg(long)]
    crun: Option<PathBuf>,

    /// The one OSPP v1 server identity (`wss://…`, or `ws://` strictly on loopback). Omit for a
    /// purely local daemon.
    #[arg(long)]
    ospp_endpoint: Option<String>,
}

fn main() -> ExitCode {
    let cli = DaemonCli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("slipforkd: {message}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: DaemonCli) -> Result<(), String> {
    // Fail closed before any directory is created when the account has no home directory.
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            "HOME is not set; slipforkd stores the 0600 vault under $XDG_DATA_HOME/slipfork or \
             $HOME/.local/share/slipfork — set HOME to the developer account's home directory"
                .to_owned()
        })?;
    let xdg_data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    let listener_config = listen_config(&cli);
    // Resolve the two explicit roots once, on disk and absolute: the Btrfs engine never resolves
    // a path against the ambient `current_dir()`.
    ensure_private_dir(&cli.state_dir)?;
    ensure_private_dir(&cli.runtime_dir)?;
    let state_dir = std::fs::canonicalize(&cli.state_dir).map_err(|err| {
        format!(
            "cannot resolve --state-dir {}: {err}",
            cli.state_dir.display()
        )
    })?;
    let runtime_dir = std::fs::canonicalize(&cli.runtime_dir).map_err(|err| {
        format!(
            "cannot resolve --runtime-dir {}: {err}",
            cli.runtime_dir.display()
        )
    })?;
    let layout = Layout::explicit(state_dir, runtime_dir, &home, xdg_data_home.as_deref());
    let state_dir = layout.state_dir().to_path_buf();
    let runtime_dir = layout.runtime_dir().to_path_buf();
    // A saved `slipfork auth` pairing reconnects this daemon after a restart; an explicit
    // `--ospp-endpoint` always wins. A corrupt record only warns: OSPP is never a prerequisite.
    let boot = resolve_boot_endpoint(cli.ospp_endpoint.as_deref(), &layout.pairing_path())?;
    if let Some(warning) = &boot.warning {
        eprintln!("slipforkd: {warning}");
    }

    let listeners = resolve_listener_set(listener_config.as_ref())
        .map_err(|err| format!("cannot resolve the ingress listeners: {err}"))?;
    let ingress = IngressInfo::new(
        listeners
            .http_addrs()
            .map_err(|err| format!("cannot read the bound HTTP ingress addresses: {err}"))?,
    )
    .map_err(|err| format!("cannot describe the bound HTTP ingress: {err}"))?;
    let forward_port = listeners
        .forward_listener
        .as_ref()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port());

    let (uid, gid) = current_uid_gid();
    let runtime = runtime_assets(layout.state_dir(), cli.crun.clone(), cli.world_init.clone())
        .map_err(|err| format!("cannot prepare the L0 runtime payloads: {err}"))?;
    let target_arch =
        target_arch().map_err(|err| format!("cannot prepare the L0 world rootfs: {err}"))?;
    let inherited = InheritedVar::ALL
        .into_iter()
        .filter_map(|var| std::env::var(var.name()).ok().map(|value| (var, value)))
        .collect();

    // The one daemon graph: `compose_daemon` owns every adapter wiring, so `slipforkd` and the
    // integration harness can never drift into two composition paths. The launcher is resolved
    // here once and injected as a value: pairing hands the live handle to it immediately.
    let graph = compose_daemon(DaemonWiring {
        layout,
        driver: KernelBtrfsDriver::new(),
        browser: resolve_browser(),
        ingress,
        forward_port,
        host_caches: host_caches(&home),
        inherited,
        runtime,
        target_arch: target_arch.to_owned(),
        uid,
        gid,
        ospp_endpoint: boot.endpoint,
        hostname: std::env::var("HOSTNAME")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "slipfork".to_owned()),
    })?;

    let ingress = HttpIngress::bind(&state_dir, listeners, Arc::clone(&graph.registry))
        .map_err(|err| format!("cannot bind the HTTP/WS ingress: {err}"))?;
    let control = DaemonControlServer::bind(&runtime_dir, graph.service)
        .map_err(|err| format!("cannot bind the daemon control socket: {err}"))?;

    serve(ingress, control)
}

/// The daemon's one desktop-launch resolution: `PATH` and the desktop-session flags read here
/// and nowhere else; the injected [`Browser`] value is the only host fact the daemon sees.
fn resolve_browser() -> Browser {
    Browser::resolve(
        std::env::var_os("PATH").as_deref(),
        ["DISPLAY", "WAYLAND_DISPLAY"]
            .into_iter()
            .any(|var| std::env::var_os(var).is_some_and(|value| !value.is_empty())),
    )
}

/// Run both daemon servers until one of them stops serving.
fn serve(ingress: HttpIngress, control: DaemonControlServer) -> Result<(), String> {
    let (failures, failure) = std::sync::mpsc::channel::<Result<(), String>>();
    {
        let failures = failures.clone();
        thread::spawn(move || {
            let outcome = control.serve().map_err(|err| err.to_string());
            let _ = failures.send(outcome);
        });
    }
    {
        let failures = failures.clone();
        thread::spawn(move || {
            let outcome = ingress.serve().map_err(|err| err.to_string());
            let _ = failures.send(outcome);
        });
    }
    drop(failures);
    match failure.recv() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(message)) => Err(message),
        Err(_) => Err("every daemon server stopped unexpectedly".to_owned()),
    }
}

/// Build the explicit `--listen` request only when the operator passed one.
fn listen_config(cli: &DaemonCli) -> Option<ListenConfig> {
    cli.listen.map(|addr| ListenConfig {
        addr,
        forward_port: cli.forward_port,
    })
}
