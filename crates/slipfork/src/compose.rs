//! Ambient host state, resolved exactly once by the composition root.
//!
//! Nothing below this module reads `std::env`, `/proc`, the terminal, or the working directory:
//! `main` calls these functions once and injects the resulting plain values.

use std::ffi::{CStr, OsStr, OsString};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use crate::mounts;
use crate::ui::UiMode;
use crate::world::HostEnv;

/// Resolves the one output mode for this invocation and whether stdout is a real terminal.
///
/// `--json` wins over everything; a TTY with `NO_COLOR` or `TERM=dumb` degrades to [`UiMode::Plain`]
/// while still counting as interactive for `env --reveal`.
pub fn detect_ui(args: &[OsString]) -> (UiMode, bool) {
    let tty = std::io::stdout().is_terminal();
    let allowed = tty
        && std::env::var_os("NO_COLOR").is_none()
        && !std::env::var_os("TERM").is_some_and(|term| term == "dumb");
    let json = args
        .iter()
        .take_while(|argument| argument.as_os_str() != OsStr::new("--"))
        .any(|argument| argument == "--json");
    let mode = if json {
        UiMode::Json
    } else if allowed {
        UiMode::InteractiveTty
    } else {
        UiMode::Plain
    };
    (mode, tty)
}

/// Assembles the injected host environment for this invocation.
pub fn host(args: &[OsString], cwd: &Path) -> Result<HostEnv, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("cannot locate the slipfork binary: {error}"))?;
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
        .map(|text| mounts::parse_mountinfo(&text))
        .unwrap_or_default();
    let project = active_project(args, cwd);
    let store_root = mounts::store_root(&mounts, &project);
    // Resolve the ingress principal and OpenRC flavour exactly once here; `install` and the
    // handoff receive plain values and never probe ambient host state themselves.
    let ingress_uid = std::env::var("SUDO_UID")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(uid);
    let install_root = std::env::var_os("SLIPFORK_INSTALL_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let ingress_user = passwd_name(ingress_uid);
    let openrc =
        install_root.join("sbin/openrc-run").exists() || install_root.join("etc/conf.d").is_dir();
    Ok(HostEnv {
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
        ingress_uid,
        ingress_user,
        openrc,
        install_root,
        hostname: host_name(),
        display: std::env::var_os("DISPLAY").is_some()
            || std::env::var_os("WAYLAND_DISPLAY").is_some(),
        ssh: std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some(),
        opener,
        env: std::env::vars().collect(),
    })
}

/// Resolves the project directory for this invocation (`--project` under `__supervise`, else `cwd`).
fn active_project(args: &[OsString], cwd: &Path) -> PathBuf {
    if let Some(path) = crate::cli::verbs::Cli::preparse_supervise_project(args) {
        return std::fs::canonicalize(&path).unwrap_or(path);
    }
    cwd.to_path_buf()
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

/// Resolves the passwd name for the injected ingress uid, falling back to the number.
fn passwd_name(uid: u32) -> String {
    let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0_u8; 16 * 1024];
    // SAFETY: `getpwuid_r` only reads the live uid and writes into `record`/`buffer`; `result`
    // points back at `record` on success, which is then initialized.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            record.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() {
        return uid.to_string();
    }
    // SAFETY: `result` is non-null and only set after a successful `getpwuid_r`.
    let record = unsafe { record.assume_init() };
    // SAFETY: `pw_name` is a NUL-terminated string owned by the live passwd record.
    let name = unsafe { CStr::from_ptr(record.pw_name) }
        .to_string_lossy()
        .into_owned();
    if name.is_empty() {
        uid.to_string()
    } else {
        name
    }
}
