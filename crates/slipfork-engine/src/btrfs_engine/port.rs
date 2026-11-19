//! The `WorldEngine` port adapter: the one trait impl for [`BtrfsEngine`], delegating each verb to
//! the cohesive module that owns it (`worlds`, `snapshots`, `boot`, `services`, `container`,
//! `teardown`).

#![forbid(unsafe_code)]

use std::net::TcpStream;
use std::path::{Path, PathBuf};

use slipfork_world::{
    EngineError, EntryKind, ExecEnv, ExecOutput, PodNamespaces, ServiceHandle, ServiceLogs,
    ServiceSpec, SnapshotId, StreamChunk, WorldEngine, WorldEntryCommand, WorldHandle,
    WorldLineage, WorldServiceInfo, WorldSpec,
};

use super::BtrfsEngine;
use super::teardown::{SERVICE_STOP_GRACE, WORLD_STOP_TERM_GRACE};

impl WorldEngine for BtrfsEngine {
    fn name(&self) -> &'static str {
        "btrfs"
    }

    /// The shared `slipfork-world-init` namespace joins a service pod carries.
    ///
    /// The world init publishes its own `user`/`network`/`ipc` join paths in `namespaces.json`
    /// (`/proc/1/ns/*` — it is world PID 1), which is exactly what a `crun` bundle can name from
    /// inside the world. A world without that publication fails closed with a `fix:` line.
    fn world_pod_namespaces(&self, world: &WorldHandle) -> Result<PodNamespaces, EngineError> {
        let ctl_dir = {
            let mut worlds = self.lock()?;
            let record = self.live_record(&mut worlds, world)?;
            record.ctl_dir.clone()
        };
        Self::read_pod_namespaces(&ctl_dir)
    }

    fn world_subvolume(&self, world: &WorldHandle) -> Result<PathBuf, EngineError> {
        let mut worlds = self.lock()?;
        Ok(self.live_record(&mut worlds, world)?.world_dir.clone())
    }

    fn pod_bundle_dir(&self, world: &WorldHandle, service: &str) -> Result<PathBuf, EngineError> {
        BtrfsEngine::pod_bundle_dir(self, world, service)
    }

    fn snapshot_exists(&self, id: &SnapshotId) -> Result<bool, EngineError> {
        self.snapshot_exists_on_disk(id)
    }

    fn create_snapshot(&self, source_dir: &Path, id: &SnapshotId) -> Result<(), EngineError> {
        self.import_snapshot(source_dir, id)
    }

    fn freeze_world(&self, world: &WorldHandle, id: &SnapshotId) -> Result<(), EngineError> {
        self.seal_world(world, id)
    }

    fn clone_world(&self, spec: &WorldSpec) -> Result<WorldHandle, EngineError> {
        self.create_world(spec)
    }

    fn reload_world(&self, world: &WorldHandle, spec: &WorldSpec) -> Result<(), EngineError> {
        self.rebuild_world(world, spec)
    }

    fn spawn_service(
        &self,
        world: &WorldHandle,
        spec: &ServiceSpec,
    ) -> Result<ServiceHandle, EngineError> {
        self.start_service(world, spec)
    }

    fn update_service_env(
        &self,
        service: &ServiceHandle,
        env: &ExecEnv,
    ) -> Result<(), EngineError> {
        self.restart_with_env(service, env)
    }

    fn stop_service(&self, service: &ServiceHandle) -> Result<(), EngineError> {
        self.stop_service_handle(service)
    }

    fn stop_world_services(&self, world: &WorldHandle) -> Result<(), EngineError> {
        self.stop_all_services(world)
    }

    fn service_logs(&self, world: &WorldHandle, id: &str) -> Result<ServiceLogs, EngineError> {
        self.retained_logs(world, id)
    }

    fn world_processes(&self, world: &WorldHandle) -> Result<Vec<WorldServiceInfo>, EngineError> {
        self.service_rows(world)
    }

    fn world_lineage(&self, world: &WorldHandle) -> Result<WorldLineage, EngineError> {
        self.lineage_of(world)
    }

    fn world_is_live(&self, world: &WorldHandle) -> Result<bool, EngineError> {
        self.is_world_live(world)
    }

    fn exec_in_service(
        &self,
        service: &ServiceHandle,
        argv: &[String],
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        self.exec_in_container(service, argv, overlay_env, on_output)
    }

    fn exec_in_service_with_input(
        &self,
        service: &ServiceHandle,
        argv: &[String],
        input: Box<dyn std::io::Read + Send>,
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        self.exec_in_container_with_input(service, argv, input, overlay_env, on_output)
    }

    fn service_entry_command(
        &self,
        service: &ServiceHandle,
    ) -> Result<WorldEntryCommand, EngineError> {
        self.container_entry_command(service)
    }

    fn exec_in_world(
        &self,
        world: &WorldHandle,
        argv: &[String],
        cwd: &Path,
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        self.run_in_world(world, argv, cwd, overlay_env, on_output)
    }

    fn entry_command(
        &self,
        world: &WorldHandle,
        cwd: &Path,
    ) -> Result<WorldEntryCommand, EngineError> {
        self.world_entry_command(world, cwd)
    }

    fn entry_command_for(
        &self,
        world: &WorldHandle,
        cwd: &Path,
        kind: EntryKind,
        argv: &[String],
    ) -> Result<WorldEntryCommand, EngineError> {
        match (kind, argv) {
            (EntryKind::Interactive, [program]) => {
                self.interactive_entry_command(world, cwd, program)
            }
            (EntryKind::Interactive, _) => Err(super::interactive_arity_error(argv.len())),
            (EntryKind::Exec, _) => self.world_entry_command(world, cwd),
        }
    }

    fn dial_world_tcp(&self, world: &WorldHandle, port: u16) -> Result<TcpStream, EngineError> {
        self.open_world_tcp(world, port)
    }

    fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError> {
        self.teardown_world(&world, SERVICE_STOP_GRACE, WORLD_STOP_TERM_GRACE)
    }
}
