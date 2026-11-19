//! Daemonless `crun` service containers: OCI bundle materialization plus the engine-side `spawn`
//! and `exec` entry points (`ADR-0009` §§2–3). The `crun` argv/state surface lives in
//! [`super::crun`].

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;

use slipfork_world::rootfs::BackingServiceKey;
use slipfork_world::{
    EngineError, ExecEnv, ExecOutput, MountMode, ServiceHandle, ServiceState, StateSubdirMount,
    StreamChunk, WorldEntryCommand, WorldMount,
};

use crate::child_env;
use crate::child_io::{run_streamed, run_streamed_with_input};
use crate::oci::{self, ContainerBundle, ContainerMount};

use super::BtrfsEngine;
use super::crun::CrunSurface;
use super::io_error;
use super::services::ServiceSpawn;

/// How `crun exec` composes the exec process environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecEnvMode {
    /// Backing services: forward the retained declared environment as `--env` flags.
    Declared,
    /// Service pods: `crun` re-applies the bundle's `process.env`, so only caller overlays are
    /// forwarded and no resolved secret ever reaches a command line.
    ContainerConfig,
}
#[derive(Debug, Clone)]
pub(crate) struct ContainerProcess {
    /// Stable `crun` container id.
    pub(crate) id: String,
    /// The world's `crun` execution surface.
    pub(crate) runtime: CrunSurface,
    /// Host path of the synthesized bundle (removed after the container exits).
    pub(crate) bundle_host: PathBuf,
}
/// Forward exactly the environment the exec process needs, never a pod's resolved secrets.
fn append_exec_env(
    command: &mut Command,
    mode: ExecEnvMode,
    env: &BTreeMap<String, String>,
    overlay: &ExecEnv,
) {
    match mode {
        ExecEnvMode::Declared => {
            for (name, value) in env {
                command.args(["--env", &format!("{name}={value}")]);
            }
        }
        ExecEnvMode::ContainerConfig => {
            for (name, value) in overlay.iter() {
                command.args(["--env", &format!("{name}={value}")]);
            }
        }
    }
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
        let image_rootfs = oci.rootfs.clone();
        if !oci.rootfs.is_dir() {
            return Err(EngineError::InvalidWorldSpec {
                reason: format!(
                    "backing-service image {image} rootfs '{}' does not exist",
                    oci.rootfs.display()
                ),
            });
        }
        let container_id = oci::container_id(spawn.world.seq(), &spawn.spec.id);
        // Bundles and `crun` state live in the world's private tmpfs, never in the world
        // subvolume and never on the host state directory.
        let runtime = CrunSurface::new(
            self,
            spawn.init_pid,
            Self::crun_host_bin(spawn.rootfs),
            spawn.ctl_dir.join("oci-run"),
            spawn.state_dir.to_path_buf(),
        );
        let bundle_guest = spawn.ctl_dir.join("oci").join(&container_id);
        let bundle_host = runtime.host_path(&bundle_guest);
        fs::create_dir_all(&bundle_host).map_err(|err| io_error(&bundle_host, &err))?;

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
                source: host.clone(),
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
            // The world mount namespace is a 1:1 copy of the host tree, so a read-only mount is
            // its host path; the target must exist for the container to bind it.
            if !readonly.host_path.exists() {
                return Err(EngineError::InvalidWorldSpec {
                    reason: format!(
                        "readonly mount '{}' does not exist; stage it before spawning the \
                         container",
                        readonly.host_path.display()
                    ),
                });
            }
            let source = readonly.host_path.clone();
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
            image_rootfs: &image_rootfs,
            argv,
            env: spawn.env,
            uid: self.host_uid,
            gid: self.host_gid,
            mounts: &mounts,
            namespace_joins: &[],
            cwd: Path::new("/"),
            passwd_guest: Some(&spawn.ctl_dir.join("etc").join("passwd")),
            group_guest: Some(&spawn.ctl_dir.join("etc").join("group")),
        })
        .map_err(|err| io_error(&bundle_host, &err))?;

        runtime.run_bundle(
            spawn.env,
            &format!("backing-service container '{}' ({image})", spawn.spec.id),
            &bundle_host,
            &bundle_guest,
            &container_id,
            spawn.log,
            spawn.process,
        )
    }

    /// Resolve a running container plus the environment its `exec` composes against: the
    /// container's own declared `spec.env`, never the world component env (see the invariant in
    /// [`BtrfsEngine::start_service`]).
    fn container_for(
        &self,
        service: &ServiceHandle,
    ) -> Result<(ContainerProcess, ExecEnv, ExecEnvMode), EngineError> {
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
        Ok((
            container,
            running.spec.env.clone(),
            match &running.spec.runtime {
                slipfork_world::ServiceRuntime::ServicePod(_) => ExecEnvMode::ContainerConfig,
                _ => ExecEnvMode::Declared,
            },
        ))
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
        let (container, base_env, mode) = self.container_for(service)?;
        let env = child_env::materialize(&base_env, overlay_env, |var| self.inherited_env.get(var));
        let mut command = container.runtime.command(&env);
        command.arg("exec");
        append_exec_env(&mut command, mode, &env, overlay_env);
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
        let (container, base_env, mode) = self.container_for(service)?;
        let env = child_env::materialize(&base_env, overlay_env, |var| self.inherited_env.get(var));
        let mut command = container.runtime.command(&env);
        command.arg("exec");
        append_exec_env(&mut command, mode, &env, overlay_env);
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
        let (container, _env, _mode) = self.container_for(service)?;
        let mut args_prefix = container.runtime.prefix();
        args_prefix.extend(["exec".to_owned(), container.id, "--".to_owned()]);
        Ok(WorldEntryCommand {
            program: PathBuf::from("nsenter"),
            args_prefix,
            cwd: PathBuf::from("/"),
            consumes_target_argv: false,
        })
    }
}
