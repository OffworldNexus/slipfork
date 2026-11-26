mod observe;
pub(crate) mod readiness;
mod teardown;

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use crate::btrfs::{self, SubvolumeFlags};
use crate::error::{Error, Result};
use crate::layout::{self, ForkName, StoreLayout, WorldName};
use crate::port::{Dirtiness, ForkPaths, WorldEngine};
use crate::resources::ResourceKind;

/// Btrfs adapter behind the [`WorldEngine`] port.
///
/// The store root arrives once through [`StoreLayout`] and every operation takes an absolute path,
/// so this crate never reads the environment or caches a global.
#[derive(Debug, Clone)]
pub struct BtrfsEngine {
    layout: StoreLayout,
}

impl BtrfsEngine {
    /// Builds the engine for one Btrfs mount store.
    #[must_use]
    pub fn new(layout: StoreLayout) -> Self {
        Self { layout }
    }

    /// The store layout this engine owns.
    #[must_use]
    pub fn layout(&self) -> &StoreLayout {
        &self.layout
    }

    /// Flushes every pending write on the store's filesystem before a CoW snapshot.
    ///
    /// The caller first checkpoints PostgreSQL over `<pods>/control.sock` (a `< 5ms` v3-wire
    /// `CHECKPOINT`), pushing dirty `shared_buffers`/`wal_buffers` into the kernel page cache.
    /// This `syncfs(2)` then commits that page cache, so `SNAP_CREATE_V2` clones a crash-consistent
    /// `PGDATA` even though PostgreSQL runs with `fsync=off`.
    pub fn flush_store(&self) -> Result<()> {
        let anchor = self.store_anchor();
        let dir = Self::open_dir(&anchor)?;
        btrfs::syncfs(&dir, &anchor)
    }

    /// Existing directory used to probe the Btrfs superblock before creating `.slipfork-store`.
    fn store_anchor(&self) -> PathBuf {
        let store = self.layout.store_root();
        if store.exists() {
            store
        } else {
            self.layout.btrfs_mount().to_path_buf()
        }
    }

    fn open_dir(path: &Path) -> Result<File> {
        let file = File::open(path).map_err(|source| Error::io("open", path, source))?;
        let is_dir = file
            .metadata()
            .map_err(|source| Error::io("inspect", path, source))?
            .is_dir();
        if is_dir {
            Ok(file)
        } else {
            Err(Error::NotADirectory {
                path: path.to_path_buf(),
            })
        }
    }

    fn open_subvolume(path: &Path) -> Result<File> {
        let dir = Self::open_dir(path)?;
        if btrfs::is_subvolume(&dir, path)? {
            Ok(dir)
        } else {
            Err(Error::NotASubvolume {
                path: path.to_path_buf(),
            })
        }
    }

    fn require_btrfs(dir: &File, path: &Path) -> Result<()> {
        if btrfs::is_btrfs(dir, path)? {
            Ok(())
        } else {
            Err(Error::NotBtrfs {
                path: path.to_path_buf(),
            })
        }
    }

    fn subvolume_flags(path: &Path) -> Result<SubvolumeFlags> {
        let dir = Self::open_subvolume(path)?;
        btrfs::subvolume_flags(&dir, path)
    }

    fn set_flags(path: &Path, flags: SubvolumeFlags) -> Result<()> {
        let dir = Self::open_subvolume(path)?;
        btrfs::set_flags(&dir, path, flags)
    }

    fn create_subvolume(path: &Path) -> Result<()> {
        let (parent, name) = layout::split_dir(path)?;
        let parent = Self::open_dir(&parent)?;
        btrfs::create_subvolume(&parent, path, &name)
    }

    fn snapshot(source: &Path, target: &Path) -> Result<()> {
        let (parent, name) = layout::split_dir(target)?;
        let parent = Self::open_dir(&parent)?;
        let source_dir = Self::open_subvolume(source)?;
        btrfs::snapshot(&parent, &source_dir, target, &name)
    }

    fn destroy_subvolume(path: &Path) -> Result<()> {
        let (parent, name) = layout::split_dir(path)?;
        let dir = Self::open_subvolume(path)?;
        let flags = btrfs::subvolume_flags(&dir, path)?;
        // A read-only subvolume must be unsealed before `SNAP_DESTROY_V2` accepts it.
        if flags.is_read_only() {
            btrfs::set_flags(&dir, path, flags.unsealed())?;
        }
        drop(dir);
        let parent = Self::open_dir(&parent)?;
        btrfs::destroy(&parent, path, &name)
    }

    /// Strips Slipfork's own bookkeeping out of a freshly created checkout.
    ///
    /// `SNAP_CREATE_V2` clones the whole project tree, so the parent's `.forks` tree and the
    /// `.slipfork-store` mount store would surface inside the new checkout (as nested stubs or as
    /// copied entries) and be advertised as phantom forks. Both names are removed from the
    /// checkout only — the parent tree is never mutated. A nested subvolume is unsealed and
    /// destroyed through the engine, anything else through `remove_dir_all`, and an entry a
    /// racing reaper already unlinked is `Ok(())`.
    fn strip_managed_dirs(&self, checkout: &Path) -> Result<()> {
        let entries =
            fs::read_dir(checkout).map_err(|source| Error::io("read", checkout, source))?;
        for entry in entries {
            let entry = entry.map_err(|source| Error::io("read", checkout, source))?;
            let name = entry.file_name();
            if !layout::is_managed_entry(&name) {
                continue;
            }
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => return Err(Error::io("inspect", &path, source)),
            };
            if metadata.is_dir() {
                let is_subvolume = match btrfs::is_subvolume_path(&path) {
                    Ok(is_subvolume) => is_subvolume,
                    Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                // Reachable when the project itself is the Btrfs mount root: `<project>/.slipfork-store`
                // is then a real nested subvolume and must go through `SNAP_DESTROY_V2`. A normal
                // project snapshot only surfaces an `ino == 2` stub, which `remove_dir_all` reaps.
                if is_subvolume {
                    Self::destroy_subvolume(&path)?;
                } else if let Err(source) = fs::remove_dir_all(&path)
                    && source.kind() != io::ErrorKind::NotFound
                {
                    return Err(Error::io("remove", &path, source));
                }
            } else if let Err(source) = fs::remove_file(&path)
                && source.kind() != io::ErrorKind::NotFound
            {
                return Err(Error::io("remove", &path, source));
            }
        }
        Ok(())
    }
}

impl BtrfsEngine {
    fn world_clones(&self, world: &WorldName) -> Result<Vec<(ResourceKind, String)>> {
        let worlds = self.layout.worlds_root();
        if !worlds.is_dir() {
            return Ok(Vec::new());
        }
        let entries = fs::read_dir(&worlds).map_err(|source| Error::io("read", &worlds, source))?;
        let mut clones = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| Error::io("read", &worlds, source))?;
            if let Some((parsed, kind, pod)) = StoreLayout::parse_world_clone(&entry.file_name())
                && &parsed == world
            {
                clones.push((kind, pod));
            }
        }
        Ok(clones)
    }

    fn snapshots(jobs: &[(PathBuf, PathBuf)]) -> Vec<Result<()>> {
        std::thread::scope(|scope| {
            let handles: Vec<_> = jobs
                .iter()
                .map(|(source, target)| scope.spawn(move || Self::snapshot(source, target)))
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle.join().unwrap_or_else(|_| {
                        Err(Error::io(
                            "snapshot",
                            Path::new("."),
                            std::io::Error::other("snapshot thread panicked"),
                        ))
                    })
                })
                .collect()
        })
    }
}

impl WorldEngine for BtrfsEngine {
    fn ensure_store(&self) -> Result<()> {
        // The data-home fallback (`<data_home>/slipfork/store`) and a fresh project mount both
        // start absent, so the resolved root is created before it is opened. Validation targets
        // that root itself (not the mount parent), which pins the failure to the path the user
        // must fix instead of surfacing an `ENOENT` on a directory Slipfork never made.
        let root = self.layout.store_root();
        fs::create_dir_all(&root).map_err(|source| Error::io("create", &root, source))?;
        let dir = Self::open_dir(&root)?;
        Self::require_btrfs(&dir, &root)?;
        drop(dir);
        for path in [self.layout.images_root(), self.layout.worlds_root()] {
            fs::create_dir_all(&path).map_err(|source| Error::io("create", &path, source))?;
        }
        Ok(())
    }

    fn create_world(&self, world: &WorldName) -> Result<PathBuf> {
        self.ensure_store()?;
        let path = self.layout.world(world);
        if !path.exists() {
            Self::create_subvolume(&path)?;
        }
        Ok(path)
    }

    fn create_subvolume(&self, path: &Path) -> Result<()> {
        BtrfsEngine::create_subvolume(path)
    }

    fn clone_subvolume(&self, source: &Path, dest: &Path) -> Result<()> {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|source| Error::io("create", parent, source))?;
        }
        Self::snapshot(source, dest)
    }
    fn fork(&self, project: &Path, world: &WorldName, fork: &ForkName) -> Result<ForkPaths> {
        self.ensure_store()?;
        // Make the parent's last writes (a fresh `CREATE DATABASE`, AOF tail, bucket PUTs) durable
        // before any `SNAP_CREATE_V2`: PG runs `fsync=off`, so page-cache-only data would be
        // cloned as a torn `PGDATA` and the fork's postgres would panic on boot.
        self.flush_store()?;
        // A checkout is snapshotted with `SNAP_CREATE_V2` only when it is itself a subvolume. A
        // plain directory is never modified: its tree is copied into the fork path afterwards.
        // Every fork lands flat at `<family_root>/.forks/<fork>` under `<fork>--<root_world>`,
        // no matter how deep the parent checkout sits (`specs/03` §7.3).
        let family = layout::fork_family(project, world)?;
        let project_is_subvolume = btrfs::is_subvolume_path(project)?;
        let forks_dir = layout::project_forks_dir(&family.root);
        fs::create_dir_all(&forks_dir).map_err(|source| Error::io("create", &forks_dir, source))?;
        let project_snapshot = layout::project_fork_path(&family.root, fork);
        let forked = StoreLayout::fork_world_name(&family.root_world, fork);
        let world_source = self.layout.world(world);
        let world_snapshot = self.layout.world(&forked);
        // Every flat sibling resource clone belongs to the fork too (live PGDATA, AOF, buckets).
        let mut jobs = vec![(world_source, world_snapshot.clone())];
        if project_is_subvolume {
            jobs.push((project.to_path_buf(), project_snapshot.clone()));
        }
        for (kind, pod) in self.world_clones(world)? {
            jobs.push((
                self.layout.world_clone(world, kind, &pod),
                self.layout.world_clone(&forked, kind, &pod),
            ));
        }
        // Snapshot live `svc-*` service rootfses in the same Btrfs transaction so the fork pays
        // zero extra `SNAP_CREATE_V2` commits for services and inherits warm runtime caches.
        let parent_pods = self.layout.world_pods(world);
        if parent_pods.is_dir() {
            let fork_pods = self.layout.world_pods(&forked);
            fs::create_dir_all(&fork_pods)
                .map_err(|source| Error::io("create", &fork_pods, source))?;
            for entry in fs::read_dir(&parent_pods)
                .map_err(|source| Error::io("read", &parent_pods, source))?
            {
                let entry = entry.map_err(|source| Error::io("read", &parent_pods, source))?;
                let file_name = entry.file_name();
                let reusable = file_name.to_str().is_some_and(crate::pod::is_reusable_pod);
                let source_path = entry.path();
                if reusable && btrfs::is_subvolume_path(&source_path).unwrap_or(false) {
                    jobs.push((source_path, fork_pods.join(file_name)));
                }
            }
        }
        // Issue every `SNAP_CREATE_V2` ioctl concurrently so they join the same Btrfs transaction
        // (`cur_trans->transaction->pending_snapshots`) and share one superblock barrier commit.
        let outcomes = Self::snapshots(&jobs);
        if let Some(failed) = outcomes.iter().position(|outcome| outcome.is_err()) {
            for (_, target) in &jobs {
                let _ = teardown::destroy_if_present(target);
            }
            return Err(outcomes
                .into_iter()
                .nth(failed)
                .and_then(Result::err)
                .unwrap_or_else(|| {
                    Error::io(
                        "snapshot",
                        project,
                        std::io::Error::other("snapshot failed"),
                    )
                }));
        }
        if !project_is_subvolume {
            // The stage is hidden from `fork_checkouts` because `ForkName::parse` rejects it, so a
            // partial copy left by a dead daemon is never advertised as a stopped fork.
            let stage = forks_dir.join(format!(".partial-{fork}"));
            let _ = fs::remove_dir_all(&stage);
            if let Err(error) = crate::checkout::copy_checkout(project, &stage) {
                for (_, target) in &jobs {
                    let _ = teardown::destroy_if_present(target);
                }
                let _ = fs::remove_dir_all(&stage);
                return Err(error);
            }
            if let Err(source) = fs::rename(&stage, &project_snapshot) {
                for (_, target) in &jobs {
                    let _ = teardown::destroy_if_present(target);
                }
                let _ = fs::remove_dir_all(&stage);
                return Err(Error::io("publish", &project_snapshot, source));
            }
        }
        // Publication cloned the parent's own `.forks`/`.slipfork-store` bookkeeping into the
        // checkout: nested snapshots surface as stubs and copied entries look like forks, so every
        // follower of a fork would report phantom `checkout missing` rows. Strip them now; a
        // failure destroys every snapshot this fork created and fails closed rather than returning
        // a checkout that still holds managed directories.
        if let Err(error) = self.strip_managed_dirs(&project_snapshot) {
            for (_, target) in &jobs {
                let _ = teardown::destroy_if_present(target);
            }
            let _ = teardown::destroy_if_present(&project_snapshot);
            return Err(error);
        }
        Ok(ForkPaths {
            project: project_snapshot,
            world: world_snapshot,
            parent: family.parent,
            root: family.root_world,
        })
    }

    /// Seals `subvolume` read-only with `BTRFS_SUBVOL_RDONLY`.
    fn seal(&self, subvolume: &Path) -> Result<()> {
        let flags = Self::subvolume_flags(subvolume)?;
        if flags.is_read_only() {
            Ok(())
        } else {
            Self::set_flags(subvolume, flags.sealed())
        }
    }

    fn is_sealed(&self, subvolume: &Path) -> Result<bool> {
        Self::subvolume_flags(subvolume).map(SubvolumeFlags::is_read_only)
    }

    fn sweep_pods(&self, pods_dir: &Path) -> Result<()> {
        teardown::sweep_pods_dir(pods_dir)
    }

    fn destroy(&self, subvolume: &Path, dirtiness: Dirtiness, force: bool) -> Result<()> {
        if dirtiness.is_dirty() && !force {
            return Err(Error::RefusedDirtyCheckout {
                target: subvolume.display().to_string(),
            });
        }
        Self::destroy_subvolume(subvolume)
    }

    fn drop_fork(
        &self,
        project: &Path,
        world: &WorldName,
        fork: &ForkName,
        dirtiness: Dirtiness,
        force: bool,
    ) -> Result<()> {
        if dirtiness.is_dirty() && !force {
            return Err(Error::RefusedDirtyCheckout {
                target: format!("fork {fork}"),
            });
        }
        // Sweep pod clones, flat resource clones, and `worlds/<fork>--<root_world>` BEFORE
        // unlinking `.forks/<fork>`, so an interrupted drop keeps the fork addressable on retry
        // instead of stranding orphan world subvolumes. The family is resolved from the called
        // project, so a drop issued at the root never compounds a nested world id.
        let family = layout::fork_family(project, world)?;
        let forked = StoreLayout::fork_world_name(&family.root_world, fork);
        teardown::sweep_pods_dir(&self.layout.world_pods(&forked))?;
        let worlds = self.layout.worlds_root();
        if worlds.is_dir() {
            let entries =
                fs::read_dir(&worlds).map_err(|source| Error::io("read", &worlds, source))?;
            for entry in entries {
                let entry = entry.map_err(|source| Error::io("read", &worlds, source))?;
                if let Some((parsed, kind, pod)) =
                    StoreLayout::parse_world_clone(&entry.file_name())
                    && parsed == forked
                {
                    teardown::destroy_world_if_present(
                        &self.layout.world_clone(&forked, kind, &pod),
                    )?;
                }
            }
        }
        teardown::destroy_world_if_present(&self.layout.world(&forked))?;
        teardown::destroy_if_present(&layout::project_fork_path(&family.root, fork))
    }

    fn drop_world(&self, world: &WorldName) -> Result<()> {
        teardown::sweep_pods_dir(&self.layout.world_pods(world))?;
        let pods = self.layout.pods_root();
        if pods.is_dir() {
            let entries = fs::read_dir(&pods).map_err(|source| Error::io("read", &pods, source))?;
            for entry in entries {
                let entry = entry.map_err(|source| Error::io("read", &pods, source))?;
                if StoreLayout::parse_fork_world(&entry.file_name())
                    .is_some_and(|(_, parent)| &parent == world)
                {
                    teardown::sweep_pods_dir(&entry.path())?;
                }
            }
        }
        let worlds = self.layout.worlds_root();
        if worlds.is_dir() {
            let entries =
                fs::read_dir(&worlds).map_err(|source| Error::io("read", &worlds, source))?;
            for entry in entries {
                let entry = entry.map_err(|source| Error::io("read", &worlds, source))?;
                // `StoreLayout` owns the `<fork>--<world>` and `<world>.<kind>.<pod>` formats;
                // comparing the parsed pair exactly keeps `drop_world("a--b")` from sweeping an
                // unrelated `x--a--b` snapshot or clone.
                let name = entry.file_name();
                let owned = StoreLayout::parse_fork_world(&name)
                    .is_some_and(|(_, parsed)| &parsed == world)
                    || StoreLayout::parse_world_clone(&name).is_some_and(|(parsed, _, _)| {
                        &parsed == world
                            || StoreLayout::parse_fork_world(std::ffi::OsStr::new(parsed.as_str()))
                                .is_some_and(|(_, parent)| &parent == world)
                    });
                if owned {
                    teardown::destroy_world_if_present(&entry.path())?;
                }
            }
        }
        teardown::destroy_world_if_present(&self.layout.world(world))
    }
}
