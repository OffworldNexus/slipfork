//! Boot-event and phase rendering: the only place ANSI colour is written for world verbs.
use crate::commands::Session;
use crate::daemon::DaemonClient;
use crate::render::paint;
use crate::world::DropArgs;
use serde_json::{Value, json};
use std::io::Write;
use std::process::ExitCode;
/// Clears an in-place, colour-only phase frame before the next full line prints.
fn clear_phase(color: bool) {
    if color {
        print!("\r\u{1b}[K");
    }
}
/// Renders one boot `event`; returns `true` when it reported a failure.
pub(super) fn render_event(event: &Value, color: bool, json: bool) -> bool {
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
            let name = event["name"]
                .as_str()
                .or_else(|| event["label"].as_str())
                .unwrap_or("step");
            let line = format!(
                "{} [{step}/{total}] {} {name}",
                paint("→", "1;36", color),
                event["kind"].as_str().unwrap_or("step"),
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
            clear_phase(color);
            let text = event["line"].as_str().unwrap_or_default();
            let prefix = match (event["step"].as_u64(), event["total"].as_u64()) {
                (Some(step), Some(total)) if total > 0 => format!("[{step}/{total}] "),
                _ => String::new(),
            };
            println!("{}", paint(&format!("{prefix}{text}"), "2", color));
            false
        }
        "finished" | "up-to-date" => {
            let line = finish_line(event, color);
            if color {
                print!("\r\u{1b}[K{line}\n");
            } else {
                println!("{line}");
            }
            let _ = std::io::stdout().flush();
            false
        }
        "stopped" => {
            clear_phase(color);
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
            render_failed(event, color);
            true
        }
        "ready" => {
            clear_phase(color);
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
pub(super) fn render_phase(event: &Value, color: bool, json: bool) {
    if json {
        println!("{event}");
        let _ = std::io::stdout().flush();
        return;
    }
    match event["event"].as_str().unwrap_or_default() {
        "started" => {
            let step = event["step"].as_u64().unwrap_or(0);
            let total = event["total"].as_u64().unwrap_or(0);
            let label = event["name"]
                .as_str()
                .or_else(|| event["label"].as_str())
                .unwrap_or("step");
            if color {
                print!("\r{} [{step}/{total}] {label}", paint("→", "1;36", color));
                let _ = std::io::stdout().flush();
            } else {
                println!("→ [{step}/{total}] {label}");
            }
        }
        "finished" | "up-to-date" => {
            let line = finish_line(event, color);
            if color {
                print!("\r\u{1b}[K{line}\n");
            } else {
                println!("{line}");
            }
            let _ = std::io::stdout().flush();
        }
        "failed" => render_failed(event, color),
        "stopped" => {
            clear_phase(color);
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
fn render_failed(event: &Value, color: bool) {
    let label = event["name"]
        .as_str()
        .or_else(|| event["label"].as_str())
        .unwrap_or("step");
    let line = event["line"].as_u64().unwrap_or(0);
    println!("{} {label}", paint("✗", "31", color));
    if line > 0 {
        println!("{}", paint(&format!("  Forkfile:{line}"), "2", color));
    }
    eprintln!(
        "error: {}",
        event["message"].as_str().unwrap_or("step failed")
    );
    let next = match event["remedy"].as_str() {
        Some(remedy) => remedy.to_owned(),
        None if line > 0 => format!("fix Forkfile:{line}, then run `slipfork up`"),
        None => "read `.slipfork/supervisor.log`, then run `slipfork up`".to_owned(),
    };
    eprintln!("next: {next}");
}
fn finish_line(event: &Value, color: bool) -> String {
    let label = event["name"]
        .as_str()
        .or_else(|| event["label"].as_str())
        .unwrap_or("step");
    let detail = if event["event"] == "up-to-date" {
        "up to date".to_owned()
    } else {
        let elapsed = elapsed_text(event["elapsed_ms"].as_u64().unwrap_or(0));
        match event["detail"].as_str().filter(|detail| !detail.is_empty()) {
            Some(detail) => format!("{detail}, {elapsed}"),
            None => elapsed,
        }
    };
    format!(
        "{} {label} {}",
        paint("✓", "32", color),
        paint(&format!("({detail})"), "2", color)
    )
}
pub(super) fn elapsed_text(millis: u64) -> String {
    if millis == 0 {
        "<1ms".to_owned()
    } else if millis < 1000 {
        format!("{millis}ms")
    } else {
        format!("{:.1}s", millis as f64 / 1000.0)
    }
}
impl Session {
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
        let color = self.color();
        let json_mode = args.json;
        let outcome = client.call_streaming(
            "drop",
            json!({ "project": self.cwd(), "name": args.name, "force": args.force }),
            &mut |event| {
                if !json_mode {
                    render_phase(event, color, false);
                }
            },
        );
        match outcome {
            Ok(reply) => {
                let elapsed = reply["elapsed_ms"].as_u64().unwrap_or(0);
                if json_mode {
                    println!(
                        "{}",
                        json!({ "dropped": reply["dropped"], "elapsed_ms": elapsed })
                    );
                    return ExitCode::SUCCESS;
                }
                println!(
                    "{} Dropped {} {}",
                    paint("✓", "32", color),
                    paint(&args.name, "1", color),
                    paint(&format!("({})", elapsed_text(elapsed)), "2", color)
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                let next = match error.code.as_str() {
                    "dirty-fork" => format!("slipfork drop {} -f", args.name),
                    "not-btrfs" => {
                        "move the project onto a btrfs mount, then retry `slipfork drop`".to_owned()
                    }
                    "no-fork" => "run `slipfork ps` to list this world's forks".to_owned(),
                    _ => "run `slipfork status` to inspect the world".to_owned(),
                };
                self.lifecycle_failure(&error, &next, json_mode, false)
            }
        }
    }
}
