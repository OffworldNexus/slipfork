//! `slipfork`: the thin CLI client (`H-010` #1, `ADR-0008` §4).
//!
//! The CLI holds **no** `WorldEngine`, `SecretProvider`, or tunnel lease for world operations: it
//! parses `clap` arguments, resolves `HOME`/`$XDG_*`/`--state-dir`/`--runtime-dir` once, and drives
//! `slipforkd` through the typed `<runtime_dir>/daemon.sock` [`DaemonControlClient`]. When that
//! socket does not answer, it auto-spawns `slipforkd --listen 127.0.0.1:0` in the background so
//! `slipfork up` and CI work zero-setup without systemd.
//!
//! `snow resolve` runs through `slipforkd` (`DaemonControl::snow_resolve`), so the daemon owns the
//! `0600` local vault and any paired `OSPP v1` remote vault. Only `doctor` and `dag plan` stay
//! CLI-local: they read host capabilities and repository files without materializing a world.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use clap::Parser;
use serde_json::json;
use slipfork::cli::{
    AuthAction, Cli, Commands, DaemonAction, DagAction, DumpArgs, HookAction, ResourceArgs,
    SecretAction, SnowAction, UnitKind,
};
use slipfork::layout::{Layout, ensure_private_dir};
use slipfork::project::{load_slipfork_config, repository_resolver};
use slipfork::user::{AccountDatabaseIdentity, UserIdentity};
use slipfork_dag::{DagPlanner, EvidenceField, FsDigestCache, FsMerkleHasher, SupportRegistry};
use slipfork_edge::{
    BootEvent, DaemonControl, DaemonControlClient, DropForkRequest, ExecTargetRequest, ForkRequest,
    ForwardRequest, ForwardSummary, HookRunRequest, LogsRequest, SnowResolveRequest, UnitSpec,
    UpRequest, WorldSelector, render_service_unit, render_socket_unit,
};
use slipfork_engine::{current_uid_gid, probe_reflink_dir};
use slipfork_snow::SnowFile;
use slipfork_vcs::VcsError;

/// Bound on waiting for a freshly spawned `slipforkd` to answer its control socket.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(15);
/// Poll interval while waiting for the daemon socket.
const SPAWN_POLL: Duration = Duration::from_millis(50);

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
    let xdg_cache = absolute_env("XDG_CACHE_HOME");
    let xdg_runtime = absolute_env("XDG_RUNTIME_DIR");
    let xdg_data = absolute_env("XDG_DATA_HOME");
    let layout = Layout::resolve(
        cli.state_dir.clone(),
        cli.runtime_dir.clone(),
        &home,
        xdg_cache.as_deref(),
        xdg_runtime.as_deref(),
        xdg_data.as_deref(),
    );
    let json = cli.json;

    match cli.command {
        Commands::Doctor => doctor(json, &layout),
        Commands::Snow {
            action:
                SnowAction::Resolve {
                    file,
                    mode,
                    component,
                    expose_secrets,
                },
        } => resolve_snow(
            json,
            &layout,
            &file,
            &mode,
            component.as_deref(),
            expose_secrets,
        ),
        Commands::Dag {
            action: DagAction::Plan { file, repo },
        } => dag_plan(json, &layout, file, repo),
        Commands::Daemon {
            action: DaemonAction::Unit { only, forward_port },
        } => daemon_unit(json, &layout, only, forward_port),
        Commands::Up {
            world,
            dump,
            resources,
            host_caches,
        } => up(json, &layout, world, dump, resources, host_caches),
        Commands::Down { world } => down(json, &layout, world),
        Commands::Fork {
            fork,
            base,
            dump,
            resources,
            host_caches,
        } => create_fork(json, &layout, fork, base, dump, resources, host_caches),
        Commands::Drop { fork } => drop_fork(json, &layout, fork),
        Commands::List => list(json, &layout),
        Commands::Prune { dry_run, yes } => prune(json, &layout, dry_run, yes),
        Commands::Secret { action } => secret(json, &layout, action),
        Commands::Share { stop, world } => share(json, &layout, stop, world),
        Commands::Exec { world, cwd, argv } => exec(json, &layout, world, cwd, argv),
        Commands::Shell { world } => shell(json, &layout, world),
        Commands::Logs {
            world,
            target,
            follow,
            stderr,
            lines,
        } => logs(json, &layout, world, target, follow, stderr, lines),
        Commands::Hook {
            action:
                HookAction::Run {
                    hook,
                    world,
                    no_tty,
                },
        } => hook_run(json, &layout, hook, world, no_tty),
        Commands::Db { db_id, world } => db(json, &layout, db_id, world),
        Commands::Redis { cluster_id, world } => redis(json, &layout, cluster_id, world),
        Commands::Forward { world, targets } => forward(json, &layout, world, targets),
        Commands::Auth {
            action: AuthAction::Login { url },
        } => auth_login(json, &layout, url),
        Commands::Support => support(json),
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

/// Read one XDG variable, refusing relative values (`XDG_*` must be absolute).
fn absolute_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn current_dir() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|err| format!("cannot read the current directory: {err}"))
}

fn doctor(json: bool, layout: &Layout) -> Result<ExitCode, String> {
    let (uid, gid) = current_uid_gid();
    let incus_sock = Path::new("/var/lib/incus/unix.socket").exists();
    let reflink_us = probe_reflink_dir(layout.state_dir()).map_err(|err| err.to_string())?;
    let netns_ok = Command::new("unshare")
        .args([
            &format!("--map-user={uid}"),
            &format!("--map-group={gid}"),
            "--keep-caps",
            "--net",
            "--pid",
            "--fork",
            "sh",
            "-c",
            "ip link set lo up",
        ])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);

    if json {
        println!(
            "{}",
            json!({
                "ok": netns_ok,
                "uid": uid,
                "gid": gid,
                "state_root": layout.state_dir(),
                "incus_daemon_socket": incus_sock,
                "reflink_64k_micros": reflink_us,
                "isolated_netns": netns_ok,
            })
        );
    } else {
        println!("state_root: {}", layout.state_dir().display());
        println!(
            "incus:      {}",
            if incus_sock {
                "available"
            } else {
                "absent (using reflink-netns)"
            }
        );
        println!("reflink:    ok (64 KiB FICLONE in {reflink_us} us)");
        println!(
            "netns:      {} (uid={uid}:{gid})",
            if netns_ok { "ok" } else { "FAILED" }
        );
    }
    Ok(if netns_ok {
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
    let client = connect_or_spawn(layout)?;
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
        println!(
            "{}",
            serde_json::to_string_pretty(&summary.vars).unwrap_or_default()
        );
    } else {
        for (key, value) in &summary.vars {
            println!("{key}={value}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn dag_plan(
    json: bool,
    layout: &Layout,
    file: Option<PathBuf>,
    repo: Option<PathBuf>,
) -> Result<ExitCode, String> {
    let (config_path, repo_root) = resolve_dag_paths(file, repo)?;
    let config = load_slipfork_config(&config_path)?;
    let hasher = FsMerkleHasher::new(&repo_root);
    let cache = FsDigestCache::new(layout.state_dir());
    let plan = DagPlanner::plan(&config, &BTreeMap::new(), &BTreeMap::new(), &hasher, &cache)
        .map_err(|err| err.to_string())?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&plan).unwrap_or_default()
        );
    } else {
        for artifact in &plan.artifacts {
            println!(
                "artifact {:16} {:?} ({})",
                artifact.id,
                artifact.action,
                &artifact.digest[..12]
            );
        }
        for (wave_index, wave) in plan.fixture_waves.iter().enumerate() {
            for fixture in wave {
                println!(
                    "wave {wave_index} fixture {:12} {:?} ({})",
                    fixture.id,
                    fixture.action,
                    &fixture.digest[..12]
                );
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn daemon_unit(
    json: bool,
    layout: &Layout,
    only: Option<UnitKind>,
    forward_port: u16,
) -> Result<ExitCode, String> {
    let user = AccountDatabaseIdentity.username()?;
    let executable = daemon_executable()?;
    let spec = UnitSpec {
        user,
        listen_addrs: vec![
            "127.0.0.1:80"
                .parse()
                .map_err(|err| format!("invalid loopback ingress: {err}"))?,
            "[::1]:80"
                .parse()
                .map_err(|err| format!("invalid loopback ingress: {err}"))?,
        ],
        forward_port,
        executable,
        state_dir: layout.state_dir().to_path_buf(),
        runtime_dir: layout.runtime_dir().to_path_buf(),
    };
    let socket = || render_socket_unit(&spec).map_err(|err| err.to_string());
    let service = || render_service_unit(&spec).map_err(|err| err.to_string());
    let text = match only {
        Some(UnitKind::Socket) => socket()?,
        Some(UnitKind::Service) => service()?,
        None => format!("{}\n{}", socket()?, service()?),
    };
    if json {
        println!(
            "{}",
            json!({
                "socket": match only { Some(UnitKind::Service) => serde_json::Value::Null, _ => serde_json::Value::String(socket()?) },
                "service": match only { Some(UnitKind::Socket) => serde_json::Value::Null, _ => serde_json::Value::String(service()?) },
            })
        );
    } else {
        print!("{text}");
    }
    Ok(ExitCode::SUCCESS)
}

fn up(
    json: bool,
    layout: &Layout,
    world: Option<String>,
    dump: DumpArgs,
    resources: ResourceArgs,
    host_caches: bool,
) -> Result<ExitCode, String> {
    let client = connect_or_spawn(layout)?;
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
            print_world_up(json, &summary);
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => fail_with_rescue(json, failure.as_ref(), err),
    }
}

fn down(json: bool, layout: &Layout, world: Option<String>) -> Result<ExitCode, String> {
    let client = connect_or_spawn(layout)?;
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

fn create_fork(
    json: bool,
    layout: &Layout,
    fork: String,
    base: Option<String>,
    dump: DumpArgs,
    resources: ResourceArgs,
    host_caches: bool,
) -> Result<ExitCode, String> {
    let client = connect_or_spawn(layout)?;
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
            print_world_up(json, &summary);
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => fail_with_rescue(json, failure.as_ref(), err),
    }
}

fn drop_fork(json: bool, layout: &Layout, fork: String) -> Result<ExitCode, String> {
    let client = connect_or_spawn(layout)?;
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
    let client = connect_or_spawn(layout)?;
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
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).unwrap_or_default()
        );
    } else {
        println!("{:<16} {:<9} {:<40} WORKTREE", "WORLD", "STATE", "ORIGIN");
        for world in &worlds {
            println!(
                "{:<16} {:<9} {:<40} {}",
                world.world,
                world_state_label(world.state),
                world.origin.url(),
                world.worktree.display()
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn prune(json: bool, layout: &Layout, dry_run: bool, yes: bool) -> Result<ExitCode, String> {
    let client = connect_or_spawn(layout)?;
    let dry_run = dry_run || !yes;
    let report = client.prune(dry_run).map_err(|err| err.to_string())?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
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
    let client = connect_or_spawn(layout)?;
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
                println!(
                    "{}",
                    serde_json::to_string_pretty(&keys).unwrap_or_default()
                );
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

fn share(
    json: bool,
    layout: &Layout,
    stop: bool,
    world: Option<String>,
) -> Result<ExitCode, String> {
    let client = connect_or_spawn(layout)?;
    let selector = WorldSelector {
        cwd: current_dir()?,
        world,
    };
    if stop {
        let origin = client.unshare(&selector).map_err(|err| err.to_string())?;
        if json {
            println!("{}", json!({ "ok": true, "origin": origin.url() }));
        } else {
            println!("[share] stopped; origin {}", origin.url());
        }
    } else {
        let shared = client.share(&selector).map_err(|err| err.to_string())?;
        if json {
            println!(
                "{}",
                json!({ "ok": true, "origin": shared.origin.url(), "lease_id": &shared.lease_id })
            );
        } else {
            println!("[share] {}", shared.origin.url());
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn exec(
    json: bool,
    layout: &Layout,
    world: Option<String>,
    cwd: Option<PathBuf>,
    argv: Vec<String>,
) -> Result<ExitCode, String> {
    let client = connect_or_spawn(layout)?;
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
    let client = connect_or_spawn(layout)?;
    let target = client
        .exec_target(&ExecTargetRequest {
            cwd: current_dir()?,
            world,
            service: None,
            argv: vec![shell],
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

fn logs(
    json: bool,
    layout: &Layout,
    world: Option<String>,
    target: Option<String>,
    follow: bool,
    stderr: bool,
    lines: Option<usize>,
) -> Result<ExitCode, String> {
    let client = connect_or_spawn(layout)?;
    let request = LogsRequest {
        cwd: current_dir()?,
        world,
        target,
        follow,
        stderr,
        lines,
    };
    let mut stdout = io::stdout();
    let mut stderr_stream = io::stderr();
    client
        .logs(&request, &mut |chunk| {
            if json {
                let stream = match chunk.source {
                    slipfork_engine::StreamSource::Stdout => "stdout",
                    slipfork_engine::StreamSource::Stderr => "stderr",
                };
                println!(
                    "{}",
                    json!({ "stream": stream, "text": String::from_utf8_lossy(&chunk.bytes) })
                );
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
    Ok(ExitCode::SUCCESS)
}

fn hook_run(
    json: bool,
    layout: &Layout,
    hook: String,
    world: Option<String>,
    no_tty: bool,
) -> Result<ExitCode, String> {
    let client = connect_or_spawn(layout)?;
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
    let client = connect_or_spawn(layout)?;
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
    let client = connect_or_spawn(layout)?;
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

fn auth_login(json: bool, layout: &Layout, url: Option<String>) -> Result<ExitCode, String> {
    let pairing_url = match url.as_deref() {
        Some("-") => {
            let mut buffer = String::new();
            io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|err| format!("cannot read the pairing URL from stdin: {err}"))?;
            buffer.trim_end_matches(['\n', '\r']).to_owned()
        }
        Some(url) => url.to_owned(),
        None => {
            return Err(
                "pass the pairing URL (`slipfork auth login '<url>#handle=<64 hex>'`) or read it from stdin with `slipfork auth login -`"
                    .to_owned(),
            );
        }
    };
    let client = connect_or_spawn(layout)?;
    let auth = client
        .auth_login(&pairing_url)
        .map_err(|err| err.to_string())?;
    if json {
        println!(
            "{}",
            json!({
                "ok": true,
                "key_id": auth.key_id,
                "backend": auth.backend,
                "account": auth.account,
            })
        );
    } else {
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

fn support(json: bool) -> Result<ExitCode, String> {
    let registry = SupportRegistry::embedded().map_err(|err| err.to_string())?;
    if json {
        let rows: Vec<serde_json::Value> = registry
            .entries()
            .iter()
            .map(|entry| {
                let required: Vec<&str> = entry
                    .tier()
                    .required_evidence()
                    .iter()
                    .map(|field| field.as_str())
                    .collect();
                json!({
                    "id": entry.id(),
                    "category": category_label(entry.category()),
                    "tier": entry.tier().to_string(),
                    "evidence": evidence_json(entry.evidence()),
                    "required_evidence": required,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).unwrap_or_default()
        );
    } else {
        println!(
            "{:<28} {:<14} {:<12} EVIDENCE",
            "INTEGRATION", "CATEGORY", "TIER"
        );
        for entry in registry.entries() {
            let evidence = EvidenceField::ALL
                .iter()
                .filter(|field| entry.evidence().is_present(**field))
                .map(|field| field.as_str())
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "{:<28} {:<14} {:<12} {}",
                entry.id(),
                category_label(entry.category()),
                entry.tier(),
                evidence
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn category_label(category: slipfork_dag::IntegrationCategory) -> String {
    serde_json::to_value(category)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn evidence_json(evidence: &slipfork_dag::Evidence) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for field in EvidenceField::ALL {
        if let Some(value) = evidence.field(*field)
            && !value.trim().is_empty()
        {
            map.insert(field.as_str().to_owned(), json!(value));
        }
    }
    serde_json::Value::Object(map)
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
fn print_event(json: bool, event: BootEvent) {
    if json {
        return;
    }
    match event {
        BootEvent::Secrets { key, vault } => {
            println!(
                "[secrets]   ensured secrets::{key} -> {} (0600)",
                vault.display()
            );
        }
        BootEvent::Resources { resource } => {
            println!(
                "[resources] {}: {} -> {} ({})",
                resource.id, resource.strategy, resource.resolved, resource.reason
            );
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

fn print_world_up(json: bool, summary: &slipfork_edge::WorldUpSummary) {
    if json {
        println!(
            "{}",
            json!({
                "world": &summary.world,
                "origin": summary.origin.url(),
                "resources": &summary.resources,
            })
        );
    } else {
        println!("[origin]    {}", summary.origin.url());
    }
}

fn world_state_label(state: slipfork_edge::WorldState) -> &'static str {
    match state {
        slipfork_edge::WorldState::Running => "running",
        slipfork_edge::WorldState::Failed => "failed",
        slipfork_edge::WorldState::Stopped => "stopped",
    }
}

/// Dial the daemon, auto-spawning it once when `<runtime_dir>/daemon.sock` does not answer.
fn connect_or_spawn(layout: &Layout) -> Result<DaemonControlClient, String> {
    let client = DaemonControlClient::bind(layout.runtime_dir());
    if client.is_served() {
        return Ok(client);
    }
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

    let deadline = Instant::now() + SPAWN_TIMEOUT;
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
                SPAWN_TIMEOUT.as_secs(),
                log_path.display()
            ));
        }
        std::thread::sleep(SPAWN_POLL);
    }
}

/// The `slipforkd` executable installed next to this CLI (never a `PATH` guess).
fn daemon_executable() -> Result<PathBuf, String> {
    let executable =
        std::env::current_exe().map_err(|err| format!("cannot resolve this executable: {err}"))?;
    let candidate = executable
        .parent()
        .ok_or_else(|| {
            format!(
                "this executable {} has no parent directory",
                executable.display()
            )
        })?
        .join("slipforkd");
    if !candidate.is_file() {
        return Err(format!(
            "cannot find slipforkd next to {}; build or install the slipforkd binary",
            executable.display()
        ));
    }
    Ok(candidate)
}

fn repo_root(start: &Path) -> Result<slipfork_vcs::RepoRoot, String> {
    repository_resolver()
        .resolve(start)
        .map_err(|err| err.to_string())
}

fn probe_repo_root(start: &Path) -> Result<Option<PathBuf>, String> {
    match repository_resolver().resolve(start) {
        Ok(repository) => Ok(Some(repository.vcs_root)),
        Err(VcsError::NotARepository { .. }) => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Resolve the DAG planner's `--file`/`--repo` pair through the shared repository resolver.
fn resolve_dag_paths(
    file: Option<PathBuf>,
    repo: Option<PathBuf>,
) -> Result<(PathBuf, PathBuf), String> {
    match (file, repo) {
        (None, None) => {
            let repository = repo_root(Path::new("."))?;
            Ok((
                repository.vcs_root.join("slipfork.toml"),
                repository.vcs_root,
            ))
        }
        (None, Some(explicit_repo)) => {
            let canonical_repo = fs::canonicalize(&explicit_repo).map_err(|err| {
                format!("cannot access --repo {}: {err}", explicit_repo.display())
            })?;
            let vcs_root = repo_root(&canonical_repo)?.vcs_root;
            if canonical_repo != vcs_root {
                let relation = if canonical_repo.starts_with(&vcs_root) {
                    "is narrower than"
                } else {
                    "diverges from"
                };
                return Err(format!(
                    "--repo {} {relation} VCS root {} (which owns {}); omit --repo (or pass --repo {}) so config and input globs share the VCS root",
                    explicit_repo.display(),
                    vcs_root.display(),
                    vcs_root.join("slipfork.toml").display(),
                    vcs_root.display(),
                ));
            }
            Ok((vcs_root.join("slipfork.toml"), vcs_root))
        }
        (Some(explicit_file), None) => {
            if let Err(err) = fs::metadata(&explicit_file) {
                return Err(format!("cannot read {}: {err}", explicit_file.display()));
            }
            let vcs_root = repo_root(&parent_dir(&explicit_file))?.vcs_root;
            Ok((explicit_file, vcs_root))
        }
        (Some(explicit_file), Some(explicit_repo)) => {
            if let Err(err) = fs::metadata(&explicit_file) {
                return Err(format!("cannot read {}: {err}", explicit_file.display()));
            }
            let canonical_file_dir = fs::canonicalize(parent_dir(&explicit_file))
                .map_err(|err| format!("cannot access {}: {err}", explicit_file.display()))?;
            let canonical_repo = fs::canonicalize(&explicit_repo).map_err(|err| {
                format!("cannot access --repo {}: {err}", explicit_repo.display())
            })?;

            match (
                probe_repo_root(&canonical_file_dir)?,
                probe_repo_root(&canonical_repo)?,
            ) {
                (Some(file_vcs_root), _) => {
                    if canonical_repo != file_vcs_root {
                        return Err(format!(
                            "--repo {} is not the VCS root ({}) of --file {} (rule: --repo must match the canonical VCS root of --file so config and input globs share one root); pass --repo {} or omit --repo",
                            explicit_repo.display(),
                            file_vcs_root.display(),
                            explicit_file.display(),
                            file_vcs_root.display(),
                        ));
                    }
                    Ok((explicit_file, canonical_repo))
                }
                (None, Some(repo_vcs_root)) => Err(format!(
                    "--file {} is outside any git or jj repository while --repo {} belongs to VCS root {} (rule: --repo must match the VCS root of --file so config and input globs share one root)",
                    explicit_file.display(),
                    explicit_repo.display(),
                    repo_vcs_root.display(),
                )),
                (None, None) => {
                    if canonical_repo != canonical_file_dir {
                        return Err(format!(
                            "--repo {} does not match the directory ({}) of --file {} outside any git or jj repository (rule: outside a VCS repository, --repo must match the parent directory of --file so config and input globs share one root)",
                            explicit_repo.display(),
                            canonical_file_dir.display(),
                            explicit_file.display(),
                        ));
                    }
                    Ok((explicit_file, canonical_repo))
                }
            }
        }
    }
}
