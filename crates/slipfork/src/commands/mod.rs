//! `fmt` and `migrate`: file I/O and rendering around the `Forkfile` compiler.
//!
//! The composition root hands this session the working directory and the color
//! decision; nothing here reads the environment or probes the terminal.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::json;
use slipfork_forkfile::format_check;

use crate::daemon::DaemonClient;
use crate::render::paint;
use crate::world::HostEnv;
use crate::{doctor, status};

mod streams;
pub use streams::{ExecArgs, LogsArgs, TargetArgs};

/// Parsed `slipfork fmt` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FmtArgs {
    /// Report only; never rewrite the file.
    pub check: bool,
    /// Emit the machine-readable object instead of human text.
    pub json: bool,
    /// The `Forkfile` path, relative to the working directory.
    pub path: PathBuf,
}

/// Parsed `slipfork migrate` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateArgs {
    /// Save the result as `Forkfile` next to the input.
    pub write: bool,
    /// Overwrite an existing `Forkfile`.
    pub force: bool,
    /// Emit the machine-readable object instead of the `Forkfile` text.
    pub json: bool,
    /// The legacy manifest path, relative to the working directory.
    pub path: PathBuf,
}

/// One CLI invocation's environment, injected by the composition root.
#[derive(Debug, Clone)]
pub struct Session {
    cwd: PathBuf,
    color: bool,
    host: HostEnv,
}

impl Session {
    /// The injected working directory.
    #[must_use]
    pub(crate) fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    /// The host state resolved once by the composition root.
    #[must_use]
    pub(crate) fn host(&self) -> &HostEnv {
        &self.host
    }

    /// Builds a session from the injected working directory, color decision, and host state.
    #[must_use]
    pub fn new(cwd: PathBuf, color: bool, host: HostEnv) -> Self {
        Self { cwd, color, host }
    }

    /// Whether ANSI styling is allowed for this invocation.
    #[must_use]
    pub fn color(&self) -> bool {
        self.color
    }

    /// Renders every running world as the `specs/02` status tree.
    pub fn status(&self, json: bool, verbose: bool) -> ExitCode {
        self.render_status(json, verbose, false)
    }

    /// Renders only this directory's world or fork.
    pub fn ps(&self, json: bool, verbose: bool) -> ExitCode {
        self.render_status(json, verbose, true)
    }

    fn render_status(&self, json: bool, verbose: bool, scoped: bool) -> ExitCode {
        let value = match DaemonClient::connect_existing(&self.host) {
            Ok(Some(mut client)) => match client.call_streaming("ps", json!({}), &mut |_| {}) {
                Ok(value) => value,
                Err(error) => {
                    return self.fail(&error.to_string(), "start it with `slipfork up` and retry");
                }
            },
            Ok(None) => json!({ "worlds": [] }),
            Err(error) => {
                return self.fail(
                    &error.to_string(),
                    "check that XDG_RUNTIME_DIR is readable and retry",
                );
            }
        };
        let reply = match status::parse(&value) {
            Ok(reply) => reply,
            Err(error) => {
                return self.fail(
                    &format!("cannot decode the daemon's ps reply: {error}"),
                    "run `slipfork status --json` and report the payload",
                );
            }
        };
        let other_worlds = !reply.worlds.is_empty();
        let reply = if scoped {
            status::scope(&reply, &self.cwd)
        } else {
            reply
        };
        if json {
            return match serde_json::to_string(&reply) {
                Ok(text) => emit(&format!("{text}\n")),
                Err(error) => self.fail(
                    &format!("cannot encode the ps reply: {error}"),
                    "run `slipfork status --json` and report the payload",
                ),
            };
        }
        if scoped && other_worlds && reply.worlds.is_empty() {
            return emit(&status::no_world_line(
                &self.cwd,
                self.host.home.as_deref(),
                self.color,
            ));
        }
        let options = status::Options::new(self.color)
            .with_now(self.host.now_ms)
            .with_home(self.host.home.clone())
            .with_verbose(verbose);
        emit(&status::render(&reply, &options))
    }

    /// Prints the three host requirements and their remedies.
    pub fn doctor(&self, json: bool) -> ExitCode {
        doctor::run(&self.host, &self.cwd, json, self.color)
    }

    /// Writes the per-user unit, and with root the system `:80` socket and profile.
    pub fn daemon_install(&self, ingress: bool, json: bool) -> ExitCode {
        crate::install::install(&self.host.install_host(), ingress, json, self.color)
    }

    /// Formats a `Forkfile` in place, or reports whether it needs formatting.
    pub fn fmt(&self, args: &FmtArgs) -> ExitCode {
        let path = self.cwd.join(&args.path);
        let label = args.path.display().to_string();
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                let message = format!("cannot read {label}: {error}");
                let next = format!("create {label} or pass the path of an existing Forkfile");
                return if args.json {
                    fail_json(&label, &message, None, None, &next)
                } else {
                    self.fail(&message, &next)
                };
            }
        };
        let formatted = match format_check(&source) {
            Ok(formatted) => formatted,
            Err(diags) => {
                if args.json {
                    let first = diags.first();
                    return fail_json(
                        &label,
                        first.map_or("invalid Forkfile", |item| item.message.as_str()),
                        first.map(|item| item.span.line()),
                        first.map(|item| item.span.col()),
                        first.map_or("fix the Forkfile", |item| item.next.as_str()),
                    );
                }
                eprintln!("{}", diags.render_as(&source, &label));
                return ExitCode::FAILURE;
            }
        };
        let canonical = formatted.is_formatted();
        let changed = !canonical;
        if !args.check
            && changed
            && let Err(error) = fs::write(&path, &formatted.formatted)
        {
            let message = format!("cannot write {label}: {error}");
            let next = format!("check that {} is writable", path.display());
            return if args.json {
                fail_json(&label, &message, None, None, &next)
            } else {
                self.fail(&message, &next)
            };
        }
        if args.json {
            let status = emit(&format!(
                "{}\n",
                json!({ "path": label, "formatted": canonical, "changed": changed })
            ));
            return if args.check && changed {
                ExitCode::FAILURE
            } else {
                status
            };
        }
        if args.check {
            if canonical {
                return emit(&format!(
                    "{} {label} is formatted\n",
                    paint("✓", "32", self.color)
                ));
            }
            let diff = slipfork_forkfile::fmt::render_diff(&source, &formatted.formatted, &label);
            eprint!("{}", tint_diff(&diff, self.color));
            eprintln!("next: slipfork fmt {label}");
            return ExitCode::FAILURE;
        }
        if changed {
            emit(&format!(
                "{} formatted {label}\n",
                paint("✓", "32", self.color)
            ))
        } else {
            emit(&format!(
                "{} {label} is already formatted\n",
                paint("✓", "32", self.color)
            ))
        }
    }

    /// Converts a legacy `snow.yml` manifest into a canonical `Forkfile`.
    pub fn migrate(&self, args: &MigrateArgs) -> ExitCode {
        let path = self.cwd.join(&args.path);
        let label = args.path.display().to_string();
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                let message = format!("cannot read {label}: {error}");
                let next = format!("create {label} or pass the path of an existing snow.yml");
                return if args.json {
                    fail_json(&label, &message, None, None, &next)
                } else {
                    self.fail(&message, &next)
                };
            }
        };
        let forkfile = match slipfork_forkfile::migrate(&source) {
            Ok(forkfile) => forkfile,
            Err(diags) => {
                if args.json {
                    let first = diags.first();
                    return fail_json(
                        &label,
                        first.map_or("invalid snow.yml", |item| item.message.as_str()),
                        first.map(|item| item.span.line()),
                        first.map(|item| item.span.col()),
                        first.map_or("fix the manifest", |item| item.next.as_str()),
                    );
                }
                eprintln!("{}", diags.render_as(&source, &label));
                return ExitCode::FAILURE;
            }
        };
        let output = path.with_file_name("Forkfile");
        let existed = output.exists();
        if args.write && existed && !args.force {
            let message = "refusing to overwrite an existing Forkfile".to_string();
            let next = format!("mv Forkfile {}", "Forkfile.old");
            return if args.json {
                fail_json(&label, &message, None, None, &next)
            } else {
                self.fail(&message, &next)
            };
        }
        if args.write {
            if let Err(error) = fs::write(&output, &forkfile) {
                let message = format!("cannot write {}: {error}", output.display());
                let next = format!("check that {} is writable", output.display());
                return if args.json {
                    fail_json(&label, &message, None, None, &next)
                } else {
                    self.fail(&message, &next)
                };
            }
            if existed {
                eprintln!("overwriting Forkfile (--force)");
            }
        }
        if args.json {
            let status = emit(&format!(
                "{}\n",
                json!({ "path": label, "written": args.write, "forkfile": forkfile })
            ));
            if args.write {
                eprintln!("next: slipfork fmt --check Forkfile");
            }
            return status;
        }
        let status = emit(&forkfile);
        if args.write {
            eprintln!("next: slipfork fmt --check Forkfile");
        }
        status
    }

    fn fail(&self, message: &str, next: &str) -> ExitCode {
        eprintln!("error: {message}");
        eprintln!("next: {next}");
        ExitCode::FAILURE
    }
}

/// Writes one already-rendered chunk to stdout without panicking on a closed pipe.
pub(crate) fn emit(text: &str) -> ExitCode {
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: failed to write output: {error}");
            eprintln!("next: check that stdout is writable");
            ExitCode::FAILURE
        }
    }
}

fn fail_json(path: &str, error: &str, line: Option<u32>, col: Option<u32>, next: &str) -> ExitCode {
    let value = json!({
        "error": error,
        "path": path,
        "line": line,
        "col": col,
        "next": next,
    });
    let status = emit(&format!("{value}\n"));
    let _ = status;
    ExitCode::FAILURE
}

/// Tints diff removals red and additions green; never emits an escape otherwise.
fn tint_diff(diff: &str, color: bool) -> String {
    let mut out = String::new();
    for line in diff.lines() {
        let code = if line.starts_with("---") || line.starts_with("+++") || line.starts_with("@@") {
            Some("1")
        } else if line.starts_with('+') {
            Some("32")
        } else if line.starts_with('-') {
            Some("31")
        } else {
            None
        };
        match (color, code) {
            (true, Some(code)) => out.push_str(&paint(line, code, true)),
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    out
}
