use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use crate::btrfs::{self, SubvolumeFlags};
use crate::error::{Error, Result};
use crate::layout::{self, ForkName, StoreLayout, WorldName};
use crate::port::{Dirtiness, ForkPaths, WorldEngine};

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

    fn destroy_if_present(path: &Path) -> Result<()> {
        if path.exists() {
            Self::destroy_subvolume(path)
        } else {
            Ok(())
        }
    }

    /// Destroys a managed `worlds/<id>` subvolume after clearing any nested pod clones inside it.
    fn destroy_world_if_present(path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let mut children = Vec::new();
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if !file_type.is_dir() || file_type.is_symlink() {
                    continue;
                }
                let child = entry.path();
                if btrfs::is_subvolume_path(&child)? {
                    children.push(child.clone());
                }
                stack.push(child);
            }
        }
        children.sort_by_key(|child| std::cmp::Reverse(child.components().count()));
        for child in children {
            Self::destroy_subvolume(&child)?;
        }
        Self::destroy_subvolume(path)
    }

    /// In-place conversion: copy the checkout into a sibling subvolume, then rename it in.
    ///
    /// Btrfs answers `EXDEV` to `rename(2)` across a subvolume root for every inode, not only
    /// directories, so the payload is reproduced instead: directories with their mode, regular
    /// files through `copy_file_range` (`fs::copy` is CoW/remap-backed on Btrfs), and symlinks as
    /// links. The original is untouched until the whole copy has landed.
    fn convert_in_place(project: &Path) -> Result<()> {
        use std::os::unix::fs::MetadataExt;

        let (parent, name) = layout::split_dir(project)?;
        let parent_dir = Self::open_dir(&parent)?;
        let temp_name = free_sibling_name(&parent, &name)?;
        let temp_path = parent.join(&temp_name);
        btrfs::create_subvolume(&parent_dir, &temp_path, &temp_name)?;
        let root_device = fs::metadata(project)
            .map_err(|source| Error::io("inspect", project, source))?
            .dev();
        if let Err(failure) = copy_tree(project, &temp_path, root_device) {
            let _ = btrfs::destroy(&parent_dir, &temp_path, &temp_name);
            return Err(failure);
        }
        if let Err(failure) = fs::remove_dir_all(project) {
            return Err(Error::io("remove", project, failure));
        }
        fs::rename(&temp_path, project).map_err(|_| Error::LeftoverSubvolume {
            path: temp_path.clone(),
            target: project.to_path_buf(),
        })
    }

    /// Compares Btrfs `fsid` UUIDs rather than `st_dev` (which differs across Btrfs subvolumes).
    fn require_same_filesystem(&self, project: &Path) -> Result<()> {
        let anchor = self.store_anchor();
        let mount = self.layout.btrfs_mount();
        let project_dir = Self::open_dir(project)?;
        let anchor_dir = Self::open_dir(&anchor)?;
        if !btrfs::is_btrfs(&anchor_dir, &anchor)? {
            return Err(Error::CrossDevice {
                path: project.to_path_buf(),
                mount: mount.to_path_buf(),
            });
        }
        let project_fsid = btrfs::fsid(&project_dir, project)?;
        let anchor_fsid = btrfs::fsid(&anchor_dir, &anchor)?;
        if project_fsid == anchor_fsid {
            Ok(())
        } else {
            Err(Error::CrossDevice {
                path: project.to_path_buf(),
                mount: mount.to_path_buf(),
            })
        }
    }
}

impl WorldEngine for BtrfsEngine {
    fn ensure_store(&self) -> Result<()> {
        let anchor = self.store_anchor();
        let dir = Self::open_dir(&anchor)?;
        Self::require_btrfs(&dir, &anchor)?;
        drop(dir);
        for path in [
            self.layout.store_root(),
            self.layout.images_root(),
            self.layout.worlds_root(),
        ] {
            fs::create_dir_all(&path).map_err(|source| Error::io("create", &path, source))?;
        }
        Ok(())
    }

    fn adopt_project(&self, project: &Path) -> Result<()> {
        let dir = Self::open_dir(project)?;
        Self::require_btrfs(&dir, project)?;
        let already_subvolume = btrfs::is_subvolume(&dir, project)?;
        drop(dir);
        if already_subvolume {
            return Ok(());
        }
        self.require_same_filesystem(project)?;
        Self::convert_in_place(project)
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
        // Probe only: the project must already be a converted subvolume.
        Self::open_subvolume(project)?;
        let forks_dir = layout::project_forks_dir(project);
        fs::create_dir_all(&forks_dir).map_err(|source| Error::io("create", &forks_dir, source))?;
        let project_snapshot = layout::project_fork_path(project, fork);
        let world_source = self.layout.world(world);
        let world_snapshot = self.layout.fork_world(world, fork);
        // Issue both `SNAP_CREATE_V2` ioctls concurrently so they join the same Btrfs transaction
        // (`cur_trans->transaction->pending_snapshots`) and share one superblock barrier commit.
        let (project_outcome, world_outcome) = std::thread::scope(|scope| {
            let left = scope.spawn(|| Self::snapshot(project, &project_snapshot));
            let right = Self::snapshot(&world_source, &world_snapshot);
            let left = left.join().unwrap_or_else(|_| {
                Err(Error::io(
                    "snapshot",
                    &project_snapshot,
                    std::io::Error::other("snapshot thread panicked"),
                ))
            });
            (left, right)
        });
        match (project_outcome, world_outcome) {
            (Ok(()), Ok(())) => Ok(ForkPaths {
                project: project_snapshot,
                world: world_snapshot,
            }),
            (Err(err), Ok(())) => {
                let _ = Self::destroy_if_present(&world_snapshot);
                Err(err)
            }
            (Ok(()), Err(err)) => {
                let _ = Self::destroy_if_present(&project_snapshot);
                Err(err)
            }
            (Err(err), Err(_)) => Err(err),
        }
    }

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
        Self::destroy_if_present(&layout::project_fork_path(project, fork))?;
        Self::destroy_world_if_present(&self.layout.fork_world(world, fork))
    }

    fn drop_world(&self, world: &WorldName) -> Result<()> {
        let worlds = self.layout.worlds_root();
        if worlds.is_dir() {
            let entries =
                fs::read_dir(&worlds).map_err(|source| Error::io("read", &worlds, source))?;
            for entry in entries {
                let entry = entry.map_err(|source| Error::io("read", &worlds, source))?;
                // `StoreLayout` owns the `<fork>--<world>` format; comparing the parsed pair exactly
                // keeps `drop_world("a--b")` from sweeping an unrelated `x--a--b` snapshot.
                let owned = StoreLayout::parse_fork_world(&entry.file_name())
                    .is_some_and(|(_, parsed)| &parsed == world);
                if owned {
                    Self::destroy_world_if_present(&entry.path())?;
                }
            }
        }
        Self::destroy_world_if_present(&self.layout.world(world))
    }
}

/// Finds an unused `.slipfork-adopt*` sibling name next to `name`.
fn free_sibling_name(parent: &Path, name: &OsStr) -> Result<OsString> {
    let stem = name.to_string_lossy();
    for attempt in 0..64 {
        let candidate = if attempt == 0 {
            format!("{stem}.slipfork-adopt")
        } else {
            format!("{stem}.slipfork-adopt.{attempt}")
        };
        let candidate = OsString::from(candidate);
        if !parent.join(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err(Error::AdoptionCollision {
        path: parent.join(name),
    })
}

/// Reproduces `from`'s tree inside `to`, refusing to cross a nested mount point.
///
/// Directories are recreated with their mode, regular files are copied with `fs::copy` (Btrfs
/// serves that from `copy_file_range`, so payloads stay CoW/remap-backed rather than being
/// rewritten by hand), and symlinks are recreated from `read_link` so they are never followed.
/// FIFOs, sockets, and device nodes have no meaning in a checkout and fail closed.
fn copy_tree(from: &Path, to: &Path, root_device: u64) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    for entry in fs::read_dir(from).map_err(|source| Error::io("read", from, source))? {
        let entry = entry.map_err(|source| Error::io("read", from, source))?;
        let name = entry.file_name();
        let source_path = from.join(&name);
        let target_path = to.join(&name);
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|source| Error::io("inspect", &source_path, source))?;
        if metadata.is_dir() {
            if metadata.dev() != root_device {
                return Err(Error::NestedMount { path: source_path });
            }
            fs::create_dir(&target_path)
                .map_err(|source| Error::io("create", &target_path, source))?;
            fs::set_permissions(&target_path, metadata.permissions())
                .map_err(|source| Error::io("chmod", &target_path, source))?;
            copy_tree(&source_path, &target_path, root_device)?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &target_path)
                .map_err(|source| Error::io("copy", &source_path, source))?;
            fs::set_permissions(&target_path, metadata.permissions())
                .map_err(|source| Error::io("chmod", &target_path, source))?;
        } else if metadata.file_type().is_symlink() {
            let target = fs::read_link(&source_path)
                .map_err(|source| Error::io("read", &source_path, source))?;
            symlink(&target, &target_path)
                .map_err(|source| Error::io("symlink", &target_path, source))?;
        } else {
            return Err(Error::UnsupportedEntry { path: source_path });
        }
    }
    Ok(())
}
