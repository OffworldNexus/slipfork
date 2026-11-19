//! World subvolume materialization and the `clone_world` / `reload_world` verbs
//! (`ADR-0010` §§2–3).

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use slipfork_world::rootfs::WorldRootfs;
use slipfork_world::{
    EngineError, RootfsLineage, ServiceImageLineage, WorldEngine, WorldHandle, WorldLineage,
    WorldSpec, WorldTarget,
};

use crate::WorldStatus;

use super::boot::abandon_unbooted_world;
use super::teardown::{SERVICE_STOP_GRACE, ServiceTeardown, WORLD_STOP_TERM_GRACE};
use super::{
    BtrfsEngine, WORLD_ROOTFS_DIR, WORLD_STATE_CATEGORIES, WORLD_STATE_DIR, WorldRecord,
    WorldSubvolume, io_error,
};

/// Validated, mutation-free pre-flight inputs resolved before [`WorldEngine::clone_world`] or
/// [`WorldEngine::reload_world`] touches a service, PID 1, control directory, or subvolume.
pub(crate) struct WorldPreflight {
    source: PathBuf,
    home: PathBuf,
}

impl BtrfsEngine {
    /// Stable subvolume name for one world: `<target slug>-<sequence>`.
    fn world_id(&self, target: &WorldTarget, seq: u64) -> String {
        format!("{}-{seq}", crate::target_slug(target))
    }

    /// The first free `<slug>-<seq>` world id, skipping names a previous daemon left behind so a
    /// restarted `slipforkd` never collides with an existing subvolume.
    fn allocate_world_id(&self, target: &WorldTarget) -> Result<(u64, String), EngineError> {
        let worlds = self.worlds_dir();
        fs::create_dir_all(&worlds).map_err(|err| io_error(&worlds, &err))?;
        let existing = self.driver.list_children(&worlds)?;
        let mut seq = self.shared.next_seq.fetch_add(1, Ordering::Relaxed) + 1;
        loop {
            let world_id = self.world_id(target, seq);
            let taken = existing.iter().any(|child| {
                child
                    .file_name()
                    .is_some_and(|name| name == OsStr::new(&world_id))
            });
            if !taken {
                return Ok((seq, world_id));
            }
            seq += 1;
        }
    }

    /// Materialize the world's required layout inside its single subvolume.
    ///
    /// `<subvol>/rootfs` references the content-addressed, immutable `L0` tree in
    /// `<state_dir>/cache/rootfs/<digest>/app` read-only: the cache is the one copy of the image,
    /// so a clone never duplicates gigabytes of rootfs content and never mutates the cache. A
    /// clone of an already-sealed world inherits that reference, and it is repointed at the spec's
    /// tree so a rebuilt image never boots a stale rootfs. `state/{pgdata,redis,s3}` are plain
    /// directories — never subvolumes — so one `BTRFS_IOC_SNAP_CREATE_V2` captures the whole world
    /// (`ADR-0010` §2).
    fn materialize_world(&self, world_dir: &Path, spec: &WorldSpec) -> Result<(), EngineError> {
        self.validate_rootfs_reference(&spec.rootfs)?;
        let rootfs = world_dir.join(WORLD_ROOTFS_DIR);
        match fs::symlink_metadata(&rootfs) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                fs::remove_file(&rootfs).map_err(|err| io_error(&rootfs, &err))?;
            }
            Ok(_) => {
                return Err(EngineError::Io {
                    path: rootfs.display().to_string(),
                    message: "the cloned world carries a rootfs that is not the engine's \
                              read-only reference; a world subvolume holds a reference to the \
                              content-addressed cache, never a second copy of the image"
                        .to_owned(),
                });
            }
            Err(_) => {}
        }
        std::os::unix::fs::symlink(&spec.rootfs.app_rootfs, &rootfs).map_err(|err| {
            EngineError::Io {
                path: rootfs.display().to_string(),
                message: format!(
                    "cannot materialize the world rootfs reference to '{}': {err}",
                    spec.rootfs.app_rootfs.display()
                ),
            }
        })?;
        for category in WORLD_STATE_CATEGORIES {
            let dir = world_dir.join(WORLD_STATE_DIR).join(category);
            fs::create_dir_all(&dir).map_err(|err| io_error(&dir, &err))?;
        }
        Ok(())
    }

    /// The world's `rootfs` reference must name the content-addressed cache tree this engine owns:
    /// `<state_dir>/cache/rootfs/<digest>/app`. Anything else would make a world boot a tree the
    /// engine never materialized (or a mutable tree inside the subvolume), so it fails closed
    /// before a subvolume is created.
    fn validate_rootfs_reference(&self, rootfs: &WorldRootfs) -> Result<(), EngineError> {
        let expected = self
            .state_dir
            .join("cache")
            .join("rootfs")
            .join(rootfs.digest.hex())
            .join("app");
        if rootfs.app_rootfs == expected {
            return Ok(());
        }
        Err(EngineError::InvalidWorldSpec {
            reason: format!(
                "rootfs '{}' is not the content-addressed cache tree '{}' for digest {}; a world \
                 references the immutable cache read-only and never copies or substitutes a \
                 rootfs tree",
                rootfs.app_rootfs.display(),
                expected.display(),
                rootfs.digest
            ),
        })
    }

    /// Validate `spec` and resolve its clone source and world-scoped `HOME` **before any
    /// mutation**: `spec.validate()`, `HOME` presence, an existing sealed snapshot, and the
    /// content-addressed cache rootfs reference.
    ///
    /// Called first by both [`WorldEngine::clone_world`] and [`WorldEngine::reload_world`] so both
    /// verbs enforce identical preconditions before stopping services or touching subvolumes.
    fn world_preflight(&self, spec: &WorldSpec) -> Result<WorldPreflight, EngineError> {
        spec.validate()?;
        let home = spec
            .base_env
            .home()
            .ok_or_else(|| EngineError::InvalidWorldSpec {
                reason: "base_env must carry a world-scoped HOME (ExecEnv::world_scoped)"
                    .to_owned(),
            })?
            .to_path_buf();
        let source = self.snapshots_dir().join(spec.snapshot.validate()?);
        if !source.is_dir() {
            return Err(EngineError::SnapshotNotFound(spec.snapshot.0.clone()));
        }
        self.validate_rootfs_reference(&spec.rootfs)?;
        Ok(WorldPreflight { source, home })
    }

    /// The seal precondition: only a `Live` world can be sealed, a gone world reports
    /// [`EngineError::WorldGone`], and an already-sealed one [`EngineError::WorldNotFreezable`].
    pub(crate) fn sealable(record: &WorldRecord, world: &WorldHandle) -> Result<(), EngineError> {
        if record.is_gone() {
            return Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            });
        }
        match record.status {
            WorldStatus::Live => Ok(()),
            WorldStatus::Freezing | WorldStatus::Frozen => Err(EngineError::WorldNotFreezable {
                world: format!("{world:?}"),
            }),
            WorldStatus::Failed => Err(EngineError::WorldNotLive {
                world: format!("{world:?}"),
            }),
            WorldStatus::Destroyed => Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            }),
        }
    }

    /// The reload precondition, the same lifecycle gate [`Self::sealable`] enforces for freeze:
    /// only a `Live` world can be re-rooted, and a world whose previous reload failed to boot may
    /// retry. A sealing/sealed world would be re-materialized over a read-only subvolume, so it
    /// fails closed instead.
    pub(crate) fn reloadable(record: &WorldRecord, world: &WorldHandle) -> Result<(), EngineError> {
        if record.is_gone() {
            return Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            });
        }
        match record.status {
            WorldStatus::Live | WorldStatus::Failed => Ok(()),
            WorldStatus::Freezing | WorldStatus::Frozen => Err(EngineError::WorldNotReloadable {
                world: format!("{world:?}"),
            }),
            WorldStatus::Destroyed => Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            }),
        }
    }

    /// The record a reload re-roots: [`Self::reloadable`], failing closed before any mutation.
    pub(crate) fn reload_record<'a>(
        &self,
        worlds: &'a mut BTreeMap<u64, WorldRecord>,
        world: &WorldHandle,
    ) -> Result<&'a mut WorldRecord, EngineError> {
        let record = self.owned(worlds, world)?;
        Self::reloadable(record, world)?;
        Ok(record)
    }

    /// Roll a failed seal back to `Live` so a transient filesystem error does not brick the world.
    pub(crate) fn reopen_after_failed_freeze(&self, world: &WorldHandle) {
        if let Ok(mut worlds) = self.lock()
            && let Ok(record) = self.owned(&mut worlds, world)
            && record.status == WorldStatus::Freezing
            && !record.destroying
        {
            record.status = WorldStatus::Live;
        }
    }
}

impl BtrfsEngine {
    pub(crate) fn create_world(&self, spec: &WorldSpec) -> Result<WorldHandle, EngineError> {
        let preflight = self.world_preflight(spec)?;
        fs::create_dir_all(&preflight.home).map_err(|err| io_error(&preflight.home, &err))?;

        let worlds = self.worlds_dir();
        fs::create_dir_all(&worlds).map_err(|err| io_error(&worlds, &err))?;
        // One ioctl clones the whole world subvolume: rootfs reference plus state directories. A
        // name a concurrent creator took between the listing and the ioctl is skipped, not fatal.
        let mut attempts = 0;
        let (seq, world_id) = loop {
            let (seq, world_id) = self.allocate_world_id(&spec.target)?;
            match self
                .driver
                .snapshot_writable(&preflight.source, &worlds, &world_id)
            {
                Ok(_) => break (seq, world_id),
                Err(EngineError::Ioctl { errno, .. }) if errno == libc::EEXIST && attempts < 8 => {
                    attempts += 1;
                }
                Err(err) => return Err(err),
            }
        };
        let world = self.mint_world_handle(seq);
        let world_dir = self.world_dir(&world_id);
        let ctl_dir = self.ctl_dir(&world_id);

        let booted = self
            .materialize_world(&world_dir, spec)
            .and_then(|()| self.boot_world(&world_id, &world_dir, &ctl_dir, spec));
        let (netns_sock, init, launcher) = match booted {
            Ok(booted) => booted,
            Err(err) => {
                let _ = self.driver.destroy_subvolume(&worlds, &world_id);
                let _ = fs::remove_dir_all(&ctl_dir);
                return Err(err);
            }
        };

        let mut locked = match self.lock() {
            Ok(locked) => locked,
            Err(err) => {
                let _ = abandon_unbooted_world(Some(&init), launcher);
                let _ = fs::remove_dir_all(&ctl_dir);
                let _ = self.driver.destroy_subvolume(&worlds, &world_id);
                return Err(err);
            }
        };
        let init_pid = init.pid;
        locked.insert(
            seq,
            WorldRecord {
                world_id,
                world_dir: world_dir.clone(),
                ctl_dir,
                state_dir: world_dir.join(WORLD_STATE_DIR),
                netns_sock,
                init_pid,
                init_identity: Some(init),
                launcher: Some(launcher),
                base_env: spec.base_env.clone(),
                rootfs: spec.rootfs.clone(),
                mounts: spec.mounts.clone(),
                status: WorldStatus::Live,
                destroying: false,
                services: BTreeMap::new(),
                lineage: boot_lineage(spec),
                subvolume: WorldSubvolume::Cloned(spec.snapshot.clone()),
            },
        );
        Ok(world)
    }

    pub(crate) fn rebuild_world(
        &self,
        world: &WorldHandle,
        spec: &WorldSpec,
    ) -> Result<(), EngineError> {
        // Shared pre-flight runs **before any mutation** (identical to `clone_world`): an invalid
        // spec, missing `HOME`, foreign rootfs, or pruned snapshot fails before touching services,
        // PID 1, or the world subvolume.
        let preflight = self.world_preflight(spec)?;
        let (world_id, world_dir, ctl_dir, services) = {
            let mut worlds = self.lock()?;
            let record = self.reload_record(&mut worlds, world)?;
            let services: Vec<ServiceTeardown> = record
                .services
                .iter()
                .map(|(id, service)| ServiceTeardown::from_record(id, service))
                .collect();
            (
                record.world_id.clone(),
                record.world_dir.clone(),
                record.ctl_dir.clone(),
                services,
            )
        };
        // 1. Stop every world-owned service, 2. reap the world PID 1: nothing from the old run may
        // hold the rootfs reference or a state directory while it is reconciled.
        self.stop_services_with_grace(&services, SERVICE_STOP_GRACE)?;
        self.reap_world_init(world, WORLD_STOP_TERM_GRACE)?;
        let current_subvolume = {
            let mut worlds = self.lock()?;
            let record = self.reload_record(&mut worlds, world)?;
            record.services.clear();
            record.status = WorldStatus::Failed;
            record.subvolume.clone()
        };

        let booted = (|| {
            // Reconcile the subvolume with `spec.snapshot`, recording each on-disk storage
            // transition as soon as it succeeds so a later failure never leaves stale base/rootfs
            // claims over replaced (or absent) storage.
            match current_subvolume {
                WorldSubvolume::Cloned(existing) if existing == spec.snapshot => {}
                WorldSubvolume::Cloned(_) => {
                    self.driver
                        .destroy_subvolume(&self.worlds_dir(), &world_id)?;
                    {
                        let mut worlds = self.lock()?;
                        let record = self.reload_record(&mut worlds, world)?;
                        record.subvolume = WorldSubvolume::Missing;
                        record.lineage = WorldLineage {
                            rootfs: None,
                            services: Vec::new(),
                            seed: None,
                            base: None,
                        };
                    }
                    self.driver.snapshot_writable(
                        &preflight.source,
                        &self.worlds_dir(),
                        &world_id,
                    )?;
                    let mut worlds = self.lock()?;
                    let record = self.reload_record(&mut worlds, world)?;
                    record.subvolume = WorldSubvolume::Cloned(spec.snapshot.clone());
                }
                WorldSubvolume::Missing => {
                    self.driver.snapshot_writable(
                        &preflight.source,
                        &self.worlds_dir(),
                        &world_id,
                    )?;
                    let mut worlds = self.lock()?;
                    let record = self.reload_record(&mut worlds, world)?;
                    record.subvolume = WorldSubvolume::Cloned(spec.snapshot.clone());
                }
            }

            self.materialize_world(&world_dir, spec)?;
            {
                let mut worlds = self.lock()?;
                let record = self.reload_record(&mut worlds, world)?;
                record.rootfs = spec.rootfs.clone();
                record.lineage = boot_lineage(spec);
            }
            fs::create_dir_all(&preflight.home).map_err(|err| io_error(&preflight.home, &err))?;

            self.boot_world(&world_id, &world_dir, &ctl_dir, spec)
        })();

        match booted {
            Ok((netns_sock, init, launcher)) => {
                let mut worlds = self.lock()?;
                let record = self.reload_record(&mut worlds, world)?;
                record.netns_sock = netns_sock;
                record.init_pid = init.pid;
                record.init_identity = Some(init);
                record.launcher = Some(launcher);
                record.mounts = spec.mounts.clone();
                record.base_env = spec.base_env.clone();
                record.status = WorldStatus::Live;
                Ok(())
            }
            Err(err) => {
                // No PID 1 survived this reload: keep only the storage/rootfs/lineage facts that
                // actually hold on disk, ensure the record stays non-live (`WorldStatus::Failed`
                // so a retry recomputes `same_base` from truth and `destroy_world` still owns it),
                // and drop the half-built control directory.
                if let Ok(mut worlds) = self.lock()
                    && let Ok(record) = self.owned(&mut worlds, world)
                {
                    record.init_pid = 0;
                    record.init_identity = None;
                    record.launcher = None;
                    record.status = WorldStatus::Failed;
                }
                let _ = fs::remove_dir_all(&ctl_dir);
                Err(err)
            }
        }
    }
}

/// The lineage facts a fresh clone records, taken verbatim from the spec it booted from.
///
/// Rootfs/service facts are the materialized trees' own cache outcome and measured `millis`; the
/// base/seed facts are the seeder's measurements. Nothing is estimated here.
fn boot_lineage(spec: &WorldSpec) -> WorldLineage {
    WorldLineage {
        rootfs: Some(RootfsLineage {
            digest: spec.rootfs.digest.hex().to_owned(),
            cache: spec.rootfs.cache,
            millis: spec.rootfs.millis,
        }),
        services: spec
            .rootfs
            .service_images
            .iter()
            .map(|(image, oci)| ServiceImageLineage {
                id: image.to_string(),
                cache: oci.cache,
                millis: oci.millis,
            })
            .collect(),
        seed: spec.boot.seed.clone(),
        base: spec.boot.base.clone(),
    }
}
