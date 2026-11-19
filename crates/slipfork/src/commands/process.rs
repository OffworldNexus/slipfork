//! Process verbs: `exec`, `shell`, `logs`, `hook run`, `db`, and `redis`.

use std::io::{self, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::json;
use slipfork_edge::{DaemonControl, DaemonControlClient};
use slipfork_vcs::WORKTREES_DIRECTORY;
use slipfork_wire::control::{
    ExecService, ExecTargetRequest, HookRunRequest, LogsRequest, WorldSelector,
};
use slipfork_world::EntryKind;
use slipfork_world::StreamSource;

use crate::CliApp;
use crate::commands::control::{connect, repo_root};
use crate::entry::spawn_entry;

/// `slipfork exec`: run a command non-interactively inside a world or service container.
pub(crate) fn exec(
    app: &CliApp,
    world: Option<String>,
    service: Option<String>,
    cwd: Option<PathBuf>,
    argv: Vec<String>,
) -> Result<ExitCode, String> {
    if app.output_mode.json {
        return Err(
            "`slipfork exec` mirrors the child's stdio and exit code and has no --json document; next: drop --json, or use `slipfork ps --json` for world state"
                .to_owned(),
        );
    }
    let client = connect(&app.layout)?;
    let cwd = match cwd {
        Some(cwd) => cwd,
        None => app.ui.cwd()?,
    };
    // `--` is optional: clap's trailing vararg may hand it through, and the daemon never expects it.
    let argv = match argv.first().map(String::as_str) {
        Some("--") => argv.into_iter().skip(1).collect(),
        _ => argv,
    };
    let service = match service {
        Some(id) => ExecService::App(id),
        None => match selected_world_service(&client, &cwd, &world, &|id| {
            format!("slipfork exec -s {id} -- <cmd>")
        })? {
            Some(id) => ExecService::App(id),
            None => ExecService::World,
        },
    };
    let target = client
        .exec_target(&ExecTargetRequest {
            cwd,
            world,
            service,
            argv,
            entry_kind: EntryKind::Exec,
        })
        .map_err(|err| err.to_string())?;
    let status = spawn_entry(&target)?;
    Ok(exit_code(&status))
}

/// `slipfork shell`: enter a service's container with the developer's login shell.
///
/// The prompt is the only orientation `shell` prints (`(slipfork:<world>/<service>)`); the entry
/// itself is handed over byte-clean.
pub(crate) fn shell(
    app: &CliApp,
    world: Option<String>,
    service: Option<String>,
) -> Result<ExitCode, String> {
    if app.output_mode.json {
        return Err(
            "`slipfork shell` is an interactive terminal hand-over and has no --json document; next: drop --json, or use `slipfork exec -- <cmd>`"
                .to_owned(),
        );
    }
    require_tty(
        app,
        "shell",
        "use `slipfork exec -- <cmd>` for non-interactive commands",
    )?;
    let shell = app.ui.shell()?;
    let client = connect(&app.layout)?;
    let cwd = app.ui.cwd()?;
    let service = match service {
        Some(id) => ExecService::App(id),
        None => match selected_world_service(&client, &cwd, &world, &|id| {
            format!("slipfork shell {id}")
        })? {
            Some(id) => ExecService::App(id),
            None => ExecService::World,
        },
    };
    let mut target = client
        .exec_target(&ExecTargetRequest {
            cwd,
            world,
            service,
            argv: vec![shell],
            // A real interactive hand-over needs both ends: `shell | tee log` must stay
            // byte-clean, so the engine only bakes a PTY when stdout is a terminal too.
            entry_kind: if app.ui.stdout_tty {
                EntryKind::Interactive
            } else {
                EntryKind::Exec
            },
        })
        .map_err(|err| err.to_string())?;
    let prompt = match &target.service {
        Some(service) => format!("(slipfork:{}/{service}) ", target.world),
        None => format!("(slipfork:{}) ", target.world),
    };
    // Both dialects read one of these; an inherited shell prompt never survives `env_clear`.
    for name in ["PS1", "PROMPT"] {
        target
            .env
            .set(name, prompt.clone())
            .map_err(|err| err.to_string())?;
    }
    let status = spawn_entry(&target)?;
    Ok(exit_code(&status))
}

/// The service the world's own `Forkfile` auto-selects for `cwd` (`None` = no declared service).
///
/// The daemon's report carries the world's real worktree and its active feature selection, so the
/// CLI evaluates exactly the manifest the running world was booted from — not a default view.
fn selected_world_service(
    client: &DaemonControlClient,
    cwd: &Path,
    world: &Option<String>,
    remedy: &dyn Fn(&str) -> String,
) -> Result<Option<String>, String> {
    let report = client
        .world_processes(&WorldSelector {
            cwd: cwd.to_path_buf(),
            world: world.clone(),
        })
        .map_err(|err| err.to_string())?;
    let fork = slipfork::project::load_project_fork(&report.worktree, &report.feature_selection)?;
    slipfork::project::select_service(&fork, &report.worktree, cwd, None, remedy)
}

/// `slipfork logs`: read or follow retained component and hook logs.
pub(crate) fn logs(
    app: &CliApp,
    world: Option<String>,
    target: Option<String>,
    follow: bool,
    stderr: bool,
    lines: Option<usize>,
) -> Result<ExitCode, String> {
    let client = connect(&app.layout)?;
    let request = LogsRequest {
        cwd: app.ui.cwd()?,
        world,
        target,
        follow,
        stderr,
        lines,
    };
    // A fan-out request streams every retained source, so each line must carry its own gutter; one
    // explicitly named target is printed byte-exact unless `-v` asks for attribution
    // (`docs/design/dx-0.1.0-ux.md` §8.3).
    let prefix = request.target.is_none() || app.terminal.verbose();
    let mut renderer = slipfork::logs::Renderer::new(
        app.output_mode.json,
        prefix,
        app.terminal.color(),
        Box::new(io::stdout()),
        Box::new(io::stderr()),
    );
    client
        .logs(&request, &mut |log| {
            renderer.chunk(log);
            ControlFlow::Continue(())
        })
        .map_err(|err| err.to_string())?;
    renderer.finish();
    Ok(ExitCode::SUCCESS)
}

/// `slipfork hook run`: re-run one hook, native TTY unless `--no-tty`.
pub(crate) fn hook_run(
    app: &CliApp,
    hook: String,
    world: Option<String>,
    no_tty: bool,
) -> Result<ExitCode, String> {
    let client = connect(&app.layout)?;
    let request = HookRunRequest {
        cwd: app.ui.cwd()?,
        world,
        hook,
    };
    if no_tty {
        let mut stdout = io::stdout();
        let mut stderr_stream = io::stderr();
        let code = client
            .hook_run(&request, &mut |chunk| {
                if app.output_mode.json {
                    return;
                }
                let stream: &mut dyn Write = match chunk.source {
                    StreamSource::Stdout => &mut stdout,
                    StreamSource::Stderr => &mut stderr_stream,
                };
                let _ = stream.write_all(&chunk.bytes);
                let _ = stream.flush();
            })
            .map_err(|err| err.to_string())?;
        if app.output_mode.json {
            println!("{}", json!({ "ok": code == 0, "exit_code": code }));
        }
        return Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)));
    }

    require_tty(
        app,
        &format!("hook run {}", request.hook),
        "pass --no-tty (or use `slipfork exec`) in non-interactive shells",
    )?;
    let target = client
        .hook_exec_target(&request)
        .map_err(|err| err.to_string())?;
    let status = spawn_entry(&target)?;
    if app.output_mode.json {
        println!(
            "{}",
            json!({ "ok": status.success(), "world": target.world, "exit_code": status.code() })
        );
    }
    Ok(exit_code(&status))
}

/// `slipfork db`: open `psql` inside the world's running PostgreSQL service container.
pub(crate) fn db(
    app: &CliApp,
    db_id: Option<String>,
    world: Option<String>,
) -> Result<ExitCode, String> {
    let cwd = app.ui.cwd()?;
    let service = match db_id {
        Some(id) => format!("pg_databases.{id}"),
        None => default_resource_id(&cwd, world.as_deref(), ResourceKind::PgDatabase)?,
    };
    service_entry(app, cwd, world, service, vec!["psql".to_owned()])
}

/// `slipfork redis`: open `redis-cli` inside the world's running Redis service container.
pub(crate) fn redis(
    app: &CliApp,
    cluster_id: Option<String>,
    world: Option<String>,
) -> Result<ExitCode, String> {
    let cwd = app.ui.cwd()?;
    let service = match cluster_id {
        Some(id) => format!("redis_clusters.{id}"),
        None => default_resource_id(&cwd, world.as_deref(), ResourceKind::RedisCluster)?,
    };
    service_entry(app, cwd, world, service, vec!["redis-cli".to_owned()])
}

fn service_entry(
    app: &CliApp,
    cwd: PathBuf,
    world: Option<String>,
    service: String,
    argv: Vec<String>,
) -> Result<ExitCode, String> {
    let client = connect(&app.layout)?;
    // Ask the daemon first: a stopped service must surface `ServiceNotRunning` even when the
    // caller has no TTY (the service refusal is the primary diagnostic).
    let target = client
        .exec_target(&ExecTargetRequest {
            cwd,
            world,
            service: ExecService::Backing(service.clone()),
            argv,
            entry_kind: EntryKind::Exec,
        })
        .map_err(|err| err.to_string())?;
    require_tty(
        app,
        &format!("{service} client"),
        "`slipfork db` / `slipfork redis` need an interactive terminal",
    )?;
    let status = spawn_entry(&target)?;
    if app.output_mode.json {
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

pub(crate) fn exit_code(status: &std::process::ExitStatus) -> ExitCode {
    ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1))
}

/// Fail closed when an interactive world entry has no terminal (`ADR-0008` §4).
fn require_tty(app: &CliApp, command: &str, hint: &str) -> Result<(), String> {
    if app.ui.stdin_tty {
        return Ok(());
    }
    Err(format!(
        "`slipfork {command}` needs an interactive terminal; {hint}"
    ))
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
    let repo = repo_root(cwd)?;
    let worktree = match world {
        Some(fork) => repo.vcs_root.join(WORKTREES_DIRECTORY).join(fork),
        None => repo.worktree_root.clone(),
    };
    let fork = slipfork::project::load_project_fork(
        &worktree,
        &slipfork_wire::control::FeatureSelection::default(),
    )?;
    let (prefix, ids) = match kind {
        ResourceKind::PgDatabase => (
            "pg_databases",
            fork.pg_databases()
                .into_iter()
                .map(|db| db.database)
                .collect::<Vec<_>>(),
        ),
        ResourceKind::RedisCluster => (
            "redis_clusters",
            fork.redis
                .iter()
                .map(|cluster| cluster.id.clone())
                .collect::<Vec<_>>(),
        ),
    };
    match ids.as_slice() {
        [id] => Ok(format!("{prefix}.{id}")),
        [] => Err(format!(
            "the `Forkfile` declares no {prefix} entry; add one before using this client"
        )),
        _ => Err(format!(
            "the `Forkfile` declares several {prefix} entries ({}); pass the id explicitly",
            ids.join(", ")
        )),
    }
}
