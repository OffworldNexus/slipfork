//! Per-service pod harness: real worlds, real `crun` pods, and real published namespaces.
//!
//! One [`ServicePod`] is one `Forkfile`-shaped service materialized exactly the way `slipforkd`
//! materializes it: its own OCI bundle in the world's control directory, the immutable `L0` rootfs,
//! a live-worktree bind (plus optional CoW volumes), and a direct `crun` join to the world init's
//! shared user/network/IPC namespaces. Every fact is read back from the kernel or the engine, never
//! synthesized: the bundle `config.json` comes from the world's private mount namespace, the
//! `-pgid` group from `/proc/<launcher>/stat`, and the namespace joins from the engine's published
//! `namespaces.json`.
//!
//! Nothing here is a mock. A host without Btrfs fails closed when the owning
//! [`WorldFixture`](crate::live::WorldFixture) boots (`require_btrfs`), so a harness caller never
//! reports green on a substrate that cannot run a pod.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use slipfork_world::{
    EngineError, ExecEnv, PodCommand, PodLocation, PodNamespaces, ServiceHandle, ServiceKind,
    ServicePodSpec, ServiceRuntime, ServiceSpec, VolumePlan, WorldEngine, WorldHandle,
};

/// Standard `PATH` a test pod inherits when it declares none of its own.
const POD_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// The complete recipe for one service pod the harness should boot.
///
/// The builder keeps the pod facts cohesive and named instead of a six-argument spawn function:
/// the service id and argv are mandatory, while the service directory, CoW volumes, and extra
/// environment values are added with the chained methods. The environment starts with the pod
/// `PATH` and is the container's **complete** environment, never a host overlay.
#[derive(Debug, Clone)]
pub struct PodPlan {
    service: String,
    directory: PathBuf,
    argv: Vec<String>,
    volumes: Vec<String>,
    env: BTreeMap<String, String>,
}

impl PodPlan {
    /// A pod for `service` running `argv` directly from the world worktree root.
    pub fn new(service: &str, argv: &[&str]) -> Self {
        Self {
            service: service.to_owned(),
            directory: PathBuf::new(),
            argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
            volumes: Vec::new(),
            env: BTreeMap::from([("PATH".to_owned(), POD_PATH.to_owned())]),
        }
    }

    /// Start the pod in `directory`, relative to the world's read-write worktree.
    pub fn in_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.directory = directory.into();
        self
    }

    /// Give one declared `volume <rel_path>` its own CoW subvolume under the world subvolume.
    pub fn volume(mut self, rel_path: &str) -> Self {
        self.volumes.push(rel_path.to_owned());
        self
    }

    /// Add one environment value to the pod's complete container environment.
    pub fn env(mut self, name: &str, value: &str) -> Self {
        self.env.insert(name.to_owned(), value.to_owned());
        self
    }

    /// The service id this plan boots.
    pub fn service(&self) -> &str {
        &self.service
    }
}

/// One running service pod, with the facts a journey asserts against.
///
/// The handle borrows the engine that spawned it, so a fixture can hand out as many pods as it
/// likes without cloning the engine or re-scanning a manifest.
pub struct ServicePod<'a> {
    engine: &'a dyn WorldEngine,
    world: WorldHandle,
    id: String,
    handle: ServiceHandle,
    bundle_dir: PathBuf,
    namespaces: PodNamespaces,
}

impl<'a> ServicePod<'a> {
    /// Write one OCI bundle and run it as a `crun` container in `world`'s shared namespaces.
    ///
    /// `worktree` is the world's read-write host mount; `plan.directory` is resolved beneath it so
    /// the engine can prove the pod's working directory is inside a live bind. `rootfs` is the
    /// world's immutable `L0` app tree.
    ///
    /// # Errors
    ///
    /// Returns the engine's typed refusal (a directory outside the worktree, a non-DNS service id,
    /// an escaping volume, an engine that declines pods) instead of degrading to another runtime.
    pub fn spawn(
        engine: &'a dyn WorldEngine,
        world: &WorldHandle,
        worktree: &Path,
        rootfs: &Path,
        plan: PodPlan,
    ) -> Result<Self, EngineError> {
        if plan.directory.is_absolute() {
            return Err(EngineError::InvalidWorldSpec {
                reason: format!(
                    "service pod '{}' directory '{}' must be relative to the world worktree\n  \
                     fix: name it relative to the worktree root",
                    plan.service,
                    plan.directory.display()
                ),
            });
        }
        let service_dir = worktree.join(&plan.directory);
        fs::create_dir_all(&service_dir).map_err(|err| EngineError::Io {
            path: service_dir.display().to_string(),
            message: format!("cannot create the service directory for its pod: {err}"),
        })?;

        let world_subvol = engine.world_subvolume(world)?;
        let namespaces = engine.world_pod_namespaces(world)?;
        let bundle_dir = engine.pod_bundle_dir(world, &plan.service)?;
        let volumes = VolumePlan::new(&world_subvol, &plan.service, &service_dir, &plan.volumes)
            .map_err(pod_refusal)?;
        let location = PodLocation::new(&world_subvol, &bundle_dir, rootfs).map_err(pod_refusal)?;
        let command = PodCommand::new(service_dir, plan.argv.clone(), plan.env);
        let pod = ServicePodSpec::new(location, namespaces.clone(), volumes, command)
            .map_err(pod_refusal)?;

        let handle = engine.spawn_service(
            world,
            &ServiceSpec {
                id: plan.service.clone(),
                kind: ServiceKind::Component,
                runtime: ServiceRuntime::ServicePod(pod),
                // The pod's complete environment lives in its bundle command.
                env: ExecEnv::default(),
                readiness_port: None,
            },
        )?;

        Ok(Self {
            engine,
            world: world.clone(),
            id: plan.service,
            handle,
            bundle_dir,
            namespaces,
        })
    }

    /// The pod's supervision id (the `Forkfile` service id).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The engine's supervision token for this pod.
    pub fn handle(&self) -> &ServiceHandle {
        &self.handle
    }

    /// `<ctl>/<world_id>/oci/<service>`: the engine-owned bundle directory, outside the subvolume.
    pub fn bundle_dir(&self) -> &Path {
        &self.bundle_dir
    }

    /// The world init's published user/network/IPC joins this pod entered.
    pub fn namespaces(&self) -> &PodNamespaces {
        &self.namespaces
    }

    /// The engine's current row for this pod (`None` once the world released it).
    pub fn row(&self) -> Option<slipfork_world::WorldServiceInfo> {
        self.engine
            .world_processes(&self.world)
            .ok()?
            .into_iter()
            .find(|row| row.id == self.id)
    }

    /// The pod supervisor's host pid, once the engine reports one.
    pub fn pid(&self) -> Option<u32> {
        self.row().and_then(|row| row.pid)
    }

    /// The pod's own host process group (`-pgid`): one group per pod, never shared.
    pub fn process_group(&self) -> Option<u32> {
        self.pid().and_then(host_pgrp)
    }

    /// Every live host pid in the pod's process group (launcher, `crun`, and descendants).
    pub fn members(&self) -> Vec<u32> {
        self.process_group().map(group_members).unwrap_or_default()
    }

    /// The OCI `config.json` the engine wrote, read through the world init's mount namespace.
    ///
    /// The bundle lives on the world's private tmpfs, so it never appears on the host state
    /// directory; reading it through `/proc/<init>/root` is the only honest view of what `crun`
    /// actually ran.
    ///
    /// # Panics
    ///
    /// Panics with the bundle path and the read/parse error when the pod's config is unreadable.
    pub fn config(&self) -> serde_json::Value {
        read_pod_config(&self.bundle_dir)
    }

    /// The pod's retained log ring as lossy text.
    pub fn log_text(&self) -> String {
        self.engine
            .service_logs(&self.world, &self.id)
            .map(|logs| {
                logs.snapshot()
                    .iter()
                    .map(|chunk| String::from_utf8_lossy(&chunk.bytes).into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// `/proc/<pid>/stat` field 5 (`pgrp`) of a host process.
pub fn host_pgrp(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .nth(2)?
        .parse()
        .ok()
}

/// Every host pid whose process group is `pgid`.
///
/// This is the `-pgid` reaping assertion every journey shares: after `down`, the group must be
/// empty (`kill(-pgid, …)` reached the `crun` runtime and every descendant).
pub fn group_members(pgid: u32) -> Vec<u32> {
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| host_pgrp(*pid) == Some(pgid))
        .collect()
}

/// Read one pod's OCI `config.json` through the world init's mount namespace.
///
/// The bundle lives on the world's private tmpfs, so it never appears on the host state directory;
/// reading it through `/proc/<init>/root` is the only honest view of what `crun` actually ran.
///
/// # Panics
///
/// Panics with the bundle path and the read/parse error when the config is unreadable.
pub fn read_pod_config(bundle_dir: &Path) -> serde_json::Value {
    let ctl_dir = bundle_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| {
            panic!(
                "pod bundle '{}' has no control directory",
                bundle_dir.display()
            )
        });
    let init_pid = world_init_pid(ctl_dir);
    let path = bundle_dir.join("config.json");
    let namespace_path = PathBuf::from(format!("/proc/{init_pid}/root{}", path.display()));
    let text = fs::read_to_string(&namespace_path).unwrap_or_else(|err| {
        panic!(
            "cannot read the pod bundle at {}: {err}",
            namespace_path.display()
        )
    });
    serde_json::from_str(&text).unwrap_or_else(|err| {
        panic!(
            "a pod wrote an invalid bundle config at {}: {err}",
            namespace_path.display()
        )
    })
}

/// The host pid of the `slipfork-world-init` that owns `ctl_dir`.
///
/// The control directory is only reachable inside the world's mount namespace, so this scan is
/// how a test reads a live pod's bundle. Findings are pinned to the exact `ctl_dir`, never to a
/// sibling world booted by a parallel suite.
///
/// # Panics
///
/// Panics with the control directory when no live world init owns it.
pub fn world_init_pid(ctl_dir: &Path) -> u32 {
    for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let argv: Vec<String> = cmdline
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect();
        if argv.iter().any(|arg| arg.ends_with("slipfork-world-init"))
            && argv.iter().any(|arg| Path::new(arg).starts_with(ctl_dir))
        {
            return pid;
        }
    }
    panic!(
        "no live `slipfork-world-init` owns the control directory {}",
        ctl_dir.display()
    );
}

/// One pod-planning refusal, carried as the engine's typed invalid-spec error.
fn pod_refusal(err: slipfork_world::PodError) -> EngineError {
    EngineError::InvalidWorldSpec {
        reason: err.to_string(),
    }
}
