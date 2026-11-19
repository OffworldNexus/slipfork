//! Per-service interaction verbs: resolved `env` and streamed `run`.
//!
//! Both verbs resolve the service from the one canonical `Forkfile` evaluator
//! ([`slipfork::project`]); `env` prints the same [`TaintedString`] values the daemon composes,
//! and `run` executes a declared task through the daemon's typed entry target so its stdio stays
//! live and its exit code is the caller's exit code.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use owo_colors::OwoColorize;
use slipfork::cli::FeatureArgs;
use slipfork_edge::DaemonControl;
use slipfork_forkfile::{
    EvaluatedFork, EvaluatedTask, FileVaultProvider, OsCsprngEntropy, Resolvers, TaintedString,
};
use slipfork_wire::control::{ExecService, ExecTargetRequest, WorldSelector};

use crate::CliApp;
use crate::commands::control::{connect, repo_root};
use crate::commands::process::exit_code;
use crate::commands::render_json;
use crate::entry::spawn_entry;

/// The one reason a verb cannot print secret values without a terminal.
const REVEAL_NEEDS_TTY: &str = "`--reveal` prints secret values and needs an interactive terminal\n  next: re-run it in a terminal, or omit `--reveal`";

/// `slipfork env [-s <service>] [--reveal]`: print one service's resolved environment.
///
/// The values come from the canonical evaluator with the caller's `-F` selection, so a
/// secret-tainted value renders `[REDACTED]` (or `null` under `--json`) unless `--reveal` is used
/// on a TTY.
pub(crate) fn env(
    app: &CliApp,
    world: Option<String>,
    service: Option<String>,
    reveal: bool,
    features: FeatureArgs,
) -> Result<ExitCode, String> {
    require_reveal_terminal(app.ui.stdout_tty, reveal)?;
    let cwd = app.ui.cwd()?;
    let repo = repo_root(&cwd)?;
    let worktree = slipfork::project::world_worktree(&repo, world.as_deref())?;
    let fork = slipfork::project::load_project_fork(&worktree, &features.selection())?;
    let service = selected_service(&fork, &worktree, &cwd, service.as_deref(), &|id| {
        format!("pass -s/--service {id}, or run from that service's directory")
    })?;
    let resolved = resolve_service_env(&app.layout.vault_path(&repo.project), &fork, &service)?;
    print!(
        "{}",
        render_env(
            &resolved,
            app.output_mode.json,
            reveal,
            app.terminal.color()
        )?
    );
    Ok(ExitCode::SUCCESS)
}

/// Refuse `--reveal` without a terminal: secret values are terminal-only.
fn require_reveal_terminal(stdout_tty: bool, reveal: bool) -> Result<(), String> {
    if reveal && !stdout_tty {
        return Err(REVEAL_NEEDS_TTY.to_owned());
    }
    Ok(())
}

/// The one declared service `env` resolves (`-s` wins, otherwise the forkfile-owned selection).
fn selected_service(
    fork: &EvaluatedFork,
    worktree: &Path,
    cwd: &Path,
    requested: Option<&str>,
    remedy: &dyn Fn(&str) -> String,
) -> Result<String, String> {
    slipfork::project::select_service(fork, worktree, cwd, requested, remedy)?.ok_or_else(|| {
        "this Forkfile declares no service to resolve an environment for\n  next: declare `service <id> ./dir { ... }` in the Forkfile"
            .to_owned()
    })
}

/// Resolve one service's environment through the canonical evaluator and the project's `0600`
/// vault (with the OS CSPRNG as the only entropy source).
fn resolve_service_env(
    vault_path: &Path,
    fork: &EvaluatedFork,
    service: &str,
) -> Result<BTreeMap<String, TaintedString>, String> {
    let provider = FileVaultProvider::open(vault_path).map_err(|err| err.to_string())?;
    let entropy = OsCsprngEntropy;
    let resolvers = Resolvers::new(&provider, &entropy);
    fork.resolve_env(service, &resolvers)
        .map_err(|diagnostics| slipfork::project::forkfile_error(diagnostics).to_string())
}

/// The exact document one `env` invocation writes: NDJSON values or `KEY=value` lines.
///
/// Human output dims each key and prints a hidden secret as bold yellow `[REDACTED]`; the `--json`
/// document is byte-pure, and `color` is the composition root's one resolved decision (never a
/// probe here).
fn render_env(
    values: &BTreeMap<String, TaintedString>,
    json: bool,
    reveal: bool,
    color: bool,
) -> Result<String, String> {
    if json {
        let document = values
            .iter()
            .map(|(key, value)| (key.clone(), json_value(value, reveal)))
            .collect::<serde_json::Map<_, _>>();
        let mut text = render_json(&serde_json::Value::Object(document))?;
        text.push('\n');
        return Ok(text);
    }
    let mut text = String::new();
    for (key, value) in values {
        let key_text = if color {
            format!("{}", key.as_str().dimmed())
        } else {
            key.clone()
        };
        text.push_str(&key_text);
        text.push('=');
        text.push_str(&rendered(value, reveal, color));
        text.push('\n');
    }
    Ok(text)
}

/// One JSON value: a redacted secret is `null`, everything else is its string.
fn json_value(value: &TaintedString, reveal: bool) -> serde_json::Value {
    if value.is_tainted() && !reveal {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(value.expose().to_owned())
    }
}

/// One human value: the canonical `[REDACTED]` spelling for a hidden secret.
fn rendered(value: &TaintedString, reveal: bool, color: bool) -> String {
    if value.is_tainted() && !reveal {
        if color {
            format!("{}", value.redacted().yellow().bold())
        } else {
            value.redacted().to_owned()
        }
    } else {
        value.expose().to_owned()
    }
}

/// `slipfork run <task> [args...]`: run a `Forkfile` task inside its service, streaming output.
///
/// A task's argv is resolved by the same canonical evaluator (`$env`, `$port`, and entity
/// references included), a multi-line block rejects trailing arguments, and the first non-zero
/// step's exit code is returned verbatim.
pub(crate) fn run(
    app: &CliApp,
    world: Option<String>,
    task: String,
    args: Vec<String>,
) -> Result<ExitCode, String> {
    if app.output_mode.json {
        return Err(
            "`slipfork run` streams the child's stdio and mirrors its exit code and has no --json document; next: drop --json, or use `slipfork logs` for retained output"
                .to_owned(),
        );
    }
    if let Some(flag) = refused_forwarded_flag(&args) {
        return Err(forwarded_flag_refusal(flag, &task));
    }
    let cwd = app.ui.cwd()?;
    let client = connect(&app.layout)?;
    let report = client
        .world_processes(&WorldSelector {
            cwd: cwd.clone(),
            world: world.clone(),
        })
        .map_err(|err| err.to_string())?;
    let repo = repo_root(&cwd)?;
    let fork = slipfork::project::load_project_fork(&report.worktree, &report.feature_selection)?;
    let definition = task_definition(&fork, &task, &args)?;
    let service_id = definition.service.clone();
    let commands = definition.commands.clone();
    let Some(service) = slipfork::project::select_service(
        &fork,
        &report.worktree,
        &cwd,
        Some(&service_id),
        &|id| format!("pass -s/--service {id}"),
    )?
    .and_then(|id| fork.services.iter().find(|service| service.id == id)) else {
        return Err(format!(
            "task '{task}' names a service the Forkfile does not declare\n  next: check the task's `in <service>` clause against the declared services"
        ));
    };
    let service_dir = report
        .worktree
        .join(slipfork::project::service_directory(service));
    let provider = FileVaultProvider::open(app.layout.vault_path(&repo.project))
        .map_err(|err| err.to_string())?;
    let entropy = OsCsprngEntropy;
    let resolvers = Resolvers::new(&provider, &entropy);
    let color = app.terminal.color();
    let total = commands.len();
    for (index, command) in commands.iter().enumerate() {
        let mut argv = fork
            .resolve_argv(&service_id, command, &resolvers)
            .map_err(|diagnostics| slipfork::project::forkfile_error(diagnostics).to_string())?
            .into_iter()
            .map(|slot: TaintedString| slot.expose().to_owned())
            .collect::<Vec<_>>();
        if total == 1 {
            argv.extend(args.iter().cloned());
        }
        if total > 1 {
            if color {
                eprintln!(
                    "{} {task} in {}",
                    format!("[{}/{}]", index + 1, total).dimmed(),
                    service_id.as_str().cyan()
                );
            } else {
                eprintln!("[{}/{}] {task} in {service_id}", index + 1, total);
            }
        }
        let target = client
            .exec_target(&ExecTargetRequest {
                cwd: service_dir.clone(),
                world: world.clone(),
                service: ExecService::App(service_id.clone()),
                argv,
            })
            .map_err(|err| err.to_string())?;
        let status = spawn_entry(&target)?;
        if !status.success() {
            eprintln!("  next: slipfork logs {service_id} | slipfork shell {service_id}");
            return Ok(exit_code(&status));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `run`'s trailing vararg steals a flag the CLI owns; refuse instead of shipping it to the task.
fn refused_forwarded_flag(args: &[String]) -> Option<&str> {
    args.first().map(String::as_str).filter(|arg| {
        matches!(*arg, "-w" | "--world" | "--json")
            || arg.starts_with("--world=")
            || arg.starts_with("--json=")
    })
}

/// The one-line refusal naming the swallowed flag and its paste-ready remedy.
fn forwarded_flag_refusal(flag: &str, task: &str) -> String {
    format!(
        "`slipfork run` forwards everything after <task> to the task, so `{flag}` here never reaches the CLI\n  next: put the flag before the task, for example `slipfork run -w <world> {task}`"
    )
}

/// The one declared task named by `slipfork run`, with its argument rules applied.
fn task_definition<'a>(
    fork: &'a EvaluatedFork,
    task: &str,
    args: &[String],
) -> Result<&'a EvaluatedTask, String> {
    let Some(definition) = fork.tasks.iter().find(|entry| entry.name == task) else {
        return Err(format!(
            "the Forkfile names no task '{task}'\n  next: run a declared task with `slipfork run <task>` (tasks: {})",
            task_ids(fork)
        ));
    };
    if definition.multi && !args.is_empty() {
        return Err(format!(
            "task '{task}' is a multi-line block and rejects extra arguments\n  next: drop the extra arguments, or run a single-command task"
        ));
    }
    Ok(definition)
}

fn task_ids(fork: &EvaluatedFork) -> String {
    let ids: Vec<&str> = fork.tasks.iter().map(|entry| entry.name.as_str()).collect();
    if ids.is_empty() {
        "none".to_owned()
    } else {
        ids.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use slipfork_forkfile::{FileVaultProvider, Namespace};
    use slipfork_wire::control::FeatureSelection;

    use super::*;

    /// One real manifest: a secret generator, a plain variable, and a feature-gated override.
    const MANIFEST: &str = "\
project acme

features {
    local
    extra
}

service api ./api {
    port 3000
    env {
        PLAIN_VAR = \"verbatim\"
        SECRET_KEY = secret(SECRET_KEY) ?? random_string(24)
    }
    on extra: env.PLAIN_VAR = \"from-extra\"
}

on extra {
    task hidden in api: /bin/echo hidden
}

task say-hi in api: /bin/echo hello
task block in api {
    /bin/echo one
    /bin/echo two
}
";

    const NO_SERVICE: &str = "\
project acme

features {
    local
}
";

    /// A real disk project with a `Forkfile` and a not-yet-created `0600` vault path.
    fn project(body: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::Builder::new()
            .prefix("slipfork-interact")
            .tempdir_in("/var/tmp")
            .expect("disk scratch");
        fs::write(dir.path().join("Forkfile"), body).expect("write Forkfile");
        let vault_dir = dir.path().join("vaults");
        fs::create_dir_all(&vault_dir).expect("vault directory");
        (dir, vault_dir.join("acme.json"))
    }

    fn evaluated(dir: &Path, extra: bool) -> EvaluatedFork {
        let selection = FeatureSelection {
            features: if extra {
                vec!["extra".to_owned()]
            } else {
                Vec::new()
            },
            ..FeatureSelection::default()
        };
        slipfork::project::load_project_fork(dir, &selection).expect("a valid evaluated manifest")
    }

    #[test]
    fn a_tainted_secret_redacts_in_text_and_json_while_plain_values_stay_verbatim() {
        let (dir, vault) = project(MANIFEST);
        let fork = evaluated(dir.path(), false);
        let resolved = resolve_service_env(&vault, &fork, "api").expect("the canonical evaluator");

        let text = render_env(&resolved, false, false, false).expect("text document");
        assert!(text.contains("PLAIN_VAR=verbatim"), "got: {text}");
        assert!(text.contains("SECRET_KEY=[REDACTED]"), "got: {text}");
        assert!(
            !text.contains('\u{1b}'),
            "off-TTY text has no ANSI: {text:?}"
        );
        let colored = render_env(&resolved, false, false, true).expect("colored text");
        assert!(
            colored.contains('\u{1b}') && colored.contains("[REDACTED]"),
            "a colored terminal dims keys and highlights the redaction: {colored:?}"
        );
        let stored = FileVaultProvider::open(&vault)
            .expect("the vault the generator wrote")
            .get(Namespace::Secrets, "SECRET_KEY")
            .expect("read the generated secret")
            .expect("the generator fallback stored a value");
        assert!(
            !text.contains(&stored),
            "a secret must never leak into text output: {text}"
        );

        let json = render_env(&resolved, true, false, true).expect("json");
        assert!(
            !json.contains('\u{1b}'),
            "the JSON document is byte-pure: {json:?}"
        );
        let document: serde_json::Value = serde_json::from_str(json.trim()).expect("valid json");
        assert_eq!(document["SECRET_KEY"], serde_json::Value::Null);
        assert_eq!(document["PLAIN_VAR"], "verbatim");
    }

    #[test]
    fn an_f_feature_changes_the_resolved_environment_and_reveal_needs_a_terminal() {
        let (dir, vault) = project(MANIFEST);
        let default = resolve_service_env(&vault, &evaluated(dir.path(), false), "api")
            .expect("default environment");
        assert_eq!(default["PLAIN_VAR"].expose(), "verbatim");

        let extra = resolve_service_env(&vault, &evaluated(dir.path(), true), "api")
            .expect("-F extra environment");
        assert_eq!(extra["PLAIN_VAR"].expose(), "from-extra");

        let refusal = require_reveal_terminal(false, true).expect_err("a pipe cannot reveal");
        assert_eq!(refusal, REVEAL_NEEDS_TTY);
        assert!(refusal.contains("\n  next: "), "got: {refusal}");
        require_reveal_terminal(true, true).expect("a terminal may reveal");
        require_reveal_terminal(false, false).expect("redaction needs no terminal");
    }

    #[test]
    fn a_swallowed_world_flag_is_refused_with_one_next_remedy() {
        let args = ["-w".to_owned(), "main".to_owned()];
        let flag = refused_forwarded_flag(&args).expect("detected");
        assert_eq!(flag, "-w");
        let refusal = forwarded_flag_refusal(flag, "test");
        assert!(refusal.contains("never reaches the CLI"), "got: {refusal}");
        assert!(
            refusal.contains("\n  next: put the flag before the task")
                && refusal.contains("`slipfork run -w <world> test`"),
            "got: {refusal}"
        );
        assert_eq!(refused_forwarded_flag(&["--".to_owned()]), None);
        assert_eq!(refused_forwarded_flag(&["plain".to_owned()]), None);
        assert_eq!(
            refused_forwarded_flag(&["--world=main".to_owned()]),
            Some("--world=main")
        );
    }

    #[test]
    fn a_service_less_manifest_refuses_env_with_one_next_remedy() {
        let (dir, _vault) = project(NO_SERVICE);
        let error = selected_service(
            &evaluated(dir.path(), false),
            dir.path(),
            dir.path(),
            None,
            &|id| format!("pass -s/--service {id}"),
        )
        .expect_err("no service to resolve");
        assert!(error.contains("declares no service"), "got: {error}");
        assert!(error.contains("\n  next: "), "got: {error}");
    }

    #[test]
    fn run_refuses_an_unknown_task_and_a_multi_line_block_with_extra_arguments() {
        let (dir, _vault) = project(MANIFEST);
        let fork = evaluated(dir.path(), false);

        let unknown = task_definition(&fork, "nope", &[]).expect_err("an unknown task");
        assert!(unknown.contains("names no task 'nope'"), "got: {unknown}");
        assert!(
            unknown.contains("say-hi"),
            "the tasks are listed: {unknown}"
        );

        let appended = task_definition(&fork, "block", &["extra".to_owned()])
            .expect_err("a block task rejects trailing arguments");
        assert!(appended.contains("multi-line block"), "got: {appended}");
        assert!(appended.contains("\n  next: "), "got: {appended}");

        let single = task_definition(&fork, "say-hi", &["extra".to_owned()])
            .expect("a single-command task appends the caller's arguments");
        assert!(!single.multi);
        assert_eq!(single.service, "api");

        // The running world's active features decide which tasks exist at all.
        let gated = evaluated(dir.path(), true);
        assert!(
            task_definition(&gated, "hidden", &[]).is_ok(),
            "a task declared under `on extra` must be visible to a world booted with -F extra"
        );
        assert!(
            task_definition(&fork, "hidden", &[]).is_err(),
            "the same task is absent from the default feature view"
        );
    }
}
