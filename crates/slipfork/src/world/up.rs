//! `up`, `down`, `restart`, `fork`, and `drop`: one daemon client, one live boot pane.
//!
//! The daemon owns the detached `__supervise` process, its process group, and every world socket.
//! This module only sends the request, feeds the typed `event` frames into [`BootTui`], and prints
//! exactly one `next:` remedy (or one `specs/02` JSON object under `--json`).

use std::fs;
use std::process::ExitCode;
use std::time::Instant;

use serde_json::{Value, json};
use slipfork_forkfile::Diagnostics;

use crate::boot::BootTui;
use crate::boot::elapsed_text;
use crate::commands::{Session, emit};
use crate::daemon::{DaemonClient, DaemonError};
use crate::ui::paint;

use super::{DownArgs, DropArgs, ForkArgs, RestartArgs, UpArgs, fail};

impl Session {
    /// Boots the world through the daemon and renders every boot node until ready.
    pub fn up(&self, args: &UpArgs) -> ExitCode {
        let project = self.cwd().to_path_buf();
        let source = match fs::read_to_string(project.join("Forkfile")) {
            Ok(source) => source,
            Err(error) => {
                return self.lifecycle_failure(
                    &DaemonError {
                        code: "forkfile".to_owned(),
                        message: format!("cannot read Forkfile: {error}"),
                    },
                    "create a `Forkfile` in this directory, then run `slipfork up`",
                    args.json,
                    false,
                );
            }
        };
        if let Err(diagnostics) = slipfork_forkfile::compile(&source) {
            return self.forkfile_failure(&diagnostics, args.json);
        }
        let mut client = match DaemonClient::connect(self.host()) {
            Ok(client) => client,
            Err(error) => {
                return self.lifecycle_failure(&error, "retry `slipfork up`", args.json, false);
            }
        };
        let began = Instant::now();
        let mut remedy: Option<String> = None;
        let mut line = 0_u32;
        let mut nodes = Vec::new();
        let mut tui = BootTui::boot(
            self.ui(),
            self.terminal().width,
            &format!("Booting {}", self.world_label(None)),
        );
        let outcome = client.call_streaming(
            "up",
            json!({ "project": project, "world": args.world }),
            &mut |event| {
                if event["event"].as_str() == Some("failed") && tui.failed() {
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
                        "read `.slipfork/supervisor.log`, then run `slipfork up`".to_owned()
                    }
                });
                self.lifecycle_failure(&error, &next, args.json, tui.failed())
            }
        }
    }

    /// Stops the world through the daemon, streaming one row per known pod and the final world row.
    pub fn down(&self, args: &DownArgs) -> ExitCode {
        let json_mode = args.json;
        let mut client = match DaemonClient::connect(self.host()) {
            Ok(client) => client,
            Err(error) => {
                return self.lifecycle_failure(
                    &error,
                    "run `slipfork up` to start a world",
                    json_mode,
                    false,
                );
            }
        };
        let mut elapsed_ms = 0_u64;
        let mut world = self.world_label(None);
        let mut tui = BootTui::rows(
            self.ui(),
            self.terminal().width,
            &format!("Stopping {world}"),
        );
        let outcome =
            client.call_streaming("down", json!({ "project": self.cwd() }), &mut |event| {
                if event["event"].as_str() == Some("stopped") {
                    elapsed_ms = event["elapsed_ms"].as_u64().unwrap_or(elapsed_ms);
                    if let Some(name) = event["world"].as_str() {
                        world = name.to_owned();
                    }
                }
                tui.observe(event);
            });
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
                &error,
                "run `slipfork up` to start a world",
                json_mode,
                tui.failed(),
            ),
        }
    }

    /// Restarts one service or every service in place through the daemon.
    pub fn restart(&self, args: &RestartArgs) -> ExitCode {
        let json_mode = args.json;
        let mut client = match DaemonClient::connect(self.host()) {
            Ok(client) => client,
            Err(error) => {
                return self.lifecycle_failure(
                    &error,
                    "run `slipfork up` if the world is no longer running",
                    json_mode,
                    false,
                );
            }
        };
        let began = Instant::now();
        let mut elapsed_ms = 0_u64;
        let mut nodes = Vec::new();
        let mut tui = BootTui::boot(
            self.ui(),
            self.terminal().width,
            &format!("Restarting {}", self.world_label(None)),
        );
        let outcome = client.call_streaming(
            "restart",
            json!({ "project": self.cwd(), "service": args.service }),
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
                if json_mode {
                    let world = reply["world"].as_str().unwrap_or_default();
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
                ExitCode::SUCCESS
            }
            Err(error) => self.lifecycle_failure(
                &error,
                "run `slipfork up` if the world is no longer running",
                json_mode,
                tui.failed(),
            ),
        }
    }

    /// Forks the running world into its own CoW snapshots and boots it behind its own URL.
    pub fn fork(&self, args: &ForkArgs) -> ExitCode {
        let mut client = match DaemonClient::connect(self.host()) {
            Ok(client) => client,
            Err(error) => {
                return self.lifecycle_failure(&error, "run `slipfork up` first", args.json, false);
            }
        };
        let mut tui = BootTui::boot(
            self.ui(),
            self.terminal().width,
            &format!("Forking {}", args.name.as_deref().unwrap_or("fork")),
        );
        let outcome = client.call_streaming(
            "fork",
            json!({ "project": self.cwd(), "name": args.name }),
            &mut |event| tui.observe(event),
        );
        match outcome {
            Ok(reply) => {
                if args.json {
                    tui.finish();
                    return emit_json(&json!({
                        "fork": reply["fork"],
                        "world": reply["world"],
                        "project": reply["project"],
                        "url": reply["url"],
                        "elapsed_ms": reply["elapsed_ms"],
                        "snapshot_ms": reply["snapshot_ms"],
                    }));
                }
                tui.finish();
                ExitCode::SUCCESS
            }
            Err(error) => {
                tui.finish();
                let next = match error.code.as_str() {
                    "not-btrfs" => {
                        "move the project onto a btrfs mount, then retry `slipfork fork`".to_owned()
                    }
                    "git-branch" | "git-missing" | "invalid-fork" => {
                        "pass a fork name: `slipfork fork <name>`".to_owned()
                    }
                    "no-world" => "run `slipfork up` first".to_owned(),
                    "checkpoint-failed" => {
                        "run `slipfork logs pg` if PostgreSQL stays unhealthy, then retry `slipfork fork`".to_owned()
                    }
                    "world-running" => {
                        "pick another name, or drop the existing fork first".to_owned()
                    }
                    "boot-failed" => {
                        "read `.slipfork/supervisor.log`, then run `slipfork up`".to_owned()
                    }
                    _ => "run `slipfork status` to inspect the world".to_owned(),
                };
                self.lifecycle_failure(&error, &next, args.json, tui.failed())
            }
        }
    }

    /// Stops a fork and deletes both of its snapshots.
    pub fn drop(&self, args: &DropArgs) -> ExitCode {
        let mut client = match DaemonClient::connect(self.host()) {
            Ok(client) => client,
            Err(error) => {
                return self.lifecycle_failure(
                    &error,
                    "run `slipfork status` to inspect the world",
                    args.json,
                    false,
                );
            }
        };
        let mut tui = BootTui::rows(
            self.ui(),
            self.terminal().width,
            &format!("Dropping {}", args.name),
        );
        let outcome = client.call_streaming(
            "drop",
            json!({ "project": self.cwd(), "name": args.name, "force": args.force }),
            &mut |event| tui.observe(event),
        );
        match outcome {
            Ok(reply) => {
                let elapsed = reply["elapsed_ms"].as_u64().unwrap_or(0);
                if args.json {
                    tui.finish();
                    return emit_json(
                        &json!({ "dropped": reply["dropped"], "elapsed_ms": elapsed }),
                    );
                }
                let color = self.color();
                tui.line(&format!(
                    "{} Dropped {} {}",
                    paint("✓", "32", color),
                    paint(&args.name, "1", color),
                    paint(&format!("({})", elapsed_text(elapsed)), "2", color)
                ));
                tui.finish();
                ExitCode::SUCCESS
            }
            Err(error) => {
                tui.finish();
                let next = match error.code.as_str() {
                    "dirty-fork" => format!("slipfork drop {} -f", args.name),
                    "not-btrfs" => {
                        "move the project onto a btrfs mount, then retry `slipfork drop`".to_owned()
                    }
                    "no-fork" => "run `slipfork ps` to list this world's forks".to_owned(),
                    _ => "run `slipfork status` to inspect the world".to_owned(),
                };
                self.lifecycle_failure(&error, &next, args.json, tui.failed())
            }
        }
    }

    /// The `.localhost` world label for this project, or `world` when it cannot be derived.
    fn world_label(&self, requested: Option<&str>) -> String {
        super::world_name(self.cwd(), requested)
            .map(|name| name.as_str().to_owned())
            .unwrap_or_else(|_| "world".to_owned())
    }

    /// Renders the first compiler diagnostic as `Forkfile:line:col`, plus its one `next:`.
    fn forkfile_failure(&self, diagnostics: &Diagnostics, json_mode: bool) -> ExitCode {
        let first = diagnostics.first();
        let message = first.map_or_else(
            || "invalid Forkfile".to_owned(),
            |item| {
                format!(
                    "Forkfile:{}:{}: {}",
                    item.span.line(),
                    item.span.col(),
                    item.message
                )
            },
        );
        let next = first.map_or("fix the Forkfile", |item| item.next.as_str());
        self.lifecycle_failure(
            &DaemonError {
                code: "forkfile".to_owned(),
                message,
            },
            next,
            json_mode,
            false,
        )
    }

    /// Prints exactly one `next:` remedy, or one JSON error object under `--json`.
    pub(super) fn lifecycle_failure(
        &self,
        error: &DaemonError,
        next: &str,
        json_mode: bool,
        already_rendered: bool,
    ) -> ExitCode {
        if json_mode {
            println!(
                "{}",
                json!({ "error": {
                    "code": error.code,
                    "message": error.message,
                    "next": next,
                }})
            );
            return ExitCode::FAILURE;
        }
        if already_rendered {
            return ExitCode::FAILURE;
        }
        fail(&error.message, next)
    }
}

/// One completed boot node, projected into the `specs/02` `nodes` array.
fn boot_node(event: &Value) -> Option<Value> {
    if !matches!(
        event["event"].as_str(),
        Some("finished") | Some("up-to-date")
    ) {
        return None;
    }
    let kind = event["kind"].as_str()?;
    let name = event["name"].as_str()?;
    Some(json!({
        "kind": kind,
        "name": name,
        "state": event["state"],
        "duration_ms": event["duration_ms"],
    }))
}

/// Emits one already-built JSON object with zero ANSI.
fn emit_json(value: &Value) -> ExitCode {
    emit(&format!("{value}\n"))
}
