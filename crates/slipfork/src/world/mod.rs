//! `up`, `down`, and `restart`: the composition-root half of one world.
//!
//! `up` spawns a detached copy of this same binary in the hidden supervisor mode. That process
//! builds the constructor-injected tree, boots the world through `WorldSupervisor`, streams typed
//! `BootEvent`s back over its stdout, then waits for the world init to exit. The init owns the one
//! `<pods>/control.sock` plane that `down`/`restart` speak to through a typed client.

mod runtime;
mod supervise;
mod up;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::{Deserialize, Serialize};

/// Ambient host state, resolved exactly once in `main.rs` and injected everywhere below.
///
/// Library crates never see this type: it exists so the composition root names `PATH`, the
/// running executable, the tmpfs runtime directory, and the invoking credentials exactly once.
#[derive(Debug, Clone)]
pub struct HostEnv {
    /// The running `slipfork` binary, used to re-exec `__supervise`.
    pub exe: PathBuf,
    /// `PATH` entries, in order.
    pub path: Vec<PathBuf>,
    /// `$XDG_RUNTIME_DIR` (a tmpfs mount), or `/run/user/<uid>`.
    pub runtime_dir: PathBuf,
    /// `$XDG_STATE_HOME/slipfork`, or `~/.local/state/slipfork`.
    pub state_dir: PathBuf,
    /// Host architecture name (`std::env::consts::ARCH`).
    pub arch: &'static str,
    /// Invoking uid.
    pub uid: u32,
    /// Invoking gid.
    pub gid: u32,
    /// Host `resolv.conf` bound read-only into every pod.
    pub resolv_conf: PathBuf,
    /// `$HOME`, used to shorten paths in the status tree.
    pub home: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME` (or `~/.config`), where `daemon install` writes the user unit.
    pub config_dir: PathBuf,
    /// Milliseconds since the Unix epoch when this process started.
    pub now_ms: u64,
    /// UID the system `:80` ingress runs as (`SUDO_UID` when installing as root).
    pub ingress_uid: u32,
    /// Prefix for system paths (`SLIPFORK_INSTALL_ROOT`, `/` by default).
    pub install_root: PathBuf,
}

impl HostEnv {
    /// The minimal install view the shared unit writer needs.
    #[must_use]
    pub(crate) fn install_host(&self) -> crate::install::InstallHost {
        crate::install::InstallHost {
            exe: self.exe.clone(),
            path: self.path.clone(),
            config_dir: self.config_dir.clone(),
            install_root: self.install_root.clone(),
            uid: self.uid,
            ingress_uid: self.ingress_uid,
        }
    }

    pub(super) fn tool(&self, name: &str) -> Result<PathBuf, String> {
        for directory in &self.path {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        Err(format!(
            "`{name}` is not on PATH\nnext: install {name} (crun, util-linux) and retry"
        ))
    }

    pub(super) fn init_binary(&self) -> Result<PathBuf, String> {
        let sibling = self.exe.parent().map(|dir| dir.join("slipfork-world-init"));
        if let Some(path) = sibling
            && path.is_file()
        {
            return Ok(path);
        }
        self.tool("slipfork-world-init")
    }
}

/// Parsed `slipfork up` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpArgs {
    /// Emit the supervisor's typed events as NDJSON.
    pub json: bool,
    /// Override the world name derived from the project directory.
    pub world: Option<String>,
}

/// Parsed `slipfork down` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownArgs {
    /// Emit `{"stopped": true}` instead of the human line.
    pub json: bool,
}

/// Parsed `slipfork restart` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartArgs {
    /// Restart one service, or every service when `None`.
    pub service: Option<String>,
    /// Emit `{"restarted": n}` instead of the human line.
    pub json: bool,
}

/// Parsed hidden `slipfork __supervise` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuperviseArgs {
    /// Project root the supervisor was spawned for.
    pub project: PathBuf,
    /// Validated world name.
    pub world: String,
    /// Keep boot log lines inside the typed NDJSON stream instead of the supervisor log.
    pub json: bool,
}

/// The on-disk handle a separate `down`/`restart` process uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct WorldState {
    pub(super) world: String,
    pub(super) socket: PathBuf,
    pub(super) project: PathBuf,
    pub(super) pid: u32,
}

pub(super) fn state_path(project: &Path) -> PathBuf {
    project.join(".slipfork/world.json")
}

/// Derives the world name from `--world` or the project directory, sanitized to `[a-z0-9._-]`.
pub(crate) fn world_name(
    project: &Path,
    requested: Option<&str>,
) -> Result<slipfork_world::WorldName, String> {
    let raw = match requested {
        Some(name) => name.to_owned(),
        None => project
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("world")
            .to_lowercase()
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' || ch == '.' || ch == '_' {
                    ch
                } else {
                    '-'
                }
            })
            .collect(),
    };
    slipfork_world::WorldName::parse(&raw).map_err(|error| error.to_string())
}

pub(super) fn fail(message: impl std::fmt::Display, next: &str) -> ExitCode {
    let text = message.to_string();
    for line in text.lines() {
        if let Some(remedy) = line.strip_prefix("next: ") {
            eprintln!("next: {remedy}");
        } else if !line.is_empty() {
            eprintln!("error: {line}");
        }
    }
    eprintln!("next: {next}");
    ExitCode::FAILURE
}
