//! `slipfork`: the thin CLI client (`H-010` #1, `ADR-0008` §4).
//!
//! The CLI holds **no** `WorldEngine` or `SecretProvider` for world operations: it
//! parses `clap` arguments, resolves `HOME`/`$XDG_*`/`--state-dir` once, and drives `slipforkd`
//! through the typed `<state_dir>/run/daemon.sock` [`DaemonControlClient`]. Only `up` and `fork`
//! auto-spawn `slipforkd --listen 127.0.0.1:0` in the background; every read/diagnostic verb fails
//! closed with one actionable line when nothing answers (`ADR-0011` §2–§3).
//!
//! `snow resolve` runs through `slipforkd` (`DaemonControl::snow_resolve`), so the daemon owns the
//! `0600` local vault and the in-memory OSPP environment. Only `doctor` stays CLI-local: it reads
//! host capabilities without materializing a world.

#![forbid(unsafe_code)]

use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

use clap::Parser;
use serde_json::json;
use slipfork::apparmor::render_userns_profile;
use slipfork::browser::{APPROVAL_HINT_PREFIX, Browser, NO_BROWSER_REMEDY};
use slipfork::cli::{
    Cli, Commands, DaemonAction, DumpArgs, HookAction, ResourceArgs, SecretAction, SnowAction,
    UnitKind,
};
use slipfork::daemon_unit_install::{
    self, APPARMOR_DIR, APPARMOR_PARSER, DAEMON_GRACE, DAEMON_HANDOVER, DAEMON_POLL,
    InstallRequest, SYSTEM_UNIT_DIR, SYSTEMCTL, UnitInstaller, ingress_unit_spec,
};
use slipfork::doctor::{
    self, HostCheckOutcome, HostFacts, NetnsCheck, OsHostProbe, SystemdIngressProbe,
};
use slipfork::layout::{Layout, absolute_path, ensure_private_dir};
use slipfork::project::repository_resolver;
use slipfork::user::{UserIdentity, account_name, kernel_hostname};
use slipfork_edge::{
    BootEvent, DaemonControl, DaemonControlClient, DropForkRequest, ExecTargetRequest, ForkRequest,
    ForwardRequest, ForwardSummary, HookRunRequest, LogsRequest, SnowResolveRequest, UpRequest,
    WorldSelector, render_service_unit, render_socket_unit,
};
use slipfork_engine::btrfs::KernelBtrfsDriver;
use slipfork_engine::current_uid_gid;
use slipfork_snow::SnowFile;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    match run(cli) {
        Ok(code) => code,
        Err(message) => {
            if json {
                println!("{}", json!({ "ok": false, "error": message }));
            } else {
                eprintln!("slipfork: {message}");
            }
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    // `HOME` is a fail-closed precondition: nothing below may create a directory, spawn a daemon,
    // or open a socket when the developer account has no home directory.
    let home = home_dir()?;
    let xdg_cache = absolute_path(std::env::var_os("XDG_CACHE_HOME"));
    let xdg_data = absolute_path(std::env::var_os("XDG_DATA_HOME"));
    let explicit_state_dir = cli.state_dir.clone();
    let layout = Layout::resolve(
        explicit_state_dir.clone(),
        &home,
        xdg_cache.as_deref(),
        xdg_data.as_deref(),
    );
    let json = cli.json;
    let verbose = cli.verbose;

    match cli.command {
        // Bare `slipfork` is the status dashboard: the exact same handler as `slipfork status`
        // (`docs/design/dx-0.1.0-ux.md` §3), never a second render path.
        None | Some(Commands::Status) => status(json, &layout),
        Some(Commands::Doctor) => doctor(json, &layout),
        Some(Commands::Snow {
            action:
                SnowAction::Resolve {
                    file,
                    mode,
                    component,
                    expose_secrets,
                },
        }) => resolve_snow(
            json,
            &layout,
            &file,
            &mode,
            component.as_deref(),
            expose_secrets,
        ),
        Some(Commands::Daemon { action }) => match action {
            DaemonAction::Unit {
                only,
                forward_port,
                user,
                bin_dir,
            } => daemon_unit(
                json,
                ClientRoots {
                    state_dir: explicit_state_dir,
                    xdg_cache_home: xdg_cache,
                    xdg_data_home: xdg_data,
                },
                only,
                forward_port,
                user,
                bin_dir,
            ),
            DaemonAction::Install {
                user,
                bin_dir,
                forward_port,
            } => daemon_install(
                json,
                ClientRoots {
                    state_dir: explicit_state_dir,
                    xdg_cache_home: xdg_cache,
                    xdg_data_home: xdg_data,
                },
                user,
                bin_dir,
                forward_port,
            ),
        },
        Some(Commands::Up {
            world,
            dump,
            resources,
            host_caches,
        }) => up(json, verbose, &layout, world, dump, resources, host_caches),
        Some(Commands::Down { world }) => down(json, &layout, world),
        Some(Commands::Restart { world }) => restart(json, verbose, &layout, world),
        Some(Commands::Ps { world }) => ps(json, &layout, world),
        Some(Commands::Open { world, remote }) => open(json, &layout, world, remote),
        Some(Commands::Fork {
            fork,
            base,
            dump,
            resources,
            host_caches,
        }) => create_fork(
            ForkOptions {
                json,
                verbose,
                fork,
                base,
                dump,
                resources,
                host_caches,
            },
            &layout,
        ),
        Some(Commands::Drop { fork }) => drop_fork(json, &layout, fork),
        Some(Commands::List) => list(json, &layout),
        Some(Commands::Prune { dry_run, yes }) => prune(json, &layout, dry_run, yes),
        Some(Commands::Secret { action }) => secret(json, &layout, action),
        Some(Commands::Exec { world, cwd, argv }) => exec(json, &layout, world, cwd, argv),
        Some(Commands::Shell { world }) => shell(json, &layout, world),
        Some(Commands::Logs {
            world,
            target,
            follow,
            stderr,
            lines,
        }) => logs(json, &layout, world, target, follow, stderr, lines),
        Some(Commands::Hook {
            action:
                HookAction::Run {
                    hook,
                    world,
                    no_tty,
                },
        }) => hook_run(json, &layout, hook, world, no_tty),
        Some(Commands::Db { db_id, world }) => db(json, &layout, db_id, world),
        Some(Commands::Redis { cluster_id, world }) => redis(json, &layout, cluster_id, world),
        Some(Commands::Forward { world, targets }) => forward(json, &layout, world, targets),
        Some(Commands::Auth { endpoint }) => auth_login(json, &layout, endpoint),
    }
}

/// `HOME` is required before any side effect.
fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            "HOME is not set; slipfork stores CoW state, the 0600 vault, and its runtime socket \
             under the developer account's home directory — set HOME and retry"
                .to_owned()
        })
}

fn current_dir() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|err| format!("cannot read the current directory: {err}"))
}

/// Render one `--json` payload, propagating a serializer failure instead of printing nothing.
fn render_json<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string_pretty(value)
        .map_err(|err| format!("cannot render the JSON response: {err}"))
}

/// `slipfork` / `slipfork status`: one `DaemonControl::status` read rendered by
/// [`slipfork::status`] (`docs/design/dx-0.1.0-ux.md` §3).
///
/// This is a read verb: it dials the daemon and never starts one, so a bare `slipfork` on a host
/// with no daemon fails closed with the one `slipfork up` line. When the daemon answers on an
/// ingress port other than `:80`, exactly one actionable line goes to stderr — never to stdout and
/// never under `--json` (§2 rule 4).
fn status(json: bool, layout: &Layout) -> Result<ExitCode, String> {
    let client = connect(layout)?;
    let payload = client.status().map_err(|err| err.to_string())?;
    if json {
        println!("{}", render_json(&slipfork::status::json(&payload))?);
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(hint) = slipfork::status::ingress_hint(payload.ingress.port()) {
        eprintln!("{hint}");
    }
    print!("{}", slipfork::status::render(&payload));
    Ok(ExitCode::SUCCESS)
}

/// `slipfork doctor`: probe the Btrfs substrate and the unprivileged namespaces without a daemon.
///
/// The CLI is the only place that constructs and observes the production probes
/// (`KernelBtrfsDriver` + `OsHostProbe` + `SystemdIngressProbe`) and then hands their plain
/// [`HostFacts`] to [`doctor::run`], which spawns nothing and holds no `WorldEngine`; this works
/// on a host where `slipforkd` refuses to boot. The report renders `state_dir`, `filesystem`,
/// `btrfs_super_magic: ok|mismatch`, `subvolume_ioctl: ok|skipped|failed`, advisory `noatime`,
/// `unshare_userns`, `netns`, and the deterministic `fixes` list (`btrfs` -> `unshare_userns` ->
/// `netns`); `--json` projects the same fields, and the exit status is 0 iff the report is `ok`.
fn doctor(json: bool, layout: &Layout) -> Result<ExitCode, String> {
    let (uid, gid) = current_uid_gid();
    let driver = KernelBtrfsDriver::new();
    let host = OsHostProbe::new(exe_dir()?);
    if !json && std::io::stdout().is_terminal() {
        eprintln!("probing namespaces and :80 ingress…");
    }
    let btrfs = driver
        .probe(layout.state_dir())
        .map_err(|err| err.to_string())?;
    let unshare_userns = host.probe_userns(uid, gid);
    let netns = if unshare_userns == HostCheckOutcome::Ok {
        NetnsCheck::Executed(host.probe_netns(uid, gid))
    } else {
        NetnsCheck::Skipped
    };
    let facts = HostFacts {
        unshare_userns,
        netns,
        ingress: SystemdIngressProbe.probe_ingress(),
    };
    let report = doctor::run(btrfs, facts, uid, gid);
    if json {
        println!("{}", render_json(&report.to_json())?);
    } else {
        print!("{}", report.render_text());
    }
    Ok(if report.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn resolve_snow(
    json: bool,
    layout: &Layout,
    file: &Path,
    mode: &str,
    component: Option<&str>,
    expose_secrets: bool,
) -> Result<ExitCode, String> {
    let client = connect(layout)?;
    let summary = client
        .snow_resolve(&SnowResolveRequest {
            cwd: current_dir()?,
            file: file.to_path_buf(),
            mode: mode.to_owned(),
            component: component.map(str::to_owned),
            expose_secrets,
        })
        .map_err(|err| err.to_string())?;
    if json {
        println!("{}", render_json(&summary.vars)?);
    } else {
        for (key, value) in &summary.vars {
            println!("{key}={value}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The client's already-resolved root overrides, shared by `daemon unit` and `daemon install`.
struct ClientRoots {
    state_dir: Option<PathBuf>,
    xdg_cache_home: Option<PathBuf>,
    xdg_data_home: Option<PathBuf>,
}

fn daemon_unit(
    json: bool,
    roots: ClientRoots,
    only: Option<UnitKind>,
    forward_port: u16,
    user: Option<String>,
    bin_dir: Vec<PathBuf>,
) -> Result<ExitCode, String> {
    let identity = UserIdentity::resolve()?;
    let (uid, _) = current_uid_gid();
    let invoking = invoking_account();
    // One account derivation shared with `daemon install`, so `--user` reproduces an install.
    let account = identity.target_account(uid == 0, user.as_deref(), invoking.as_deref())?;
    let layout = Layout::for_account(
        &account,
        roots.state_dir,
        roots.xdg_cache_home.as_deref(),
        roots.xdg_data_home.as_deref(),
    );
    let spec = ingress_unit_spec(&account, &layout, daemon_executable()?, forward_port)?;
    let bin_dirs = bin_dirs_or_executable_dir(bin_dir)?;
    let socket = || render_socket_unit(&spec).map_err(|err| err.to_string());
    let service = || render_service_unit(&spec).map_err(|err| err.to_string());
    let apparmor = || render_userns_profile(&bin_dirs);
    let (text, wants_socket, wants_service, wants_apparmor) = match only {
        Some(UnitKind::Socket) => (socket()?, true, false, false),
        Some(UnitKind::Service) => (service()?, false, true, false),
        Some(UnitKind::Apparmor) => (apparmor()?, false, false, true),
        None => (
            format!("{}\n{}\n{}", socket()?, service()?, apparmor()?),
            true,
            true,
            true,
        ),
    };
    if json {
        println!(
            "{}",
            json!({
                "ok": true,
                "socket": if wants_socket { serde_json::Value::String(socket()?) } else { serde_json::Value::Null },
                "service": if wants_service { serde_json::Value::String(service()?) } else { serde_json::Value::Null },
                "apparmor": if wants_apparmor { serde_json::Value::String(apparmor()?) } else { serde_json::Value::Null },
            })
        );
    } else {
        print!("{text}");
    }
    Ok(ExitCode::SUCCESS)
}

/// The one `SUDO_USER` read in the workspace (`ADR-0011` §1): a bare `sudo slipfork daemon
/// install` installs for the operator's login account instead of `root`, and nothing else ever
/// reads the variable.
fn invoking_account() -> Option<String> {
    std::env::var_os("SUDO_USER")
        .map(|value| value.to_string_lossy().trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// `slipfork daemon install`: write `/etc/systemd/system/slipforkd.{socket,service}` plus the
/// AppArmor profile, then `daemon-reload`, `apparmor_parser -r`, and `enable --now`.
///
/// The composition root constructs the concrete [`UnitInstaller`] over the real system unit
/// directory; the orchestration itself fails closed before any effect when the process is not root.
fn daemon_install(
    json: bool,
    roots: ClientRoots,
    user: Option<String>,
    bin_dir: Vec<PathBuf>,
    forward_port: u16,
) -> Result<ExitCode, String> {
    let (uid, _) = current_uid_gid();
    // The root gate runs before any account-database read, so a non-root host always hears the
    // sudo remedy instead of an identity-probe failure.
    if uid != 0 {
        return Err(daemon_unit_install::non_root_remedy(&account_name()?));
    }
    let identity = UserIdentity::resolve()?;
    let request = InstallRequest {
        is_root: true,
        explicit_user: user,
        invoking_user: invoking_account(),
        executable: daemon_executable()?,
        bin_dirs: bin_dirs_or_executable_dir(bin_dir)?,
        forward_port,
        state_dir: roots.state_dir,
        xdg_cache_home: roots.xdg_cache_home,
        xdg_data_home: roots.xdg_data_home,
    };
    let installer = UnitInstaller::new(
        PathBuf::from(SYSTEM_UNIT_DIR),
        PathBuf::from(APPARMOR_DIR),
        PathBuf::from(SYSTEMCTL),
        PathBuf::from(APPARMOR_PARSER),
    );
    let report = daemon_unit_install::install(&identity, &installer, DAEMON_HANDOVER, &request)?;
    if json {
        println!(
            "{}",
            json!({
                "ok": true,
                "user": report.user,
                "socket": report.socket.as_str(),
                "service": report.service.as_str(),
                "apparmor": report.apparmor.as_str(),
                "paths": {
                    "socket": report.paths.socket,
                    "service": report.paths.service,
                    "apparmor": report.paths.apparmor,
                },
            })
        );
    } else {
        print!("{}", report.render_text());
    }
    Ok(ExitCode::SUCCESS)
}

/// The repeatable `--bin-dir` list, defaulting to this executable's directory.
fn bin_dirs_or_executable_dir(bin_dir: Vec<PathBuf>) -> Result<Vec<PathBuf>, String> {
    if bin_dir.is_empty() {
        Ok(vec![exe_dir()?])
    } else {
        Ok(bin_dir)
    }
}

fn up(
    json: bool,
    verbose: bool,
    layout: &Layout,
    world: Option<String>,
    dump: DumpArgs,
    resources: ResourceArgs,
    host_caches: bool,
) -> Result<ExitCode, String> {
    let client = connect_or_spawn(layout)?;
    let options = block_options(verbose, world.is_some());
    let request = UpRequest {
        cwd: current_dir()?,
        world,
        dump: dump.request(),
        resources: resources.request(),
        host_caches,
    };
    let mut failure = None;
    match client.up(&request, &mut |event| {
        note_hook_failure(&event, &mut failure);
        print_event(json, event);
    }) {
        Ok(summary) => {
            print_world_up(
                json,
                &client,
                slipfork::restart::Verb::Up,
                options,
                &summary,
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => fail_with_rescue(json, failure.as_ref(), err),
    }
}

/// `slipfork restart`: one `DaemonControl::restart` round trip, never `down` + `up` composed here.
///
/// The verb refuses to spawn a daemon (only `up`/`fork` may, `ADR-0011` §3): a restart of a world
/// the daemon never booted fails closed in one line naming `slipfork up`. A `failed` world is the
/// rescue path — the daemon re-runs the same boot pipeline in place.
fn restart(
    json: bool,
    verbose: bool,
    layout: &Layout,
    world: Option<String>,
) -> Result<ExitCode, String> {
    let client = connect(layout)?;
    let options = block_options(verbose, world.is_some());
    let selector = WorldSelector {
        cwd: current_dir()?,
        world,
    };
    let mut failure = None;
    match client.restart(&selector, &mut |event| {
        note_hook_failure(&event, &mut failure);
        print_event(json, event);
    }) {
        Ok(summary) => {
            print_world_up(
                json,
                &client,
                slipfork::restart::Verb::Restart,
                options,
                &summary,
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => fail_with_rescue(json, failure.as_ref(), err),
    }
}

/// `slipfork ps`: one `DaemonControl::world_processes` read rendered by [`slipfork::ps`].
///
/// The world's `state`, its lineage, and every service row come from that single read — never a
/// second `status` call — and the CLI reads no `/proc`, `/sys`, or log directory. A stopped or
/// failed world still renders its rows and exits `0`; only an unknown world fails closed.
fn ps(json: bool, layout: &Layout, world: Option<String>) -> Result<ExitCode, String> {
    let client = connect(layout)?;
    let report = client
        .world_processes(&WorldSelector {
            cwd: current_dir()?,
            world,
        })
        .map_err(|err| err.to_string())?;
    if json {
        println!("{}", render_json(&slipfork::ps::json(&report))?);
    } else {
        print!("{}", slipfork::ps::render(&report));
    }
    Ok(ExitCode::SUCCESS)
}

/// `slipfork open [--remote]`: hand the world's typed origin to the one concrete [`Browser`], or
/// print the verified SSH recipe for a laptop (`docs/design/dx-0.1.0-ux.md` §6).
///
/// The world and its origin are the daemon's facts (`DaemonStatus.worlds`), the remote recipe's
/// port is `DaemonStatus.ingress.port` (never a literal `80`), and the account/host come from the
/// composition root's resolved [`UserIdentity`] and the `uname -n` probe — never
/// `$USER`/`$HOSTNAME`.
/// A non-`:80` ingress announces §2's one stderr line (never under `--json`).
fn open(
    json: bool,
    layout: &Layout,
    world: Option<String>,
    remote: bool,
) -> Result<ExitCode, String> {
    let client = connect(layout)?;
    let status = client.status().map_err(|err| err.to_string())?;
    // Only the cwd-selected form needs a checkout; an explicit `--world` is a fleet-wide lookup
    // over the daemon's own world ids.
    let repo = match world {
        Some(_) => None,
        None => Some(repo_root(&current_dir()?)?),
    };
    let selection = slipfork::open::resolve(&status.worlds, repo.as_ref(), world.as_deref())?;
    if !json && let Some(hint) = slipfork::status::ingress_hint(status.ingress.port()) {
        eprintln!("{hint}");
    }
    if remote {
        let identity = UserIdentity::resolve()?;
        let recipe = slipfork::open::remote_recipe(
            &selection.origin,
            status.ingress.port(),
            &identity.account,
            &kernel_hostname(),
        )?;
        if json {
            println!(
                "{}",
                render_json(&slipfork::open::json(&selection, Some(&recipe)))?
            );
        } else {
            print!("{}", slipfork::open::render_remote(&selection, &recipe));
        }
        return Ok(ExitCode::SUCCESS);
    }
    let browser = resolve_browser();
    match browser.open(selection.origin.url()) {
        Ok(true) => {}
        Ok(false) => {
            if !json {
                eprintln!("{APPROVAL_HINT_PREFIX}{}", selection.origin.url());
            }
            return Err(NO_BROWSER_REMEDY.to_owned());
        }
        Err(err) => return Err(err),
    }
    if json {
        println!("{}", render_json(&slipfork::open::json(&selection, None))?);
    } else {
        print!("{}", slipfork::open::render_local(&selection));
    }
    Ok(ExitCode::SUCCESS)
}

/// The one desktop-launch resolution: `DISPLAY`/`WAYLAND_DISPLAY` and `PATH` read here and
/// nowhere else in the CLI; the value handed to [`Browser`] is the only host fact it sees.
fn resolve_browser() -> Browser {
    Browser::resolve(
        std::env::var_os("PATH").as_deref(),
        ["DISPLAY", "WAYLAND_DISPLAY"]
            .into_iter()
            .any(|var| std::env::var_os(var).is_some_and(|value| !value.is_empty())),
    )
}

fn down(json: bool, layout: &Layout, world: Option<String>) -> Result<ExitCode, String> {
    let client = connect(layout)?;
    let selector = WorldSelector {
        cwd: current_dir()?,
        world,
    };
    let released = client.down(&selector).map_err(|err| err.to_string())?;
    if json {
        println!("{}", json!({ "ok": true, "released": released }));
    } else if released {
        println!("[down] world stopped (worktree kept)");
    } else {
        println!("[down] no running world matched; nothing to do");
    }
    Ok(ExitCode::SUCCESS)
}

/// The full `slipfork fork` surface, bundled so the entry point stays a two-argument contract.
struct ForkOptions {
    json: bool,
    verbose: bool,
    /// `None` lets `slipforkd` derive the name from the current branch (`§7.1`); the client never
    /// runs `git` and never reads `.git`.
    fork: Option<String>,
    base: Option<String>,
    dump: DumpArgs,
    resources: ResourceArgs,
    host_caches: bool,
}

fn create_fork(options: ForkOptions, layout: &Layout) -> Result<ExitCode, String> {
    let ForkOptions {
        json,
        verbose,
        fork,
        base,
        dump,
        resources,
        host_caches,
    } = options;
    let client = connect_or_spawn(layout)?;
    let options = block_options(verbose, true);
    let request = ForkRequest {
        cwd: current_dir()?,
        fork,
        base,
        dump: dump.request(),
        resources: resources.request(),
        host_caches,
    };
    let mut failure = None;
    match client.fork(&request, &mut |event| {
        note_hook_failure(&event, &mut failure);
        print_event(json, event);
    }) {
        Ok(summary) => {
            print_world_up(
                json,
                &client,
                slipfork::restart::Verb::Fork,
                options,
                &summary,
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => fail_with_rescue(json, failure.as_ref(), err),
    }
}

fn drop_fork(json: bool, layout: &Layout, fork: String) -> Result<ExitCode, String> {
    let client = connect(layout)?;
    let request = DropForkRequest {
        cwd: current_dir()?,
        fork,
    };
    let released = client.drop_fork(&request).map_err(|err| err.to_string())?;
    if json {
        println!("{}", json!({ "ok": true, "dropped": released }));
    } else if released {
        println!("[drop] fork world and worktree removed");
    } else {
        println!("[drop] no such fork world; nothing to do");
    }
    Ok(ExitCode::SUCCESS)
}

fn list(json: bool, layout: &Layout) -> Result<ExitCode, String> {
    let client = connect(layout)?;
    let worlds = client.list(None).map_err(|err| err.to_string())?;
    if json {
        let rows: Vec<serde_json::Value> = worlds
            .iter()
            .map(|world| {
                json!({
                    "world": &world.world,
                    "state": world.state,
                    "origin": world.origin.url(),
                    "worktree": &world.worktree,
                })
            })
            .collect();
        println!("{}", render_json(&rows)?);
    } else {
        let column = slipfork::status::world_column(&worlds);
        println!(
            "{:<column$} {:<9} {:<40} WORKTREE",
            "WORLD", "STATE", "ORIGIN"
        );
        for world in &worlds {
            print!("{}", slipfork::status::world_row(world, "", column));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn prune(json: bool, layout: &Layout, dry_run: bool, yes: bool) -> Result<ExitCode, String> {
    let client = connect(layout)?;
    let dry_run = dry_run || !yes;
    let report = client.prune(dry_run).map_err(|err| err.to_string())?;
    if json {
        println!("{}", render_json(&report)?);
    } else {
        let prefix = if report.dry_run {
            "[dry-run]"
        } else {
            "[prune]"
        };
        for world in &report.worlds {
            println!(
                "{prefix} {} world {world}",
                if report.dry_run {
                    "would reap"
                } else {
                    "reaped"
                }
            );
        }
        for snapshot in &report.snapshots {
            println!("{prefix} snapshot {snapshot}");
        }
        if report.worlds.is_empty() && report.snapshots.is_empty() {
            println!("{prefix} nothing to reap");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn secret(json: bool, layout: &Layout, action: SecretAction) -> Result<ExitCode, String> {
    let client = connect(layout)?;
    let cwd = current_dir()?;
    match action {
        SecretAction::Set { key, force } => {
            let value = read_secret()?;
            client
                .secret_set(&cwd, &key, &value, force)
                .map_err(|err| err.to_string())?;
            if json {
                println!("{}", json!({ "ok": true, "key": key }));
            } else {
                println!("[secret] stored secrets::{key} (value never printed)");
            }
        }
        SecretAction::Unset { key } => {
            let removed = client
                .secret_unset(&cwd, &key)
                .map_err(|err| err.to_string())?;
            if json {
                println!("{}", json!({ "ok": true, "key": key, "removed": removed }));
            } else if removed {
                println!("[secret] removed secrets::{key}");
            } else {
                println!("[secret] secrets::{key} was not stored; nothing to remove");
            }
        }
        SecretAction::List => {
            let keys = client.secret_list(&cwd).map_err(|err| err.to_string())?;
            if json {
                println!("{}", render_json(&keys)?);
            } else {
                for key in keys {
                    println!("{key}");
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn read_secret() -> Result<String, String> {
    let mut stdin = io::stdin();
    if stdin.is_terminal() {
        return Err(
            "refusing to read a secret from an interactive terminal (it would be echoed); \
             pipe the value instead: printf '%s' \"$VALUE\" | slipfork secret set KEY"
                .to_owned(),
        );
    }
    let mut value = String::new();
    stdin
        .read_to_string(&mut value)
        .map_err(|err| format!("cannot read the secret from stdin: {err}"))?;
    while value.ends_with('\n') || value.ends_with('\r') {
        value.pop();
    }
    if value.is_empty() {
        return Err("the secret read from stdin is empty".to_owned());
    }
    Ok(value)
}

fn exec(
    json: bool,
    layout: &Layout,
    world: Option<String>,
    cwd: Option<PathBuf>,
    argv: Vec<String>,
) -> Result<ExitCode, String> {
    let client = connect(layout)?;
    let cwd = match cwd {
        Some(cwd) => cwd,
        None => current_dir()?,
    };
    let target = client
        .exec_target(&ExecTargetRequest {
            cwd,
            world,
            service: None,
            argv: argv.clone(),
        })
        .map_err(|err| err.to_string())?;
    let status = spawn_entry(&target)?;
    if json {
        println!(
            "{}",
            json!({ "ok": status.success(), "world": target.world, "exit_code": status.code() })
        );
    }
    Ok(exit_code(&status))
}

/// Enter a world (or a backing-service container) with native terminal stdio.
fn spawn_entry(
    target: &slipfork_edge::WorldExecTarget,
) -> Result<std::process::ExitStatus, String> {
    // `crun exec` takes the command directly after the container id and does not strip a
    // trailing `--` separator, so drop it before appending the interactive argv.
    let mut args_prefix = target.entry.args_prefix.clone();
    if args_prefix.last().map(String::as_str) == Some("--") {
        args_prefix.pop();
    }
    let mut command = Command::new(&target.entry.program);
    command
        .args(&args_prefix)
        .args(&target.target_argv)
        .current_dir(&target.entry.cwd)
        .env_clear()
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    for (name, value) in target.env.iter() {
        command.env(name, value.as_ref());
    }
    command
        .status()
        .map_err(|err| format!("cannot enter world '{}': {err}", target.world))
}

fn exit_code(status: &std::process::ExitStatus) -> ExitCode {
    ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1))
}

/// Fail closed when an interactive world entry has no terminal (`ADR-0008` §4).
fn require_tty(command: &str, hint: &str) -> Result<(), String> {
    if io::stdin().is_terminal() {
        return Ok(());
    }
    Err(format!(
        "`slipfork {command}` needs an interactive terminal; {hint}"
    ))
}

fn shell(json: bool, layout: &Layout, world: Option<String>) -> Result<ExitCode, String> {
    require_tty(
        "shell",
        "use `slipfork exec -- <cmd>` for non-interactive commands",
    )?;
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|shell| !shell.trim().is_empty())
        .ok_or_else(|| {
            "SHELL is not set; `slipfork shell` enters the world with the developer's shell — set SHELL (for example /bin/sh) and retry".to_owned()
        })?;
    let client = connect(layout)?;
    let target = client
        .exec_target(&ExecTargetRequest {
            cwd: current_dir()?,
            world,
            service: None,
            argv: vec![shell],
        })
        .map_err(|err| err.to_string())?;
    // One orientation line before the TTY is handed over, and never under `--json`: the world
    // rootfs is the app stage, not the host, so `ps`/`curl` do not exist inside it.
    if !json {
        eprintln!("world rootfs is the app stage (no host tools; use slipfork ps / logs / open)");
    }
    let status = spawn_entry(&target)?;
    if json {
        println!(
            "{}",
            json!({ "ok": status.success(), "world": target.world, "exit_code": status.code() })
        );
    }
    Ok(exit_code(&status))
}

fn logs(
    json: bool,
    layout: &Layout,
    world: Option<String>,
    target: Option<String>,
    follow: bool,
    stderr: bool,
    lines: Option<usize>,
) -> Result<ExitCode, String> {
    let client = connect(layout)?;
    let request = LogsRequest {
        cwd: current_dir()?,
        world,
        target,
        follow,
        stderr,
        lines,
    };
    // A fan-out request streams every retained source, so each line must name its own source; one
    // explicitly named target is printed byte-exact (`docs/design/dx-0.1.0-ux.md` §8.3).
    let mut renderer = slipfork::logs::Renderer::new(
        json,
        request.target.is_none(),
        Box::new(io::stdout()),
        Box::new(io::stderr()),
    );
    client
        .logs(&request, &mut |log| renderer.chunk(log))
        .map_err(|err| err.to_string())?;
    renderer.finish();
    Ok(ExitCode::SUCCESS)
}

fn hook_run(
    json: bool,
    layout: &Layout,
    hook: String,
    world: Option<String>,
    no_tty: bool,
) -> Result<ExitCode, String> {
    let client = connect(layout)?;
    let request = HookRunRequest {
        cwd: current_dir()?,
        world,
        hook,
    };
    if no_tty {
        let mut stdout = io::stdout();
        let mut stderr_stream = io::stderr();
        let code = client
            .hook_run(&request, &mut |chunk| {
                if json {
                    return;
                }
                let stream: &mut dyn Write = match chunk.source {
                    slipfork_engine::StreamSource::Stdout => &mut stdout,
                    slipfork_engine::StreamSource::Stderr => &mut stderr_stream,
                };
                let _ = stream.write_all(&chunk.bytes);
                let _ = stream.flush();
            })
            .map_err(|err| err.to_string())?;
        if json {
            println!("{}", json!({ "ok": code == 0, "exit_code": code }));
        }
        return Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)));
    }

    require_tty(
        &format!("hook run {}", request.hook),
        "pass --no-tty (or use `slipfork exec`) in non-interactive shells",
    )?;
    let target = client
        .hook_exec_target(&request)
        .map_err(|err| err.to_string())?;
    let status = spawn_entry(&target)?;
    if json {
        println!(
            "{}",
            json!({ "ok": status.success(), "world": target.world, "exit_code": status.code() })
        );
    }
    Ok(exit_code(&status))
}

fn db(
    json: bool,
    layout: &Layout,
    db_id: Option<String>,
    world: Option<String>,
) -> Result<ExitCode, String> {
    let cwd = current_dir()?;
    let service = match db_id {
        Some(id) => format!("pg_databases.{id}"),
        None => default_resource_id(&cwd, world.as_deref(), ResourceKind::PgDatabase)?,
    };
    service_entry(json, layout, cwd, world, service, vec!["psql".to_owned()])
}

fn redis(
    json: bool,
    layout: &Layout,
    cluster_id: Option<String>,
    world: Option<String>,
) -> Result<ExitCode, String> {
    let cwd = current_dir()?;
    let service = match cluster_id {
        Some(id) => format!("redis_clusters.{id}"),
        None => default_resource_id(&cwd, world.as_deref(), ResourceKind::RedisCluster)?,
    };
    service_entry(
        json,
        layout,
        cwd,
        world,
        service,
        vec!["redis-cli".to_owned()],
    )
}

fn service_entry(
    json: bool,
    layout: &Layout,
    cwd: PathBuf,
    world: Option<String>,
    service: String,
    argv: Vec<String>,
) -> Result<ExitCode, String> {
    let client = connect(layout)?;
    // Ask the daemon first: a stopped service must surface `ServiceNotRunning` even when the
    // caller has no TTY (the service refusal is the primary diagnostic).
    let target = client
        .exec_target(&ExecTargetRequest {
            cwd,
            world,
            service: Some(service.clone()),
            argv,
        })
        .map_err(|err| err.to_string())?;
    require_tty(
        &format!("{service} client"),
        "`slipfork db` / `slipfork redis` need an interactive terminal",
    )?;
    let status = spawn_entry(&target)?;
    if json {
        println!(
            "{}",
            json!({
                "ok": status.success(),
                "world": target.world,
                "service": service,
                "exit_code": status.code(),
            })
        );
    }
    Ok(exit_code(&status))
}

/// The resource kinds `slipfork db` / `slipfork redis` infer when no id is passed.
enum ResourceKind {
    PgDatabase,
    RedisCluster,
}

/// Infer the single declared resource id so `--world` alone remains sufficient on one-service worlds.
fn default_resource_id(
    cwd: &Path,
    world: Option<&str>,
    kind: ResourceKind,
) -> Result<String, String> {
    let repo = repository_resolver()
        .resolve(cwd)
        .map_err(|err| err.to_string())?;
    let worktree = match world {
        Some(fork) => repo
            .vcs_root
            .join(slipfork_vcs::WORKTREES_DIRECTORY)
            .join(fork),
        None => repo.worktree_root.clone(),
    };
    let path = worktree.join("snow.yml");
    let raw = fs::read_to_string(&path)
        .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    let snow = SnowFile::parse_yaml(&raw).map_err(|err| format!("{}: {err}", path.display()))?;
    let (prefix, ids) = match kind {
        ResourceKind::PgDatabase => (
            "pg_databases",
            snow.resources
                .pg_databases
                .iter()
                .map(|db| db.id.clone())
                .collect::<Vec<_>>(),
        ),
        ResourceKind::RedisCluster => (
            "redis_clusters",
            snow.resources
                .redis_clusters
                .iter()
                .map(|cluster| cluster.id.clone())
                .collect::<Vec<_>>(),
        ),
    };
    match ids.as_slice() {
        [id] => Ok(format!("{prefix}.{id}")),
        [] => Err(format!(
            "snow.yml declares no {prefix} entry; add one before using this client"
        )),
        _ => Err(format!(
            "snow.yml declares several {prefix} entries ({}); pass the id explicitly",
            ids.join(", ")
        )),
    }
}

fn forward(
    json: bool,
    layout: &Layout,
    world: Option<String>,
    targets: Vec<String>,
) -> Result<ExitCode, String> {
    let client = connect(layout)?;
    let request = ForwardRequest {
        cwd: current_dir()?,
        world,
        targets,
    };
    client
        .forward(&request, &mut |summary: ForwardSummary| {
            print_forward(json, &summary);
            // The open control connection *is* the bridge lifetime (`ADR-0008` §4): hold it until
            // the process is interrupted, at which point the socket closes and `slipforkd` drops
            // every host listener.
            loop {
                std::thread::park();
            }
        })
        .map_err(|err| err.to_string())?;
    Ok(ExitCode::SUCCESS)
}

fn print_forward(json: bool, summary: &ForwardSummary) {
    if json {
        println!(
            "{}",
            json!({
                "world": &summary.world,
                "targets": summary.targets.iter().map(|target| json!({
                    "id": &target.id,
                    "host_addr": &target.host_addr,
                    "uri": &target.uri,
                })).collect::<Vec<_>>(),
            })
        );
    } else {
        for target in &summary.targets {
            println!(
                "[forward] {:<24} {} ({})",
                target.id, target.uri, target.host_addr
            );
        }
    }
    let _ = io::stdout().flush();
}

fn auth_login(json: bool, layout: &Layout, endpoint: Option<String>) -> Result<ExitCode, String> {
    let client = connect(layout)?;
    let auth = client
        .auth_login(endpoint.as_deref())
        .map_err(|err| err.to_string())?;
    if json {
        println!(
            "{}",
            json!({
                "ok": true,
                "key_id": auth.key_id,
                "backend": auth.backend,
                "account": auth.account,
                "machine": auth.machine,
                "pairing_url": auth.pairing_url.expose(),
            })
        );
    } else {
        println!("{}", auth.pairing_url.expose());
        println!(
            "[auth] paired {} (backend={}){}",
            auth.key_id,
            auth.backend,
            auth.account
                .as_ref()
                .map(|account| format!(" account={account}"))
                .unwrap_or_default()
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Remember the last hook failure so a failed `up` / `fork` can print the rescue block.
fn note_hook_failure(event: &BootEvent, failure: &mut Option<BootEvent>) {
    if matches!(event, BootEvent::HookFailed { .. }) {
        *failure = Some(event.clone());
    }
}

/// Print the full hook failure and its one-line rescue commands, then exit non-zero.
fn fail_with_rescue(
    json: bool,
    failure: Option<&BootEvent>,
    error: slipfork_edge::EdgeError,
) -> Result<ExitCode, String> {
    let Some(BootEvent::HookFailed {
        id,
        argv,
        exit_code,
        world,
        log_path,
    }) = failure
    else {
        return Err(error.to_string());
    };
    if json {
        return Err(error.to_string());
    }
    let world_selector = world
        .split_once(':')
        .map(|(_, fork)| format!(" --world {fork}"))
        .unwrap_or_default();
    eprintln!("error: hook '{id}' exited with code {exit_code} in world '{world}'");
    eprintln!("  argv: {argv:?}");
    eprintln!("  log:  {}", log_path.display());
    eprintln!();
    eprintln!("Rescue commands (world '{world}' kept alive for inspection):");
    eprintln!(
        "  slipfork hook run {id}{world_selector}          # add --no-tty in non-interactive shells"
    );
    eprintln!("  slipfork logs{world_selector} {id}");
    eprintln!(
        "  slipfork shell{world_selector}                            # or: slipfork exec{world_selector} -- <cmd>"
    );
    Ok(ExitCode::from(1))
}

/// Print one streamed boot event (never in `--json` mode, which writes only JSON).
///
/// `[resources]` resolution rows are developer diagnostics on an otherwise healthy boot, so they
/// never stream: the block's `-v` detail renders the daemon's recorded `resources` instead
/// (`docs/design/dx-0.1.0-ux.md` §1, §8.2).
fn print_event(json: bool, event: BootEvent) {
    if json {
        return;
    }
    match event {
        BootEvent::Resources { .. } => {
            // Rendered once, from the returned summary, and only under `-v`.
        }
        BootEvent::Hook { id, argv } => {
            println!("[hook]      {id}: {}", argv.join(" "));
        }
        BootEvent::HookOutput { chunk, .. } => {
            let mut stream: Box<dyn Write> = match chunk.source {
                slipfork_engine::StreamSource::Stdout => Box::new(io::stdout()),
                slipfork_engine::StreamSource::Stderr => Box::new(io::stderr()),
            };
            let _ = stream.write_all(&chunk.bytes);
            let _ = stream.flush();
        }
        BootEvent::HookFailed { .. } => {
            // `fail_with_rescue` renders the structured failure after the terminal daemon error.
        }
        BootEvent::Ready { component, port } => {
            println!("[ready]     {component} (internal :{port} in world netns)");
        }
    }
}

/// The block's two presentation facts: `-v`, and whether `next` needs `--world`.
fn block_options(verbose: bool, qualify_next: bool) -> slipfork::restart::Options {
    slipfork::restart::Options {
        qualify_next,
        verbose,
    }
}

/// Print the `up` / `fork` / `restart` result: the `--json` document or the human block.
///
/// Under `--json` the daemon's [`slipfork_edge::WorldUpSummary`] is serialized as-is (one
/// projection, never a hand-built subset) with the envelope's `ok`. In human mode exactly one
/// advisory stderr line announces a daemon that is not bound to `:80` (§2 rule 4); it is never
/// written to stdout and never under `--json`.
fn print_world_up(
    json: bool,
    client: &DaemonControlClient,
    verb: slipfork::restart::Verb,
    options: slipfork::restart::Options,
    summary: &slipfork_edge::WorldUpSummary,
) -> Result<(), String> {
    if json {
        println!("{}", render_json(&slipfork::restart::json(summary)?)?);
        return Ok(());
    }
    note_ingress(client);
    print!("{}", slipfork::restart::render(verb, summary, options));
    Ok(())
}

/// The one `:80` line when the daemon serves another ingress port (`docs/design/dx-0.1.0-ux.md` §2).
///
/// The port is the daemon's own `DaemonStatus.ingress` fact, read after the mutation succeeded and
/// rendered by the single [`slipfork::status::ingress_hint`]; the CLI never parses a port out of an
/// origin URL. The boot already succeeded, so an advisory read that fails changes nothing.
fn note_ingress(client: &DaemonControlClient) {
    let Ok(status) = client.status() else {
        return;
    };
    if let Some(hint) = slipfork::status::ingress_hint(status.ingress.port()) {
        eprintln!("{hint}");
    }
}

/// Dial the daemon, failing closed with the one line that fixes an absent `slipforkd`.
///
/// Read/diagnostic verbs never spawn a daemon: auto-spawn is reserved for `up`/`fork`, so a
/// `slipfork list` can never leave a mystery `--listen 127.0.0.1:0` process behind.
fn connect(layout: &Layout) -> Result<DaemonControlClient, String> {
    served_client(layout).ok_or_else(|| no_daemon_line(layout))
}

/// The bound control client when a daemon answers, else `None`.
fn served_client(layout: &Layout) -> Option<DaemonControlClient> {
    let client = DaemonControlClient::bind(layout.runtime_dir());
    client.is_served().then_some(client)
}

/// The single one-line refusal read/diagnostic verbs print when nothing answers.
fn no_daemon_line(layout: &Layout) -> String {
    format!(
        "no slipforkd on {} — run `slipfork up`",
        DaemonControlClient::bind(layout.runtime_dir())
            .socket_path()
            .display()
    )
}

/// Dial the daemon, auto-spawning it once when `<state_dir>/run/daemon.sock` does not answer.
///
/// Only `up` and `fork` call this (`ADR-0011` §3); every other verb uses [`connect`].
fn connect_or_spawn(layout: &Layout) -> Result<DaemonControlClient, String> {
    if let Some(client) = served_client(layout) {
        return Ok(client);
    }
    let client = DaemonControlClient::bind(layout.runtime_dir());
    ensure_private_dir(layout.runtime_dir())?;
    let log_path = layout.runtime_dir().join("daemon.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)
        .map_err(|err| format!("cannot open {}: {err}", log_path.display()))?;
    fs::set_permissions(&log_path, std::fs::Permissions::from_mode(0o600))
        .map_err(|err| format!("cannot restrict {} to 0600: {err}", log_path.display()))?;
    let stderr = log
        .try_clone()
        .map_err(|err| format!("cannot duplicate {}: {err}", log_path.display()))?;
    let daemon = daemon_executable()?;
    let mut child = Command::new(&daemon)
        .arg("--listen")
        .arg("127.0.0.1:0")
        .arg("--state-dir")
        .arg(layout.state_dir())
        .arg("--runtime-dir")
        .arg(layout.runtime_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .map_err(|err| {
            format!(
                "cannot start {} zero-setup: {err}; start it manually with `slipforkd --listen 127.0.0.1:0 --state-dir {} --runtime-dir {}`",
                daemon.display(),
                layout.state_dir().display(),
                layout.runtime_dir().display()
            )
        })?;

    let deadline = Instant::now() + DAEMON_GRACE;
    loop {
        if client.is_served() {
            return Ok(client);
        }
        if let Ok(Some(status)) = child.try_wait() {
            // A concurrent CLI may have won the socket race while this child exited cleanly.
            if client.is_served() {
                return Ok(client);
            }
            return Err(format!(
                "slipforkd exited with {status} before serving {}; see {}",
                client.socket_path().display(),
                log_path.display()
            ));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "slipforkd did not serve {} within {}s; see {}",
                client.socket_path().display(),
                DAEMON_GRACE.as_secs(),
                log_path.display()
            ));
        }
        std::thread::sleep(DAEMON_POLL);
    }
}

/// Directory of this `slipfork` executable (never a `PATH` guess, never a `/usr/bin` fallback).
///
/// The single fail-closed resolution used by `doctor` (the AppArmor hint attachment) and
/// `daemon unit` (the printed profile attachment), so both name the same executable directory.
fn exe_dir() -> Result<PathBuf, String> {
    let executable =
        std::env::current_exe().map_err(|err| format!("cannot resolve this executable: {err}"))?;
    executable.parent().map(Path::to_path_buf).ok_or_else(|| {
        format!(
            "this executable {} has no parent directory",
            executable.display()
        )
    })
}

/// The `slipforkd` executable installed next to this CLI (never a `PATH` guess).
fn daemon_executable() -> Result<PathBuf, String> {
    let candidate = exe_dir()?.join("slipforkd");
    if !candidate.is_file() {
        return Err(format!(
            "cannot find slipforkd at {}; build or install the slipforkd binary next to slipfork",
            candidate.display()
        ));
    }
    Ok(candidate)
}

fn repo_root(start: &Path) -> Result<slipfork_vcs::RepoRoot, String> {
    repository_resolver()
        .resolve(start)
        .map_err(|err| err.to_string())
}
