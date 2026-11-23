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
mod install;
mod mounts;
#[path = "daemon/ps.rs"]
mod ps;
mod secrets;
mod status;
mod ui;
mod world;

use std::ffi::OsString;
use std::net::{SocketAddr, TcpListener};
use std::process::ExitCode;
use std::sync::Arc;

use slipfork_vcs::GitWorkspace;

fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let args = argv.get(1..).unwrap_or_default();
    let terminal = compose::terminal(args);
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            eprintln!("error: cannot determine the working directory: {error}");
            eprintln!("next: cd into a readable directory and run `slipfork --help`.");
            return ExitCode::from(2);
        }
    };
    let host = match compose::host(args, &cwd, terminal) {
        Ok(host) => host,
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("next: run `slipfork` from an installed binary.");
            return ExitCode::from(2);
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
                eprintln!("error: {message}");
                eprintln!(
                    "next: run `slipfork __ingress --listen 127.0.0.1:80`, or let systemd pass the socket on stdin."
                );
                return ExitCode::from(2);
            }
        };
        let listener = match mode {
            daemon::IngressMode::Listen(address) if host.uid != 0 => match bind_ingress(address) {
                Ok((listener, bound)) => {
                    println!("slipfork: ingress listening on {bound}");
                    Some(listener)
                }
                Err(error) => {
                    eprintln!("error: cannot bind {address}: {error}");
                    if error.kind() == std::io::ErrorKind::PermissionDenied {
                        eprintln!(
                            "next: sudo slipfork daemon install --ingress  # or set net.ipv4.ip_unprivileged_port_start=0"
                        );
                    } else {
                        eprintln!(
                            "next: choose a free address or stop the process holding {address}"
                        );
                    }
                    return ExitCode::FAILURE;
                }
            },
            _ => None,
        };
        return daemon::run_handoff(&host, mode, listener);
    }
    cli::run(argv, commands::Session::new(cwd, terminal, host))
}

/// Binds the `--listen` ingress and reports the address the kernel actually assigned.
fn bind_ingress(address: SocketAddr) -> std::io::Result<(TcpListener, SocketAddr)> {
    let listener = TcpListener::bind(address)?;
    let bound = listener.local_addr()?;
    Ok((listener, bound))
}
