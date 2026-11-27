//! `up`, `down`, `restart`, `fork`, and `drop`: one daemon client, one live boot pane.
//!
//! The daemon owns the detached `__supervise` process, its process group, and every world socket.
//! This module only sends the request, feeds the typed `event` frames into [`BootTui`], and prints
//! exactly one `next:` remedy (or one `specs/02` JSON object under `--json`).

mod helpers;
mod reply;

use std::process::ExitCode;
use std::time::Instant;

use serde_json::json;

use crate::boot::BootTui;
use crate::commands::Session;
use crate::ui::report::Failure;
use crate::world::features::WorldFeatures;
use crate::world::runtime::frames::ROTATION_NOTE;

use self::reply::{boot_node, ps_reply};

use super::{DownArgs, RestartArgs, Targets, UpArgs, WorldIdentities};

use super::emit::{emit_json, emit_stream};

impl Session {
    /// Boots the world through the daemon and renders every boot node until ready.
    pub fn up(&self, args: &UpArgs) -> ExitCode {
        let identities = match WorldIdentities::open(self.host().state_dir.clone()) {
            Ok(identities) => identities,
            Err(error) => {
                return self.report(
                    "ledger",
                    &error,
                    "run `slipfork doctor` and retry",
                    args.json,
                );
            }
        };
        let local = Targets::new(
            self.cwd(),
            &crate::ps::PsReply::default(),
            identities.ledger(),
        )
        .resolve_local(args.target.as_deref());
        let mut client = None;
        let resolved = match local {
            Some(Ok(resolved)) => resolved,
            Some(Err(error)) => return self.target_failure(&error, args.json),
            None => {
                let mut connected = match self.connect() {
                    Ok(client) => client,
                    Err(error) => {
                        return self.lifecycle_failure(
                            Failure::remedied(
                                error.code.clone(),
                                error.message.clone(),
                                "retry `slipfork up`",
                            ),
                            args.json,
                            false,
                        );
                    }
                };
                let reply = match ps_reply(&mut connected) {
                    Ok(reply) => reply,
                    Err(error) => {
                        return self.lifecycle_failure(
                            Failure::remedied(
                                error.code.clone(),
                                error.message.clone(),
                                "retry `slipfork up`",
                            ),
                            args.json,
                            false,
                        );
                    }
                };
                let resolved = match Targets::new(self.cwd(), &reply, identities.ledger())
                    .resolve(args.target.as_deref())
                {
                    Ok(resolved) => resolved,
                    Err(error) => return self.target_failure(&error, args.json),
                };
                client = Some(connected);
                resolved
            }
        };
        let project = resolved.project;
        let request = WorldFeatures::new(args.no_default_features, args.features.clone());
        let typed = !args.features.is_empty() || args.no_default_features;
        let features = match self.project_features(&project, &request, args.json) {
            Ok(features) => features,
            Err(code) => return code,
        };
        // A typed `-F`/`--no-default-features` becomes the world's persisted configuration; the
        // daemon and supervisor re-read it, so no feature payload crosses the socket.
        if typed && let Err(error) = features.save(&project) {
            return self.report(
                "io",
                &format!(
                    "cannot save `{}`: {error}",
                    WorldFeatures::path(&project).display()
                ),
                "check that the project's `.slipfork` directory is writable",
                args.json,
            );
        }
        let mut client = match client {
            Some(client) => client,
            None => match self.connect() {
                Ok(client) => client,
                Err(error) => {
                    return self.lifecycle_failure(
                        Failure::remedied(
                            error.code.clone(),
                            error.message.clone(),
                            "retry `slipfork up`",
                        ),
                        args.json,
                        false,
                    );
                }
            },
        };
        let began = Instant::now();
        let mut remedy: Option<String> = None;
        let mut line = 0_u32;
        let mut nodes = Vec::new();
        let mut tui = BootTui::boot(
            self.ui(),
            self.terminal().cols,
            &format!("Booting {}", resolved.world.as_str()),
            args.verbose,
        );
        let outcome = client.call_streaming(
            "up",
            json!({
                "project": project,
                "world": resolved.world.as_str(),
                "no_cache": args.no_cache,
            }),
            &mut |event| {
                if event["event"].as_str() == Some("failed") && tui.failed() {
                    return;
                }
                // The daemon rotated a read-only `supervisor.log`; one plain line, never verbose-only
                // and never forwarded to the pane (no double row under `-v`, dropped under `--json`).
                if event["kind"].as_str() == Some(ROTATION_NOTE) {
                    if !args.json {
                        tui.line(event["text"].as_str().unwrap_or_default());
                    }
                    return;
                }
                if let Some(value) = event["remedy"].as_str() {
                    remedy = Some(value.to_owned());
                }
                if let Some(value) = event["line"].as_u64() {
                    line = u32::try_from(value).unwrap_or(0);
                }
                if let Some(node) = boot_node(event) {
                    nodes.push(node);
                }
                emit_stream(args.json, args.verbose, event);
                tui.observe(event);
            },
        );
        tui.finish();
        match outcome {
            Ok(reply) if !tui.failed() => {
                if args.json {
                    let elapsed = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
                    return emit_json(&json!({
                        "world": reply["world"],
                        "url": reply["url"],
                        "elapsed_ms": elapsed,
                        "nodes": nodes,
                        "no_cache": args.no_cache,
                    }));
                }
                ExitCode::SUCCESS
            }
            Ok(_) => ExitCode::FAILURE,
            Err(error) => {
                let next = remedy.unwrap_or_else(|| {
                    if line > 0 {
                        format!("fix Forkfile:{line}, then run `slipfork up`")
                    } else {
                        "run `slipfork logs` for the failing step, then retry `slipfork up`"
                            .to_owned()
                    }
                });
                self.lifecycle_failure(
                    Failure::remedied(error.code.clone(), error.message.clone(), next),
                    args.json,
                    tui.failed(),
                )
            }
        }
    }

    /// Stops the world through the daemon, streaming one row per known pod and the final world row.
    pub fn down(&self, args: &DownArgs) -> ExitCode {
        let json_mode = args.json;
        let identities = match WorldIdentities::open(self.host().state_dir.clone()) {
            Ok(identities) => identities,
            Err(error) => {
                return self.report(
                    "ledger",
                    &error,
                    "run `slipfork doctor` and retry",
                    json_mode,
                );
            }
        };
        let mut client = match self.connect() {
            Ok(client) => client,
            Err(error) => {
                return self.lifecycle_failure(
                    Failure::remedied(
                        error.code.clone(),
                        error.message.clone(),
                        "run `slipfork up` to start a world",
                    ),
                    json_mode,
                    false,
                );
            }
        };
        let reply = if args.target.is_none() {
            crate::ps::PsReply::default()
        } else {
            match ps_reply(&mut client) {
                Ok(reply) => reply,
                Err(error) => {
                    return self.lifecycle_failure(
                        Failure::remedied(
                            error.code.clone(),
                            error.message.clone(),
                            "run `slipfork up` to start a world",
                        ),
                        json_mode,
                        false,
                    );
                }
            }
        };
        let resolved = match Targets::new(self.cwd(), &reply, identities.ledger())
            .resolve(args.target.as_deref())
        {
            Ok(resolved) => resolved,
            Err(error) => return self.target_failure(&error, json_mode),
        };
        let mut elapsed_ms = 0_u64;
        let mut world = resolved.world.as_str().to_owned();
        let mut tui = BootTui::rows(
            self.ui(),
            self.terminal().cols,
            &format!("Stopping {world}"),
        );
        let outcome = client.call_streaming(
            "down",
            json!({ "project": resolved.project }),
            &mut |event| {
                if event["event"].as_str() == Some("stopped") {
                    elapsed_ms = event["elapsed_ms"].as_u64().unwrap_or(elapsed_ms);
                    if let Some(name) = event["world"].as_str() {
                        world = name.to_owned();
                    }
                }
                tui.observe(event);
            },
        );
        tui.finish();
        match outcome {
            Ok(reply) => {
                if json_mode {
                    if world.is_empty() {
                        world = reply["world"].as_str().unwrap_or_default().to_owned();
                    }
                    return emit_json(&json!({ "world": world, "elapsed_ms": elapsed_ms }));
                }
                ExitCode::SUCCESS
            }
            Err(error) => self.lifecycle_failure(
                Failure::remedied(
                    error.code.clone(),
                    error.message.clone(),
                    "run `slipfork up` to start a world",
                ),
                json_mode,
                tui.failed(),
            ),
        }
    }

    /// Restarts one service or every service in place through the daemon.
    pub fn restart(&self, args: &RestartArgs) -> ExitCode {
        let json_mode = args.json;
        let identities = match WorldIdentities::open(self.host().state_dir.clone()) {
            Ok(identities) => identities,
            Err(error) => {
                return self.report(
                    "ledger",
                    &error,
                    "run `slipfork doctor` and retry",
                    json_mode,
                );
            }
        };
        let mut client = match self.connect() {
            Ok(client) => client,
            Err(error) => {
                return self.lifecycle_failure(
                    Failure::remedied(
                        error.code.clone(),
                        error.message.clone(),
                        "run `slipfork up` if the world is no longer running",
                    ),
                    json_mode,
                    false,
                );
            }
        };
        let reply = if args.target.is_none() {
            crate::ps::PsReply::default()
        } else {
            match ps_reply(&mut client) {
                Ok(reply) => reply,
                Err(error) => {
                    return self.lifecycle_failure(
                        Failure::remedied(
                            error.code.clone(),
                            error.message.clone(),
                            "run `slipfork up` if the world is no longer running",
                        ),
                        json_mode,
                        false,
                    );
                }
            }
        };
        let resolved = match Targets::new(self.cwd(), &reply, identities.ledger())
            .resolve_restart(args.target.as_deref(), args.service.as_deref())
        {
            Ok(resolved) => resolved,
            Err(error) => return self.target_failure(&error, json_mode),
        };
        let began = Instant::now();
        let mut elapsed_ms = 0_u64;
        let mut nodes = Vec::new();
        let mut tui = BootTui::boot(
            self.ui(),
            self.terminal().cols,
            &format!("Restarting {}", resolved.world.as_str()),
            args.verbose,
        );
        let outcome = client.call_streaming(
            "restart",
            json!({ "project": resolved.project, "service": resolved.service }),
            &mut |event| {
                if let Some(node) = boot_node(event) {
                    nodes.push(node);
                }
                if let Some(millis) = event["elapsed_ms"].as_u64() {
                    elapsed_ms = millis;
                }
                tui.observe(event);
            },
        );
        tui.finish();
        match outcome {
            Ok(reply) => {
                let world = reply["world"].as_str().unwrap_or_default().to_owned();
                if json_mode {
                    let elapsed_ms = if elapsed_ms == 0 {
                        u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX)
                    } else {
                        elapsed_ms
                    };
                    return emit_json(&json!({
                        "world": world,
                        "url": format!("http://{world}.localhost"),
                        "elapsed_ms": elapsed_ms,
                        "nodes": nodes,
                    }));
                }
                if reply["reconciled"].as_bool() == Some(true) {
                    // The boot stream already printed its own `✓ Ready -> …`; a `Restarted …`
                    // line (with a fabricated `0 pods`, or a single service after a world-wide
                    // rebuild) would contradict the rows the user just watched.
                } else {
                    match &resolved.service {
                        Some(service) => tui.line(&format!("✓ Restarted {service}")),
                        None => {
                            let restarted = reply["restarted"].as_u64().unwrap_or(0);
                            let plural = if restarted == 1 { "pod" } else { "pods" };
                            tui.line(&format!("✓ Restarted {world} ({restarted} {plural})"));
                        }
                    }
                }
                ExitCode::SUCCESS
            }
            Err(error) => self.lifecycle_failure(
                Failure::remedied(
                    error.code.clone(),
                    error.message.clone(),
                    "run `slipfork up` if the world is no longer running",
                ),
                json_mode,
                tui.failed(),
            ),
        }
    }
}
