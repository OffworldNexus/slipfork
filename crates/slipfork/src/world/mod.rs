//! `up`, `down`, and `restart`: the composition-root half of one world.
//!
//! `up` spawns a detached copy of this same binary in the hidden supervisor mode. That process
//! builds the constructor-injected tree, boots the world through `WorldSupervisor`, streams typed
//! `BootEvent`s back over its stdout, then waits for the world init to exit. The init owns the one
//! `<pods>/control.sock` plane that `down`/`restart` speak to through a typed client.

pub(crate) mod features;
mod fork;
mod heads;
mod runtime;
mod supervise;
mod target;
mod up;

pub(crate) use heads::{HEADS_VERSION, ImageHead, WorldHeads};
pub(crate) use runtime::capture::{RunTarget, strip_ansi};
pub(crate) use runtime::frames::{StepPin, log_frame, note_frame, step_frame};
pub(crate) use runtime::notes::RunNotes;
pub(crate) use target::{TargetError, Targets};

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::{Deserialize, Serialize};
use slipfork_forkfile::ast::Forkfile;
use slipfork_forkfile::{FeatureEdit, FeatureSet};
use slipfork_image::RegistryCredentials;
use slipfork_world::ImageNode;

/// The canonical build-input digest per build image (`Dockerfile` bytes + context hash).
///
/// `slipfork-image` owns the `.dockerignore` walk and the digest; every caller (the daemon's
/// staleness check and the world stamp's epoch) folds this same map so a `Dockerfile` or copied
/// context edit always rewrites the key and rebuilds.
pub(crate) fn image_inputs(
    project: &Path,
    doc: &Forkfile,
    images: &[ImageNode],
    features: &FeatureSet,
) -> BTreeMap<String, String> {
    images
        .iter()
        .filter(|image| image.from.is_none())
        .filter_map(|image| {
            let paths = image.paths(project);
            let target = doc.eval_image_stage_with(&image.id, features);
            slipfork_image::build_inputs_digest(
                &paths.context,
                &paths.dockerfile,
                target.as_deref(),
            )
            .ok()
            .map(|digest| (image.id.clone(), digest))
        })
        .collect()
}

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
    /// Prefix for system paths (`SLIPFORK_INSTALL_ROOT`, `/` by default).
    pub install_root: PathBuf,
    /// Host name from `gethostname(2)`, read once by the composition root.
    #[allow(dead_code)] // read by the dormant commands/auth.rs
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
    /// `$XDG_CONFIG_HOME`, else `~/.config`: the root of `slipfork/registries.json`.
    pub config_dir: PathBuf,
    /// Per-host registry credentials, loaded once by the composition root.
    pub registry_credentials: RegistryCredentials,
}

impl HostEnv {
    /// `<mount>/.slipfork-store` (or `<data_home>/slipfork/store`) for `project`.
    ///
    /// The mount table, the data home, and the selected mount's writability are all injected by
    /// this composition root; [`slipfork_world::LogStore::root_for`] owns the rule.
    #[must_use]
    pub(crate) fn store_root_for(&self, project: &Path) -> PathBuf {
        let mount = crate::mounts::mount_for(&self.mounts, project);
        slipfork_world::LogStore::root_for(
            &self.mounts,
            project,
            &self.data_home(),
            can_write(mount),
        )
    }

    /// `$XDG_DATA_HOME`, else `$HOME/.local/share`, read from the once-snapshotted environment.
    fn data_home(&self) -> PathBuf {
        self.env
            .get("XDG_DATA_HOME")
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .or_else(|| self.home.as_ref().map(|home| home.join(".local/share")))
            .unwrap_or_else(|| PathBuf::from(format!("/var/tmp/slipfork-data-{}", self.uid)))
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

/// Whether this user can write `path`; probed by the composition root, never by a library crate.
fn can_write(path: &Path) -> bool {
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `access(2)` only reads this process's effective credentials and `path`'s mode.
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

/// Parsed `slipfork up [target] [-F ...] [--no-default-features]` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpArgs {
    /// Emit the supervisor's typed events as NDJSON.
    pub json: bool,
    /// World, fork, or `.forks/<name>` to boot; `None` means this directory.
    pub target: Option<String>,
    /// Feature edits typed on this invocation; empty means "use the persisted file".
    pub features: Vec<FeatureEdit>,
    /// Start from an empty base instead of the built-in `dev` default.
    pub no_default_features: bool,
    /// Ignore sealed image and step caches for this run, forcing a full rebuild.
    pub no_cache: bool,
    /// `-v`: add the dim `runtime:` plumbing row between a boot verdict and its `next:`.
    pub verbose: bool,
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
    /// `-v`: show engine narration and runtime plumbing rows.
    pub verbose: bool,
}

/// Parsed `slipfork fork [name]` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkArgs {
    /// Fork name; defaults to the checkout's current git branch.
    pub name: Option<String>,
    /// Emit `{"fork","world","project","url","elapsed_ms","snapshot_ms"}` instead of the summary.
    pub json: bool,
    /// Feature edits to apply on top of the fork's inherited `.slipfork/features.json`.
    pub features: Vec<FeatureEdit>,
    /// Start the fork from an empty feature base instead of the inherited configuration.
    pub no_default_features: bool,
    /// `-v`: add the dim `runtime:` plumbing row between a boot verdict and its `next:`.
    pub verbose: bool,
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
    /// The daemon-sealed `WorldHeads` file this boot reads.
    pub heads: PathBuf,
    /// Ignore the trusted world stamp for this boot.
    pub no_cache: bool,
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

/// Splits an embedded `next: <remedy>` line out of a daemon message.
pub(crate) fn split_remedy(message: &str) -> (String, Option<String>) {
    let mut lines = Vec::new();
    let mut remedy = None;
    for line in message.lines() {
        if let Some(next) = line.strip_prefix("next: ") {
            remedy.get_or_insert_with(|| next.to_owned());
        } else if !line.is_empty() {
            lines.push(line);
        }
    }
    (lines.join("\n"), remedy)
}

pub(super) fn fail(message: impl std::fmt::Display, next: &str) -> ExitCode {
    let (body, embedded) = split_remedy(&message.to_string());
    for line in body.lines() {
        eprintln!("error: {line}");
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
