//! `up`, `down`, and `restart`: the composition-root half of one world.
//!
//! `up` spawns a detached copy of this same binary in the hidden supervisor mode. That process
//! builds the constructor-injected tree, boots the world through `WorldSupervisor`, streams typed
//! `BootEvent`s back over its stdout, then waits for the world init to exit. The init owns the one
//! `<pods>/control.sock` plane that `down`/`restart` speak to through a typed client.

mod runtime;
mod supervise;
mod target;
mod up;

pub(crate) use target::{TargetError, Targets};

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::{Deserialize, Serialize};

/// Ambient host state, resolved exactly once in `main.rs` and injected everywhere below.
///
/// Library crates never see this type: it exists so the composition root names `PATH`, the
/// running executable, the Btrfs mount store, the tmpfs runtime directory, and the invoking
/// credentials exactly once.
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
    /// `<mount>/.slipfork-store` resolved by `main.rs` for the active project directory.
    pub store_root: PathBuf,
    /// Host mount points read once by `main.rs` from `/proc/self/mountinfo`.
    pub mounts: Vec<PathBuf>,
    /// Host architecture name (`std::env::consts::ARCH`).
    pub arch: &'static str,
    /// Invoking uid.
    pub uid: u32,
    /// Invoking gid.
    pub gid: u32,
    /// Resolver file bound read-only as `/etc/resolv.conf` in every pod and image step.
    ///
    /// Selected once by [`crate::compose::pod_resolv_conf`]: a pod lives in its own network
    /// namespace, where the systemd-resolved stub (`nameserver 127.0.0.53`) listens on a
    /// loopback socket that only exists in the host namespace, so it must never reach a pod.
    pub resolv_conf: PathBuf,
    /// `$HOME`, used to shorten paths in the status tree.
    pub home: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME` (or `~/.config`), where `daemon install` writes the user unit.
    pub config_dir: PathBuf,
    /// UID the system `:80` ingress runs as (`SUDO_UID` when installing as root).
    pub ingress_uid: u32,
    /// Passwd name of [`Self::ingress_uid`], resolved once by the composition root.
    pub ingress_user: String,
    /// The install root runs OpenRC (`/sbin/openrc-run` or `/etc/conf.d`).
    pub openrc: bool,
    /// Prefix for system paths (`SLIPFORK_INSTALL_ROOT`, `/` by default).
    pub install_root: PathBuf,
    /// Host name from `gethostname(2)`, read once by the composition root.
    pub hostname: String,
    /// `$DISPLAY` or `$WAYLAND_DISPLAY` is set, so a browser can be launched.
    pub display: bool,
    /// `$SSH_CONNECTION` or `$SSH_TTY` is set, so the session is remote.
    pub ssh: bool,
    /// ANSI styling is allowed for this invocation (`UiMode::color`).
    pub color: bool,
    /// The first `xdg-open`/`open` on the injected `PATH`, if any.
    pub opener: Option<PathBuf>,
    /// The process environment snapshot, read once by the composition root.
    pub env: BTreeMap<String, String>,
}

impl HostEnv {
    /// `<mount>/.slipfork-store` for `project`, selected from the mount table injected by `main.rs`.
    #[must_use]
    pub(crate) fn store_root_for(&self, project: &Path) -> PathBuf {
        if project.as_os_str().is_empty() || self.mounts.is_empty() {
            return self.store_root.clone();
        }
        crate::mounts::store_root(&self.mounts, project)
    }

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
            ingress_user: self.ingress_user.clone(),
            openrc: self.openrc,
        }
    }

    pub(crate) fn tool(&self, name: &str) -> Result<PathBuf, String> {
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

/// Parsed `slipfork up [target]` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpArgs {
    /// Emit the supervisor's typed events as NDJSON.
    pub json: bool,
    /// World, fork, or `.forks/<name>` to boot; `None` means this directory.
    pub target: Option<String>,
}

/// Parsed `slipfork down [target]` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownArgs {
    /// World or fork to stop; `None` means this directory's world.
    pub target: Option<String>,
    /// Emit `{"stopped": true}` instead of the human line.
    pub json: bool,
}

/// Parsed `slipfork restart [target] [-s service]` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartArgs {
    /// Service, world, or fork to restart; `None` means every service here.
    pub target: Option<String>,
    /// Service id to restart; an explicit `-s` always wins over a positional target.
    pub service: Option<String>,
    /// Emit `{"restarted": n}` instead of the human line.
    pub json: bool,
}

/// Parsed `slipfork fork [name]` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkArgs {
    /// Fork name; defaults to the checkout's current git branch.
    pub name: Option<String>,
    /// Emit `{"fork","world","project","url","elapsed_ms","snapshot_ms"}` instead of the summary.
    pub json: bool,
}

/// Parsed `slipfork drop <name> [-f]` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropArgs {
    /// Fork to stop and delete.
    pub name: String,
    /// Discard uncommitted changes in the fork checkout.
    pub force: bool,
    /// Emit `{"dropped","elapsed_ms"}` instead of the summary line.
    pub json: bool,
}

/// Parsed hidden `slipfork __supervise` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuperviseArgs {
    /// Project root the supervisor was spawned for.
    pub project: PathBuf,
    /// Validated world name.
    pub world: String,
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

/// Derives the world name for a project, in resolution order: a requested name, the world
/// recorded in `<project>/.slipfork/world.json`, the canonical `<project>/.forks/<fork>` id
/// (`<fork>--<parent>`), then the directory's RFC 1123 slug.
pub(crate) fn world_name(
    project: &Path,
    requested: Option<&str>,
) -> Result<slipfork_world::WorldName, String> {
    if let Some(name) = requested {
        return slipfork_world::WorldName::parse(name).map_err(|error| error.to_string());
    }
    if let Some(recorded) = recorded_world(project) {
        return Ok(recorded);
    }
    if let Some(forked) = fork_world(project) {
        return Ok(forked);
    }
    let directory = project
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("world");
    let raw =
        slipfork_forkfile::DnsLabel::slug(directory).map_err(|error| error.message(directory))?;
    slipfork_world::WorldName::parse(&raw).map_err(|error| error.to_string())
}

/// The world a live or stopped project recorded in `<project>/.slipfork/world.json`.
fn recorded_world(project: &Path) -> Option<slipfork_world::WorldName> {
    let raw = fs::read_to_string(state_path(project)).ok()?;
    let state: WorldState = serde_json::from_str(&raw).ok()?;
    slipfork_world::WorldName::parse(&state.world).ok()
}

/// The canonical `<fork>--<parent>` world id when `project` is `<parent>/.forks/<fork>`.
fn fork_world(project: &Path) -> Option<slipfork_world::WorldName> {
    let (parent, fork) = slipfork_world::fork_checkout(project)?;
    let parent_world = world_name(&parent, None).ok()?;
    Some(slipfork_world::StoreLayout::fork_world_name(
        &parent_world,
        &fork,
    ))
}

pub(super) fn fail(message: impl std::fmt::Display, next: &str) -> ExitCode {
    let text = message.to_string();
    let mut embedded = None;
    for line in text.lines() {
        if let Some(remedy) = line.strip_prefix("next: ") {
            embedded.get_or_insert(remedy.to_owned());
        } else if !line.is_empty() {
            eprintln!("error: {line}");
        }
    }
    eprintln!("next: {}", embedded.as_deref().unwrap_or(next));
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    fn record(project: &Path, world: &str) {
        fs::create_dir_all(project.join(".slipfork")).expect("create .slipfork");
        fs::write(
            state_path(project),
            format!(
                "{{\"world\":\"{world}\",\"socket\":\"/run/s\",\"project\":\"{}\",\"pid\":1}}",
                project.display()
            ),
        )
        .expect("write world.json");
    }

    /// A stopped project resolves its world from the record, the `.forks` layout, or the slug.
    #[test]
    fn world_name_resolves_recorded_forks_then_the_directory_slug() {
        let root = temp("slipfork-world-name");

        let plain = root.path().join("plainworld");
        fs::create_dir_all(&plain).expect("create plain project");
        assert_eq!(
            world_name(&plain, None).expect("slug").as_str(),
            "plainworld",
            "an unrecorded project falls back to its directory slug"
        );
        assert_eq!(
            world_name(&plain, Some("explicit"))
                .expect("requested")
                .as_str(),
            "explicit",
            "an explicit requested name always wins"
        );

        let recorded = root.path().join("demo");
        fs::create_dir_all(&recorded).expect("create recorded project");
        record(&recorded, "feat-auth--demo");
        assert_eq!(
            world_name(&recorded, None).expect("recorded").as_str(),
            "feat-auth--demo",
            "the recorded world beats the directory slug"
        );
        assert_eq!(
            world_name(&recorded, Some("explicit"))
                .expect("requested")
                .as_str(),
            "explicit",
            "an explicit requested name beats the recorded world"
        );

        let parent = root.path().join("app");
        fs::create_dir_all(&parent).expect("create fork parent");
        let fork = parent.join(".forks").join("feat-auth");
        fs::create_dir_all(&fork).expect("create fork checkout");
        assert_eq!(
            world_name(&fork, None).expect("fork layout").as_str(),
            "feat-auth--app",
            "a fork checkout without a record uses the canonical <fork>--<parent> id"
        );

        record(&fork, "chosen--app");
        assert_eq!(
            world_name(&fork, None).expect("fork record").as_str(),
            "chosen--app",
            "a fork's own record still beats the layout derivation"
        );
    }
}
