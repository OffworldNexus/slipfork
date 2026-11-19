//! Per-service pod planning, OCI bundle synthesis, and `crun run` spawn
//! (`ARCHITECTURE.md` §3.3, `docs/design/forkfile.md` §3).
//!
//! The world owns no root filesystem: its mount namespace is a private copy of the host's, so
//! every bundle path — rootfs, `process.cwd`, bind sources, the bundle itself, `crun --root` — is
//! the real host path. A pod joins the shared init's published `user`/`network`/`ipc` namespaces.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use slipfork_world::{
    EngineError, ExecOutput, MountMode, PodNamespaces, ServiceLogs, ServicePodSpec, StreamChunk,
    WorldHandle, WorldMount,
};

use crate::child_io::run_streamed;
use crate::oci::{self, ContainerBundle, ContainerMount};

use super::BtrfsEngine;
use super::crun::CrunSurface;
use super::io_error;
use super::services::{ServiceProcess, ServiceSpawn};

/// Materialize one `Forkfile` service pod's OCI bundle from its injected spec.
///
/// The spec is the sole source of every value. Paths are host paths because the world mount
/// namespace shows the host tree 1:1; the bundle is created `0700` and `config.json` is left
/// `0600` because the environment may carry resolved secrets.
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

/// One legacy world process (`ServiceRuntime::WorldProcess` or `exec_in_world`), materialized as a
/// pod-shaped OCI bundle in the world's private tmpfs.
///
/// It runs in the world's namespaces on the app rootfs with the world's bind mounts, exactly like
/// a pod, so the same supervision, log, and stop paths apply.
pub(crate) struct WorldProcessBundle<'a> {
    host_uid: u32,
    host_gid: u32,
    rootfs: PathBuf,
    state_dir: PathBuf,
    mounts: Vec<WorldMount>,
    namespaces: PodNamespaces,
    argv: &'a [String],
    cwd: &'a Path,
    env: BTreeMap<String, String>,
    label: String,
    bundle: PathBuf,
    runtime: CrunSurface,
    container_id: String,
}

impl<'a> WorldProcessBundle<'a> {
    /// The bundle of one `ServiceRuntime::WorldProcess` service.
    pub(crate) fn for_service(
        engine: &BtrfsEngine,
        spawn: &ServiceSpawn<'a>,
        argv: &'a [String],
        cwd: &'a Path,
    ) -> Result<Self, EngineError> {
        let runtime = CrunSurface::new(
            engine,
            spawn.init_pid,
            BtrfsEngine::crun_host_bin(spawn.rootfs),
            spawn.ctl_dir.join("oci-run"),
            spawn.state_dir.to_path_buf(),
        );
        Ok(Self {
            host_uid: engine.host_uid,
            host_gid: engine.host_gid,
            rootfs: spawn.rootfs.app_rootfs.clone(),
            state_dir: spawn.state_dir.to_path_buf(),
            mounts: spawn.mounts.to_vec(),
            namespaces: BtrfsEngine::read_pod_namespaces(spawn.ctl_dir)?,
            argv,
            cwd,
            env: BTreeMap::new(),
            label: format!("service '{}'", spawn.spec.id),
            bundle: BtrfsEngine::pod_bundle_dir_of(spawn.ctl_dir, &spawn.spec.id)?,
            runtime,
            container_id: oci::container_id(spawn.world.seq(), &spawn.spec.id),
        })
    }

    /// The throw-away bundle of one `exec_in_world` call (slot `seat` of the world).
    pub(crate) fn for_exec(
        engine: &BtrfsEngine,
        world: &WorldHandle,
        seat: u64,
        argv: &'a [String],
        cwd: &'a Path,
        env: BTreeMap<String, String>,
    ) -> Result<Self, EngineError> {
        let (init_pid, ctl_dir, state_dir, rootfs, mounts) = {
            let mut worlds = engine.lock()?;
            let record = engine.live_record(&mut worlds, world)?;
            (
                record.init_pid,
                record.ctl_dir.clone(),
                record.state_dir.clone(),
                record.rootfs.clone(),
                record.mounts.clone(),
            )
        };
        let runtime = CrunSurface::new(
            engine,
            init_pid,
            BtrfsEngine::crun_host_bin(&rootfs),
            ctl_dir.join("oci-run"),
            state_dir.clone(),
        );
        Ok(Self {
            host_uid: engine.host_uid,
            host_gid: engine.host_gid,
            rootfs: rootfs.app_rootfs,
            state_dir,
            mounts,
            namespaces: BtrfsEngine::read_pod_namespaces(&ctl_dir)?,
            argv,
            cwd,
            env,
            label: format!("exec slot {seat}"),
            bundle: ctl_dir.join("oci").join("_exec").join(seat.to_string()),
            runtime,
            container_id: format!("sfexec{}-{seat}", world.seq()),
        })
    }

    /// Replace the bundle environment (the exec path composes its own overlay first).
    pub(crate) fn with_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }

    /// Materialize the bundle in the world's private tmpfs and return its host-side config path.
    pub(crate) fn write(&mut self) -> Result<PathBuf, EngineError> {
        self.env.insert(
            "SLIPFORK_WORLD_STATE".to_owned(),
            self.state_dir.display().to_string(),
        );
        let bundle_host = self.runtime.host_path(&self.bundle);
        let mut mounts: Vec<ContainerMount> = self
            .mounts
            .iter()
            .map(|mount| ContainerMount {
                source: mount.host_path.clone(),
                destination: mount.guest_path.clone(),
                read_only: mount.mode == MountMode::ReadOnly,
            })
            .collect();
        // The world's CoW state directory is exposed at its own host path, so
        // `SLIPFORK_WORLD_STATE` (and every seed/freeze journey's script) resolves inside the
        // container.
        mounts.push(ContainerMount {
            source: self.state_dir.clone(),
            destination: self.state_dir.clone(),
            read_only: false,
        });
        create_private_bundle_dir(&bundle_host)?;
        let joins = joined_namespaces(self.namespaces.entries().into_iter());
        oci::write_bundle(&ContainerBundle {
            bundle_dir: &bundle_host,
            image_rootfs: &self.rootfs,
            argv: self.argv,
            env: &self.env,
            uid: self.host_uid,
            gid: self.host_gid,
            mounts: &mounts,
            namespace_joins: &joins,
            cwd: self.cwd,
            passwd_guest: None,
            group_guest: None,
        })
        .map_err(|err| io_error(&bundle_host, &err))?;
        make_private(&bundle_host.join(oci::OCI_CONFIG_FILE))
    }

    /// Start the supervised container for a `WorldProcess` service.
    pub(crate) fn run(
        &self,
        log: &ServiceLogs,
        process: &Arc<Mutex<ServiceProcess>>,
    ) -> Result<(), EngineError> {
        self.runtime.run_bundle(
            &self.env,
            &self.label,
            &self.runtime.host_path(&self.bundle),
            &self.bundle,
            &self.container_id,
            log,
            process,
        )
    }

    /// Run one one-shot world exec, collecting its streamed output, then reclaim the bundle.
    pub(crate) fn execute(
        self,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        let mut command = self.runtime.command(&self.env);
        command
            .args([
                "run".to_owned(),
                "-b".to_owned(),
                self.bundle.display().to_string(),
                self.container_id.clone(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = run_streamed(command, on_output);
        let _ = self.runtime.delete(&self.container_id);
        let _ = std::fs::remove_dir_all(self.runtime.host_path(&self.bundle));
        output
    }
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

    /// Materialize and run one legacy world process as a pod-shaped container.
    pub(crate) fn spawn_world_process_pod(
        &self,
        spawn: &ServiceSpawn<'_>,
        argv: &[String],
        cwd: &Path,
    ) -> Result<(), EngineError> {
        let mut bundle =
            WorldProcessBundle::for_service(self, spawn, argv, cwd)?.with_env(spawn.env.clone());
        bundle.write()?;
        bundle.run(spawn.log, spawn.process)
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

    pub(crate) fn exec_bundle_slot(&self) -> u64 {
        self.shared
            .next_exec
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}
