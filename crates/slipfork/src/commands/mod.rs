//! Command bodies for the thin `slipfork` client.
//!
//! Every verb receives the already-composed [`CliApp`] (resolved layout, output mode, and host
//! facts) and returns the process exit code; nothing here reads `HOME`, `$XDG_*`, `SHELL`, `PATH`, or
//! `SUDO_USER`, so the composition root stays the only place that binds ambient host state.

pub(crate) mod auth;
pub(crate) mod boot;
pub(crate) mod control;
pub(crate) mod daemon;
pub(crate) mod doctor;
pub(crate) mod forward;
pub(crate) mod inspect;
pub(crate) mod process;
pub(crate) mod secret;

use std::process::ExitCode;

use slipfork::cli::{Commands, DaemonAction, HookAction};

use crate::CliApp;

/// Route one parsed verb to its command module.
pub(crate) fn dispatch(app: &CliApp, command: Option<Commands>) -> Result<ExitCode, String> {
    match command {
        // Bare `slipfork` is the status dashboard: the exact same handler as `slipfork status`
        // (`docs/design/dx-0.1.0-ux.md` §3), never a second render path.
        None | Some(Commands::Status) => inspect::status(app),
        Some(Commands::Doctor) => doctor::doctor(app),
        Some(Commands::Daemon { action }) => match action {
            DaemonAction::Unit {
                only,
                forward_port,
                user,
                bin_dir,
            } => daemon::unit(app, only, forward_port, user, bin_dir),
            DaemonAction::Install {
                user,
                bin_dir,
                forward_port,
            } => daemon::install(app, user, bin_dir, forward_port),
        },
        Some(Commands::Up {
            world,
            dump,
            resources,
            host_caches,
        }) => boot::up(app, world, dump, resources, host_caches),
        Some(Commands::Down { world }) => boot::down(app, world),
        Some(Commands::Restart { world }) => boot::restart(app, world),
        Some(Commands::Ps { world }) => inspect::ps(app, world),
        Some(Commands::Open { world, remote }) => inspect::open(app, world, remote),
        Some(Commands::Fork {
            fork,
            base,
            dump,
            resources,
            host_caches,
        }) => boot::fork(app, fork, base, dump, resources, host_caches),
        Some(Commands::Drop { fork }) => boot::drop_fork(app, fork),
        Some(Commands::List) => inspect::list(app),
        Some(Commands::Prune { dry_run, yes }) => inspect::prune(app, dry_run, yes),
        Some(Commands::Secret { action }) => secret::secret(app, action),
        Some(Commands::Exec { world, cwd, argv }) => process::exec(app, world, cwd, argv),
        Some(Commands::Shell { world }) => process::shell(app, world),
        Some(Commands::Logs {
            world,
            target,
            follow,
            stderr,
            lines,
        }) => process::logs(app, world, target, follow, stderr, lines),
        Some(Commands::Hook { action }) => match action {
            HookAction::Run {
                hook,
                world,
                no_tty,
            } => process::hook_run(app, hook, world, no_tty),
        },
        Some(Commands::Db { db_id, world }) => process::db(app, db_id, world),
        Some(Commands::Redis { cluster_id, world }) => process::redis(app, cluster_id, world),
        Some(Commands::Forward { world, targets }) => forward::forward(app, world, targets),
        Some(Commands::Auth { endpoint }) => auth::login(app, endpoint),
    }
}

/// Render one `--json` payload, propagating a serializer failure instead of printing nothing.
pub(crate) fn render_json<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string_pretty(value)
        .map_err(|err| format!("cannot render the JSON response: {err}"))
}
