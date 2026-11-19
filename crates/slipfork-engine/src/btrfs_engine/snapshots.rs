//! Sealing a world into an immutable read-only snapshot: import, seal, and freeze
//! (`ADR-0010` §3).

#![forbid(unsafe_code)]

use std::fs;
use std::path::Path;

use slipfork_world::{EngineError, SnapshotId, WorldHandle};

use crate::WorldStatus;

use super::{BtrfsEngine, io_error};

impl BtrfsEngine {
    pub(crate) fn snapshot_exists_on_disk(&self, id: &SnapshotId) -> Result<bool, EngineError> {
        let name = id.validate()?;
        Ok(self.snapshots_dir().join(name).is_dir())
    }

    pub(crate) fn import_snapshot(
        &self,
        source_dir: &Path,
        id: &SnapshotId,
    ) -> Result<(), EngineError> {
        let name = id.validate()?;
        let snapshots = self.snapshots_dir();
        let worlds = self.worlds_dir();
        fs::create_dir_all(&snapshots).map_err(|err| io_error(&snapshots, &err))?;
        fs::create_dir_all(&worlds).map_err(|err| io_error(&worlds, &err))?;
        if !source_dir.is_dir() {
            return Err(EngineError::Io {
                path: source_dir.display().to_string(),
                message: "snapshot source must be an existing directory".to_owned(),
            });
        }
        // A sealed snapshot is immutable (`ADR-0010` §3): never replace one, and never stage an
        // import that could not be sealed anyway.
        if snapshots.join(name).exists() {
            return Err(EngineError::SnapshotExists {
                id: name.to_owned(),
            });
        }
        // A snapshot source is either a subvolume this engine already manages (under `worlds/` or
        // `snapshots/`) or a plain external directory (the daemon's base-seed directory). A
        // managed subvolume is sealed with one ioctl and never byte-copied.
        for parent in [&worlds, &snapshots] {
            if source_dir.parent() != Some(parent.as_path()) {
                continue;
            }
            if !self.is_managed_subvolume(parent, source_dir)? {
                return Err(EngineError::Io {
                    path: source_dir.display().to_string(),
                    message: format!(
                        "snapshot source is not a managed subvolume under '{}' or '{}'; a managed \
                         subvolume is sealed with one ioctl, and only a plain external directory \
                         (the base-seed directory) is imported",
                        worlds.display(),
                        snapshots.display()
                    ),
                });
            }
            return self.seal_snapshot(source_dir, name);
        }
        // A plain external directory is imported into one staging subvolume, sealed read-only, and
        // dropped: a snapshot is always a subvolume, so every clone and freeze path speaks the
        // same `BTRFS_IOC_SNAP_CREATE_V2` contract.
        let staging = format!("stage-{name}");
        let staging_path = worlds.join(&staging);
        if staging_path.exists() {
            self.driver.destroy_subvolume(&worlds, &staging)?;
        }
        let imported = (|| {
            self.driver.create_subvolume(&worlds, &staging)?;
            copy_tree(source_dir, &staging_path)?;
            self.seal_snapshot(&staging_path, name)
        })();
        let destroyed = self.driver.destroy_subvolume(&worlds, &staging);
        match (imported, destroyed) {
            (Ok(()), Ok(())) => Ok(()),
            // A failed cleanup is reported, never hidden: the driver names the residue.
            (Ok(()), Err(err)) => Err(err),
            (Err(err), _) => Err(err),
        }
    }

    pub(crate) fn seal_world(
        &self,
        world: &WorldHandle,
        id: &SnapshotId,
    ) -> Result<(), EngineError> {
        let name = id.validate()?;

        // Services are stopped while the world is still live so the sealed subvolume is
        // crash-consistent (`ADR-0010` §3).
        {
            let mut worlds = self.lock()?;
            Self::sealable(self.owned(&mut worlds, world)?, world)?;
        }
        self.stop_all_services(world)?;

        let world_dir = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            Self::sealable(record, world)?;
            record.status = WorldStatus::Freezing;
            record.world_dir.clone()
        };

        if let Err(err) = self.seal_snapshot(&world_dir, name) {
            self.reopen_after_failed_freeze(world);
            return Err(err);
        }

        let sealed = {
            let mut worlds = self.lock()?;
            match self.owned(&mut worlds, world) {
                Ok(record) if record.is_gone() => Err(EngineError::WorldGone {
                    world: format!("{world:?}"),
                }),
                Ok(record) if record.status == WorldStatus::Freezing => {
                    record.status = WorldStatus::Frozen;
                    Ok(())
                }
                Ok(_) => Err(EngineError::WorldNotFreezable {
                    world: format!("{world:?}"),
                }),
                Err(err) => Err(err),
            }
        };
        if let Err(err) = sealed {
            // The world was destroyed while the seal was running: drop the half-claimed snapshot
            // and fail closed instead of resurrecting a destroyed world.
            let _ = self.driver.destroy_subvolume(&self.snapshots_dir(), name);
            return Err(err);
        }
        Ok(())
    }
}

impl BtrfsEngine {
    /// `true` when `path` is a subvolume this engine manages under `parent`.
    pub(crate) fn is_managed_subvolume(
        &self,
        parent: &Path,
        path: &Path,
    ) -> Result<bool, EngineError> {
        Ok(self
            .driver
            .list_children(parent)?
            .iter()
            .any(|child| child == path))
    }

    /// Seal `source` into `<state_dir>/snapshots/<name>`, refusing to replace an existing snapshot
    /// (`ADR-0010` §3: a sealed snapshot is immutable).
    pub(crate) fn seal_snapshot(&self, source: &Path, name: &str) -> Result<(), EngineError> {
        let snapshots = self.snapshots_dir();
        fs::create_dir_all(&snapshots).map_err(|err| io_error(&snapshots, &err))?;
        if snapshots.join(name).exists() {
            return Err(EngineError::SnapshotExists {
                id: name.to_owned(),
            });
        }
        self.driver
            .snapshot_ro(source, &snapshots, name)
            .map(|_| ())
    }
}

/// Copy one plain directory tree into a freshly created subvolume.
///
/// Only used when [`WorldEngine::create_snapshot`] imports a source that is not already a managed
/// subvolume (the daemon's empty base-seed directory): a snapshot is always a subvolume, so the
/// one non-CoW source is materialized once instead of teaching every consumer a second kind of
/// snapshot.
fn copy_tree(from: &Path, to: &Path) -> Result<(), EngineError> {
    fs::create_dir_all(to).map_err(|err| io_error(to, &err))?;
    for entry in fs::read_dir(from).map_err(|err| io_error(from, &err))? {
        let entry = entry.map_err(|err| io_error(from, &err))?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let file_type = entry.file_type().map_err(|err| io_error(&source, &err))?;
        if file_type.is_dir() {
            copy_tree(&source, &target)?;
        } else if file_type.is_file() {
            fs::copy(&source, &target).map_err(|err| io_error(&target, &err))?;
        } else if file_type.is_symlink() {
            let link = fs::read_link(&source).map_err(|err| io_error(&source, &err))?;
            std::os::unix::fs::symlink(link, &target).map_err(|err| io_error(&target, &err))?;
        } else {
            return Err(EngineError::Io {
                path: source.display().to_string(),
                message: "cannot import a snapshot source containing a socket, fifo, or device \
                          node"
                    .to_owned(),
            });
        }
    }
    Ok(())
}
