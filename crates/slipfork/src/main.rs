//! The `slipfork` binary: the single Composition Root for the whole system.
//!
//! Only this entry point reads ambient process state (`std::env`, `/proc/self/mountinfo`, the
//! terminal, the working directory) and assembles the constructor-injected tree. Library crates
//! stay pure: they receive their dependencies as arguments and never inspect the environment.

mod boot;
mod cli;
mod commands;
mod compose;
mod daemon;
mod doctor;
mod mounts;
#[path = "daemon/ps.rs"]
mod ps;
mod secrets;
mod status;
mod world;

use slipfork::ui;

use std::ffi::OsString;
use std::net::{SocketAddr, TcpListener};
use std::process::ExitCode;
use std::sync::Arc;

use slipfork_vcs::GitWorkspace;

use crate::ui::report::{Failure, Report, ReportRenderer};

fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let args = argv.get(1..).unwrap_or_default();
    let terminal = compose::terminal(args);
    let renderer = ReportRenderer::new(terminal, compose::wall_zone());
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            let message = format!("cannot determine the working directory: {error}");
            return fail(
                &renderer,
                "cwd",
                message,
                "cd into a readable directory and run `slipfork --help`.",
                2,
            );
        }
    };
    let host = match compose::host(args, &cwd, terminal) {
        Ok(host) => host,
        Err(error) => {
            let (message, next) = ui::split_remedy(&error);
            let next =
                next.unwrap_or_else(|| "run `slipfork` from an installed binary.".to_owned());
            return fail(&renderer, "host", message, next, 2);
        }
    };

    // The daemon is the same binary in a hidden mode; it owns every world under this UID.
    if args.first().is_some_and(|arg| arg == "__daemon") {
        // The daemon is long-lived and must never depend on the cwd of whichever CLI spawned it:
        // that directory may be a fork scratch tree the user deletes while the daemon is alive.
        let _ = std::env::set_current_dir("/");
        // One `git` adapter for the whole daemon: only the composition root resolves the binary.
        // A host without `git` yields `None`, so `fork`/`drop` surface the typed `git-missing`
        // refusal instead of silently falling back to an ambient `PATH` lookup.
        let git = host.tool("git").ok();
        return daemon::run(host, Arc::new(GitWorkspace::new(git)));
    }
    if args.first().is_some_and(|arg| arg == "__ingress") {
        let mode = match daemon::IngressMode::parse(&args[1..]) {
            Ok(mode) => mode,
            Err(message) => {
                return fail(
                    &renderer,
                    "ingress-usage",
                    message,
                    "run `slipfork __ingress --listen 127.0.0.1:80`, or let systemd pass the socket on stdin.",
                    2,
                );
            }
        };
        let listener = match mode {
            daemon::IngressMode::Listen(address) if host.uid != 0 => match bind_ingress(address) {
                Ok((listener, bound)) => {
                    let _ = renderer.emit(&Report::notice(format!(
                        "slipfork: ingress listening on {bound}"
                    )));
                    Some(listener)
                }
                Err(error) => {
                    let message = format!("cannot bind {address}: {error}");
                    let next = if error.kind() == std::io::ErrorKind::PermissionDenied {
                        "sudo systemctl enable --now slipfork-ingress.socket  # or set net.ipv4.ip_unprivileged_port_start=0"
                    } else {
                        "choose a free address or stop the process holding this port"
                    };
                    return fail(&renderer, "ingress-bind", message, next, 1);
                }
            },
            _ => None,
        };
        return daemon::run_handoff(&host, mode, listener, &renderer);
    }
    let clock = commands::LocalClock::new(renderer.wall().clone());
    cli::run(argv, commands::Session::new(cwd, terminal, host, clock))
}

/// Emits one typed failure and returns its exit status.
fn fail(
    renderer: &ReportRenderer,
    code: &str,
    message: impl Into<String>,
    next: impl Into<String>,
    status: u8,
) -> ExitCode {
    let _ = renderer.emit(&Report::failure(Failure::new(code, message, next)));
    ExitCode::from(status)
}

/// Binds the `--listen` ingress and reports the address the kernel actually assigned.
fn bind_ingress(address: SocketAddr) -> std::io::Result<(TcpListener, SocketAddr)> {
    let listener = TcpListener::bind(address)?;
    let bound = listener.local_addr()?;
    Ok((listener, bound))
}
