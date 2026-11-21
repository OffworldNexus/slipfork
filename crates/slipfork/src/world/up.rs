//! `up`, `down`, and `restart`: one daemon client, one event renderer.
//!
//! The daemon owns the detached `__supervise` process, its process group, and every world socket.
//! This module only sends the request, renders the typed `event` frames, and prints exactly one
//! `next:` remedy (or one JSON error object under `--json`).

use std::fs;
use std::io::Write;
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::commands::Session;
use crate::daemon::{DaemonClient, DaemonError};
use crate::render::paint;

use super::{DownArgs, RestartArgs, UpArgs, fail};

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
    fn lifecycle_failure(
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

/// Renders one boot `event`; returns `true` when it reported a failure.
fn render_event(event: &Value, color: bool, json: bool) -> bool {
    if json {
        // The single error object is printed by the caller; avoid claiming the world stays up.
        if event["event"].as_str() != Some("failed") {
            println!("{event}");
            let _ = std::io::stdout().flush();
        }
        return event["event"].as_str() == Some("failed");
    }
    match event["event"].as_str().unwrap_or_default() {
        "started" => {
            let step = event["step"].as_u64().unwrap_or(0);
            let total = event["total"].as_u64().unwrap_or(0);
            let line = format!(
                "{} [{step}/{total}] {} {}",
                paint("→", "1;36", color),
                event["kind"].as_str().unwrap_or("step"),
                event["label"].as_str().unwrap_or("step"),
            );
            if color {
                print!("\r{line}");
                let _ = std::io::stdout().flush();
            } else {
                println!("{line}");
            }
            false
        }
        "log" => {
            let text = event["line"].as_str().unwrap_or_default();
            let prefix = match (event["step"].as_u64(), event["total"].as_u64()) {
                (Some(step), Some(total)) if total > 0 => format!("[{step}/{total}] "),
                _ => String::new(),
            };
            println!("{}", paint(&format!("{prefix}{text}"), "2", color));
            false
        }
        "finished" | "up-to-date" => {
            let label = event["label"].as_str().unwrap_or("step");
            let detail = if event["event"] == "up-to-date" {
                "up to date".to_owned()
            } else {
                let elapsed = event["elapsed_ms"].as_u64().unwrap_or(0);
                format!(
                    "{}, {}",
                    event["detail"].as_str().unwrap_or_default(),
                    elapsed_text(elapsed)
                )
            };
            let line = format!(
                "{} {label} {}",
                paint("✓", "32", color),
                paint(&format!("({detail})"), "2", color)
            );
            if color {
                print!("\r\u{1b}[K{line}\n");
            } else {
                println!("{line}");
            }
            let _ = std::io::stdout().flush();
            false
        }
        "stopped" => {
            let world = event["world"].as_str().unwrap_or("world");
            let elapsed = event["elapsed_ms"].as_u64().unwrap_or(0);
            println!(
                "{} Stopped {world} {}",
                paint("✓", "32", color),
                paint(&format!("({})", elapsed_text(elapsed)), "2", color)
            );
            false
        }
        "failed" => {
            let label = event["label"].as_str().unwrap_or("step");
            let line = event["line"].as_u64().unwrap_or(0);
            println!("{} {label}", paint("✗", "31", color));
            let message = event["message"].as_str().unwrap_or("step failed");
            eprintln!("error: {message}");
            match event["remedy"].as_str() {
                Some(remedy) => eprintln!("next: {remedy}"),
                None => eprintln!("next: fix Forkfile:{line}, then run `slipfork up`"),
            }
            true
        }
        "ready" => {
            let url = event["url"].as_str().unwrap_or_default();
            println!(
                "{} {}",
                paint("✓ Ready ->", "1;32", color),
                paint(url, "1;36", color)
            );
            println!(
                "{}",
                paint(
                    "world keeps running; use `slipfork down` when finished",
                    "2",
                    color
                )
            );
            false
        }
        _ => false,
    }
}

/// Renders one teardown/restart phase (`started`, `finished`, `stopped`).
fn render_phase(event: &Value, color: bool, json: bool) {
    if json {
        println!("{event}");
        let _ = std::io::stdout().flush();
        return;
    }
    match event["event"].as_str().unwrap_or_default() {
        "started" => {
            let step = event["step"].as_u64().unwrap_or(0);
            let total = event["total"].as_u64().unwrap_or(0);
            if color {
                print!(
                    "\r{} [{step}/{total}] {}",
                    paint("→", "1;36", color),
                    event["label"].as_str().unwrap_or("step")
                );
                let _ = std::io::stdout().flush();
            } else {
                println!(
                    "→ [{step}/{total}] {}",
                    event["label"].as_str().unwrap_or("step")
                );
            }
        }
        "finished" => {
            let elapsed = event["elapsed_ms"].as_u64().unwrap_or(0);
            let detail = event["detail"].as_str().unwrap_or_default();
            let line = format!(
                "{} {} {}",
                paint("✓", "32", color),
                event["label"].as_str().unwrap_or("step"),
                paint(
                    &format!("({detail}, {})", elapsed_text(elapsed)),
                    "2",
                    color
                )
            );
            if color {
                print!("\r\u{1b}[K{line}\n");
            } else {
                println!("{line}");
            }
            let _ = std::io::stdout().flush();
        }
        "stopped" => {
            let elapsed = event["elapsed_ms"].as_u64().unwrap_or(0);
            println!(
                "{} Stopped {} {}",
                paint("✓", "32", color),
                event["world"].as_str().unwrap_or("world"),
                paint(&format!("({})", elapsed_text(elapsed)), "2", color)
            );
        }
        _ => {}
    }
}

fn elapsed_text(millis: u64) -> String {
    if millis == 0 {
        "<1ms".to_owned()
    } else if millis < 1000 {
        format!("{millis}ms")
    } else {
        format!("{:.1}s", millis as f64 / 1000.0)
    }
}
