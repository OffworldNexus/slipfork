//! Daemonless `crun` execution for backing-service containers: the pinned static binary, the OCI
//! bundle, and every `run`/`exec`/`kill`/`delete` invocation (`ADR-0009` §§2–3).

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;

use slipfork_world::rootfs::BackingServiceKey;
use slipfork_world::{
    EngineError, ExecEnv, ExecOutput, MountMode, ServiceHandle, ServicePodSpec, ServiceState,
    StateSubdirMount, StreamChunk, WorldEntryCommand, WorldMount,
};

use crate::child_env;
use crate::child_io::{run_streamed, run_streamed_with_input, spawn_log_reader};
use crate::oci::{self, ContainerBundle, ContainerMount};
use crate::world_init::{
    GROUP_GUEST, OCI_BUNDLES_GUEST, OCI_RUN_GUEST, PASSWD_GUEST, WORLD_STATE_GUEST,
    image_guest_rootfs, world_guest_for_host,
};

use super::BtrfsEngine;
use super::io_error;
use super::services::ServiceSpawn;

/// The `crun` invocation coordinates of one running backing-service container.
#[derive(Debug, Clone)]
pub(crate) struct ContainerProcess {
    /// Stable `crun` container id.
    pub(crate) id: String,
    /// The world's `crun` execution surface.
    pub(crate) runtime: CrunSurface,
    /// Host path of the synthesized bundle (removed after the container exits).
    pub(crate) bundle_host: PathBuf,
}

/// One world's `crun` execution surface: the exact `nsenter` → `crun` prefix resolved once at
/// construction (world PID 1, pinned static `crun`, disabled cgroup manager, state root).
///
/// This type is the **sole** `crun` argv owner in the engine, and it is also the single writer of
/// per-service OCI bundles: a backing-service container gets an id-addressed bundle in the world
/// control dir, and a `ServicePod` gets `ctl/<world_id>/oci/<service>/config.json` from its
/// injected [`ServicePodSpec`].
#[derive(Debug, Clone)]
pub struct CrunSurface {
    prefix: Vec<String>,
}

impl CrunSurface {
    pub(crate) fn new(
        engine: &BtrfsEngine,
        init_pid: u32,
        crun_guest: PathBuf,
        crun_root_guest: PathBuf,
    ) -> Self {
        // `crun` is a world binary and each container's own cwd comes from its OCI bundle, so the
        // shared entry prefix only has to land on the app stage root.
        let mut prefix = engine.entry_prefix(init_pid, &[], Path::new("/"));
        prefix.push(crun_guest.display().to_string());
        prefix.extend([
            "--cgroup-manager=disabled".to_owned(),
            "--root".to_owned(),
            crun_root_guest.display().to_string(),
        ]);
        Self { prefix }
    }

    /// The one `nsenter` → `crun` prefix every invocation (`run`, `exec`, `kill`, `delete`) reuses.
    pub(crate) fn prefix(&self) -> Vec<String> {
        self.prefix.clone()
    }

    /// A `crun` invocation that runs inside the world's mount/net/ipc/pid namespaces.
    pub(crate) fn command(&self, env: &BTreeMap<String, String>) -> Command {
        let mut command = Command::new("nsenter");
        command
            .args(&self.prefix)
            .env_clear()
            .envs(env)
            .env("SLIPFORK_WORLD_STATE", WORLD_STATE_GUEST);
        command
    }

    fn control(&self, args: &[&str]) -> Command {
        let mut command = Command::new("nsenter");
        command
            .args(&self.prefix)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    fn run_control(&self, args: &[&str], action: &str, id: &str) -> Result<(), EngineError> {
        let status = self.control(args).status().map_err(|err| {
            EngineError::Execution(format!(
                "cannot run `crun {action}` for container '{id}': {err}"
            ))
        })?;
        if status.success() {
            Ok(())
        } else {
            Err(EngineError::Execution(format!(
                "`crun {action}` for container '{id}' exited with {status}"
            )))
        }
    }

    /// `crun kill` one container; the caller decides whether a refusal is fatal.
    pub(crate) fn signal(&self, id: &str, signal: &str) -> Result<(), EngineError> {
        self.run_control(&["kill", id, signal], "kill", id)
    }

    /// `crun delete --force` after the container's supervisor exited.
    pub(crate) fn delete(&self, id: &str) -> Result<(), EngineError> {
        self.run_control(&["delete", "--force", id], "delete", id)
    }

    /// Materialize one `Forkfile` service pod's OCI bundle from its injected spec.
    ///
    /// The spec is the sole source of every value, and **every path in the emitted document is a
    /// world-visible guest path**: `root.path`, `process.cwd`, and each bind `source` are resolved
    /// through the world's own mount table ([`world_guest_for_host`]), while the bundle itself is
    /// addressed by [`OCI_BUNDLES_GUEST`] (the engine boots `<ctl>/<world_id>/oci` there). A path
    /// the world does not expose fails closed with a remedy instead of leaking a host path that
    /// only happens to resolve because a mount is 1:1.
    ///
    /// `world_mounts` is the live world's [`WorldMount`] table (`WorldSpec.mounts`); `worktree` is
    /// the host checkout whose volumes override. The bundle directory is the spec's own
    /// `ctl/<world_id>/oci/<service>/` (never the world subvolume: [`slipfork_world::PodLocation`]
    /// enforced that when the pod was planned). It is created `0700` and `config.json` is left
    /// `0600`, because the environment may carry resolved secrets. Returns the `config.json` path.
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
                "mount the live worktree and the world subvolume (`volumes/`) in \
                 WorldSpec.mounts",
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
            "mount the rootfs cache read-only in WorldSpec.mounts at a `/.slipfork/images/…` \
             guest path",
        )?;
        let cwd = guest(
            spec.cwd(),
            "working directory",
            "add the worktree to WorldSpec.mounts as MountMode::ReadWrite",
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
}

/// Guest path of one service's bundle: the engine boots `<ctl>/<world_id>/oci` at
/// [`OCI_BUNDLES_GUEST`], so `crun -b` addresses the bundle by service id alone.
pub fn pod_bundle_guest(service: &str) -> PathBuf {
    PathBuf::from(OCI_BUNDLES_GUEST).join(service)
}

/// Create (or reuse) a bundle directory only the invoking developer can traverse or list.
fn create_private_bundle_dir(dir: &Path) -> Result<(), EngineError> {
    fs::create_dir_all(dir).map_err(|err| io_error(dir, &err))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|err| io_error(dir, &err))
}

impl BtrfsEngine {
    /// Start one daemonless `crun` backing-service container (`ServiceRuntime::OciContainer`).
    pub(crate) fn spawn_oci_container(
        &self,
        spawn: &ServiceSpawn<'_>,
        image: &BackingServiceKey,
        argv: &[String],
        state_mounts: &[StateSubdirMount],
        readonly_mounts: &[WorldMount],
    ) -> Result<(), EngineError> {
        let oci = spawn.rootfs.service_images.get(image).ok_or_else(|| {
            EngineError::InvalidWorldSpec {
                reason: format!(
                    "service '{}' requests backing-service image {image}, but WorldSpec.rootfs \
                     carries no materialized OCI rootfs for it; materialize it with \
                     BaseImageBuilder before cloning the world",
                    spawn.spec.id
                ),
            }
        })?;
        let image_guest = image_guest_rootfs(&oci.digest)?;
        if !oci.rootfs.is_dir() {
            return Err(EngineError::InvalidWorldSpec {
                reason: format!(
                    "backing-service image {image} rootfs '{}' does not exist",
                    oci.rootfs.display()
                ),
            });
        }
        let container_id = oci::container_id(spawn.world.seq(), &spawn.spec.id);
        // Bundles and `crun` state live in the control directory, never in the world subvolume, so
        // a seal never captures OCI runtime state (`ADR-0010` §2).
        let bundle_host = spawn.ctl_dir.join("oci").join(&container_id);
        fs::create_dir_all(&bundle_host).map_err(|err| io_error(&bundle_host, &err))?;
        let bundle_guest = PathBuf::from(OCI_BUNDLES_GUEST).join(&container_id);

        let mut mounts = Vec::with_capacity(state_mounts.len() + readonly_mounts.len());
        for state in state_mounts {
            oci::validate_state_subdir(&state.host_subdir)
                .map_err(|reason| EngineError::InvalidWorldSpec { reason })?;
            oci::validate_container_path(&state.container_path)
                .map_err(|reason| EngineError::InvalidWorldSpec { reason })?;
            let host = spawn.state_dir.join(&state.host_subdir);
            fs::create_dir_all(&host).map_err(|err| io_error(&host, &err))?;
            oci::ensure_container_destination(oci, image, &state.container_path)?;
            mounts.push(ContainerMount {
                source: PathBuf::from(WORLD_STATE_GUEST).join(&state.host_subdir),
                destination: state.container_path.clone(),
                read_only: false,
            });
        }
        for readonly in readonly_mounts {
            if readonly.mode != MountMode::ReadOnly {
                return Err(EngineError::InvalidWorldSpec {
                    reason: format!(
                        "service '{}' readonly mount '{}' must carry MountMode::ReadOnly",
                        spawn.spec.id,
                        readonly.host_path.display()
                    ),
                });
            }
            oci::validate_container_path(&readonly.guest_path)
                .map_err(|reason| EngineError::InvalidWorldSpec { reason })?;
            oci::ensure_container_destination(oci, image, &readonly.guest_path)?;
            let source =
                world_guest_for_host(spawn.mounts, &readonly.host_path).ok_or_else(|| {
                    EngineError::InvalidWorldSpec {
                        reason: format!(
                            "readonly mount '{}' is not visible inside the world; add it to \
                             WorldSpec.mounts as MountMode::ReadOnly",
                            readonly.host_path.display()
                        ),
                    }
                })?;
            for state in state_mounts {
                if oci::paths_overlap(&readonly.guest_path, &state.container_path) {
                    return Err(EngineError::InvalidWorldSpec {
                        reason: format!(
                            "readonly mount {} collides with state mount {} in service '{}'",
                            readonly.guest_path.display(),
                            state.container_path.display(),
                            spawn.spec.id
                        ),
                    });
                }
            }
            mounts.push(ContainerMount {
                source,
                destination: readonly.guest_path.clone(),
                read_only: true,
            });
        }

        oci::write_bundle(&ContainerBundle {
            bundle_dir: &bundle_host,
            image_rootfs_guest: &image_guest,
            argv,
            env: spawn.env,
            uid: self.host_uid,
            gid: self.host_gid,
            mounts: &mounts,
            namespace_joins: &[],
            cwd: Path::new("/"),
            passwd_guest: Some(Path::new(PASSWD_GUEST)),
            group_guest: Some(Path::new(GROUP_GUEST)),
        })
        .map_err(|err| io_error(&bundle_host, &err))?;

        let runtime = CrunSurface::new(
            self,
            spawn.init_pid,
            spawn.rootfs.crun_bin.clone(),
            PathBuf::from(OCI_RUN_GUEST),
        );
        let mut command = runtime.command(spawn.env);
        command
            .args([
                "run".to_owned(),
                "-b".to_owned(),
                bundle_guest.display().to_string(),
                container_id.clone(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|err| {
            EngineError::Execution(format!(
                "cannot start backing-service container '{}' ({image}): {err}",
                spawn.spec.id
            ))
        })?;
        if let Some(stdout) = child.stdout.take() {
            spawn_log_reader(
                stdout,
                slipfork_world::StreamSource::Stdout,
                spawn.log.clone(),
            );
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_reader(
                stderr,
                slipfork_world::StreamSource::Stderr,
                spawn.log.clone(),
            );
        }
        if let Ok(mut process) = spawn.process.lock() {
            process.supervisor = Some(child);
            process.container = Some(ContainerProcess {
                id: container_id,
                runtime,
                bundle_host,
            });
        }
        Ok(())
    }

    /// Resolve a running container plus the environment its `exec` composes against: the
    /// container's own declared `spec.env`, never the world component env (see the invariant in
    /// [`BtrfsEngine::start_service`]).
    fn container_for(
        &self,
        service: &ServiceHandle,
    ) -> Result<(ContainerProcess, ExecEnv), EngineError> {
        let mut worlds = self.lock()?;
        let record = self.live_record(&mut worlds, service.world())?;
        let running =
            record
                .services
                .get(service.id())
                .ok_or_else(|| EngineError::UnknownService {
                    world: format!("{:?}", service.world()),
                    id: service.id().to_owned(),
                })?;
        if ServiceState::from_bits(running.state.load(Ordering::Relaxed)) != ServiceState::Running {
            return Err(EngineError::ServiceNotRunning {
                world: format!("{:?}", service.world()),
                id: service.id().to_owned(),
            });
        }
        let process = running.process.lock().map_err(|_| {
            EngineError::Execution(format!(
                "service '{}' process state is poisoned",
                service.id()
            ))
        })?;
        let container =
            process
                .container
                .clone()
                .ok_or_else(|| EngineError::ServiceNotRunning {
                    world: format!("{:?}", service.world()),
                    id: service.id().to_owned(),
                })?;
        Ok((container, running.spec.env.clone()))
    }

    pub(crate) fn exec_in_container(
        &self,
        service: &ServiceHandle,
        argv: &[String],
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        if argv.is_empty() {
            return Err(EngineError::Execution(
                "service exec argv must not be empty".to_owned(),
            ));
        }
        let (container, base_env) = self.container_for(service)?;
        let env = child_env::materialize(&base_env, overlay_env, |var| self.inherited_env.get(var));
        let mut command = container.runtime.command(&env);
        command.arg("exec");
        for (name, value) in &env {
            command.args(["--env", &format!("{name}={value}")]);
        }
        command
            .arg(&container.id)
            .args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        run_streamed(command, on_output)
    }

    pub(crate) fn exec_in_container_with_input(
        &self,
        service: &ServiceHandle,
        argv: &[String],
        input: Box<dyn std::io::Read + Send>,
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        if argv.is_empty() {
            return Err(EngineError::Execution(
                "service exec argv must not be empty".to_owned(),
            ));
        }
        let (container, base_env) = self.container_for(service)?;
        let env = child_env::materialize(&base_env, overlay_env, |var| self.inherited_env.get(var));
        let mut command = container.runtime.command(&env);
        command.arg("exec");
        for (name, value) in &env {
            command.args(["--env", &format!("{name}={value}")]);
        }
        command
            .arg(&container.id)
            .args(argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        run_streamed_with_input(command, input, on_output)
    }

    pub(crate) fn container_entry_command(
        &self,
        service: &ServiceHandle,
    ) -> Result<WorldEntryCommand, EngineError> {
        let (container, _env) = self.container_for(service)?;
        let mut args_prefix = container.runtime.prefix();
        args_prefix.extend(["exec".to_owned(), container.id, "--".to_owned()]);
        Ok(WorldEntryCommand {
            program: PathBuf::from("nsenter"),
            args_prefix,
            cwd: PathBuf::from("/"),
        })
    }
}
