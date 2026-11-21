use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use crate::btrfs::{self, SubvolumeFlags};
use crate::error::{Error, Result};
use crate::layout::{self, ForkName, StoreLayout, WorldName};
use crate::port::{Dirtiness, ForkPaths, WorldEngine};

/// Btrfs adapter behind the [`WorldEngine`] port.
///
/// The mount root arrives once through [`StoreLayout`] and every operation takes an absolute path,
/// so this crate never reads the environment or caches a global.
#[derive(Debug, Clone)]
pub struct BtrfsEngine {
    layout: StoreLayout,
}

impl BtrfsEngine {
    /// Builds the engine for one Btrfs mount.
    #[must_use]
    pub fn new(layout: StoreLayout) -> Self {
        Self { layout }
    }

    /// The store layout this engine owns.
    #[must_use]
    pub fn layout(&self) -> &StoreLayout {
        &self.layout
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

    /// Zero-copy conversion: move every child into a sibling subvolume, then rename it into place.
    fn convert_in_place(project: &Path) -> Result<()> {
        let (parent, name) = layout::split_dir(project)?;
        let parent_dir = Self::open_dir(&parent)?;
        let temp_name = free_sibling_name(&parent, &name)?;
        let temp_path = parent.join(&temp_name);
        btrfs::create_subvolume(&parent_dir, &temp_path, &temp_name)?;
        if let Err(failure) = move_children(project, &temp_path) {
            let _ = fs::remove_dir(&temp_path);
            return Err(Error::io("rename", project, failure));
        }
        if let Err(failure) = fs::remove_dir(project) {
            let _ = move_children(&temp_path, project);
            let _ = fs::remove_dir(&temp_path);
            return Err(Error::io("rmdir", project, failure));
        }
        fs::rename(&temp_path, project).map_err(|failure| Error::io("rename", &temp_path, failure))
    }

    fn require_same_filesystem(&self, project: &Path) -> Result<()> {
        use std::os::unix::fs::MetadataExt;

        let mount = self.layout.btrfs_mount();
        let project_device = fs::metadata(project)
            .map_err(|source| Error::io("inspect", project, source))?
            .dev();
        let mount_device = fs::metadata(mount)
            .map_err(|source| Error::io("inspect", mount, source))?
            .dev();
        if project_device == mount_device {
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
        let mount = self.layout.btrfs_mount();
        let dir = Self::open_dir(mount)?;
        Self::require_btrfs(&dir, mount)?;
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

    fn fork(&self, project: &Path, world: &WorldName, fork: &ForkName) -> Result<ForkPaths> {
        self.ensure_store()?;
        // Probe only: the project must already be a converted subvolume.
        Self::open_subvolume(project)?;
        let forks_dir = layout::project_forks_dir(project);
        fs::create_dir_all(&forks_dir).map_err(|source| Error::io("create", &forks_dir, source))?;
        let project_snapshot = layout::project_fork_path(project, fork);
        Self::snapshot(project, &project_snapshot)?;
        let world_snapshot = self.layout.fork_world(world, fork);
        Self::snapshot(&self.layout.world(world), &world_snapshot)?;
        Ok(ForkPaths {
            project: project_snapshot,
            world: world_snapshot,
        })
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
        Self::destroy_if_present(&self.layout.fork_world(world, fork))
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
                    Self::destroy_if_present(&entry.path())?;
                }
            }
        }
        Self::destroy_if_present(&self.layout.world(world))
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

/// `rename(2)` every child of `from` into `to`, rolling back on the first failure.
fn move_children(from: &Path, to: &Path) -> io::Result<()> {
    let mut moved: Vec<OsString> = Vec::new();
    let mut failure = None;
    for entry in fs::read_dir(from)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                failure = Some(err);
                break;
            }
        };
        let name = entry.file_name();
        if let Err(err) = fs::rename(entry.path(), to.join(&name)) {
            failure = Some(err);
            break;
        }
        moved.push(name);
    }
    if let Some(err) = failure {
        for name in moved.iter().rev() {
            let _ = fs::rename(to.join(name), from.join(name));
        }
        return Err(err);
    }
    Ok(())
}
