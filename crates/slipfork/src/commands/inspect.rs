//! Read verbs: `status`, `ps`, `prune`, and `open`.

use std::process::ExitCode;

use slipfork::browser::{APPROVAL_HINT_PREFIX, NO_BROWSER_REMEDY};
use slipfork_edge::DaemonControl;
use slipfork_wire::control::WorldSelector;

use crate::CliApp;
use crate::commands::control::{connect, repo_root, served_client};
use crate::commands::render_json;

/// `slipfork` / `slipfork status`: one `DaemonControl::status` read rendered by
/// [`slipfork::status`] (`docs/design/dx-0.1.0-ux.md` §3).
///
/// This is a read verb: it never starts a daemon, and a host with no daemon is simply an empty
/// fleet (`○ No worlds running`, exit `0`), never a refusal. When the daemon answers on an ingress
/// port other than `:80`, exactly one actionable line goes to stderr — never to stdout and never
/// under `--json` (§2 rule 4).
pub(crate) fn status(app: &CliApp) -> Result<ExitCode, String> {
    let Some(client) = served_client(&app.layout) else {
        if app.output_mode.json {
            println!("{}", render_json(&slipfork::status::json_empty())?);
        } else {
            print!("{}", slipfork::status::render_worlds(&[], &app.terminal));
        }
        return Ok(ExitCode::SUCCESS);
    };
    let payload = client.status().map_err(|err| err.to_string())?;
    if app.output_mode.json {
        println!("{}", render_json(&slipfork::status::json(&payload))?);
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(hint) = slipfork::status::ingress_hint(payload.ingress.port()) {
        eprintln!("{} {hint}", app.terminal.warn("!"));
    }
    print!("{}", slipfork::status::render(&payload, &app.terminal));
    Ok(ExitCode::SUCCESS)
}

/// `slipfork ps`: one `DaemonControl::world_processes` read rendered by [`slipfork::ps`].
///
/// The world's `state`, its lineage, and every service row come from that single read — never a
/// second `status` call — and the CLI reads no `/proc`, `/sys`, or log directory. A stopped or
/// failed world still renders its rows and exits `0`; only an unknown world fails closed.
pub(crate) fn ps(app: &CliApp, world: Option<String>) -> Result<ExitCode, String> {
    let client = connect(&app.layout)?;
    let report = client
        .world_processes(&WorldSelector {
            cwd: app.ui.cwd()?,
            world,
        })
        .map_err(|err| err.to_string())?;
    if app.output_mode.json {
        println!("{}", render_json(&slipfork::ps::json(&report))?);
    } else {
        print!("{}", slipfork::ps::render(&report, &app.terminal));
    }
    Ok(ExitCode::SUCCESS)
}

/// `slipfork open [--remote] [-p PORT]`: hand the world's typed origin to the one concrete
/// [`Browser`], or print the verified SSH recipe for a laptop (`docs/design/dx-cli-0.1.0.md` §2).
///
/// The world and its origin are the daemon's facts (`DaemonStatus.worlds`), the remote recipe's
/// ports are `DaemonStatus.ingress.port` and the caller's `-p/--port`, and the account/host come from
/// the resolved identity plus `$SSH_CONNECTION`'s server address (falling back to the `uname -n`
/// probe) — never `$USER`/`$HOSTNAME`. An active SSH session auto-selects the recipe, and a non-`:80`
/// ingress announces §2's one stderr line (never under `--json`).
pub(crate) fn open(
    app: &CliApp,
    world: Option<String>,
    remote: bool,
    port: u16,
) -> Result<ExitCode, String> {
    let client = connect(&app.layout)?;
    let status = client.status().map_err(|err| err.to_string())?;
    // Only the cwd-selected form needs a checkout; an explicit `--world` is a fleet-wide lookup
    // over the daemon's own world ids.
    let repo = match world {
        Some(_) => None,
        None => Some(repo_root(&app.ui.cwd()?)?),
    };
    let selection = slipfork::open::resolve(&status.worlds, repo.as_ref(), world.as_deref())?;
    if !app.output_mode.json
        && let Some(hint) = slipfork::status::ingress_hint(status.ingress.port())
    {
        eprintln!("{} {hint}", app.terminal.warn("!"));
    }
    // A headless host reached over SSH gets the paste-ready tunnel without remembering `-r`.
    let remote = remote || app.ui.ssh_connection().is_some();
    if remote {
        let identity = app.ui.identity()?;
        let host = slipfork::open::remote_host(app.ui.ssh_connection(), &app.ui.kernel_hostname());
        let recipe = slipfork::open::remote_recipe(
            &selection.origin,
            status.ingress.port(),
            port,
            &identity.account,
            &host,
        )?;
        if app.output_mode.json {
            println!(
                "{}",
                render_json(&slipfork::open::json(&selection, Some(&recipe)))?
            );
        } else {
            print!(
                "{}",
                slipfork::open::render_remote(&selection, &recipe, &app.terminal)
            );
        }
        return Ok(ExitCode::SUCCESS);
    }
    let browser = app.ui.browser();
    match browser.open(selection.origin.url()) {
        Ok(true) => {}
        Ok(false) => {
            if !app.output_mode.json {
                eprintln!("{APPROVAL_HINT_PREFIX}{}", selection.origin.url());
            }
            return Err(NO_BROWSER_REMEDY.to_owned());
        }
        Err(err) => return Err(err),
    }
    if app.output_mode.json {
        println!("{}", render_json(&slipfork::open::json(&selection, None))?);
    } else {
        print!("{}", slipfork::open::render_local(&selection));
    }
    Ok(ExitCode::SUCCESS)
}

/// `slipfork prune`: preview (default) or reap worlds whose checkout directory is gone.
pub(crate) fn prune(app: &CliApp, dry_run: bool, yes: bool) -> Result<ExitCode, String> {
    let client = connect(&app.layout)?;
    let dry_run = dry_run || !yes;
    let report = client.prune(dry_run).map_err(|err| err.to_string())?;
    if app.output_mode.json {
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
