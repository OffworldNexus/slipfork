use super::super::DaemonConfig;
use super::super::DaemonPorts;
use super::super::HostCaches;
use super::super::PairingStore;
use super::super::postgres::PostgresReady;
use super::super::service::DaemonService;
use crate::browser::Browser;
use crate::layout::Layout;
use crate::vaults::LayoutProjectVaults;
use crate::vcs::RepositoryWorkspaces;
use slipfork_edge::WorldRegistry;
use slipfork_engine::rootfs::BaseImageBuilder;
use slipfork_engine::rootfs::RuntimeAssets;
use slipfork_ospp::{DeviceKeySigner, OsppClient, OsppEndpoint, OsppTimeouts, WsOsppClient};
use slipfork_snow::OsCsprngEntropy;
use slipfork_vcs::{RepoRoot, RepoRootResolver};
use slipfork_wire::control::IngressInfo;
use slipfork_world::rootfs::WorldRootfs;
use slipfork_world::{
    EngineError, ExecEnv, ExecOutput, ProjectSlug, ServiceHandle, ServiceKind, ServiceSpec,
    ServiceState, SnapshotId, StreamChunk, WorldEngine, WorldHandle, WorldSpec,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// The world every restart test boots: one enabled component, one pre-deploy hook, one
/// ingress, and one `modes.local` env value — each editable between runs.
pub(super) fn restart_snow(port: u16, hook: &str, greeting: &str) -> String {
    format!(
        r#"
images:
  - id: app
    context: .
    dockerfile: Dockerfile

components:
  - id: api
    image: app
    envs: [app]
    run_command: "exec sleep 300"

ingresses:
  - match:
      prefix: "/"
    target:
      component: api
      port: {port}

hooks:
  - component: api
    when: pre-deploy
    run_command: "{hook}"

modes:
  local:
    envs:
      app:
        GREETING: '{greeting}'
"#
    )
}

/// A short, disk-backed scratch root.
///
/// Never `/tmp` (tmpfs: the seed-dump store refuses it) and never the repository path (the
/// daemon's ingress socket must stay under the 107-byte `AF_UNIX` budget).
pub(super) fn disk_scratch(label: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let root = PathBuf::from("/var/tmp").join("sf-m32817").join(format!(
        "{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("scratch dir");
    root
}

/// An engine double that models handles, the supervision rows, and every call the daemon makes.
///
/// It returns handles through the public `WorldEngine::mint_service_handle` seam and keeps its
/// own `WorldServiceInfo` rows, so `world_processes` is the record the daemon reads back (never
/// the daemon's own handle list). It records the spawn environments and hook argv, which is how
/// the test proves an edited `snow.yml` is what the world runs.
pub(super) struct RecordingEngine {
    pub(super) live: Mutex<Vec<WorldHandle>>,
    pub(super) snapshots: Mutex<Vec<SnapshotId>>,
    pub(super) next_seq: AtomicU64,
    pub(super) spawns: AtomicUsize,
    pub(super) world_stops: AtomicUsize,
    pub(super) destroyed: AtomicUsize,
    pub(super) hooks: Mutex<Vec<Vec<String>>>,
    pub(super) greetings: Mutex<Vec<Option<String>>>,
    pub(super) rows: Mutex<Vec<RecordedRow>>,
    /// The reload failure shape the next `reload_world` produces, if any.
    pub(super) reload_fault: Mutex<Option<ReloadFault>>,
    /// Handles a reload tore down and stamped non-live (`WorldStatus::Failed`): still owned,
    /// no live PID 1, and every read fails closed with `WorldNotLive`.
    pub(super) torndown: Mutex<Vec<WorldHandle>>,
    /// Handles a reload tore down while its record still reads `Live` — the real engine's
    /// window between reaping the PID 1 and stamping `Failed`, where only `world_is_live` can
    /// tell the truth.
    pub(super) init_gone: Mutex<Vec<WorldHandle>>,
}

/// The reload failure shapes the real engine distinguishes, and the daemon must not confuse.
#[derive(Clone, Copy)]
pub(super) enum ReloadFault {
    /// A pre-flight refusal: nothing was stopped or re-rooted, the world stays live.
    Refused,
    /// A boot failure past the gate: every service stopped and the record stamped
    /// `WorldStatus::Failed`.
    AfterTheGate,
    /// A teardown failure past the gate: the PID 1 is gone but the record is not stamped
    /// `Failed` yet, so its lineage still reads and only `world_is_live` says it is dead.
    TeardownBeforeStamp,
}

pub(super) struct RecordedRow {
    pub(super) world: WorldHandle,
    pub(super) id: String,
    pub(super) state: ServiceState,
}

/// One environment value an `ExecEnv` resolves to (`None` when the name is absent).
pub(super) fn env_value(env: &ExecEnv, name: &str) -> Option<String> {
    env.iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.into_owned())
}

impl RecordingEngine {
    pub(super) fn new() -> Self {
        Self {
            live: Mutex::new(Vec::new()),
            snapshots: Mutex::new(Vec::new()),
            next_seq: AtomicU64::new(0),
            spawns: AtomicUsize::new(0),
            world_stops: AtomicUsize::new(0),
            destroyed: AtomicUsize::new(0),
            hooks: Mutex::new(Vec::new()),
            greetings: Mutex::new(Vec::new()),
            rows: Mutex::new(Vec::new()),
            reload_fault: Mutex::new(None),
            torndown: Mutex::new(Vec::new()),
            init_gone: Mutex::new(Vec::new()),
        }
    }

    /// `(spawn_service, stop_world_services, hooks run)` — the daemon's supervision footprint.
    pub(super) fn counts(&self) -> (usize, usize, usize) {
        (
            self.spawns.load(Ordering::SeqCst),
            self.world_stops.load(Ordering::SeqCst),
            self.hooks.lock().expect("hook records").len(),
        )
    }

    /// The double owns `world` (it was minted here and never released).
    pub(super) fn owns(&self, world: &WorldHandle) -> Result<(), EngineError> {
        let live = self.live.lock().expect("live worlds");
        if live.contains(world) {
            Ok(())
        } else {
            Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            })
        }
    }

    /// The double owns `world` and its last reload did not fail: a read that needs a live world
    /// fails closed with `WorldNotLive` for a record stamped `Failed`, exactly like the engine.
    pub(super) fn live_world(&self, world: &WorldHandle) -> Result<(), EngineError> {
        self.owns(world)?;
        let torndown = self
            .torndown
            .lock()
            .expect("torndown worlds")
            .contains(world);
        if torndown {
            return Err(EngineError::WorldNotLive {
                world: format!("{world:?}"),
            });
        }
        Ok(())
    }

    /// Arm the failure shape the next `reload_world` produces.
    pub(super) fn fail_next_reload(&self, fault: ReloadFault) {
        *self.reload_fault.lock().expect("reload fault") = Some(fault);
    }
}

impl WorldEngine for RecordingEngine {
    fn name(&self) -> &'static str {
        "recording"
    }

    fn create_snapshot(&self, _source_dir: &Path, id: &SnapshotId) -> Result<(), EngineError> {
        self.snapshots.lock().expect("snapshots").push(id.clone());
        Ok(())
    }

    fn snapshot_exists(&self, _id: &SnapshotId) -> Result<bool, EngineError> {
        // Every base is already sealed: the seeding pipeline is never entered by this double.
        Ok(true)
    }

    fn freeze_world(&self, _world: &WorldHandle, _id: &SnapshotId) -> Result<(), EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "freeze_world",
        })
    }

    fn clone_world(&self, spec: &WorldSpec) -> Result<WorldHandle, EngineError> {
        spec.validate()?;
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let handle = self.mint_world_handle(seq);
        self.live.lock().expect("live worlds").push(handle.clone());
        Ok(handle)
    }

    fn spawn_service(
        &self,
        world: &WorldHandle,
        spec: &ServiceSpec,
    ) -> Result<ServiceHandle, EngineError> {
        self.live_world(world)?;
        self.spawns.fetch_add(1, Ordering::SeqCst);
        self.greetings
            .lock()
            .expect("spawn envs")
            .push(env_value(&spec.env, "GREETING"));
        {
            let mut rows = self.rows.lock().expect("rows");
            rows.retain(|row| !(row.world == *world && row.id == spec.id));
            rows.push(RecordedRow {
                world: world.clone(),
                id: spec.id.clone(),
                state: ServiceState::Running,
            });
        }
        Ok(self.mint_service_handle(
            world,
            &spec.id,
            Arc::new(std::sync::atomic::AtomicU8::new(
                ServiceState::Running.to_bits(),
            )),
        ))
    }

    fn stop_service(&self, _service: &ServiceHandle) -> Result<(), EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "stop_service",
        })
    }

    fn stop_world_services(&self, world: &WorldHandle) -> Result<(), EngineError> {
        self.live_world(world)?;
        self.world_stops.fetch_add(1, Ordering::SeqCst);
        for row in self.rows.lock().expect("rows").iter_mut() {
            if row.world == *world {
                row.state = ServiceState::Stopped;
            }
        }
        Ok(())
    }

    /// The real engine's reload contract: the handle survives, the services do not — and a
    /// failure either refuses before any mutation or leaves the record non-live.
    fn reload_world(&self, world: &WorldHandle, spec: &WorldSpec) -> Result<(), EngineError> {
        spec.validate()?;
        self.live_world(world)?;
        let fault = self.reload_fault.lock().expect("reload fault").take();
        if let Some(ReloadFault::Refused) = fault {
            return Err(EngineError::SnapshotNotFound("refused".to_owned()));
        }
        self.world_stops.fetch_add(1, Ordering::SeqCst);
        self.rows
            .lock()
            .expect("rows")
            .retain(|row| row.world != *world);
        if let Some(ReloadFault::AfterTheGate) = fault {
            self.torndown
                .lock()
                .expect("torndown worlds")
                .push(world.clone());
            return Err(EngineError::WorldInitFailed {
                world: format!("{world:?}"),
                reason: "the world PID 1 never became ready".to_owned(),
            });
        }
        if let Some(ReloadFault::TeardownBeforeStamp) = fault {
            // The recorded messages of a stop that burned its grace: the daemon must reach the
            // same verdict from the engine's liveness answer alone, never by reading this text.
            self.init_gone
                .lock()
                .expect("dead init worlds")
                .push(world.clone());
            return Err(EngineError::Execution(
                "service 'api' survived SIGTERM and SIGKILL for 70.0s: pid 4242 is still in \
                     /proc state 'D'"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    fn service_logs(
        &self,
        _world: &WorldHandle,
        _id: &str,
    ) -> Result<slipfork_world::ServiceLogs, EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "service_logs",
        })
    }

    fn world_processes(
        &self,
        world: &WorldHandle,
    ) -> Result<Vec<slipfork_world::WorldServiceInfo>, EngineError> {
        self.live_world(world)?;
        Ok(self
            .rows
            .lock()
            .expect("rows")
            .iter()
            .filter(|row| row.world == *world)
            .map(|row| slipfork_world::WorldServiceInfo {
                id: row.id.clone(),
                kind: ServiceKind::Component,
                state: row.state,
                pid: None,
                port: None,
                uptime_secs: None,
            })
            .collect())
    }

    fn world_lineage(
        &self,
        world: &WorldHandle,
    ) -> Result<slipfork_world::WorldLineage, EngineError> {
        self.live_world(world)?;
        Ok(slipfork_world::WorldLineage {
            rootfs: None,
            services: Vec::new(),
            seed: None,
            base: None,
        })
    }

    fn world_is_live(&self, world: &WorldHandle) -> Result<bool, EngineError> {
        // Ownership decides "gone"; the two teardown shapes decide "not live". The double keeps
        // no PID 1, so it models the engine's answer directly.
        self.owns(world)?;
        let torndown = self
            .torndown
            .lock()
            .expect("torndown worlds")
            .contains(world);
        let init_gone = self
            .init_gone
            .lock()
            .expect("dead init worlds")
            .contains(world);
        Ok(!torndown && !init_gone)
    }

    fn exec_in_service(
        &self,
        _service: &ServiceHandle,
        _argv: &[String],
        _overlay_env: &ExecEnv,
        _on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "exec_in_service",
        })
    }

    fn service_entry_command(
        &self,
        _service: &ServiceHandle,
    ) -> Result<slipfork_world::WorldEntryCommand, EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "service_entry_command",
        })
    }

    fn exec_in_world(
        &self,
        world: &WorldHandle,
        argv: &[String],
        _cwd: &Path,
        _overlay_env: &ExecEnv,
        _on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        self.live_world(world)?;
        self.hooks.lock().expect("hook records").push(argv.to_vec());
        Ok(ExecOutput {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    fn entry_command(
        &self,
        _world: &WorldHandle,
        _cwd: &Path,
    ) -> Result<slipfork_world::WorldEntryCommand, EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "entry_command",
        })
    }

    fn dial_world_tcp(
        &self,
        world: &WorldHandle,
        port: u16,
    ) -> Result<std::net::TcpStream, EngineError> {
        Err(EngineError::DialFailed {
            world: format!("{world:?}"),
            port,
            errno: 111,
        })
    }

    fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError> {
        let mut live = self.live.lock().expect("live worlds");
        let index = live
            .iter()
            .position(|candidate| candidate == &world)
            .ok_or_else(|| EngineError::WorldGone {
                world: format!("{world:?}"),
            })?;
        live.remove(index);
        self.rows
            .lock()
            .expect("rows")
            .retain(|row| row.world != world);
        self.destroyed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A rootfs builder double: the pipeline never shells out to Docker in a unit test.
pub(super) struct StubRootfs;

impl BaseImageBuilder for StubRootfs {
    fn materialize(
        &self,
        _req: &slipfork_engine::rootfs::WorldRootfsRequest,
        _on_event: &mut dyn FnMut(slipfork_engine::rootfs::RootfsBuildEvent),
    ) -> Result<WorldRootfs, slipfork_world::rootfs::BaseImageError> {
        Ok(WorldRootfs {
            digest: slipfork_world::rootfs::RootfsDigest::from_hex("e".repeat(64))
                .expect("valid digest"),
            app_rootfs: PathBuf::from("/nonexistent/m3-28-17/app"),
            crun_bin: PathBuf::from("/.slipfork/bin/crun"),
            service_images: BTreeMap::new(),
            metadata: slipfork_world::rootfs::ImageContractMetadata {
                os_release: slipfork_world::rootfs::OsRelease {
                    id: "alpine".to_owned(),
                    version_id: "3.21".to_owned(),
                    version_codename: String::new(),
                },
                elf_abi: slipfork_world::rootfs::ElfAbi {
                    arch: "x86_64".to_owned(),
                    libc: slipfork_world::rootfs::LibcFamily::Musl,
                    libc_version: "1.2".to_owned(),
                },
                exposed_ports: BTreeSet::new(),
            },
            host_caches: Vec::new(),
            cache: slipfork_world::CacheOutcome::Miss,
            millis: None,
        })
    }
}

/// One fixed repository: the daemon's `cwd`-resolution seam without shelling out to `git`.
pub(super) struct FixedRepo {
    pub(super) repo: RepoRoot,
}

impl RepoRootResolver for FixedRepo {
    fn resolve(&self, _start: &Path) -> Result<RepoRoot, slipfork_vcs::VcsError> {
        Ok(self.repo.clone())
    }

    fn remote(&self, _repo: &RepoRoot) -> Result<Option<String>, slipfork_vcs::VcsError> {
        Ok(None)
    }
}

/// Build the real `DaemonService` over doubled engine/rootfs/repository, the real registry,
/// vault, and workspace factory.
pub(super) fn test_service(
    scratch: &Path,
    worktree: &Path,
) -> (Arc<DaemonService>, Arc<WorldRegistry>, Arc<RecordingEngine>) {
    build_service(
        scratch,
        worktree,
        None,
        Arc::new(slipfork_ospp_testkit::signers::software_signer()),
        Browser::none(),
    )
}

/// The same graph wired to a live `ospp-v1` reference-server endpoint.
pub(super) fn test_service_with_ospp(
    scratch: &Path,
    worktree: &Path,
    endpoint: OsppEndpoint,
    signer: Arc<dyn DeviceKeySigner>,
) -> Arc<DaemonService> {
    // A real opener that exits zero: the pairing URL is handed to it, never logged.
    build_service(
        scratch,
        worktree,
        Some(endpoint),
        signer,
        Browser::new("/bin/true", true),
    )
    .0
}

fn build_service(
    scratch: &Path,
    worktree: &Path,
    endpoint: Option<OsppEndpoint>,
    signer: Arc<dyn DeviceKeySigner>,
    browser: Browser,
) -> (Arc<DaemonService>, Arc<WorldRegistry>, Arc<RecordingEngine>) {
    let layout = Layout::explicit(scratch.join("state"), scratch.join("run"), scratch, None);
    let engine = Arc::new(RecordingEngine::new());
    let registry = Arc::new(WorldRegistry::new(engine.clone(), 80));
    let ospp: Option<Arc<dyn OsppClient>> = endpoint.map(|endpoint| {
        Arc::new(WsOsppClient::new(
            endpoint,
            Arc::clone(&signer),
            Arc::new(OsCsprngEntropy),
            OsppTimeouts::default(),
        )) as Arc<dyn OsppClient>
    });
    let service = DaemonService::new(
        DaemonPorts {
            engine: engine.clone(),
            builder: Arc::new(StubRootfs),
            registry: registry.clone(),
            repo_roots: Arc::new(FixedRepo {
                repo: RepoRoot {
                    vcs_root: worktree.to_path_buf(),
                    worktree_root: worktree.to_path_buf(),
                    project: ProjectSlug::parse("acme").expect("valid project slug"),
                },
            }),
            workspaces: Arc::new(RepositoryWorkspaces::new()),
            vaults: Arc::new(LayoutProjectVaults::new(layout.clone())),
            ospp,
            signer,
            hostname: "test-machine".to_owned(),
            browser,
            entropy: Arc::new(OsCsprngEntropy),
            pairing: PairingStore::new(layout.pairing_path()),
        },
        DaemonConfig {
            layout,
            inherited: Vec::new(),
            runtime: RuntimeAssets {
                crun: PathBuf::from("/nonexistent/crun"),
                world_init: PathBuf::from("/nonexistent/slipfork-world-init"),
            },
            target_arch: "x86_64".to_owned(),
            uid: 1000,
            gid: 1000,
            host_caches: HostCaches::default(),
            postgres_ready: PostgresReady::production(),
        },
        IngressInfo::new(vec![SocketAddr::from(([127, 0, 0, 1], 80))]).expect("ingress"),
        None,
    );
    (Arc::new(service), registry, engine)
}
