//! Boot and teardown verbs: `up`, `fork`, `restart`, `down`, and `drop`.

use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use serde_json::json;
use slipfork::cli::{DumpArgs, FeatureArgs, ResourceArgs};
use slipfork_edge::{DaemonControl, DaemonControlClient, EdgeError};
use slipfork_wire::control::{
    BootEvent, DropForkRequest, ForkRequest, RestartRequest, UpRequest, WorldSelector,
    WorldUpSummary,
};

use crate::CliApp;
use crate::commands::control::{connect, connect_or_spawn};
use crate::commands::render_json;

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

/// `slipfork drop [FORK]`: destroy a fork world and remove its checkout.
///
/// An omitted fork defaults to the `.worktrees/<fork>` checkout containing `cwd`; outside one it
/// fails closed naming the explicit form rather than guessing a destructive target.
pub(crate) fn drop_fork(app: &CliApp, fork: Option<String>) -> Result<ExitCode, String> {
    let client = connect(&app.layout)?;
    let cwd = app.ui.cwd()?;
    let fork = match fork {
        Some(fork) => fork,
        None => cwd_fork(&cwd)?,
    };
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
        if component.as_os_str() == ".worktrees"
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

/// Remember the last hook failure so a failed `up` / `fork` can print the rescue block.
fn note_hook_failure(event: &BootEvent, failure: &mut Option<BootEvent>) {
    if matches!(event, BootEvent::HookFailed { .. }) {
        *failure = Some(event.clone());
    }
}

/// Print the full hook failure and its one-line rescue commands, then exit non-zero.
fn fail_with_rescue(
    app: &CliApp,
    failure: Option<&BootEvent>,
    error: EdgeError,
) -> Result<ExitCode, String> {
    app.terminal.finish_progress();
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
    if app.output_mode.json {
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
