//! Boot and teardown verbs: `up`, `fork`, `restart`, `down`, and `drop`.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde_json::json;
use slipfork::cli::{DumpArgs, FeatureArgs, ResourceArgs};
use slipfork_edge::{DaemonControl, DaemonControlClient, EdgeError};
use slipfork_vcs::WORKTREES_DIRECTORY;
use slipfork_wire::control::{
    BootEvent, DropForkRequest, ForkRequest, RestartRequest, UpRequest, WorldSelector,
    WorldUpSummary,
};

use crate::CliApp;
use crate::commands::control::{connect, connect_or_spawn};
use crate::commands::render_json;
use slipfork::hook_failure::HookFailure;

/// `slipfork up`: boot the selected world, streaming every daemon event.
pub(crate) fn up(
    app: &CliApp,
    world: Option<String>,
    dump: DumpArgs,
    resources: ResourceArgs,
    features: FeatureArgs,
    host_caches: bool,
) -> Result<ExitCode, String> {
    let client = connect_or_spawn(app)?;
    app.terminal.start_progress("booting the world…");
    let options = block_options(app.terminal.verbose(), world.is_some());
    let request = UpRequest {
        cwd: app.ui.cwd()?,
        world,
        dump: dump.request(),
        resources: resources.request(),
        host_caches,
        feature_selection: features.selection(),
    };
    let mut failure = None;
    match client.up(&request, &mut |event| {
        note_hook_failure(&event, &mut failure);
        print_event(app, event);
    }) {
        Ok(summary) => {
            print_world_up(app, &client, slipfork::restart::Verb::Up, options, &summary)?;
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => fail_with_rescue(app, failure.as_ref(), err),
    }
}

/// `slipfork restart`: one `DaemonControl::restart` round trip, never `down` + `up` composed here.
///
/// The verb refuses to spawn a daemon (only `up`/`fork` may, `ADR-0011` §3): a restart of a world
/// the daemon never booted fails closed in one line naming `slipfork up`. A `failed` world is the
/// rescue path — the daemon re-runs the same boot pipeline in place.
pub(crate) fn restart(
    app: &CliApp,
    world: Option<String>,
    features: FeatureArgs,
) -> Result<ExitCode, String> {
    let client = connect(&app.layout)?;
    app.terminal.start_progress("restarting the world…");
    let options = block_options(app.terminal.verbose(), world.is_some());
    let request = RestartRequest {
        selector: WorldSelector {
            cwd: app.ui.cwd()?,
            world,
        },
        feature_selection: features.selection(),
    };
    let mut failure = None;
    match client.restart(&request, &mut |event| {
        note_hook_failure(&event, &mut failure);
        print_event(app, event);
    }) {
        Ok(summary) => {
            print_world_up(
                app,
                &client,
                slipfork::restart::Verb::Restart,
                options,
                &summary,
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => fail_with_rescue(app, failure.as_ref(), err),
    }
}

/// `slipfork fork`: create `.worktrees/<fork>` plus its CoW world through `slipforkd`.
///
/// `fork = None` lets `slipforkd` derive the name from the current branch (`§7.1`); the client never
/// runs `git` and never reads `.git`.
pub(crate) fn fork(
    app: &CliApp,
    fork: Option<String>,
    base: Option<String>,
    dump: DumpArgs,
    resources: ResourceArgs,
    features: FeatureArgs,
    host_caches: bool,
) -> Result<ExitCode, String> {
    let client = connect_or_spawn(app)?;
    app.terminal.start_progress("forking the world…");
    let options = block_options(app.terminal.verbose(), true);
    let request = ForkRequest {
        cwd: app.ui.cwd()?,
        fork,
        base,
        dump: dump.request(),
        resources: resources.request(),
        host_caches,
        feature_selection: features.selection(),
    };
    let mut failure = None;
    match client.fork(&request, &mut |event| {
        note_hook_failure(&event, &mut failure);
        print_event(app, event);
    }) {
        Ok(summary) => {
            print_world_up(
                app,
                &client,
                slipfork::restart::Verb::Fork,
                options,
                &summary,
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => fail_with_rescue(app, failure.as_ref(), err),
    }
}

/// `slipfork down`: stop a world's services while keeping its worktree (idempotent).
pub(crate) fn down(app: &CliApp, world: Option<String>) -> Result<ExitCode, String> {
    let client = connect(&app.layout)?;
    let selector = WorldSelector {
        cwd: app.ui.cwd()?,
        world,
    };
    let released = client.down(&selector).map_err(|err| err.to_string())?;
    if app.output_mode.json {
        println!("{}", json!({ "ok": true, "released": released }));
    } else if released {
        println!("[down] world stopped (worktree kept)");
    } else {
        println!("[down] no running world matched; nothing to do");
    }
    Ok(ExitCode::SUCCESS)
}

/// `slipfork drop [FORK] [-f]`: destroy a fork world and remove its checkout.
///
/// An omitted fork defaults to the `.worktrees/<fork>` checkout containing `cwd`; outside one it
/// fails closed naming the explicit form rather than guessing a destructive target. A checkout with
/// uncommitted or untracked paths is never destroyed silently: the refusal is one line plus the
/// `-f` remedy, and only `-f/--force` discards those changes.
pub(crate) fn drop_fork(
    app: &CliApp,
    fork: Option<String>,
    force: bool,
) -> Result<ExitCode, String> {
    let client = connect(&app.layout)?;
    let cwd = app.ui.cwd()?;
    let fork = match fork {
        Some(fork) => fork,
        None => cwd_fork(&cwd)?,
    };
    if !force && let Some(changed) = dirty_fork(&cwd, &fork)? {
        return Err(dirty_refusal(&fork, changed));
    }
    let request = DropForkRequest { cwd, fork };
    let released = client.drop_fork(&request).map_err(|err| err.to_string())?;
    if app.output_mode.json {
        println!("{}", json!({ "ok": true, "dropped": released }));
    } else if released {
        println!("[drop] fork world and worktree removed");
    } else {
        println!("[drop] no such fork world; nothing to do");
    }
    Ok(ExitCode::SUCCESS)
}

/// The fork whose `.worktrees/<fork>` checkout contains `cwd`.
fn cwd_fork(cwd: &Path) -> Result<String, String> {
    let mut components = cwd.components();
    while let Some(component) = components.next() {
        if component.as_os_str() == WORKTREES_DIRECTORY
            && let Some(fork) = components.next()
        {
            return Ok(fork.as_os_str().to_string_lossy().into_owned());
        }
    }
    Err(format!(
        "cannot tell which fork `{}` belongs to; next: slipfork drop <fork>",
        cwd.display()
    ))
}

/// The `.worktrees/<fork>` checkout this invocation may destroy, if it exists on disk.
///
/// The selector is resolved by the one grammar `project::world_worktree` already owns
/// (`<project>`, `<project>:<fork>`, or `<fork>`), so a qualified id can never silently miss the
/// dirty guard; only a path that really is a fork checkout beneath `.worktrees/` is returned.
fn fork_checkout(cwd: &Path, selector: &str) -> Option<PathBuf> {
    let repo = crate::commands::control::repo_root(cwd).ok()?;
    let path = slipfork::project::world_worktree(&repo, Some(selector)).ok()?;
    let checkouts = repo.vcs_root.join(WORKTREES_DIRECTORY);
    (path.starts_with(&checkouts) && path.is_dir()).then_some(path)
}

/// How many paths the fork checkout has changed relative to its branch (`None` = clean/unknown).
///
/// `git status --porcelain=v1 -z --untracked-files=all` is the exact view `slipfork-vcs` snapshots;
/// a checkout the probe cannot read (foreign backend, absent git) yields `None` so the daemon, not a
/// guessed client answer, owns the authoritative refusal.
fn dirty_fork(cwd: &Path, fork: &str) -> Result<Option<usize>, String> {
    let Some(checkout) = fork_checkout(cwd, fork) else {
        return Ok(None);
    };
    let output = Command::new("git")
        .arg("-C")
        .arg(&checkout)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();
    let Ok(output) = output else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    // `-z` separates entries with NUL; a rename contributes a second, empty-terminated field.
    let changed = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .count();
    Ok((changed > 0).then_some(changed))
}

/// The one-line dirty-fork refusal plus its `-f` remedy (never a bare "dirty worktree").
fn dirty_refusal(fork: &str, changed: usize) -> String {
    format!(
        "fork '{fork}' has {changed} uncommitted path(s) in its checkout\n  next: commit or stash them, or run `slipfork drop {fork} --force` to discard"
    )
}

/// Remember the last hook failure so a failed `up` / `fork` can print the rescue block.
fn note_hook_failure(event: &BootEvent, failure: &mut Option<BootEvent>) {
    if matches!(event, BootEvent::HookFailed { .. }) {
        *failure = Some(event.clone());
    }
}

/// Print the inline hook failure block (last 15 log lines + `Forkfile:<line>`) and its rescue.
fn fail_with_rescue(
    app: &CliApp,
    failure: Option<&BootEvent>,
    error: EdgeError,
) -> Result<ExitCode, String> {
    app.terminal.finish_progress();
    let Some(failure) = failure.and_then(HookFailure::from_event) else {
        return Err(error.to_string());
    };
    if app.output_mode.json {
        return Err(error.to_string());
    }
    // The hook ran in the world's worktree; the caller's checkout resolves to it without asking the
    // daemon a second time. A non-repository `cwd` simply prints the block without a line.
    let worktree = app
        .ui
        .cwd()
        .and_then(|cwd| crate::commands::control::repo_root(&cwd))
        .ok()
        .map(|repo| repo.worktree_root);
    eprint!("{}", failure.render(worktree.as_deref(), &app.terminal));
    Ok(ExitCode::from(1))
}

/// Print one streamed boot event (never in `--json` mode, which writes only JSON).
///
/// `[resources]` resolution rows are developer diagnostics on an otherwise healthy boot, so they
/// never stream: the block's `-v` detail renders the daemon's recorded `resources` instead
/// (`docs/design/dx-0.1.0-ux.md` §1, §8.2).
fn print_event(app: &CliApp, event: BootEvent) {
    if app.output_mode.json {
        return;
    }
    match event {
        BootEvent::Progress {
            step,
            total,
            phase,
            detail,
        } => {
            app.terminal
                .progress(step, total, &phase, detail.as_deref());
        }
        BootEvent::Resources { .. } => {
            // Rendered once, from the returned summary, and only under `-v`.
        }
        BootEvent::Hook { id, argv } => {
            println!("[hook]      {id}: {}", argv.join(" "));
        }
        BootEvent::HookOutput { chunk, .. } => {
            let mut stream: Box<dyn Write> = match chunk.source {
                slipfork_world::StreamSource::Stdout => Box::new(io::stdout()),
                slipfork_world::StreamSource::Stderr => Box::new(io::stderr()),
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
/// Under `--json` the daemon's [`WorldUpSummary`] is serialized as-is (one projection, never a
/// hand-built subset) with the envelope's `ok`. In human mode exactly one advisory stderr line
/// announces a daemon that is not bound to `:80` (§2 rule 4); it is never written to stdout and
/// never under `--json`.
fn print_world_up(
    app: &CliApp,
    client: &DaemonControlClient,
    verb: slipfork::restart::Verb,
    options: slipfork::restart::Options,
    summary: &WorldUpSummary,
) -> Result<(), String> {
    app.terminal.finish_progress();
    if app.output_mode.json {
        println!("{}", render_json(&slipfork::restart::json(summary)?)?);
        return Ok(());
    }
    note_ingress(client);
    print!(
        "{}",
        slipfork::restart::render(verb, summary, options, &app.terminal)
    );
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// Run real `git` plumbing with pinned identity, exactly like the internals do.
    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=slipfork",
                "-c",
                "user.email=slipfork@localhost",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("spawn git");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn a_dirty_fork_checkout_is_detected_and_clean_or_unknown_is_not() {
        let tmp = tempfile::Builder::new()
            .prefix("slipfork-dirty-")
            .tempdir_in("/var/tmp")
            .expect("disk scratch");
        // A valid directory name is the project slug `repo_root` derives (never a tempdir label).
        let repo = tmp.path().join("demoapp");
        fs::create_dir(&repo).expect("repo directory");
        git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("seed.txt"), "seed\n").expect("seed file");
        git(&repo, &["add", "seed.txt"]);
        git(&repo, &["commit", "-qm", "seed"]);
        git(
            &repo,
            &["worktree", "add", "-q", ".worktrees/feat", "-b", "feat"],
        );
        let worktree = repo.join(WORKTREES_DIRECTORY).join("feat");

        // A clean checkout is `None` (nothing to refuse) and resolves from the repo or the fork.
        assert_eq!(dirty_fork(&repo, "feat").expect("probe"), None);
        assert_eq!(
            fork_checkout(&repo, "feat").as_deref(),
            Some(worktree.as_path())
        );
        assert_eq!(
            fork_checkout(&worktree, "feat").as_deref(),
            Some(worktree.as_path())
        );
        // A project-qualified id resolves through the same canonical grammar, never by skipping.
        assert_eq!(
            fork_checkout(&repo, "demoapp:feat").as_deref(),
            Some(worktree.as_path())
        );

        // One untracked file is the dirty view `-f` is needed to discard.
        fs::write(worktree.join("dirty.txt"), "dirty\n").expect("dirty file");
        assert_eq!(dirty_fork(&repo, "feat").expect("probe"), Some(1));
        assert_eq!(dirty_fork(&repo, "demoapp:feat").expect("probe"), Some(1));
        assert_eq!(dirty_fork(&worktree, "feat").expect("probe"), Some(1));

        // An absent fork never invents a destructive target.
        assert_eq!(fork_checkout(&repo, "missing"), None);
        assert_eq!(dirty_fork(&repo, "missing").expect("probe"), None);

        // The refusal is exactly one line plus one `next:` remedy naming `--force`.
        assert_eq!(
            dirty_refusal("feat", 2),
            "fork 'feat' has 2 uncommitted path(s) in its checkout\n  \
             next: commit or stash them, or run `slipfork drop feat --force` to discard"
        );
        assert!(!dirty_refusal("feat", 2).contains('\x1b'));
    }
}
