//! `up`, `down`, and `restart`: one daemon client, one event renderer.
//!
//! The daemon owns the detached `__supervise` process, its process group, and every world socket.
//! This module only sends the request, renders the typed `event` frames, and prints exactly one
//! `next:` remedy (or one `specs/02` JSON object under `--json`).

use std::fs;
use std::process::ExitCode;
use std::time::Instant;

use serde_json::{Value, json};
use slipfork_forkfile::Diagnostics;

use crate::commands::{Session, emit};
use crate::daemon::{DaemonClient, DaemonError};
use crate::render::paint;

use super::{DownArgs, ForkArgs, RestartArgs, UpArgs, fail};

mod nodes;

use nodes::{elapsed_text, render_event, render_phase};

impl Session {
    /// Boots the world through the daemon and renders every boot phase until ready.
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
        let color = self.color();
        let json_mode = args.json;
        let began = Instant::now();
        let mut failed = false;
        let mut remedy: Option<String> = None;
        let mut line = 0_u32;
        let mut nodes = Vec::new();
        let outcome = client.call_streaming(
            "up",
            json!({ "project": project, "world": args.world }),
            &mut |event| {
                if event["event"].as_str() == Some("failed") && failed {
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
                if json_mode {
                    failed |= event["event"].as_str() == Some("failed");
                } else {
                    failed |= render_event(event, color, false);
                }
            },
        );
        match outcome {
            Ok(reply) if !failed => {
                if json_mode {
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
                self.lifecycle_failure(&error, &next, json_mode, failed)
            }
        }
    }

    /// Stops the world through the daemon; its Btrfs data subvolumes stay intact.
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
        let color = self.color();
        let mut elapsed_ms = 0_u64;
        let mut world = String::new();
        let outcome =
            client.call_streaming("down", json!({ "project": self.cwd() }), &mut |event| {
                if event["event"].as_str() == Some("stopped") {
                    elapsed_ms = event["elapsed_ms"].as_u64().unwrap_or(elapsed_ms);
                    if let Some(name) = event["world"].as_str() {
                        world = name.to_owned();
                    }
                } else if !json_mode {
                    render_phase(event, color, false);
                }
            });
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
                false,
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
        let color = self.color();
        let began = Instant::now();
        let mut elapsed_ms = 0_u64;
        let mut nodes = Vec::new();
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
                if !json_mode {
                    render_phase(event, color, false);
                }
            },
        );
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
                false,
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
        let color = self.color();
        let json_mode = args.json;
        let began = Instant::now();
        let mut forked = false;
        let outcome = client.call_streaming(
            "fork",
            json!({ "project": self.cwd(), "name": args.name }),
            &mut |event| {
                if json_mode {
                    return;
                }
                if event["event"].as_str() == Some("ready") {
                    let name = event["fork"].as_str().unwrap_or("fork");
                    let url = event["url"].as_str().unwrap_or_default();
                    let elapsed = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
                    println!(
                        "{} Forked {} {} -> {}",
                        paint("✓", "32", color),
                        paint(name, "1", color),
                        paint(&format!("({})", elapsed_text(elapsed)), "2", color),
                        paint(url, "1;36", color)
                    );
                    forked = true;
                    return;
                }
                let _ = render_event(event, color, false);
            },
        );
        match outcome {
            Ok(reply) => {
                if json_mode {
                    return emit_json(&json!({
                        "fork": reply["fork"],
                        "world": reply["world"],
                        "project": reply["project"],
                        "url": reply["url"],
                        "elapsed_ms": reply["elapsed_ms"],
                        "snapshot_ms": reply["snapshot_ms"],
                    }));
                }
                if !forked {
                    let name = reply["fork"].as_str().unwrap_or("fork");
                    let elapsed = reply["elapsed_ms"].as_u64().unwrap_or(0);
                    let url = reply["url"].as_str().unwrap_or_default();
                    println!(
                        "{} Forked {} {} -> {}",
                        paint("✓", "32", color),
                        paint(name, "1", color),
                        paint(&format!("({})", elapsed_text(elapsed)), "2", color),
                        paint(url, "1;36", color)
                    );
                }
                ExitCode::SUCCESS
            }
            Err(error) => {
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
                self.lifecycle_failure(&error, &next, json_mode, false)
            }
        }
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
