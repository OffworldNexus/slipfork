//! `up`, `down`, and `restart`: one daemon client, one event renderer.
//!
//! The daemon owns the detached `__supervise` process, its process group, and every world socket.
//! This module only sends the request, renders the typed `event` frames, and prints exactly one
//! `next:` remedy (or one JSON error object under `--json`).

use std::fs;
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::commands::Session;
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
        if let Err(diagnostics) = slipfork_forkfile::parse(&source) {
            return self.lifecycle_failure(
                &DaemonError {
                    code: "forkfile".to_owned(),
                    message: diagnostics
                        .first()
                        .map_or("invalid Forkfile".to_owned(), |item| item.message.clone()),
                },
                diagnostics
                    .first()
                    .map_or("fix the Forkfile", |item| item.next.as_str()),
                args.json,
                false,
            );
        }
        let mut client = match DaemonClient::connect(self.host()) {
            Ok(client) => client,
            Err(error) => {
                return self.lifecycle_failure(&error, "retry `slipfork up`", args.json, false);
            }
        };
        let color = self.color();
        let json_mode = args.json;
        let mut failed = false;
        let mut remedy: Option<String> = None;
        let mut line = 0_u32;
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
                failed |= render_event(event, color, json_mode);
            },
        );
        match outcome {
            Ok(_) if !failed => ExitCode::SUCCESS,
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
        self.stream_lifecycle(
            "down",
            json!({ "project": self.cwd() }),
            args.json,
            "run `slipfork up` to start a world",
        )
    }

    /// Restarts one service or every service in place through the daemon.
    pub fn restart(&self, args: &RestartArgs) -> ExitCode {
        self.stream_lifecycle(
            "restart",
            json!({ "project": self.cwd(), "service": args.service }),
            args.json,
            "run `slipfork up` if the world is no longer running",
        )
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
        let began = std::time::Instant::now();
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
                    println!(
                        "{}",
                        json!({
                            "fork": reply["fork"],
                            "world": reply["world"],
                            "project": reply["project"],
                            "url": reply["url"],
                            "elapsed_ms": reply["elapsed_ms"],
                            "snapshot_ms": reply["snapshot_ms"],
                        })
                    );
                    return ExitCode::SUCCESS;
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
                    "git-branch" | "git-missing" => {
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

    fn stream_lifecycle(
        &self,
        method: &str,
        params: Value,
        json_mode: bool,
        next: &str,
    ) -> ExitCode {
        let mut client = match DaemonClient::connect(self.host()) {
            Ok(client) => client,
            Err(error) => return self.lifecycle_failure(&error, next, json_mode, false),
        };
        let color = self.color();
        let raised = std::cell::Cell::new(false);
        let outcome = client.call_streaming(method, params, &mut |event| {
            raised.set(true);
            render_phase(event, color, json_mode);
        });
        match outcome {
            Ok(reply) => {
                if json_mode && raised.get() {
                    // Progress frames already carried the story; echo the final payload once.
                    println!("{reply}");
                }
                ExitCode::SUCCESS
            }
            Err(error) => self.lifecycle_failure(&error, next, json_mode, false),
        }
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
