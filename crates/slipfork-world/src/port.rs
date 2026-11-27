use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::layout::{ForkName, WorldName};

/// Whether the caller's checkout has uncommitted changes.
///
/// The Btrfs engine never inspects git itself; the composition root passes the answer as a plain
/// value, so no clock/probe/mock trait ever appears beside the port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dirtiness {
    /// Safe to discard.
    Clean,
    /// Refused without `force`.
    Dirty,
}

impl Dirtiness {
    /// Whether the checkout carries uncommitted changes.
    #[must_use]
    pub const fn is_dirty(self) -> bool {
        matches!(self, Self::Dirty)
    }
}

/// Both snapshots created by a fork.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkPaths {
    /// `<family_root>/.forks/<fork>` project snapshot.
    pub project: PathBuf,
    /// `<mount>/.slipfork-store/worlds/<fork>--<root_world>` world data snapshot.
    pub world: PathBuf,
    /// Immediate parent world id: `<fork>--<root>` when forking a fork, else the root world.
    pub parent: WorldName,
    /// The family root world id every fork world id is suffixed with.
    pub root: WorldName,
}

/// Canonical Btrfs port (`specs/03-under-the-hood.md` §5).
///
/// Every method is absolute-path in, typed error out: the adapter never reads the environment and
/// never caches a mount point.
pub trait WorldEngine: Send + Sync {
    /// Creates `<mount>/.slipfork-store/{images,worlds}` when missing.
    fn ensure_store(&self) -> Result<()>;

    /// Creates the `worlds/<world>` data subvolume and returns its path.
    fn create_world(&self, world: &WorldName) -> Result<PathBuf>;

    /// CoW-clones the project and world data subvolumes in under 50ms.
    fn fork(&self, project: &Path, world: &WorldName, fork: &ForkName) -> Result<ForkPaths>;

    /// Creates a fresh read-write subvolume at `path` (parent directory must exist).
    fn create_subvolume(&self, path: &Path) -> Result<()>;

    /// Creates a writable CoW snapshot at `dest` from `source` (`BTRFS_IOC_SNAP_CREATE_V2`).
    ///
    /// Image builds clone the parent step subvolume before every filesystem instruction, so this
    /// is the single snapshot primitive behind `< 1ms` cached `Dockerfile` steps.
    fn clone_subvolume(&self, source: &Path, dest: &Path) -> Result<()>;

    /// Seals a subvolume read-only with `BTRFS_SUBVOL_RDONLY`.
    fn seal(&self, subvolume: &Path) -> Result<()>;

    /// Reports whether `BTRFS_SUBVOL_RDONLY` is set on `subvolume`.
    fn is_sealed(&self, subvolume: &Path) -> Result<bool>;

    /// Removes one subvolume, unsealing it first; refuses dirty work without `force`.
    fn destroy(&self, subvolume: &Path, dirtiness: Dirtiness, force: bool) -> Result<()>;

    /// Removes every ephemeral pod rootfs under `<store>/pods/<world>`, then the emptied dirs.
    fn sweep_pods(&self, pods_dir: &Path) -> Result<()>;

    /// Removes both snapshots of a fork; refuses dirty work without `force`.
    fn drop_fork(
        &self,
        project: &Path,
        world: &WorldName,
        fork: &ForkName,
        dirtiness: Dirtiness,
        force: bool,
    ) -> Result<()>;

    /// Removes world data plus every `<fork>--<world>` snapshot of that world.
    fn drop_world(&self, world: &WorldName) -> Result<()>;
}
