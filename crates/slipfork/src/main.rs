//! The `slipfork` binary: the single Composition Root for the whole system.
//!
//! Only this entry point reads ambient process state (`std::env`, the terminal,
//! the working directory) and assembles the constructor-injected tree. Library
//! crates stay pure: they receive their dependencies as arguments and never
//! inspect the environment.

mod cli;
mod commands;
mod render;
mod world;

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let term_is_dumb = std::env::var_os("TERM").is_some_and(|term| term == "dumb");
    let color =
        std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() && !term_is_dumb;
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            eprintln!("error: cannot determine the working directory: {error}");
            eprintln!("next: cd into a readable directory and run `slipfork --help`.");
            return ExitCode::from(2);
        }
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            eprintln!("error: cannot locate the slipfork binary: {error}");
            eprintln!("next: run `slipfork` from an installed binary.");
            return ExitCode::from(2);
        }
    };
    // SAFETY: `geteuid(2)`/`getegid(2)` only read this process's credentials.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    let host = world::HostEnv {
        exe,
        path: std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).collect())
            .unwrap_or_default(),
        runtime_dir: std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}"))),
        arch: std::env::consts::ARCH,
        uid,
        gid,
        resolv_conf: PathBuf::from("/etc/resolv.conf"),
    };

    match cli::CommandLine::parse(&args) {
        Ok(line) => line.run(commands::Session::new(cwd, color, host)),
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("next: run `slipfork --help` to see the available surface.");
            ExitCode::from(2)
        }
    }
}
