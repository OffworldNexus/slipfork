//! The sharded runtime that executes boot steps concurrently from the daemon-sealed image heads,
//! plus lock-free `control.sock` `RunOnce`/`Dial`/`ConfigureS3` and narrow supervisor launches.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use slipfork_forkfile::{FeatureSet, ast::Forkfile};
use slipfork_ospp::FileSecretVault;
use slipfork_world::{
    BtrfsEngine, Dirtiness, PodHost, ResourceKind, ResourceNode, ServiceNode, StoreLayout,
    WorldEngine, WorldHost, WorldName, WorldPlan, WorldStampStore, WorldSupervisor, WorldTools,
};

use super::HostEnv;
use super::heads::{ImageHead, WorldHeads};
use super::supervise::Emitter;

pub(crate) mod capture;
mod dev;
mod execute;
pub(crate) mod frames;
mod helpers;
mod lifecycle;
pub(crate) mod notes;
mod pods;
mod ready;
mod resources;
mod rootfs;
mod steps;
use dev::DevMounts;

pub(super) use lifecycle::elapsed_ms;

/// The one line a non-zero pod exit writes into both the capture and the failure message.
pub(crate) fn exit_line(command: &str, code: i32) -> String {
    format!("`{command}` exited with {code}")
}

pub(super) struct WorldRuntime {
    engine: BtrfsEngine,
    layout: StoreLayout,
    world: WorldName,
    project: PathBuf,
    features: FeatureSet,
    resolved_env: BTreeMap<String, BTreeMap<String, (String, bool)>>,
    supervisor: Mutex<WorldSupervisor>,
    pods_dir: PathBuf,
    pod_host: PodHost,
    boot_timeout: Duration,
    heads: BTreeMap<String, ImageHead>,
    dev_mounts: DevMounts,
    services: Vec<ServiceNode>,
    resources: Vec<ResourceNode>,
    clones: Mutex<Vec<PathBuf>>,
    live_world: AtomicBool,
    counter: AtomicUsize,
    total_steps: usize,
    emitter: Arc<Mutex<Emitter>>,
}

/// The caller-injected inputs one world boot needs; every path and host fact is composed once.
pub(super) struct WorldBoot<'a> {
    pub(super) project: &'a Path,
    pub(super) world: WorldName,
    pub(super) doc: &'a Forkfile,
    pub(super) plan: &'a WorldPlan,
    pub(super) features: FeatureSet,
    pub(super) heads: &'a WorldHeads,
    pub(super) no_cache: bool,
    pub(super) env: &'a HostEnv,
    pub(super) emitter: Arc<Mutex<Emitter>>,
}

impl WorldRuntime {
    pub(super) fn new(boot: WorldBoot<'_>) -> Result<Self, String> {
        let WorldBoot {
            project,
            world,
            doc,
            plan,
            features,
            heads,
            no_cache,
            env,
            emitter,
        } = boot;
        let scheme = "http";
        // The Dockerfile owner answers where each dev image mounts its checkout; a stage that
        // never left `/` refuses the whole boot before the supervisor or any pod exists.
        let dev_mounts = DevMounts::load(
            project,
            world.as_str(),
            scheme,
            &plan.images,
            doc,
            &features,
        );
        dev_mounts.preflight()?;
        let is_fork = StoreLayout::parse_fork_world(std::ffi::OsStr::new(world.as_str())).is_some();
        let engine = BtrfsEngine::new(StoreLayout::at(env.store_root_for(project)));
        let layout = engine.layout().clone();
        engine.ensure_store().map_err(|error| error.to_string())?;
        let crun = env.tool("crun")?;
        // A world whose `world_epoch` and paired PG stamps match its planned DAG keeps its
        // pre-snapshotted pod clones. The daemon already proved every head rootfs is sealed; this
        // supervisor never builds, pulls, or re-seals an image.
        let stamps = WorldStampStore::new(layout.world(&world));
        let pg_clones: Vec<_> = plan
            .resources
            .iter()
            .filter(|resource| resource.resource == ResourceKind::Postgres)
            .map(|resource| layout.world_clone(&world, resource.resource, &resource.id))
            .collect();
        // `--no-cache` bypasses the recorded stamp, so a fork rebuilds its pod clones instead of
        // reusing them. Image heads are already the daemon's cold build; this only gates stamps.
        let stamp_trusted = !no_cache
            && stamps
                .trusted_stamp(
                    plan,
                    &features,
                    &heads.image_inputs,
                    pg_clones.iter().map(PathBuf::as_path),
                )
                .is_some();
        let host = WorldHost::new(
            WorldTools {
                crun,
                unshare: env.tool("unshare")?,
                slirp4netns: env.tool("slirp4netns")?,
                world_init: env.init_binary()?,
            },
            env.runtime_dir.clone(),
            layout.logs_root(),
            env.uid,
            env.gid,
            Some(env.resolv_conf.clone()),
        );
        let supervisor = WorldSupervisor::boot(host, &world).map_err(|error| error.to_string())?;
        let pods_dir = supervisor.pods_dir().to_path_buf();
        let pod_host = supervisor.pod_host();
        let boot_timeout = supervisor.boot_timeout();
        if !layout.world(&world).exists() {
            engine
                .create_world(&world)
                .map_err(|error| error.to_string())?;
        }
        // Pre-snapshotted `svc-*`/`res-*` pod clones come from the parent's sealed images; only a
        // fork whose `(plan, features)` epoch matches the parent may reuse them. A cross-feature
        // fork (`-F -dev`, `--no-default-features`) sweeps them and clones its own stage rootfses.
        let mut initial_clones = Vec::new();
        if is_fork && stamp_trusted && layout.world_pods(&world).is_dir() {
            if let Ok(entries) = fs::read_dir(layout.world_pods(&world)) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let keep = entry
                        .file_name()
                        .to_str()
                        .is_some_and(slipfork_world::is_reusable_pod)
                        && slipfork_world::is_subvolume_path(&path).unwrap_or(false);
                    if keep {
                        initial_clones.push(path);
                    } else if slipfork_world::is_subvolume_path(&path).unwrap_or(false) {
                        let _ = engine.destroy(&path, Dirtiness::Clean, true);
                    } else if path.is_dir() {
                        let _ = fs::remove_dir_all(&path);
                    } else {
                        let _ = fs::remove_file(&path);
                    }
                }
            }
        } else {
            engine
                .sweep_pods(&layout.world_pods(&world))
                .map_err(|error| error.to_string())?;
        }
        // The `Forkfile` compiler owns every `arg`/`env` expression; this composition root only
        // supplies the world name and the vault lookup the compiler asks for.
        let vault = FileSecretVault::new(env.state_dir.join("secrets.json"));
        let vars = FileSecretVault::new(env.state_dir.join("vars.json"));
        let resolver = crate::secrets::EnvironmentResolver::new(&vault, &vars, &env.env);
        let resolved_env = {
            let failure = std::cell::RefCell::new(None);
            let secret =
                |component: &str,
                 key: &str,
                 generator: Option<&slipfork_forkfile::GeneratorSpec>| {
                    resolver
                        .secret(component, key, generator)
                        .unwrap_or_else(|error| {
                            failure.borrow_mut().replace(error);
                            None
                        })
                };
            let var = |key: &str, generator: Option<&slipfork_forkfile::GeneratorSpec>| {
                resolver.var(key, generator).unwrap_or_else(|error| {
                    failure.borrow_mut().replace(error);
                    None
                })
            };
            let resolved = doc.eval_all_env_with_features(
                world.as_str(),
                scheme,
                &slipfork_forkfile::Resolvers {
                    secret: &secret,
                    var: &var,
                },
                &features,
            );
            if let Some(error) = failure.into_inner() {
                return Err(format!("{}\nnext: {}", error.message, error.next));
            }
            resolved
        };
        Ok(Self {
            engine,
            layout,
            world,
            project: project.to_path_buf(),
            features,
            resolved_env,
            supervisor: Mutex::new(supervisor),
            pods_dir,
            pod_host,
            boot_timeout,
            heads: heads.images.clone(),
            dev_mounts,
            services: plan.services.clone(),
            resources: plan.resources.clone(),
            clones: Mutex::new(initial_clones),
            live_world: AtomicBool::new(false),
            counter: AtomicUsize::new(0),
            total_steps: plan.steps.len(),
            emitter,
        })
    }

    pub(in crate::world) fn pods_dir(&self) -> &Path {
        &self.pods_dir
    }

    /// The injected store layout (used to resolve sibling resource clones).
    pub(in crate::world) fn store_layout(&self) -> StoreLayout {
        self.layout.clone()
    }

    /// `<mount>/.slipfork-store/worlds/<world>`.
    pub(in crate::world) fn world_dir(&self) -> PathBuf {
        self.layout.world(&self.world)
    }
}
