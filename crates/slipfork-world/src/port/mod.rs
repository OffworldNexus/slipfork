//! The [`WorldEngine`] port and the plain values every engine adapter, `slipforkd`, and the edge
//! proxy exchange.
//!
//! The port is dependency-inverted at the composition root: `slipfork-engine` owns the single
//! production implementation (`BtrfsEngine`), while this module owns the contract so no consumer
//! ever names a concrete engine. Nothing here performs I/O, spawns a process, reads the ambient
//! environment, or holds a global.

#![forbid(unsafe_code)]

mod values;

pub use values::{
    BootLineage, EngineError, ExecOutput, LocalIngressBridge, MountMode, ServiceHandle,
    ServiceRuntime, ServiceSpec, SnapshotId, StateSubdirMount, WorldHandle, WorldMount, WorldSpec,
};

use crate::{
    EntryKind, ExecEnv, PodNamespaces, ServiceLogs, StreamChunk, WorldEntryCommand, WorldLineage,
    WorldServiceInfo,
};

/// Core Dependency Injection port for CoW world creation, cloning, execution, and teardown.
pub trait WorldEngine: Send + Sync {
    /// Short engine identifier used in diagnostics and world-handle ownership checks.
    fn name(&self) -> &'static str;

    /// `Self: Sized` excludes `dyn WorldEngine` so dynamic-port consumers obtain handles only from
    /// [`WorldEngine::clone_world`], while out-of-crate adapters and test doubles can construct
    /// their return token. Minting is an in-process lookup-token constructor, not an unforgeable
    /// capability: minting a live seq exercises full authority on any engine with a matching
    /// `name()`, and grants nothing across distinct engine tags or over IPC.
    fn mint_world_handle(&self, seq: u64) -> WorldHandle
    where
        Self: Sized,
    {
        WorldHandle::mint(self.name(), seq)
    }

    /// Mint the handle an out-of-crate adapter returns from [`WorldEngine::spawn_service`].
    ///
    /// `state` is the adapter's **own** supervision cell: the adapter stores
    /// [`crate::ServiceState::Exited`] / [`crate::ServiceState::Stopped`] when it reaps the service, and
    /// [`ServiceHandle::state`] reads that cell back. `Self: Sized` excludes `dyn WorldEngine`,
    /// mirroring [`WorldEngine::mint_world_handle`]: minting is an in-process lookup token, not an
    /// unforgeable capability, and grants nothing across engines or over IPC.
    fn mint_service_handle(
        &self,
        world: &WorldHandle,
        id: &str,
        state: std::sync::Arc<std::sync::atomic::AtomicU8>,
    ) -> ServiceHandle
    where
        Self: Sized,
    {
        ServiceHandle::mint(world.clone(), id, state)
    }

    /// Capture an immutable read-only snapshot of `source_dir` under `id`.
    fn create_snapshot(
        &self,
        source_dir: &std::path::Path,
        id: &SnapshotId,
    ) -> Result<(), EngineError>;

    /// Whether the sealed snapshot `id` already exists on this engine's own substrate.
    ///
    /// A read-only lookup: it never creates, seals, or clones anything. The daemon uses it to skip
    /// a seeding pipeline whose only possible outcome is "the base already exists".
    fn snapshot_exists(&self, id: &SnapshotId) -> Result<bool, EngineError>;

    /// Seal the live world `world` into the immutable read-only snapshot `id`.
    ///
    /// Every live world-owned service is stopped cleanly **before** the world's `state_dir` is
    /// sealed, so the snapshot is crash-consistent (`H-011` #2). Freezing an already-frozen world
    /// fails closed with [`EngineError::WorldNotFreezable`]; a destroyed world is gone and fails
    /// closed with [`EngineError::WorldGone`]. Clones of the sealed snapshot are independent:
    /// writes in one clone are never visible to a sibling clone.
    fn freeze_world(&self, world: &WorldHandle, id: &SnapshotId) -> Result<(), EngineError>;

    /// Instantaneously clone `spec.snapshot` into a fresh isolated world.
    fn clone_world(&self, spec: &WorldSpec) -> Result<WorldHandle, EngineError>;

    /// Re-run an existing world over `spec` (`slipfork restart`, and `up`'s rescue path).
    ///
    /// The world keeps its handle and therefore its identity in the daemon's index and routing
    /// table. The engine stops every world-owned service, reaps the world PID 1, reconciles the
    /// world's one subvolume with `spec.snapshot` — the same subvolume (and therefore every
    /// backing-service state directory) when the snapshot is unchanged, a fresh clone of the new
    /// base otherwise — re-materializes the read-only rootfs reference, boots a new PID 1, and
    /// records `spec`'s own rootfs/base/seed facts as this world's lineage. Services are not
    /// restarted here: the caller's pipeline declares them again after the reload.
    ///
    /// Adapters that cannot re-root a live world report [`EngineError::Unsupported`] rather than
    /// pretending the new spec was applied.
    fn reload_world(&self, _world: &WorldHandle, _spec: &WorldSpec) -> Result<(), EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "reload_world",
        })
    }

    /// The shared-init namespace joins every [`ServiceRuntime::ServicePod`] of `world` must carry.
    ///
    /// The engine returns the injected `user`/`network`/`ipc` paths its own `slipfork-world-init`
    /// published for the world, so a caller planning a pod passes them straight into
    /// [`crate::pod::ServicePodSpec`] and this crate never probes `/proc` itself. A world whose
    /// shared init has not published its namespace set reports [`EngineError::Unsupported`]:
    /// there is no fabricated path and no degraded fallback.
    fn world_pod_namespaces(&self, world: &WorldHandle) -> Result<PodNamespaces, EngineError>;

    /// The world's one writable CoW subvolume (the root of every per-service volume plan).
    ///
    /// Only the engine knows the `<world_id>` its substrate allocated, so a caller planning a
    /// [`crate::pod::VolumePlan`] asks here instead of deriving an engine layout path.
    fn world_subvolume(&self, world: &WorldHandle) -> Result<std::path::PathBuf, EngineError>;

    /// The engine-owned OCI bundle directory of one service pod under the world's control dir.
    ///
    /// The bundle's `config.json` carries the pod's resolved environment, so the engine keeps it
    /// outside the world subvolume; a caller planning a pod passes this exact path into
    /// [`crate::pod::PodLocation`].
    fn pod_bundle_dir(
        &self,
        world: &WorldHandle,
        service: &str,
    ) -> Result<std::path::PathBuf, EngineError>;

    /// Start `spec` as a supervised world-owned service.
    ///
    /// The engine captures its stdout/stderr into a size-capped retained [`ServiceLogs`] readable
    /// through [`WorldEngine::service_logs`], and — when `spec.readiness_port` is set — only
    /// returns once that port accepts connections inside the world. A service that exits is never
    /// silently restarted.
    fn spawn_service(
        &self,
        world: &WorldHandle,
        spec: &ServiceSpec,
    ) -> Result<ServiceHandle, EngineError>;

    /// Stop `service` with a bounded grace period.
    ///
    /// Idempotent: stopping an already-stopped or self-exited service succeeds, and the captured
    /// logs stay readable afterwards.
    fn stop_service(&self, service: &ServiceHandle) -> Result<(), EngineError>;

    /// Stop every service this engine supervises in `world`.
    ///
    /// The engine's own supervision record is the single owner of "what this world runs", so a
    /// caller that wants a world's services stopped (a `restart`, a rescue) asks here instead of
    /// replaying the handles it happens to hold: a partial spawn failure can never leave a running
    /// service behind. Services are stopped in `ADR-0009` §3 order — app components first, then
    /// backing-service containers — with the engine's bounded `SIGTERM` → `SIGKILL` grace.
    ///
    /// Idempotent: already-stopped and self-exited services stay stopped and keep their retained
    /// logs, and rows remain readable through [`WorldEngine::world_processes`]. A destroyed,
    /// foreign, or teardown-in-progress `world` fails closed with [`EngineError::WorldGone`] —
    /// never a silent no-op.
    fn stop_world_services(&self, world: &WorldHandle) -> Result<(), EngineError>;

    /// Retained (size-capped) output of the service `id` inside `world`.
    fn service_logs(&self, world: &WorldHandle, id: &str) -> Result<ServiceLogs, EngineError>;

    /// Every service this engine supervises in `world`, ordered by service id.
    ///
    /// Rows come from the engine's own supervision records (`ServiceProcess` /
    /// `ContainerProcess`), never from a caller-side list: a service that exited or was stopped
    /// **keeps its row** with [`crate::ServiceState::Exited`] / [`crate::ServiceState::Stopped`] and `pid: None`,
    /// and two worlds never share a row. `uptime_secs` is stamped from the engine's own spawn
    /// instant (zero `Instant` on the wire); a service whose serving port the engine does not know
    /// reports `port: None` rather than `0`.
    ///
    /// A destroyed, teardown-in-progress, or foreign `world` fails closed with
    /// [`EngineError::WorldGone`] — never an empty row set.
    fn world_processes(&self, world: &WorldHandle) -> Result<Vec<WorldServiceInfo>, EngineError>;

    /// This world's own [`WorldLineage`], recorded on its handle when it booted.
    ///
    /// Rootfs and per-service image facts are the materialized [`crate::rootfs::WorldRootfs`] the clone was
    /// handed (cache outcome and `millis` measured on the same injected clock); base/seed facts
    /// are the [`BootLineage`] the seeder measured. A fact the engine did not record is `None` —
    /// there is no placeholder lineage and no daemon-wide "latest build" cell, so one world can
    /// never report another world's facts.
    ///
    /// A destroyed, teardown-in-progress, or foreign `world` fails closed with
    /// [`EngineError::WorldGone`] — never an all-`None` lineage.
    fn world_lineage(&self, world: &WorldHandle) -> Result<WorldLineage, EngineError>;

    /// Whether this engine still holds a **live** world behind `world`.
    ///
    /// `true` only when the engine's own record is `Live` **and** the world PID 1 it booted is still
    /// that process (verified against `/proc`, never by a bare pid number). A record whose status is
    /// still `Live` but whose PID 1 already exited — the window between a reload's teardown and its
    /// `Failed` stamp — is `false`, as is a `Freezing`/`Frozen`/`Failed` world. Callers use this to
    /// decide whether a failure tore the world down; they never interpret an error variant or
    /// message to mean "still running".
    ///
    /// A destroyed, teardown-in-progress, or foreign `world` fails closed with
    /// [`EngineError::WorldGone`].
    fn world_is_live(&self, world: &WorldHandle) -> Result<bool, EngineError>;

    /// Replace a supervised service's overlay environment and restart it with its retained
    /// `ServiceSpec` (`M3.29` live `env_updated`).
    ///
    /// The caller owns the new environment (already decrypted and locally substituted in memory);
    /// the engine owns the argv, cwd, mounts, and readiness contract it captured at spawn time, so
    /// `DaemonService` never re-authors a `ServiceSpec`. Only this one service is stopped and
    /// respawned; sibling services are untouched. A read-only test adapter that cannot restart
    /// services reports [`EngineError::Unsupported`] rather than pretending it applied the update.
    fn update_service_env(
        &self,
        _service: &ServiceHandle,
        _env: &ExecEnv,
    ) -> Result<(), EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "update_service_env",
        })
    }

    /// Execute `argv` **inside a running service's own rootfs** — never the app rootfs — so
    /// `pg_restore`, `CHECKPOINT;`, and `psql` match the backing-service image (`ADR-0008` §2).
    ///
    /// Fails closed with [`EngineError::ServiceNotRunning`] unless the service is
    /// [`crate::ServiceState::Running`].
    fn exec_in_service(
        &self,
        service: &ServiceHandle,
        argv: &[String],
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError>;

    /// Execute `argv` inside a running service with `input` streamed to its stdin.
    ///
    /// This is the one dump-restore seam: the decrypted `OsppSession::dump` stream is piped into
    /// `zstd -d | pg_restore` inside the container without materializing plaintext on disk. A
    /// read error from `input` is never swallowed (a truncated sealed dump must fail the restore
    /// even when the child happened to exit `0`). Adapters that cannot stream stdin report
    /// [`EngineError::Unsupported`].
    fn exec_in_service_with_input(
        &self,
        _service: &ServiceHandle,
        _argv: &[String],
        _input: Box<dyn std::io::Read + Send>,
        _overlay_env: &ExecEnv,
        _on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "exec_in_service_with_input",
        })
    }

    /// `crun exec` entry command for interactive service clients (`slipfork db` -> `psql`,
    /// `slipfork redis` -> `redis-cli`, `ADR-0009` §3).
    ///
    /// Fails closed with [`EngineError::ServiceNotRunning`] unless the service is
    /// [`crate::ServiceState::Running`].
    fn service_entry_command(
        &self,
        service: &ServiceHandle,
    ) -> Result<WorldEntryCommand, EngineError>;

    /// Execute `argv` inside `world` under the effective environment
    /// (`world.base_env ⊕ overlay_env`), streaming interleaved output chunks to `on_output`.
    ///
    /// `cwd` is the caller's **host** directory: the engine maps it through the world's bind
    /// mounts and starts `argv` on that in-world path (the rootfs `/` when the directory is not
    /// mounted), so the child's first `getcwd(2)` succeeds.
    fn exec_in_world(
        &self,
        world: &WorldHandle,
        argv: &[String],
        cwd: &std::path::Path,
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError>;

    /// Argv prefix letting the thin CLI enter `world` directly with inherited stdio (`H-010` #1).
    ///
    /// `cwd` is the caller's **host** directory; the returned `args_prefix` starts the target on
    /// that directory's in-world mapping (or the world rootfs `/`), never on the host pathname.
    fn entry_command(
        &self,
        world: &WorldHandle,
        cwd: &std::path::Path,
    ) -> Result<WorldEntryCommand, EngineError>;

    /// Like [`Self::entry_command`], but for a caller-declared [`EntryKind`].
    ///
    /// `Exec` keeps the plain bridge prefix (the CLI appends the target argv). `Interactive` lets
    /// an engine bake a PTY relay into the prefix: the relay runs `script -qec 'exec <program>'`,
    /// which `/bin/sh -c` re-parses, so `argv` must be exactly one **literal program path** and
    /// the engine owns quoting and fails closed otherwise. Every engine must implement this.
    fn entry_command_for(
        &self,
        world: &WorldHandle,
        cwd: &std::path::Path,
        kind: EntryKind,
        argv: &[String],
    ) -> Result<WorldEntryCommand, EngineError>;

    /// Open a TCP stream to `127.0.0.1:<port>` **inside** `world`'s network namespace.
    ///
    /// Daemon-internal only (`slipforkd` HTTP/WS ingress and daemon-owned `forward`): the stream
    /// never crosses IPC. Dialing a destroyed world fails closed with [`EngineError::WorldGone`].
    fn dial_world_tcp(
        &self,
        world: &WorldHandle,
        port: u16,
    ) -> Result<std::net::TcpStream, EngineError>;

    /// Destroy `world`, reap its world init, and release its CoW state immediately.
    fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError>;
}
