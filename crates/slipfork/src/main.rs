//! The `slipfork` binary: the single Composition Root for the whole system.
//!
//! Only this entry point reads ambient process state (`std::env`, the terminal,
//! the working directory) and assembles the constructor-injected tree. Library
//! crates stay pure: they receive their dependencies as arguments and never
//! inspect the environment.

mod cli;
mod commands;
mod daemon;
mod doctor;
mod install;
#[path = "daemon/ps.rs"]
mod ps;
mod render;
mod status;
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
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(format!("/var/tmp/slipfork-state-{uid}")));
    // Under `sudo`, install into the invoking user's config dir, never `/root`.
    let invoking_home = std::env::var("SUDO_USER")
        .ok()
        .map(|user| PathBuf::from("/home").join(user));
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| invoking_home.clone());
    let config_base = if uid == 0 {
        invoking_home.or_else(|| home.clone())
    } else {
        home.clone()
    };
    let host = world::HostEnv {
        exe,
        path: std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).collect())
            .unwrap_or_default(),
        runtime_dir: std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}"))),
        state_dir: state_home.join("slipfork"),
        arch: std::env::consts::ARCH,
        uid,
        gid,
        resolv_conf: PathBuf::from("/etc/resolv.conf"),
        home,
        config_dir: std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| config_base.map(|base| base.join(".config")))
            .unwrap_or_else(|| PathBuf::from(format!("/var/tmp/slipfork-config-{uid}"))),
        now_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as u64),
        ingress_uid: std::env::var("SUDO_UID")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(uid),
        install_root: std::env::var_os("SLIPFORK_INSTALL_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/")),
    };

    // The daemon is the same binary in a hidden mode; it owns every world under this UID.
    if args.first().is_some_and(|arg| arg == "__daemon") {
        return daemon::run(host);
    }
    if args.first().is_some_and(|arg| arg == "__ingress") {
        return daemon::run_handoff(&host);
    }
    match cli::CommandLine::parse(&args) {
        Ok(line) => line.run(commands::Session::new(cwd, color, host)),
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("next: run `slipfork --help` to see the available surface.");
            ExitCode::from(2)
        }
    }
}
