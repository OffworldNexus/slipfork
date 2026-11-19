//! Process verbs: `exec`, `shell`, `logs`, `hook run`, `db`, and `redis`.

use std::io::{self, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::json;
use slipfork_edge::{DaemonControl, DaemonControlClient, EdgeError};
use slipfork_vcs::WORKTREES_DIRECTORY;
use slipfork_wire::control::{
    ExecService, ExecTargetRequest, HookRunRequest, LogsRequest, WorldSelector,
};
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

/// `slipfork db`: open `psql` on a terminal, or print the database's published URI on a pipe.
pub(crate) fn db(
    app: &CliApp,
    db_id: Option<String>,
    world: Option<String>,
) -> Result<ExitCode, String> {
    let cwd = app.ui.cwd()?;
    let selection = resource_selection(&cwd, world.as_deref(), ResourceKind::PgDatabase, db_id)?;
    service_entry(app, cwd, world, selection, vec!["psql".to_owned()], "db")
}

/// `slipfork redis`: open `redis-cli` on a terminal, or print the cluster's published URI.
pub(crate) fn redis(
    app: &CliApp,
    cluster_id: Option<String>,
    world: Option<String>,
) -> Result<ExitCode, String> {
    let cwd = app.ui.cwd()?;
    let selection = resource_selection(
        &cwd,
        world.as_deref(),
        ResourceKind::RedisCluster,
        cluster_id,
    )?;
    service_entry(
        app,
        cwd,
        world,
        selection,
        vec!["redis-cli".to_owned()],
        "redis",
    )
}

/// One data-client target: the daemon's `Backing` service id and the world's published URI.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResourceSelection {
    service: String,
    uri: String,
}

fn service_entry(
    app: &CliApp,
    cwd: PathBuf,
    world: Option<String>,
    selection: ResourceSelection,
    argv: Vec<String>,
    verb: &str,
) -> Result<ExitCode, String> {
    if app.output_mode.json {
        return Err(format!(
            "`slipfork {verb}` prints the service URI on a pipe and streams the client on a terminal, so it has no --json document\n  next: drop --json, or use `slipfork --json forward` for a URI-bearing document"
        ));
    }
    let client = connect(&app.layout)?;
    // Ask the daemon first: a stopped service must surface `ServiceNotRunning` for a piped call too
    // (the service refusal is the primary diagnostic, never a TTY complaint).
    let target = client
        .exec_target(&ExecTargetRequest {
            cwd,
            world,
            service: ExecService::Backing(selection.service.clone()),
            argv,
        })
        .map_err(entry_refusal)?;
    // A pipe is a script's turn: the world's own published `...url` resource fact is the connection
    // URI (exactly the value the app reads as `DATABASE_URL`), printed byte-clean with no spinner,
    // no ANSI, and no exit-code mirroring to corrupt it.
    if !app.ui.stdin_tty {
        println!("{}", selection.uri);
        return Ok(ExitCode::SUCCESS);
    }
    let status = spawn_entry(&target)?;
    Ok(exit_code(&status))
}

/// The CLI-facing refusal for a data-client target: the daemon's one line plus the remedy.
///
/// A stopped service (`ServiceNotRunning`) is the primary diagnostic, and no URI is ever printed
/// because the daemon produced no entry target.
fn entry_refusal(err: EdgeError) -> String {
    let message = err.to_string();
    if message.contains("ServiceNotRunning") {
        format!("{message}\n  next: slipfork up")
    } else {
        message
    }
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

/// Resolve the declared resource id and its published connection URI from the world's `Forkfile`.
///
/// The id rule is unchanged (`--world` alone suffices on a single-resource world, several entries
/// require an explicit id); the URI is never assembled here — it is the canonical
/// [`slipfork_forkfile::EvaluatedFork::world_facts`] `url` fact, so the CLI can never print a recipe
/// that diverges from what the world's own `DATABASE_URL`/`REDIS_URL` resolves to.
fn resource_selection(
    cwd: &Path,
    world: Option<&str>,
    kind: ResourceKind,
    explicit: Option<String>,
) -> Result<ResourceSelection, String> {
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
    let id = match explicit {
        Some(id) => id,
        None => match ids.as_slice() {
            [id] => id.clone(),
            [] => {
                return Err(format!(
                    "the `Forkfile` declares no {prefix} entry; add one before using this client"
                ));
            }
            _ => {
                return Err(format!(
                    "the `Forkfile` declares several {prefix} entries ({}); pass the id explicitly",
                    ids.join(", ")
                ));
            }
        },
    };
    let uri = fork
        .world_facts()
        .into_iter()
        .find(|fact| fact.entity == id && fact.property == "url")
        .map(|fact| fact.value.expose().to_owned())
        .ok_or_else(|| {
            format!(
                "the `Forkfile` publishes no connection URI for {prefix}.{id}\n  next: declare `{id}` as a {prefix} entry before using this client"
            )
        })?;
    Ok(ResourceSelection {
        service: format!("{prefix}.{id}"),
        uri,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn a_stopped_service_refuses_with_one_line_and_never_a_uri() {
        use slipfork_wire::control::{DaemonError, DaemonErrorKind};

        let error = EdgeError::Control(DaemonError::new(
            DaemonErrorKind::ServiceNotRunning,
            "world 'demo' has no running service 'pg_clusters.main' (ServiceNotRunning)",
        ));
        let refusal = entry_refusal(error);
        assert_eq!(
            refusal.lines().count(),
            2,
            "one error line plus one next: {refusal}"
        );
        assert!(refusal.contains("ServiceNotRunning"), "{refusal}");
        assert!(refusal.contains("next: slipfork up"), "{refusal}");
        assert!(
            !refusal.contains("postgres://") && !refusal.contains("redis://"),
            "a stopped service never prints a URI: {refusal}"
        );
    }

    /// A real on-disk repository (never `/tmp` tmpfs) whose directory name is the project slug.
    fn disk_repo(forkfile: &str) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::Builder::new()
            .prefix("slipfork-db-")
            .tempdir_in("/var/tmp")
            .expect("a disk tempdir");
        let repo = tmp.path().join("demoapp");
        std::fs::create_dir(&repo).expect("repo directory");
        let status = std::process::Command::new("git")
            .current_dir(&repo)
            .args(["init", "-q", "-b", "main"])
            .status()
            .expect("git init");
        assert!(status.success(), "git init");
        std::fs::write(repo.join("Forkfile"), forkfile).expect("Forkfile");
        (tmp, repo)
    }

    #[test]
    fn a_piped_db_and_redis_print_the_worlds_published_uri_byte_clean() {
        let _guard = disk_repo(
            "project demoapp\n\
             \n\
             features {\n    local\n}\n\
             \n\
             postgres pg {\n    database app_db\n}\n\
             redis cache\n",
        );
        let (_tmp, repo) = _guard;
        let pg = resource_selection(&repo, None, ResourceKind::PgDatabase, None)
            .expect("one declared database");
        assert_eq!(pg.service, "pg_databases.app_db");
        assert_eq!(pg.uri, "postgres://slipfork@127.0.0.1:5432/app_db");
        assert!(!pg.uri.contains('\x1b'), "a piped URI carries no ANSI");

        let redis = resource_selection(&repo, None, ResourceKind::RedisCluster, None)
            .expect("one declared cluster");
        assert_eq!(redis.service, "redis_clusters.cache");
        assert_eq!(redis.uri, "redis://127.0.0.1:6379");
        assert!(!redis.uri.contains('\x1b'), "a piped URI carries no ANSI");
    }

    #[test]
    fn several_declared_databases_require_an_explicit_id() {
        let (_tmp, repo) = disk_repo(
            "project demoapp\n\
             \n\
             features {\n    local\n}\n\
             \n\
             postgres pg {\n    database app_db\n    database jobs_db\n}\n",
        );
        let error = resource_selection(&repo, None, ResourceKind::PgDatabase, None)
            .expect_err("two databases are ambiguous");
        assert!(error.contains("several pg_databases entries"), "{error}");
        assert!(error.contains("app_db, jobs_db"), "{error}");
        let explicit = resource_selection(
            &repo,
            None,
            ResourceKind::PgDatabase,
            Some("jobs_db".to_owned()),
        )
        .expect("an explicit id resolves");
        assert_eq!(explicit.uri, "postgres://slipfork@127.0.0.1:5432/jobs_db");
    }
}
