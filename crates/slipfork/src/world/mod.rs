//! `up`, `down`, and `restart`: the composition-root half of one world.
//!
//! `up` spawns a detached copy of this same binary in the hidden supervisor mode. That process
//! builds the constructor-injected tree, boots the world through `WorldSupervisor`, streams typed
//! `BootEvent`s back over its stdout, then waits for the world init to exit. The init owns the one
//! `<pods>/control.sock` plane that `down`/`restart` speak to through a typed client.

pub(crate) mod features;
mod fork;
mod heads;
mod init;
mod inputs;
mod runtime;
mod supervise;
mod target;
mod up;

pub(crate) use heads::{HEADS_VERSION, ImageHead, WorldHeads};
pub(crate) use inputs::{INGRESS_SCHEME, build_args, image_inputs};
pub(crate) use runtime::capture::RunTarget;
pub(crate) use runtime::frames::{StepPin, log_frame, note_frame, step_frame};
pub(crate) use runtime::notes::RunNotes;
pub(crate) use target::{TargetError, Targets};

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::Value;
use slipfork_forkfile::FeatureEdit;
use slipfork_image::RegistryCredentials;
use slipfork_world::{ForkLedger, WorldName};

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
        init::resolve(
            &self.exe,
            &self.path,
            self.env.get("SLIPFORK_WORLD_INIT").map(String::as_str),
        )
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

/// The one owner of "what world is this checkout": the `ForkLedger` row keyed by its path.
///
/// Identity resolution order is a requested name, then the ledger row whose `checkout_path` is
/// this project (root or fork), then the canonical `<fork>--<parent>` id of a `.forks/<fork>`
/// checkout, then the directory's RFC 1123 slug. `.slipfork/world.json` is never consulted: the
/// ledger owns identity, and a stale record is settled (removed) whenever it is found.
pub(crate) struct WorldIdentities<'a> {
    borrowed: Option<&'a ForkLedger>,
    owned: Option<ForkLedger>,
}

impl<'a> WorldIdentities<'a> {
    /// Injects the ledger a caller holds; `None` is a daemonless reader with no row to consult.
    #[must_use]
    pub(crate) fn new(ledger: Option<&'a ForkLedger>) -> Self {
        Self {
            borrowed: ledger,
            owned: None,
        }
    }

    /// Opens `<state_dir>/state.db` once, so a real ledger error never means "no row".
    ///
    /// An absent `state.db` still opens as an empty, readable ledger; a pending migration, a
    /// newer schema, or an unreadable database fails closed before any record is unlinked.
    pub(crate) fn open(state_dir: PathBuf) -> Result<Self, String> {
        let ledger = ForkLedger::open_reader(state_dir).map_err(|error| error.to_string())?;
        Ok(Self {
            borrowed: None,
            owned: Some(ledger),
        })
    }

    /// The ledger this resolver reads, borrowed or owned.
    pub(crate) fn ledger(&self) -> Option<&ForkLedger> {
        self.borrowed.or(self.owned.as_ref())
    }

    /// Resolves `project`'s world id, then settles any legacy `.slipfork/world.json` record.
    ///
    /// # Errors
    ///
    /// Fails with one line plus `next: rm .slipfork/world.json` when a stale record names a world
    /// other than the ledger's row, so neither identity is silently preferred.
    pub(crate) fn name(
        &self,
        project: &Path,
        requested: Option<&str>,
    ) -> Result<WorldName, String> {
        let (world, registered) = self.resolve(project, requested)?;
        settle_record(project, registered.as_ref())?;
        Ok(world)
    }

    fn resolve(
        &self,
        project: &Path,
        requested: Option<&str>,
    ) -> Result<(WorldName, Option<WorldName>), String> {
        let registered = self.registered(project)?;
        if let Some(name) = requested {
            let world = WorldName::parse(name).map_err(|error| error.to_string())?;
            return Ok((world, registered));
        }
        if let Some(world) = registered.clone() {
            return Ok((world.clone(), Some(world)));
        }
        if let Some((parent, fork)) = slipfork_world::fork_checkout(project) {
            let parent_world = self.resolve(&parent, None)?.0;
            return Ok((
                slipfork_world::StoreLayout::fork_world_name(&parent_world, &fork),
                None,
            ));
        }
        let directory = project
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("world");
        let raw = slipfork_forkfile::DnsLabel::slug(directory)
            .map_err(|error| error.message(directory))?;
        Ok((
            WorldName::parse(&raw).map_err(|error| error.to_string())?,
            None,
        ))
    }

    /// The world name on this checkout's ledger row, root or fork; `None` before registration.
    fn registered(&self, project: &Path) -> Result<Option<WorldName>, String> {
        let Some(ledger) = self.ledger() else {
            return Ok(None);
        };
        let rows = ledger.list().map_err(|error| error.to_string())?;
        let Some(row) = rows
            .worlds
            .into_iter()
            .find(|row| row.checkout_path == project)
        else {
            return Ok(None);
        };
        WorldName::parse(&row.world_name)
            .map(Some)
            .map_err(|error| error.to_string())
    }
}

/// The legacy `<project>/.slipfork/world.json` path; never read as identity.
fn legacy_record(project: &Path) -> PathBuf {
    project.join(".slipfork/world.json")
}

/// Settles a stale `.slipfork/world.json` against the ledger that now owns identity.
///
/// A fresh registration (`registered == None`) ignores and silently removes the record. Once a
/// ledger row exists, a record naming a different world fails closed with one line and one
/// `next: rm .slipfork/world.json`; a matching record is removed because the row is the only owner.
fn settle_record(project: &Path, registered: Option<&WorldName>) -> Result<(), String> {
    let path = legacy_record(project);
    let recorded = fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value.get("world")?.as_str().map(str::to_owned))
        .and_then(|world| WorldName::parse(&world).ok());
    if let (Some(recorded), Some(registered)) = (recorded.as_ref(), registered)
        && recorded != registered
    {
        return Err(format!(
            "`.slipfork/world.json` names world `{recorded}`, but this checkout is registered as `{registered}`\nnext: rm .slipfork/world.json"
        ));
    }
    let _ = fs::remove_file(&path);
    Ok(())
}

pub(super) fn fail(
    message: impl std::fmt::Display,
    next: &str,
    palette: crate::ui::Palette,
) -> ExitCode {
    for line in crate::ui::failure_block(&message.to_string(), next, palette) {
        eprintln!("{line}");
    }
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

    fn legacy(project: &Path, world: &str) {
        fs::create_dir_all(project.join(".slipfork")).expect("create .slipfork");
        fs::write(
            legacy_record(project),
            format!(
                "{{\"world\":\"{world}\",\"socket\":\"/run/s\",\"project\":\"{}\",\"pid\":1}}",
                project.display()
            ),
        )
        .expect("write world.json");
    }

    /// Identity comes from the request, the `.forks` layout, or the slug — never the old record.
    #[test]
    fn name_ignores_the_legacy_record_and_removes_it() {
        let root = temp("slipfork-world-name");
        let identities = WorldIdentities::new(None);

        let plain = root.path().join("plainworld");
        fs::create_dir_all(&plain).expect("create plain project");
        assert_eq!(
            identities.name(&plain, None).expect("slug").as_str(),
            "plainworld",
            "an unregistered project falls back to its directory slug"
        );
        assert_eq!(
            identities
                .name(&plain, Some("explicit"))
                .expect("requested")
                .as_str(),
            "explicit",
            "an explicit requested name always wins"
        );

        let stale = root.path().join("demo");
        fs::create_dir_all(&stale).expect("create recorded project");
        legacy(&stale, "feat-auth--demo");
        assert_eq!(
            identities.name(&stale, None).expect("slug").as_str(),
            "demo",
            "a stale record never beats the directory slug"
        );
        assert!(
            !legacy_record(&stale).exists(),
            "a fresh registration removes the stale record silently"
        );

        let parent = root.path().join("app");
        fs::create_dir_all(&parent).expect("create fork parent");
        let fork = parent.join(".forks").join("feat-auth");
        fs::create_dir_all(&fork).expect("create fork checkout");
        assert_eq!(
            identities.name(&fork, None).expect("fork layout").as_str(),
            "feat-auth--app",
            "a fork checkout uses the canonical <fork>--<parent> id"
        );
        legacy(&fork, "chosen--app");
        assert_eq!(
            identities.name(&fork, None).expect("fork layout").as_str(),
            "feat-auth--app",
            "a fork's stale record never renames the canonical layout id"
        );
        assert!(!legacy_record(&fork).exists());
    }

    /// Once a ledger row exists, a disagreeing record fails closed and is never deleted.
    #[test]
    fn settle_record_fails_closed_on_a_disagreement_and_drops_a_match() {
        let root = temp("slipfork-world-settle");
        let project = root.path().join("demo");
        fs::create_dir_all(&project).expect("create project");
        let registered = WorldName::parse("demo").expect("valid world name");

        legacy(&project, "other--demo");
        let error = settle_record(&project, Some(&registered)).expect_err("conflict must fail");
        assert_eq!(
            error.lines().count(),
            2,
            "a conflict is exactly one line plus one remedy: {error:?}"
        );
        assert!(
            error.starts_with("`.slipfork/world.json` names world `other--demo`"),
            "the verdict names the stale record: {error:?}"
        );
        assert!(
            error.ends_with("next: rm .slipfork/world.json"),
            "the one remedy clears the stale record: {error:?}"
        );
        assert!(
            legacy_record(&project).is_file(),
            "a conflict never deletes the evidence"
        );

        legacy(&project, "demo");
        settle_record(&project, Some(&registered)).expect("a matching record settles");
        assert!(
            !legacy_record(&project).exists(),
            "the ledger row now owns identity, so the record is removed"
        );
    }

    /// A corrupt record names no world, so it is ignored and removed without an error.
    #[test]
    fn corrupt_legacy_record_is_ignored_and_removed() {
        let root = temp("slipfork-world-corrupt");
        let project = root.path().join("demo");
        fs::create_dir_all(project.join(".slipfork")).expect("create .slipfork");
        fs::write(legacy_record(&project), "not json at all").expect("write the corrupt record");

        let identities = WorldIdentities::new(None);
        assert_eq!(
            identities.name(&project, None).expect("slug").as_str(),
            "demo",
            "an unparseable record never blocks the path-derived slug"
        );
        assert!(
            !legacy_record(&project).exists(),
            "an unparseable record is removed with no error"
        );
    }
}
