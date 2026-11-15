//! `slipforkd`: the sole stateful Rust Composition Root (`H-010` #1, `ADR-0008` §4–§5).
//!
//! This binary is the only place that resolves `HOME`/`$XDG_DATA_HOME`, the developer's uid/gid,
//! and the sibling `slipfork-world-init` helper, then wires every stateful adapter in-process:
//! `ReflinkNetnsEngine`, `FileVaultProvider`, `ResourcePlan::resolve`, `GitRepoRootResolver` over
//! the one `SlipforkConfig` `ProjectSlugOverride`, `GitWorktreeAdapter`, `WorldRegistry`, and
//! `UnavailableTunnelRegistrar`. It serves the HTTP/WS ingress (`LISTEN_FDS` or `--listen`, plus
//! `0600` `<state_dir>/http-ingress.sock`) and the typed `DaemonControl` socket
//! (`0600` `<runtime_dir>/daemon.sock`).
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
use slipfork::daemon::{DaemonConfig, DaemonService};
use slipfork::layout::{Layout, ensure_private_dir};
use slipfork::project::repository_resolver;
use slipfork::vaults::LayoutProjectVaults;
use slipfork_edge::{
    DaemonControlServer, HttpIngress, ListenConfig, UnavailableTunnelRegistrar, WorldRegistry,
    resolve_listener_set,
};
use slipfork_engine::{InheritedVar, ReflinkNetnsEngine, current_uid_gid};
use slipfork_vcs::GitWorktreeAdapter;

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
    let layout = Layout::resolve(
        Some(cli.state_dir),
        Some(cli.runtime_dir),
        &home,
        None,
        None,
        xdg_data_home.as_deref(),
    );
    ensure_private_dir(layout.state_dir())?;
    ensure_private_dir(layout.runtime_dir())?;
    let state_dir = layout.state_dir().to_path_buf();
    let runtime_dir = layout.runtime_dir().to_path_buf();

    let listeners = resolve_listener_set(listener_config.as_ref())
        .map_err(|err| format!("cannot resolve the ingress listeners: {err}"))?;
    let http_ingress_port = listeners
        .http_port()
        .map_err(|err| format!("cannot read the bound HTTP ingress port: {err}"))?;
    let forward_port = listeners
        .forward_listener
        .as_ref()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port());

    let (uid, gid) = current_uid_gid();
    let engine: Arc<dyn slipfork_engine::WorldEngine> = match cli.world_init {
        Some(program) => Arc::new(ReflinkNetnsEngine::with_world_init_program(
            layout.state_dir(),
            uid,
            gid,
            program,
        )),
        None => Arc::new(ReflinkNetnsEngine::new(layout.state_dir(), uid, gid)),
    };

    let registrar = Arc::new(UnavailableTunnelRegistrar::new());
    let registry = Arc::new(WorldRegistry::new(
        Arc::clone(&engine),
        registrar,
        http_ingress_port,
        forward_port,
    ));
    let repo_roots = repository_resolver();
    let workspaces = Arc::new(GitWorktreeAdapter::new());
    let vaults = Arc::new(LayoutProjectVaults::new(layout.clone()));

    let inherited = InheritedVar::ALL
        .into_iter()
        .filter_map(|var| std::env::var(var.name()).ok().map(|value| (var, value)))
        .collect();
    let service = Arc::new(DaemonService::new(
        slipfork::daemon::DaemonPorts {
            engine,
            registry: Arc::clone(&registry),
            repo_roots,
            workspaces,
            vaults,
        },
        DaemonConfig { layout, inherited },
        http_ingress_port,
        forward_port,
    ));

    let ingress = HttpIngress::bind(&state_dir, listeners, Arc::clone(&registry))
        .map_err(|err| format!("cannot bind the HTTP/WS ingress: {err}"))?;
    let control = DaemonControlServer::bind(&runtime_dir, service)
        .map_err(|err| format!("cannot bind the daemon control socket: {err}"))?;

    serve(ingress, control)
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
