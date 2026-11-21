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
use std::sync::Arc;

use slipfork::surface;
use slipfork_vcs::GitWorkspace;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let term_is_dumb = std::env::var_os("TERM").is_some_and(|term| term == "dumb");
    let tty = std::io::stdout().is_terminal();
    let color = tty && std::env::var_os("NO_COLOR").is_none() && !term_is_dumb;
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
    let path: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect())
        .unwrap_or_default();
    let opener = ["xdg-open", "open"].iter().find_map(|name| {
        path.iter()
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    });
    let host = world::HostEnv {
        exe,
        path,
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
        hostname: host_name(),
        display: std::env::var_os("DISPLAY").is_some()
            || std::env::var_os("WAYLAND_DISPLAY").is_some(),
        ssh: std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some(),
        opener,
    };

    // The daemon is the same binary in a hidden mode; it owns every world under this UID.
    if args.first().is_some_and(|arg| arg == "__daemon") {
        // One `git` adapter for the whole daemon: only the composition root resolves the binary.
        // A host without `git` yields `None`, so `fork`/`drop` surface the typed `git-missing`
        // refusal instead of silently falling back to an ambient `PATH` lookup.
        let git = host.tool("git").ok();
        return daemon::run(host, Arc::new(GitWorkspace::new(git)));
    }
    if args.first().is_some_and(|arg| arg == "__ingress") {
        return daemon::run_handoff(&host);
    }
    match cli::CommandLine::parse(&args) {
        Ok(line) => line.run(commands::Session::new(cwd, color, tty, host)),
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("next: run `slipfork --help` to see the available surface.");
            ExitCode::from(2)
        }
    }
}

/// Reads this host's name once; the kernel truncates anything longer than 255 bytes.
fn host_name() -> String {
    let mut buffer = [0_u8; 256];
    // SAFETY: the kernel writes at most `buffer.len()` bytes into the live array.
    let ret = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if ret != 0 {
        return "localhost".to_owned();
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}
