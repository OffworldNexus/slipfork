use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::{Error, NameKind, Result};
use crate::resources::ResourceKind;

/// Name of the store directory Slipfork owns on the Btrfs mount.
pub const STORE_DIR: &str = ".slipfork-store";
/// Read-only image/build-step subvolumes live here.
pub const IMAGES_DIR: &str = "images";
/// Per-world data subvolumes live here.
pub const WORLDS_DIR: &str = "worlds";
/// Ephemeral per-pod writable rootfs clones live here, outside `worlds/<world>`.
pub const PODS_DIR: &str = "pods";
/// Persistent per-pod capture files live here, so `logs` survives `down`.
pub const LOGS_DIR: &str = "logs";
/// Content-addressed sealed nodes live here (`specs/03` §6).
pub const SNAPSHOTS_DIR: &str = "snapshots";
/// Project-relative directory holding project snapshots.
pub const FORKS_DIR: &str = ".forks";

/// Absolute Btrfs store paths owned by Slipfork (`<mount>/.slipfork-store/{images,worlds,pods}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreLayout {
    store_root: PathBuf,
}

impl StoreLayout {
    /// Builds a layout whose store is `<btrfs_mount>/.slipfork-store`.
    #[must_use]
    pub fn new(btrfs_mount: impl Into<PathBuf>) -> Self {
        Self {
            store_root: btrfs_mount.into().join(STORE_DIR),
        }
    }

    /// Builds a layout directly at an injected `store_root` (`<mount>/.slipfork-store`).
    #[must_use]
    pub fn at(store_root: impl Into<PathBuf>) -> Self {
        Self {
            store_root: store_root.into(),
        }
    }

    /// The Btrfs mount (or parent directory) owning [`Self::store_root`].
    #[must_use]
    pub fn btrfs_mount(&self) -> &Path {
        self.store_root
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(&self.store_root)
    }

    /// `<mount>/.slipfork-store`.
    #[must_use]
    pub fn store_root(&self) -> PathBuf {
        self.store_root.clone()
    }

    /// `<mount>/.slipfork-store/images`.
    #[must_use]
    pub fn images_root(&self) -> PathBuf {
        self.store_root.join(IMAGES_DIR)
    }

    /// `<mount>/.slipfork-store/worlds`.
    #[must_use]
    pub fn worlds_root(&self) -> PathBuf {
        self.store_root.join(WORLDS_DIR)
    }

    /// `<mount>/.slipfork-store/pods`.
    #[must_use]
    pub fn pods_root(&self) -> PathBuf {
        self.store_root.join(PODS_DIR)
    }

    /// `<mount>/.slipfork-store/pods/<world>`: ephemeral pod rootfs clones for one world, kept
    /// outside `worlds/<world>` so snapshotting a live world never creates `ino == 2` stubs.
    #[must_use]
    pub fn world_pods(&self, world: &WorldName) -> PathBuf {
        self.pods_root().join(world.as_str())
    }

    /// `<mount>/.slipfork-store/logs`: every persistent pod/image capture file.
    #[must_use]
    pub fn logs_root(&self) -> PathBuf {
        self.store_root.join(LOGS_DIR)
    }

    /// `<mount>/.slipfork-store/snapshots`: every sealed content-addressed node plus `.lock`.
    #[must_use]
    pub fn snapshots_root(&self) -> PathBuf {
        self.store_root.join(SNAPSHOTS_DIR)
    }

    /// `<mount>/.slipfork-store/logs/<world>`: one world's capture directory.
    #[must_use]
    pub fn world_logs(&self, world: &WorldName) -> PathBuf {
        self.logs_root().join(world.as_str())
    }

    /// `<mount>/.slipfork-store/logs/<world>/<id>.log`, the canonical capture file.
    #[must_use]
    pub fn pod_log(&self, world: &WorldName, id: &str) -> PathBuf {
        crate::pod_log::log_path(&self.world_logs(world), id)
    }

    /// Recovers the store layout and world from a pod rootfs `<store>/pods/<world>/<slot>`.
    ///
    /// Pod rootfs clones are created by [`Self::world_pods`], so the init can resolve the world's
    /// canonical log root from the `PodSpec` it was handed instead of probing the environment.
    /// The `.slipfork-store` directory name is the discriminator: a tmpfs run-once bundle such as
    /// `/run/user/1000/slipfork/pods/<world>/<slot>` yields `None`.
    #[must_use]
    pub fn from_pod_rootfs(rootfs: &Path) -> Option<(Self, WorldName)> {
        let world_dir = rootfs.parent()?;
        let world = WorldName::parse(world_dir.file_name()?.to_str()?).ok()?;
        let pods_dir = world_dir.parent()?;
        if pods_dir.file_name()? != OsStr::new(PODS_DIR) {
            return None;
        }
        let store_root = pods_dir.parent()?;
        if store_root.file_name()? != OsStr::new(STORE_DIR) {
            return None;
        }
        Some((
            Self {
                store_root: store_root.to_path_buf(),
            },
            world,
        ))
    }

    /// `<mount>/.slipfork-store/worlds/<world>`.
    #[must_use]
    pub fn world(&self, world: &WorldName) -> PathBuf {
        self.worlds_root().join(world.as_str())
    }

    /// `<mount>/.slipfork-store/worlds/<fork>--<world>`.
    #[must_use]
    pub fn fork_world(&self, world: &WorldName, fork: &ForkName) -> PathBuf {
        self.worlds_root()
            .join(Self::fork_world_name(world, fork).as_str())
    }

    /// `<project>/.forks/<fork>`: the fork's own checkout snapshot.
    #[must_use]
    pub fn project_fork(project: &Path, fork: &ForkName) -> PathBuf {
        project_fork_path(project, fork)
    }

    /// `<fork>--<world>`: the world id a fork boots under.
    #[must_use]
    pub fn fork_world_name(world: &WorldName, fork: &ForkName) -> WorldName {
        WorldName(format!("{}--{}", fork.as_str(), world.as_str()))
    }

    /// `<mount>/.slipfork-store/worlds/<world>.<kind>.<pod>`: a flat sibling of the world so a
    /// world never contains a nested subvolume (`fork` snapshots would turn it into a stub).
    ///
    /// [`ResourceKind`] is the only accepted kind, so an entry outside the clone vocabulary can
    /// never be addressed (or accidentally created) through this path.
    #[must_use]
    pub fn world_clone(&self, world: &WorldName, kind: ResourceKind, pod: &str) -> PathBuf {
        self.worlds_root()
            .join(format!("{}.{}.{pod}", world.as_str(), kind.data_dir()))
    }

    /// `<mount>/.slipfork-store/worlds/<fork>--<world>.<kind>.<pod>`: the fork's clone of one
    /// flat resource sibling, snapshotted alongside the world so live databases survive `fork`.
    #[must_use]
    pub fn fork_world_clone(
        &self,
        world: &WorldName,
        fork: &ForkName,
        kind: ResourceKind,
        pod: &str,
    ) -> PathBuf {
        self.world_clone(&Self::fork_world_name(world, fork), kind, pod)
    }

    /// Splits a `worlds/` entry name back into its `<world>.<kind>.<pod>` triple.
    ///
    /// Splitting on the **last** two `.` keeps world names that themselves contain `.` intact;
    /// only the kinds [`ResourceKind::data_dir`] owns are accepted, so unrelated entries survive.
    #[must_use]
    pub fn parse_world_clone(entry: &std::ffi::OsStr) -> Option<(WorldName, ResourceKind, String)> {
        let name = entry.to_str()?;
        let (prefix, pod) = name.rsplit_once('.')?;
        let (world, directory) = prefix.rsplit_once('.')?;
        let kind = ResourceKind::from_data_dir(directory)?;
        Some((
            WorldName::parse(world).ok()?,
            kind,
            validate_name(NameKind::Pod, pod).ok()?,
        ))
    }

    /// Splits a `worlds/` entry name back into its `<fork>--<world>` pair.
    ///
    /// The separator and both name validators live here, so no caller re-scans entry names.
    /// Splitting on the **last** `--` keeps fork names that themselves contain `-` intact.
    #[must_use]
    pub fn parse_fork_world(entry: &std::ffi::OsStr) -> Option<(ForkName, WorldName)> {
        let (fork, world) = entry.to_str()?.rsplit_once("--")?;
        Some((ForkName::parse(fork).ok()?, WorldName::parse(world).ok()?))
    }
}

/// A validated `worlds/<world>` data subvolume name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorldName(String);

impl WorldName {
    /// Parses `raw` (`[a-z0-9][a-z0-9._-]*`) into a world name.
    pub fn parse(raw: &str) -> Result<Self> {
        validate_name(NameKind::World, raw).map(Self)
    }

    /// The validated name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorldName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A validated fork name shared by both snapshots of a fork.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ForkName(String);

impl ForkName {
    /// Parses `raw` (`[a-z0-9][a-z0-9._-]*`) into a fork name.
    pub fn parse(raw: &str) -> Result<Self> {
        validate_name(NameKind::Fork, raw).map(Self)
    }

    /// The validated name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ForkName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub(crate) fn validate_name(kind: NameKind, raw: &str) -> Result<String> {
    let mut bytes = raw.bytes();
    let starts_well = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    let rest_well = bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    });
    if !starts_well || !rest_well || matches!(raw, "." | "..") {
        return Err(Error::InvalidName {
            kind,
            name: raw.to_owned(),
        });
    }
    Ok(raw.to_owned())
}

/// Top-level project entries that belong to Slipfork's bookkeeping, never to a checkout.
///
/// `.forks` holds the project's own fork tree (copying it in would recurse into the destination)
/// and `.slipfork-store` holds the Btrfs store, which must never be copied into a checkout.
pub(crate) const MANAGED_ENTRIES: [&str; 2] = [FORKS_DIR, STORE_DIR];

/// Whether a project-root entry is one of Slipfork's own [`MANAGED_ENTRIES`].
pub(crate) fn is_managed_entry(name: &OsStr) -> bool {
    MANAGED_ENTRIES
        .iter()
        .any(|managed| name == OsStr::new(managed))
}

/// `<project>/.forks`.
#[must_use]
pub(crate) fn project_forks_dir(project: &Path) -> PathBuf {
    project.join(FORKS_DIR)
}

/// `<project>/.forks/<fork>`.
#[must_use]
pub(crate) fn project_fork_path(project: &Path, fork: &ForkName) -> PathBuf {
    project_forks_dir(project).join(fork.as_str())
}

/// One `<project>/.forks/<name>` checkout on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkDir {
    /// Fork name, the `.forks/<name>` directory entry.
    pub name: String,
    /// Absolute checkout path.
    pub path: PathBuf,
}

/// The `<parent>/.forks/<fork>` pair owning `project`, when `project` is a fork checkout.
///
/// This is the one structural predicate every caller shares: no caller re-inspects
/// `path.parent()` names to decide whether a directory is a fork checkout.
#[must_use]
pub fn fork_checkout(project: &Path) -> Option<(PathBuf, ForkName)> {
    let fork = ForkName::parse(project.file_name()?.to_str()?).ok()?;
    let forks_dir = project.parent()?;
    if forks_dir.file_name()? != OsStr::new(FORKS_DIR) {
        return None;
    }
    Some((forks_dir.parent()?.to_path_buf(), fork))
}

/// The nearest ancestor of `cwd` holding a `Forkfile` or `.slipfork` record.
///
/// A `<parent>/.forks/<fork>` checkout is never itself a root: resolution climbs to the project
/// that owns it.
#[must_use]
pub fn project_root(cwd: &Path) -> Option<PathBuf> {
    let mut current = Some(cwd);
    while let Some(dir) = current {
        let project = dir.join("Forkfile").is_file() || dir.join(".slipfork").is_dir();
        if project && fork_checkout(dir).is_none() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

/// The `<root>/.forks/<fork>` checkout containing `cwd`, when that checkout still exists on disk.
#[must_use]
pub fn fork_under(root: &Path, cwd: &Path) -> Option<(PathBuf, ForkName)> {
    let mut components = cwd.strip_prefix(root).ok()?.components();
    if components.next()?.as_os_str() != OsStr::new(FORKS_DIR) {
        return None;
    }
    let fork = ForkName::parse(components.next()?.as_os_str().to_str()?).ok()?;
    let path = project_fork_path(root, &fork);
    path.is_dir().then_some((path, fork))
}

/// Every `<project>/.forks/<name>` checkout present on disk.
///
/// A fork checkout is either a Btrfs subvolume (a fork of a subvolume checkout) or a plain copied
/// directory (a fork of a plain checkout), so any existing directory named like a fork is reported
/// instead of posing as a phantom stopped fork. A missing `.forks` directory is a normal project
/// with no forks; any other `read_dir` failure is surfaced to the caller.
///
/// # Errors
///
/// Returns the I/O failure from listing `<project>/.forks`, except a missing directory, which is
/// `Ok(Vec::new())`.
pub fn fork_checkouts(project: &Path) -> Result<Vec<ForkDir>> {
    let forks_dir = project_forks_dir(project);
    let entries = match std::fs::read_dir(&forks_dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(Error::io("list", &forks_dir, source)),
    };
    let mut forks = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| Error::io("list", &forks_dir, source))?;
        let Some(name) = entry
            .file_name()
            .to_str()
            .and_then(|name| ForkName::parse(name).ok())
        else {
            continue;
        };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        forks.push(ForkDir {
            name: name.as_str().to_owned(),
            path,
        });
    }
    forks.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(forks)
}

/// `<fork>--<parent>`: the canonical world id for a fork of `parent`, validating both halves.
#[must_use]
pub fn fork_world_id(parent: &str, fork: &str) -> Option<WorldName> {
    let parent = WorldName::parse(parent).ok()?;
    let fork = ForkName::parse(fork).ok()?;
    Some(StoreLayout::fork_world_name(&parent, &fork))
}

/// The world id a stopped project displays: the canonical `<fork>--<parent>` for a fork
/// checkout, else the directory's own name.
#[must_use]
pub fn world_id_for_project(project: &Path) -> String {
    if let Some((parent, fork)) = fork_checkout(project)
        && let Some(parent) = parent.file_name().and_then(|name| name.to_str())
        && let Ok(parent) = WorldName::parse(parent)
    {
        return StoreLayout::fork_world_name(&parent, &fork)
            .as_str()
            .to_owned();
    }
    project
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("world")
        .to_owned()
}

/// Which project world a `ps` invocation is scoped to, resolved once from the caller's cwd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectScope {
    /// No project root above the cwd.
    None,
    /// The cwd sits inside one `<root>/.forks/<fork>` checkout.
    Fork {
        /// Absolute checkout path.
        path: PathBuf,
        /// Fork name.
        name: String,
        /// Canonical `<fork>--<parent>` world id.
        world: String,
    },
    /// The cwd sits in a project root: its own row plus every `.forks` checkout on disk.
    Root {
        /// Absolute project root.
        root: PathBuf,
        /// The root project's world id.
        world: String,
        /// Every fork checkout the host proves is a subvolume.
        forks: Vec<ForkDir>,
        /// Why `.forks` could not be listed, when it could not.
        problem: Option<String>,
    },
}

impl ProjectScope {
    /// Resolves `cwd` against the on-disk `.forks` layout exactly once.
    #[must_use]
    pub fn resolve(cwd: &Path) -> Self {
        let Some(root) = project_root(cwd) else {
            return Self::None;
        };
        if let Some((path, fork)) = fork_under(&root, cwd) {
            return Self::Fork {
                world: world_id_for_project(&path),
                path,
                name: fork.as_str().to_owned(),
            };
        }
        let (forks, problem) = match fork_checkouts(&root) {
            Ok(forks) => (forks, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        Self::Root {
            world: world_id_for_project(&root),
            root,
            forks,
            problem,
        }
    }
}

/// Splits `path` into its parent directory and single-component file name.
pub(crate) fn split_dir(path: &Path) -> Result<(PathBuf, OsString)> {
    let name = path
        .file_name()
        .ok_or_else(|| Error::UnsupportedPath {
            path: path.to_path_buf(),
        })?
        .to_owned();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| Error::UnsupportedPath {
            path: path.to_path_buf(),
        })?;
    Ok((parent.to_path_buf(), name))
}
