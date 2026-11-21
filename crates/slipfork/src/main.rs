//! The `slipfork` binary: the single Composition Root for the whole system.
//!
//! Only this entry point reads ambient process state (`std::env`, `/proc/self/mountinfo`, the
//! terminal, the working directory) and assembles the constructor-injected tree. Library crates
//! stay pure: they receive their dependencies as arguments and never inspect the environment.

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
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
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
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")
        .map(|text| parse_mountinfo(&text))
        .unwrap_or_default();
    let project = active_project(&args, &cwd);
    let store_root = world::store_root_for_mounts(&mounts, &project);
    let host = world::HostEnv {
        exe,
        path,
        runtime_dir: std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}"))),
        state_dir: state_home.join("slipfork"),
        store_root,
        mounts,
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

/// Resolves the project directory for this invocation (`--project` under `__supervise`, else `cwd`).
fn active_project(args: &[OsString], cwd: &Path) -> PathBuf {
    if args.first().is_some_and(|arg| arg == "__supervise") {
        let mut iter = args.iter().skip(1);
        while let Some(arg) = iter.next() {
            if arg == "--project"
                && let Some(raw) = iter.next()
            {
                let path = PathBuf::from(raw);
                return std::fs::canonicalize(&path).unwrap_or(path);
            }
        }
    }
    cwd.to_path_buf()
}

/// Extracts mount points (field 5) in kernel stacking order from `/proc/self/mountinfo`.
fn parse_mountinfo(text: &str) -> Vec<PathBuf> {
    let mut mounts = Vec::new();
    for line in text.lines() {
        let Some((head, _)) = line.split_once(" - ") else {
            continue;
        };
        let Some(mangled) = head.split_ascii_whitespace().nth(4) else {
            continue;
        };
        mounts.push(unmangle_mount_path(mangled));
    }
    mounts
}

/// Decodes Linux `mangle_path` `\NNN` octal escapes (`\040`, `\011`, `\012`, `\134`).
fn unmangle_mount_path(raw: &str) -> PathBuf {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && let (Some(a), Some(b), Some(c)) = (
                octal_digit(bytes[index + 1]),
                octal_digit(bytes[index + 2]),
                octal_digit(bytes[index + 3]),
            )
        {
            decoded.push((a << 6) | (b << 3) | c);
            index += 4;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    PathBuf::from(OsString::from_vec(decoded))
}

const fn octal_digit(byte: u8) -> Option<u8> {
    if byte >= b'0' && byte <= b'7' {
        Some(byte - b'0')
    } else {
        None
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

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_MOUNTINFO: &str = "\
29 1 253:1 / / rw,relatime shared:1 - ext4 /dev/vda1 rw
49 29 0:34 / /var/lib/slipfork rw,noatime shared:46 - btrfs /dev/sda rw,user_subvol_rm_allowed
50 29 0:35 / /var/lib/slipfork2 rw,noatime - btrfs /dev/sdb rw
55 29 0:41 / /mnt/my\\040drive\\134data rw,relatime - btrfs /dev/sdc rw
";

    #[test]
    fn mountinfo_resolves_longest_component_mount_and_unescapes_octal() {
        let mounts = parse_mountinfo(SAMPLE_MOUNTINFO);
        assert_eq!(
            mounts,
            vec![
                PathBuf::from("/"),
                PathBuf::from("/var/lib/slipfork"),
                PathBuf::from("/var/lib/slipfork2"),
                PathBuf::from(r"/mnt/my drive\data"),
            ]
        );
        assert_eq!(
            world::store_root_for_mounts(
                &mounts,
                Path::new("/var/lib/slipfork/t/fork-live-1/demo")
            ),
            PathBuf::from("/var/lib/slipfork/.slipfork-store")
        );
        assert_eq!(
            world::store_root_for_mounts(&mounts, Path::new("/var/lib/slipfork2/demo")),
            PathBuf::from("/var/lib/slipfork2/.slipfork-store")
        );
        assert_eq!(
            world::store_root_for_mounts(&mounts, Path::new("/var/lib/slipfork-other/demo")),
            PathBuf::from("/.slipfork-store")
        );
        assert_eq!(
            world::store_root_for_mounts(&mounts, Path::new(r"/mnt/my drive\data/app")),
            PathBuf::from(r"/mnt/my drive\data/.slipfork-store")
        );
    }
}
