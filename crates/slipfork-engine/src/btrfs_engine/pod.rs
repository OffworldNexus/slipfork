//! Per-service pod planning, OCI bundle synthesis, and `crun run` spawn
//! (`ARCHITECTURE.md` §3.3, `docs/design/forkfile.md` §3).
//!
//! The world owns no root filesystem: its mount namespace is a private copy of the host's, so
//! every bundle path — rootfs, `process.cwd`, bind sources, the bundle itself, `crun --root` — is
//! the real host path. A pod joins the shared init's published `user`/`network`/`ipc` namespaces.

#![forbid(unsafe_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use slipfork_world::{EngineError, MountMode, PodNamespaces, ServicePodSpec, WorldHandle};

use crate::oci::{self, ContainerBundle, ContainerMount};

use super::crun::CrunSurface;
use super::io_error;
use super::services::ServiceSpawn;

/// Materialize one `Forkfile` service pod's OCI bundle from its injected spec.
///
/// The spec is the sole source of every value. Paths are host paths because the world mount
/// namespace shows the host tree 1:1; the bundle is created `0700` and `config.json` is left
/// `0600` because the environment may carry resolved secrets.
///
/// This is bundle synthesis only — for a live world use `pod_bundle_dir` + `world_mount_path`.
pub fn write_pod_bundle(
    host_uid: u32,
    host_gid: u32,
    worktree: &Path,
    spec: &ServicePodSpec,
) -> Result<PathBuf, EngineError> {
    write_pod_bundle_at(host_uid, host_gid, worktree, spec, &spec.bundle_dir)
}

/// Materialize one pod bundle at `bundle_host`, which may be a world-visible path reached through
/// `/proc/<init_pid>/root` when the control directory is covered by the world's private tmpfs.
pub fn write_pod_bundle_at(
    host_uid: u32,
    host_gid: u32,
    worktree: &Path,
    spec: &ServicePodSpec,
    bundle_host: &Path,
) -> Result<PathBuf, EngineError> {
    let mut mounts = Vec::new();
    for mount in spec
        .mounts(worktree)
        .map_err(|err| EngineError::InvalidWorldSpec {
            reason: err.to_string(),
        })?
    {
        mounts.push(ContainerMount {
            source: mount.source,
            destination: mount.target,
            read_only: mount.mode == MountMode::ReadOnly,
        });
    }
    create_private_bundle_dir(bundle_host)?;
    let joins = joined_namespaces(spec.namespace_joins().into_iter());
    oci::write_bundle(&ContainerBundle {
        bundle_dir: bundle_host,
        image_rootfs: &spec.rootfs,
        argv: spec.argv(),
        env: &spec.command.env,
        uid: host_uid,
        gid: host_gid,
        mounts: &mounts,
        namespace_joins: &joins,
        cwd: spec.cwd(),
        passwd_guest: None,
        group_guest: None,
    })
    .map_err(|err| io_error(bundle_host, &err))?;
    make_private(&bundle_host.join(oci::OCI_CONFIG_FILE))
}

/// Only the **IPC** namespace is inherited: the runtime already runs inside the world's IPC
/// namespace (`nsenter --ipc`), and an OCI `path` join cannot be opened from inside the world.
/// The user namespace is created identity-mapped, and `pid`/`mount`/`uts` are always fresh.
fn joined_namespaces<'a>(
    entries: impl Iterator<Item = (&'a str, &'a Path)>,
) -> Vec<(&'a str, &'a Path)> {
    entries.filter(|(kind, _path)| *kind == "ipc").collect()
}

/// Create (or reuse) a bundle directory only the invoking developer can traverse or list.
pub(crate) fn create_private_bundle_dir(dir: &Path) -> Result<(), EngineError> {
    fs::create_dir_all(dir).map_err(|err| io_error(dir, &err))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|err| io_error(dir, &err))
}

fn make_private(config: &Path) -> Result<PathBuf, EngineError> {
    fs::set_permissions(config, fs::Permissions::from_mode(0o600))
        .map_err(|err| io_error(config, &err))?;
    Ok(config.to_path_buf())
}

impl super::BtrfsEngine {
    /// `<ctl>/<world_id>/oci/<service>`: the engine-owned bundle directory of one service pod.
    ///
    /// The daemon cut-over plans a component pod with this path, the world's published
    /// `world_pod_namespaces`, and a `VolumePlan` rooted on the world subvolume.
    pub fn pod_bundle_dir(
        &self,
        world: &WorldHandle,
        service: &str,
    ) -> Result<PathBuf, EngineError> {
        let mut worlds = self.lock()?;
        let record = self.live_record(&mut worlds, world)?;
        Self::pod_bundle_dir_of(&record.ctl_dir, service)
    }

    pub(crate) fn pod_bundle_dir_of(ctl_dir: &Path, service: &str) -> Result<PathBuf, EngineError> {
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

    /// Read the namespace joins the world init published (`namespaces.json`).
    pub(crate) fn read_pod_namespaces(ctl_dir: &Path) -> Result<PodNamespaces, EngineError> {
        let path = ctl_dir.join(crate::world_init::WORLD_INIT_NAMESPACES_FILE);
        let raw = fs::read_to_string(&path).map_err(|err| EngineError::WorldInitFailed {
            world: ctl_dir.display().to_string(),
            reason: format!(
                "cannot read the world's published namespace joins '{}': {err}\n  fix: restart \
                 the world with `slipfork up` (the shared init exited before publishing its \
                 namespaces)",
                path.display()
            ),
        })?;
        let published: PodNamespaces =
            serde_json::from_str(&raw).map_err(|err| EngineError::WorldInitFailed {
                world: ctl_dir.display().to_string(),
                reason: format!(
                    "invalid namespace publication '{}': {err}\n  fix: restart the world with \
                     `slipfork up`",
                    path.display()
                ),
            })?;
        PodNamespaces::new(&published.userns, &published.netns, &published.ipcns).map_err(|err| {
            EngineError::WorldInitFailed {
                world: ctl_dir.display().to_string(),
                reason: format!(
                    "the world published a relative namespace join: {err}\n  fix: restart the \
                     world with `slipfork up`"
                ),
            }
        })
    }

    /// Materialize and run one `Forkfile` service pod joined to the world's shared namespaces.
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
                     fix: plan the pod with the engine bundle directory (`pod_bundle_dir`)",
                    pod.service,
                    pod.bundle_dir.display(),
                    expected.display()
                ),
            });
        }
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
        // Each CoW volume is a plain directory in the world subvolume, so the one snap create
        // captures it; it must exist before the bundle can bind it over the live worktree.
        for volume in pod.volumes.volumes() {
            fs::create_dir_all(&volume.host_subvolume)
                .map_err(|err| io_error(&volume.host_subvolume, &err))?;
        }
        let runtime = self.pod_crun_surface(spawn);
        let bundle_host = runtime.host_path(&pod.bundle_dir);
        write_pod_bundle_at(self.host_uid, self.host_gid, worktree, pod, &bundle_host)?;

        let container_id = oci::container_id(spawn.world.seq(), &pod.service);
        runtime.run_bundle(
            spawn.env,
            &format!("service pod '{}'", pod.service),
            &bundle_host,
            &pod.bundle_dir,
            &container_id,
            spawn.log,
            spawn.process,
        )
    }

    fn pod_crun_surface(&self, spawn: &ServiceSpawn<'_>) -> CrunSurface {
        CrunSurface::new(
            self,
            spawn.init_pid,
            Self::crun_host_bin(spawn.rootfs),
            spawn.ctl_dir.join("oci-run"),
            spawn.state_dir.to_path_buf(),
        )
    }
}
