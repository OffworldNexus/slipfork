//! Per-service pod planning, OCI bundle synthesis, and `crun run` spawn
//! (`ARCHITECTURE.md` §3.3, `docs/design/forkfile.md` §3).
//!
//! A pod is one `crun` container whose bundle the engine owns at
//! `<ctl>/<world_id>/oci/<service>/`, outside the world subvolume. Its rootfs, live worktree, and
//! CoW volumes are announced as the guest paths the world itself mounts
//! ([`BtrfsEngine::pod_world_mounts`]), and its `user`/`network`/`ipc` namespace entries join the
//! shared init that is world PID 1 (`/proc/1/ns/*`).

#![forbid(unsafe_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use slipfork_world::{EngineError, MountMode, ServicePodSpec, WorldHandle, WorldMount};

use crate::oci::{self, ContainerBundle, ContainerMount};
use crate::world_init::{
    OCI_BUNDLES_GUEST, OCI_RUN_GUEST, POD_ROOTFS_GUEST, POD_WORLD_GUEST, world_guest_for_host,
};

use super::container::CrunSurface;
use super::io_error;
use super::services::ServiceSpawn;

/// Guest path of one service's bundle: the engine boots `<ctl>/<world_id>/oci` at
/// [`OCI_BUNDLES_GUEST`], so `crun -b` addresses the bundle by service id alone.
pub fn pod_bundle_guest(service: &str) -> PathBuf {
    PathBuf::from(OCI_BUNDLES_GUEST).join(service)
}

/// The two guest trees every world exposes to service pods: the read-only app-rootfs cache
/// (`root.path`) and the world's own subvolume (CoW volumes). Boot turns them into world mounts;
/// pod spawn appends them to the world's mount table so guest paths resolve.
pub(crate) fn pod_world_mounts(state_dir: &Path, world_dir: &Path) -> [WorldMount; 2] {
    [
        WorldMount {
            host_path: state_dir.join("cache").join("rootfs"),
            guest_path: PathBuf::from(POD_ROOTFS_GUEST),
            mode: MountMode::ReadOnly,
        },
        WorldMount {
            host_path: world_dir.to_path_buf(),
            guest_path: PathBuf::from(POD_WORLD_GUEST),
            mode: MountMode::ReadWrite,
        },
    ]
}

/// Materialize one `Forkfile` service pod's OCI bundle from its injected spec.
///
/// The spec is the sole source of every value, and **every path in the emitted document is a
/// world-visible guest path**: `root.path`, `process.cwd`, and each bind `source` are resolved
/// through the world's own mount table ([`world_guest_for_host`]), while the bundle itself is
/// addressed by [`pod_bundle_guest`]. A path the world does not expose fails closed with a remedy
/// instead of leaking a host path that only happens to resolve because a mount is 1:1.
///
/// The bundle directory is created `0700` and `config.json` is left `0600`, because the
/// environment may carry resolved secrets. Returns the written `config.json` path.
pub fn write_pod_bundle(
    host_uid: u32,
    host_gid: u32,
    world_mounts: &[WorldMount],
    worktree: &Path,
    spec: &ServicePodSpec,
) -> Result<PathBuf, EngineError> {
    let guest = |host: &Path, what: &str, fix: &str| {
        world_guest_for_host(world_mounts, host).ok_or_else(|| EngineError::InvalidWorldSpec {
            reason: format!(
                "service pod '{}' {what} '{}' is not visible inside the world\n  fix: {fix}",
                spec.service,
                host.display()
            ),
        })
    };
    let mut mounts = Vec::new();
    for mount in spec
        .mounts(worktree)
        .map_err(|err| EngineError::InvalidWorldSpec {
            reason: err.to_string(),
        })?
    {
        let source = guest(
            &mount.source,
            "mount source",
            "mount the live worktree read-write in the world",
        )?;
        mounts.push(ContainerMount {
            source,
            destination: mount.target,
            read_only: mount.mode == MountMode::ReadOnly,
        });
    }
    let image_rootfs_guest = guest(
        &spec.rootfs,
        "rootfs",
        "mount the rootfs cache read-only in the world at a `/.slipfork/images/…` guest path",
    )?;
    let cwd = guest(
        spec.cwd(),
        "working directory",
        "mount the live worktree read-write in the world",
    )?;
    create_private_bundle_dir(&spec.bundle_dir)?;
    let joins: Vec<(&str, &Path)> = spec
        .namespace_joins()
        .iter()
        .map(|(kind, path)| (*kind, *path))
        .collect();
    oci::write_bundle(&ContainerBundle {
        bundle_dir: &spec.bundle_dir,
        image_rootfs_guest: &image_rootfs_guest,
        argv: spec.argv(),
        env: &spec.command.env,
        uid: host_uid,
        gid: host_gid,
        mounts: &mounts,
        namespace_joins: &joins,
        cwd: &cwd,
        // A pod inherits the world init's identity map, so it carries no synthesized
        // `/etc/passwd` of its own.
        passwd_guest: None,
        group_guest: None,
    })
    .map_err(|err| io_error(&spec.bundle_dir, &err))?;
    let config = spec.bundle_dir.join(oci::OCI_CONFIG_FILE);
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600))
        .map_err(|err| io_error(&config, &err))?;
    Ok(config)
}

/// Create (or reuse) a bundle directory only the invoking developer can traverse or list.
fn create_private_bundle_dir(dir: &Path) -> Result<(), EngineError> {
    fs::create_dir_all(dir).map_err(|err| io_error(dir, &err))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|err| io_error(dir, &err))
}

impl super::BtrfsEngine {
    /// `<ctl>/<world_id>/oci/<service>`: the engine-owned bundle directory of one service pod.
    ///
    /// A caller planning a [`ServicePodSpec`] asks here instead of inventing a directory, so the
    /// bundle can never land inside the world subvolume and `crun -b` always addresses it by the
    /// guest path [`pod_bundle_guest`] names.
    pub fn pod_bundle_dir(
        &self,
        world: &WorldHandle,
        service: &str,
    ) -> Result<PathBuf, EngineError> {
        let mut worlds = self.lock()?;
        let record = self.live_record(&mut worlds, world)?;
        Self::pod_bundle_dir_of(&record.ctl_dir, service)
    }

    fn pod_bundle_dir_of(ctl_dir: &Path, service: &str) -> Result<PathBuf, EngineError> {
        if !slipfork_world::is_valid_dns_label(service) {
            return Err(EngineError::InvalidWorldSpec {
                reason: format!(
                    "service pod id '{service}' is not a single lowercase DNS label\n  fix: name \
                     the service block with a slug such as `service api`"
                ),
            });
        }
        Ok(ctl_dir.join("oci").join(service))
    }

    /// Materialize and run one `Forkfile` service pod joined to the world's shared namespaces.
    ///
    /// The bundle is written from the spec by [`write_pod_bundle`], then `crun run` starts it as
    /// the supervised child attached to the world's own `nsenter` prefix. Logs stream into the
    /// service's retained ring, and the recorded container process is the same one the stop path
    /// signals (`crun kill`), deletes, and whose bundle it removes.
    pub(crate) fn spawn_service_pod(
        &self,
        spawn: &ServiceSpawn<'_>,
        pod: &ServicePodSpec,
    ) -> Result<(), EngineError> {
        let expected = Self::pod_bundle_dir_of(spawn.ctl_dir, &pod.service)?;
        if pod.bundle_dir != expected {
            return Err(EngineError::InvalidWorldSpec {
                reason: format!(
                    "service pod '{}' bundle '{}' is not the engine control-dir bundle '{}'\n  \
                     fix: plan the pod with `slipfork up`'s engine bundle directory \
                     (`pod_bundle_dir`), never a path inside the world subvolume",
                    pod.service,
                    pod.bundle_dir.display(),
                    expected.display()
                ),
            });
        }
        let mut mapping = spawn.mounts.to_vec();
        mapping.extend(pod_world_mounts(&self.state_dir, spawn.world_dir));
        let worktree = spawn
            .mounts
            .iter()
            .find(|mount| {
                mount.mode == MountMode::ReadWrite && pod.cwd().starts_with(&mount.host_path)
            })
            .map(|mount| mount.host_path.as_path())
            .ok_or_else(|| EngineError::InvalidWorldSpec {
                reason: format!(
                    "service pod '{}' working directory '{}' is not inside a read-write world \
                     mount\n  fix: mount the live worktree read-write in the world",
                    pod.service,
                    pod.cwd().display()
                ),
            })?;
        write_pod_bundle(self.host_uid, self.host_gid, &mapping, worktree, pod)?;

        let container_id = oci::container_id(spawn.world.seq(), &pod.service);
        let runtime = CrunSurface::new(
            self,
            spawn.init_pid,
            spawn.rootfs.crun_bin.clone(),
            PathBuf::from(OCI_RUN_GUEST),
        );
        runtime.run_bundle(
            spawn.env,
            &format!("service pod '{}'", pod.service),
            &pod.bundle_dir,
            &pod_bundle_guest(&pod.service),
            &container_id,
            spawn.log,
            spawn.process,
        )
    }
}
