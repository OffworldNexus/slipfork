//! `BtrfsEngine`: one unified Btrfs subvolume per world (`ADR-0010` §§2–3, `ADR-0002`, `H-011`).
//!
//! Every live world is exactly one writable Btrfs subvolume `<state_dir>/worlds/<world_id>`,
//! cloned from the read-only snapshot `<state_dir>/snapshots/<spec.snapshot>` with one
//! `BTRFS_IOC_SNAP_CREATE_V2` call, and sealed back into `<state_dir>/snapshots/<id>` with
//! `BTRFS_SUBVOL_RDONLY` through the concrete [`KernelBtrfsDriver`]. Inside that single subvolume
//! live the read-only `L0` rootfs reference (`<subvol>/rootfs`) and the backing-service state
//! (`<subvol>/state/{pgdata,redis,s3}`) as **plain directories** — never nested subvolumes, which
//! `BTRFS_IOC_SNAP_CREATE_V2` would not recurse into — so freeze, fork, and teardown are metadata
//! operations.
//!
//! Ephemeral runtime state — `netns.sock`, `world-init.json`, `init.ready`, `oci/`, `oci-run/`,
//! `mountpoints/`, `merged/`, and the synthesized `/etc/{passwd,group}` — lives in the per-world
//! control directory `<state_dir>/ctl/<world_id>` derived by this engine, **outside** the
//! subvolume, so a seal never captures a socket or `crun` runtime state (`ADR-0010` §2).
//!
//! Supervision is the persistent per-world PID 1 (`src/bin/slipfork-world-init.rs`) spawned inside
//! `unshare --user --map-user=$UID --map-group=$GID --keep-caps --mount --net --ipc --pid --fork
//! --kill-child=SIGKILL --mount-proc` with a piped `stdin` lifeline. `--ipc` gives every world a
//! private System V IPC namespace, so parallel `postgres` containers never collide on `shmget` key
//! `90000` or port-derived keys (`ADR-0010` §6). No `unsafe` code lives here:
//! `statfs`/`BTRFS_IOC_*` are confined to [`crate::btrfs`] and `SCM_RIGHTS`/`kill(2)` to
//! [`crate::linux_cow`].
//!
//! Module layout: the private `worlds` module materializes a world subvolume and `snapshots` seals
//! one, `boot` runs the world PID 1 and its dial socket, `services`/`container` spawn and query
//! supervised service trees, and `teardown` owns the one stop-and-reap order.

#![forbid(unsafe_code)]

mod boot;
mod container;
mod pod;
mod port;
mod services;
mod snapshots;
mod teardown;
mod worlds;

pub use container::CrunSurface;
pub use pod::{pod_bundle_guest, write_pod_bundle};

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use slipfork_world::rootfs::WorldRootfs;
use slipfork_world::{
    EngineError, ExecEnv, ExecOutput, StreamChunk, WorldEngine, WorldEntryCommand, WorldHandle,
    WorldLineage, WorldMount,
};

use crate::WorldStatus;
use crate::btrfs::KernelBtrfsDriver;
use crate::child_env::{self, InheritedEnv};
use crate::child_io::run_streamed;
use crate::supervise::ProcessIdentity;
use crate::world_init::WORLD_STATE_GUEST;

use services::ServiceRecord;

/// Subdirectory of the world subvolume holding the read-only `L0` rootfs reference.
pub(crate) const WORLD_ROOTFS_DIR: &str = "rootfs";
/// Subdirectory of the world subvolume holding backing-service state.
pub(crate) const WORLD_STATE_DIR: &str = "state";
/// Category parents materialized inside `<subvol>/state`; concrete leaves (`pgdata/main`,
/// `redis/cache`, `s3/<bucket>`) are created by the owning service spec.
pub(crate) const WORLD_STATE_CATEGORIES: [&str; 3] = ["pgdata", "redis", "s3"];

/// The only Btrfs-backed [`WorldEngine`] (`ADR-0010` §1): one unified subvolume per world plus an
/// unprivileged `userns`/`netns`/`ipcns`/`pidns`/`mntns` PID 1. `slipforkd` wires it in `M3.15`.
pub struct BtrfsEngine {
    pub(crate) state_dir: PathBuf,
    pub(crate) host_uid: u32,
    pub(crate) host_gid: u32,
    pub(crate) world_init_bin: PathBuf,
    pub(crate) driver: KernelBtrfsDriver,
    /// Ambient allowlist (`PATH`/`TERM`/`LANG`) the composition root resolved once.
    pub(crate) inherited_env: InheritedEnv,
    pub(crate) shared: Arc<Shared>,
}

impl fmt::Debug for BtrfsEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BtrfsEngine")
            .field("state_dir", &self.state_dir)
            .field("host_uid", &self.host_uid)
            .field("host_gid", &self.host_gid)
            .field("world_init_bin", &self.world_init_bin)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) worlds: Mutex<BTreeMap<u64, WorldRecord>>,
    pub(crate) next_seq: AtomicU64,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            worlds: Mutex::new(BTreeMap::new()),
            next_seq: AtomicU64::new(0),
        }
    }
}

impl Drop for BtrfsEngine {
    fn drop(&mut self) {
        self.shared.worlds.clear_poison();
        let leaked: Vec<(u64, String, PathBuf, PathBuf)> = match self.lock() {
            Ok(worlds) => worlds
                .iter()
                .map(|(&seq, record)| {
                    (
                        seq,
                        record.world_id.clone(),
                        record.ctl_dir.clone(),
                        record.world_dir.clone(),
                    )
                })
                .collect(),
            Err(_) => return,
        };
        for (seq, world_id, ctl_dir, world_dir) in leaked {
            let handle = self.mint_world_handle(seq);
            match self.teardown_world(&handle, Duration::ZERO, teardown::WORLD_DROP_TERM_GRACE) {
                Ok(()) => {
                    eprintln!(
                        "slipfork-engine: leaked world '{world_id}' reclaimed on BtrfsEngine drop \
                         (ctl={}, subvolume={})",
                        ctl_dir.display(),
                        world_dir.display()
                    );
                }
                Err(err) => {
                    // Even if a service survived its stop bound, still reap the world launcher/init
                    // so no `unshare` supervisor outlives the dropped engine.
                    let _ = self.reap_world_init(&handle, Duration::ZERO);
                    eprintln!(
                        "slipfork-engine: failed to reclaim leaked world '{world_id}' on \
                         BtrfsEngine drop (ctl={}, subvolume={}): {err}",
                        ctl_dir.display(),
                        world_dir.display()
                    );
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorldSubvolume {
    /// `<state_dir>/worlds/<world_id>` exists on disk, cloned from this sealed snapshot.
    Cloned(slipfork_world::SnapshotId),
    /// A base-changing reload destroyed the previous subvolume and has not cloned its replacement.
    Missing,
}

#[derive(Debug)]
pub(crate) struct WorldRecord {
    /// Subvolume name: `<state_dir>/worlds/<world_id>` and `<state_dir>/ctl/<world_id>`.
    pub(crate) world_id: String,
    /// The one writable world subvolume.
    pub(crate) world_dir: PathBuf,
    /// Ephemeral control directory, strictly outside the subvolume.
    pub(crate) ctl_dir: PathBuf,
    /// `<world_dir>/state`, bind-mounted read-write at `/.slipfork/state`.
    pub(crate) state_dir: PathBuf,
    /// `<ctl_dir>/netns.sock`, the `SCM_RIGHTS` dial socket.
    pub(crate) netns_sock: PathBuf,
    pub(crate) init_pid: u32,
    pub(crate) init_identity: Option<ProcessIdentity>,
    pub(crate) launcher: Option<std::process::Child>,
    pub(crate) base_env: ExecEnv,
    pub(crate) rootfs: WorldRootfs,
    /// On-disk base snapshot of this world's subvolume: a reload naming the same snapshot keeps
    /// the existing subvolume, while a different snapshot (or `Missing`) clones from `spec`.
    pub(crate) subvolume: WorldSubvolume,
    pub(crate) mounts: Vec<WorldMount>,
    pub(crate) status: WorldStatus,
    /// A teardown started and may still be running: every caller sees the world as gone, and
    /// [`WorldEngine::destroy_world`] re-enters where the previous attempt stopped.
    pub(crate) destroying: bool,
    pub(crate) services: BTreeMap<String, ServiceRecord>,
    /// This world's own boot lineage, recorded when its on-disk rootfs was materialized.
    pub(crate) lineage: WorldLineage,
}

impl WorldRecord {
    /// A world whose teardown has started or finished is gone for every caller.
    pub(crate) fn is_gone(&self) -> bool {
        self.destroying || self.status == WorldStatus::Destroyed
    }
}

impl BtrfsEngine {
    /// Construct the engine over the concrete [`KernelBtrfsDriver`] injected by the composition
    /// root — the one dependency-injection seam for the CoW substrate (`ADR-0010` §5).
    ///
    /// This is the **only** constructor. Construction probes `statfs(<state_dir>)` through the
    /// driver and fails closed **before** creating a snapshot, world subvolume, or control
    /// directory; there is no clock to wire, because the engine records the plain [`std::time::Instant`]
    /// it observes when it spawns each service.
    pub fn new(
        state_dir: impl Into<PathBuf>,
        host_uid: u32,
        host_gid: u32,
        world_init_bin: impl Into<PathBuf>,
        driver: KernelBtrfsDriver,
        inherited_env: InheritedEnv,
    ) -> Result<Self, EngineError> {
        let state_dir = state_dir.into();
        let world_init_bin = world_init_bin.into();
        for path in [&state_dir, &world_init_bin] {
            if !path.is_absolute() {
                return Err(EngineError::Io {
                    path: path.display().to_string(),
                    message: "must be absolute (the Btrfs engine never resolves a path against \
                              the ambient current_dir())"
                        .to_owned(),
                });
            }
        }
        // Fail closed before anything exists (`ADR-0010` §4): a non-Btrfs `<state_dir>` never
        // reaches a snapshot or a world directory, and no fallback filesystem is ever used.
        driver.ensure_supported(&state_dir)?;
        Ok(Self {
            state_dir,
            host_uid,
            host_gid,
            world_init_bin,
            driver,
            inherited_env,
            shared: Arc::new(Shared::default()),
        })
    }

    /// `<state_dir>/snapshots`: parent of every sealed read-only snapshot subvolume.
    pub(crate) fn snapshots_dir(&self) -> PathBuf {
        self.state_dir.join("snapshots")
    }

    /// `<state_dir>/worlds`: parent of every live writable world subvolume.
    pub(crate) fn worlds_dir(&self) -> PathBuf {
        self.state_dir.join("worlds")
    }

    /// `<state_dir>/ctl/<world_id>`: ephemeral per-world control directory, never sealed.
    pub(crate) fn ctl_dir(&self, world_id: &str) -> PathBuf {
        self.state_dir.join("ctl").join(world_id)
    }

    /// `<state_dir>/worlds/<world_id>`: the one writable world subvolume.
    pub(crate) fn world_dir(&self, world_id: &str) -> PathBuf {
        self.worlds_dir().join(world_id)
    }

    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, BTreeMap<u64, WorldRecord>>, EngineError> {
        self.shared
            .worlds
            .lock()
            .map_err(|_| EngineError::Execution("btrfs world registry lock is poisoned".to_owned()))
    }

    pub(crate) fn owned<'a>(
        &self,
        worlds: &'a mut BTreeMap<u64, WorldRecord>,
        world: &WorldHandle,
    ) -> Result<&'a mut WorldRecord, EngineError> {
        if !world.belongs_to(self.name()) {
            return Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            });
        }
        worlds
            .get_mut(&world.seq())
            .ok_or_else(|| EngineError::WorldGone {
                world: format!("{world:?}"),
            })
    }

    /// The record of a world that still exists and whose last reload did not fail, failing closed
    /// with [`EngineError::WorldGone`] once its teardown has started and with
    /// [`EngineError::WorldNotLive`] after a reload whose boot failed.
    ///
    /// Status is **not** a liveness proof: a reload that dies while stopping services or reaping
    /// the PID 1 returns before it stamps `Failed`, so a `Live` record can describe a world whose
    /// init is gone. Callers that must know ask [`WorldEngine::world_is_live`].
    pub(crate) fn live_record<'a>(
        &self,
        worlds: &'a mut BTreeMap<u64, WorldRecord>,
        world: &WorldHandle,
    ) -> Result<&'a mut WorldRecord, EngineError> {
        let record = self.owned(worlds, world)?;
        if record.is_gone() {
            return Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            });
        }
        if record.status == WorldStatus::Failed {
            return Err(EngineError::WorldNotLive {
                world: format!("{world:?}"),
            });
        }
        Ok(record)
    }
}

impl BtrfsEngine {
    pub(crate) fn lineage_of(&self, world: &WorldHandle) -> Result<WorldLineage, EngineError> {
        let mut worlds = self.lock()?;
        let record = self.live_record(&mut worlds, world)?;
        Ok(record.lineage.clone())
    }

    pub(crate) fn is_world_live(&self, world: &WorldHandle) -> Result<bool, EngineError> {
        let mut worlds = self.lock()?;
        let record = self.owned(&mut worlds, world)?;
        if record.is_gone() {
            return Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            });
        }
        // Both halves are load-bearing: a reload stamps `Failed` only *after* it reaped the PID 1,
        // so a `Live` status alone can still describe a dead world, and a live pin alone cannot
        // vouch for a record being sealed mid-freeze. Leader-only: a service left behind by dead
        // init is not a live world.
        Ok(record.status == WorldStatus::Live
            && record
                .init_identity
                .as_ref()
                .is_some_and(ProcessIdentity::is_live_leader))
    }

    pub(crate) fn run_in_world(
        &self,
        world: &WorldHandle,
        argv: &[String],
        cwd: &Path,
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        if argv.is_empty() {
            return Err(EngineError::Execution(
                "exec argv must not be empty".to_owned(),
            ));
        }
        let (init_pid, mounts, env) = {
            let mut worlds = self.lock()?;
            let record = self.live_record(&mut worlds, world)?;
            (
                record.init_pid,
                record.mounts.clone(),
                child_env::materialize(&record.base_env, overlay_env, |var| {
                    self.inherited_env.get(var)
                }),
            )
        };

        let mut command = Command::new("nsenter");
        command
            .args(self.entry_prefix(init_pid, &mounts, cwd))
            .args(argv)
            .env_clear()
            .envs(&env)
            .env("SLIPFORK_WORLD_STATE", WORLD_STATE_GUEST)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        run_streamed(command, on_output)
    }

    pub(crate) fn world_entry_command(
        &self,
        world: &WorldHandle,
        cwd: &Path,
    ) -> Result<WorldEntryCommand, EngineError> {
        let (init_pid, mounts) = {
            let mut worlds = self.lock()?;
            let record = self.live_record(&mut worlds, world)?;
            (record.init_pid, record.mounts.clone())
        };
        Ok(WorldEntryCommand {
            program: PathBuf::from("nsenter"),
            args_prefix: self.entry_prefix(init_pid, &mounts, cwd),
            // The thin CLI `chdir`s here before `exec`ing `nsenter`, so this must be a host path:
            // `/` is the one directory that exists on the host *and* in every world rootfs. The
            // in-world start directory is `--wdns=<mapped dir>` inside `args_prefix`.
            cwd: PathBuf::from("/"),
        })
    }
}

pub(crate) fn io_error(path: &Path, err: &std::io::Error) -> EngineError {
    EngineError::Io {
        path: path.display().to_string(),
        message: err.to_string(),
    }
}
