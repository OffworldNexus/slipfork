//! The sharded runtime that executes boot steps concurrently: `ImageCache`, `ClonePool`,
//! lock-free `control.sock` `RunOnce`/`Dial`/`ConfigureS3`, and narrow `WorldSupervisor` launches.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use slipfork_forkfile::{FeatureSet, ast::Forkfile};
use slipfork_image::{Architecture, StepHost};
use slipfork_ospp::FileSecretVault;
use slipfork_world::{
    BootFailure, BootKind, BootState, BootStep, BtrfsEngine, Dirtiness, ImageStamp, PodEnv,
    PodHost, PodSpec, PostgresLauncher, ResourceKind, ResourceNode, ServiceNode, StepOutcome,
    StoreLayout, Stream, WorldEngine, WorldHost, WorldName, WorldPlan, WorldStampStore,
    WorldSupervisor, WorldTools, control, write_bundle,
};

use super::HostEnv;
use super::supervise::{BootSink, Emitter};

mod helpers;
mod images;
mod ready;
mod resources;
mod rootfs;

use helpers::{ServiceLaunch, merge_env, merge_path, project_binds, shell_command, upsert_env};

/// The one line a non-zero pod exit writes into both the capture and the failure message.
pub(crate) fn exit_line(command: &str, code: i32) -> String {
    format!("`{command}` exited with {code}")
}
use images::{BuiltImage, ImageCache};
use ready::{Ready, Staged};

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
    images: ImageCache,
    services: Vec<ServiceNode>,
    resources: Vec<ResourceNode>,
    clones: Mutex<Vec<PathBuf>>,
    live_world: AtomicBool,
    counter: AtomicUsize,
    total_steps: usize,
    emitter: Arc<Mutex<Emitter>>,
}

impl WorldRuntime {
    pub(super) fn new(
        project: &Path,
        world: WorldName,
        doc: &Forkfile,
        plan: &WorldPlan,
        features: FeatureSet,
        env: &HostEnv,
        emitter: Arc<Mutex<Emitter>>,
    ) -> Result<Self, String> {
        let scheme = "http";
        let is_fork = StoreLayout::parse_fork_world(std::ffi::OsStr::new(world.as_str())).is_some();
        let engine = BtrfsEngine::new(StoreLayout::at(env.store_root_for(project)));
        let layout = engine.layout().clone();
        engine.ensure_store().map_err(|error| error.to_string())?;
        let world_engine: Arc<dyn WorldEngine> = Arc::new(engine.clone());
        let architecture = Architecture::from_oci(env.arch)
            .ok_or_else(|| format!("unsupported host architecture `{}`", env.arch))?;
        let agent = format!("slipfork/{}", env!("CARGO_PKG_VERSION"));
        let crun = env.tool("crun")?;
        let step_host = StepHost::new(
            crun.clone(),
            env.resolv_conf.clone(),
            layout.store_root().join("build"),
            env.uid,
            env.gid,
        );
        // A world whose `world_epoch` and paired PG stamps match its planned DAG adopts its sealed
        // image subvolumes directly: no Dockerfile arg re-evaluation (so `arg VITE_CDN_URL =
        // static-cdn.url` keeps its digest), no context walk, no re-seal. A warm root world after
        // `down` qualifies exactly like a fork; `ImageCache` still refuses a vanished subvolume.
        let stamps = WorldStampStore::new(layout.world(&world));
        let pg_clones: Vec<_> = plan
            .resources
            .iter()
            .filter(|resource| resource.resource == ResourceKind::Postgres)
            .map(|resource| layout.world_clone(&world, resource.resource, &resource.id))
            .collect();
        let trusted_stamp =
            stamps.trusted_stamp(plan, &features, pg_clones.iter().map(PathBuf::as_path));
        let stamp_trusted = trusted_stamp.is_some();
        let adopted = trusted_stamp.map(|stamp| stamp.images).unwrap_or_default();
        let images = ImageCache::new(
            world_engine,
            &layout,
            world.clone(),
            project,
            doc,
            plan.images.clone(),
            adopted,
            architecture,
            &agent,
            step_host,
            scheme,
            features.clone(),
        );
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
            images,
            services: plan.services.clone(),
            resources: plan.resources.clone(),
            clones: Mutex::new(initial_clones),
            live_world: AtomicBool::new(false),
            counter: AtomicUsize::new(0),
            total_steps: plan.steps.len(),
            emitter,
        })
    }

    /// Executes one DAG node with zero global lock: staging uses narrow collaborator scopes and
    /// [`Staged::await_ready`] polls sockets outside all locks.
    ///
    /// Every step opens its run capture eagerly, before it executes anything; a `cached`/`up to
    /// date` outcome discards it. A capture that cannot be opened fails the step instead of
    /// silently running it unrecorded.
    pub(super) fn execute(
        &self,
        index: usize,
        step: &BootStep,
    ) -> Result<StepOutcome, BootFailure> {
        let mut log = BootSink::for_step(
            Arc::clone(&self.emitter),
            index + 1,
            self.total_steps,
            &step.label,
            &step.node_name(),
        );
        // Armed for every kind: a pod that dies inside `launch_pod` must still leave a record and a
        // readable capture, and only a cached outcome throws it away.
        let command = step.command.clone().unwrap_or_else(|| step.subject.clone());
        let store = slipfork_world::LogStore::at(self.layout.store_root(), self.world.clone());
        log = log.capture_run(
            &store,
            &self.project,
            step.kind.as_str(),
            &step.node_name(),
            &command,
        )?;
        let started = std::time::Instant::now();
        let mut staged = match self.execute_step(step, &mut log) {
            Ok(staged) => staged,
            Err(mut error) => {
                error.run_id = log.finish_run("failed", elapsed_ms(started));
                let _ = log.flush();
                return Err(error);
            }
        };
        let _ = log.flush();
        staged.attach_progress(log.clone());
        let outcome = staged.await_ready();
        let duration = elapsed_ms(started);
        let run_id = match &outcome {
            // A no-op step executed zero commands: its capture is removed and no id is shown.
            Ok(done) if matches!(done.state, BootState::Cached | BootState::UpToDate) => {
                log.discard_run();
                None
            }
            Ok(done) => log.finish_run(done.state.as_str(), duration),
            Err(_) => log.finish_run("failed", duration),
        };
        let _ = log.flush();
        let mut done = match outcome {
            Ok(done) => done,
            Err(mut error) => {
                error.run_id = run_id;
                return Err(error);
            }
        };
        done.run_id = run_id;
        Ok(done)
    }

    fn execute_step(&self, step: &BootStep, log: &mut BootSink) -> Result<Staged, BootFailure> {
        match step.kind {
            BootKind::Image => self
                .images
                .build(&step.subject, log)
                .map(|(_, state)| Staged {
                    state,
                    detail: state.as_str().to_owned(),
                    ready: Ready::None,
                    progress: None,
                })
                .map_err(BootFailure::new),
            BootKind::Resource => {
                resources::start_resource(self, &step.subject, log).map_err(BootFailure::new)
            }
            BootKind::Setup | BootKind::Hook => {
                let detail = self.run_once(step, log)?;
                Ok(Staged {
                    state: BootState::Ready,
                    detail,
                    ready: Ready::None,
                    progress: None,
                })
            }
            BootKind::Service => self.start_service(step, log).map_err(BootFailure::new),
        }
    }

    pub(super) fn pods_dir(&self) -> &Path {
        &self.pods_dir
    }

    /// Runs one `CHECKPOINT` inside every live postgres pod, over its readiness-proven socket.
    ///
    /// Dials `127.0.0.1:5432` through `<pods>/control.sock` (`SCM_RIGHTS`) and speaks the minimal
    /// PostgreSQL v3 wire protocol, so the checkpoint costs `< 5ms` with zero `crun`/`psql` spawn.
    /// The clone keeps `fsync=off`, so this is what makes the next `fork` snapshot durable data.
    pub(super) fn checkpoint_postgres(&self) -> Result<(), String> {
        PostgresLauncher::new(self.layout.clone(), self.world.clone())
            .checkpoint_live(&self.pods_dir)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Flushes the store filesystem after the checkpoint so the next fork snapshots durable data.
    pub(super) fn flush_store(&self) -> Result<(), String> {
        self.engine.flush_store().map_err(|error| error.to_string())
    }

    /// Records that at least one long-lived pod or native S3 endpoint entered the world.
    pub(super) fn mark_live_world(&self) {
        self.live_world.store(true, Ordering::Release);
    }

    /// Whether any resource or service entered the world; a cold build/hook failure with `false`
    /// tears the empty world down immediately instead of orphaning `slipfork-world-init`.
    pub(super) fn has_live_world(&self) -> bool {
        self.live_world.load(Ordering::Acquire)
    }

    /// Writes `<pods>/<id>/config.json` and launches the detached container concurrently over
    /// `<pods>/control.sock`, with zero supervisor lock held.
    pub(super) fn launch_pod(&self, spec: &PodSpec) -> Result<u32, String> {
        write_bundle(spec, &self.pod_host, &self.pods_dir.join(&spec.id))
            .map_err(|error| error.to_string())?;
        let pid = control::launch_pod(&self.pods_dir, spec, self.boot_timeout)
            .map_err(|error| error.to_string())?;
        self.mark_live_world();
        Ok(pid)
    }

    /// Runs one setup/hook/`initdb` pod over `<pods>/control.sock` with zero supervisor lock held.
    pub(super) fn run_pod_once(
        &self,
        spec: &PodSpec,
        on_line: &mut dyn FnMut(Stream, &str),
    ) -> slipfork_world::Result<i32> {
        write_bundle(spec, &self.pod_host, &self.pods_dir.join(&spec.id))?;
        control::run_once(&self.pods_dir, spec, self.boot_timeout, on_line)
    }

    /// Returns a writable pod rootfs subvolume: reuses a pre-snapshotted `<store>/pods/<world>/<slot>`
    /// from `fork` in 0ms when `reusable` is true, otherwise clones `sealed`.
    pub(super) fn clone_rootfs(
        &self,
        sealed: &Path,
        slot: &str,
        reusable: bool,
    ) -> Result<PathBuf, String> {
        let dir = self.layout.world_pods(&self.world);
        fs::create_dir_all(&dir).map_err(|error| format!("create `{}`: {error}", dir.display()))?;
        let dest = if reusable {
            let candidate = dir.join(slot);
            if slipfork_world::is_subvolume_path(&candidate).unwrap_or(false) {
                return Ok(candidate);
            }
            candidate
        } else {
            let seq = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
            dir.join(format!("{slot}-{seq}"))
        };
        if slipfork_world::is_subvolume_path(&dest).unwrap_or(false) {
            self.engine
                .destroy(&dest, Dirtiness::Clean, true)
                .map_err(|error| error.to_string())?;
        } else if dest.is_dir() {
            fs::remove_dir(&dest)
                .map_err(|error| format!("remove `{}`: {error}", dest.display()))?;
        }
        self.engine
            .clone_subvolume(sealed, &dest)
            .map_err(|error| error.to_string())?;
        if let Ok(mut clones) = self.clones.lock() {
            clones.push(dest.clone());
        }
        Ok(dest)
    }

    fn built_for(&self, image: Option<&String>, log: &mut BootSink) -> Result<BuiltImage, String> {
        let image = image.ok_or_else(|| "step has no enclosing image".to_owned())?;
        self.images.build(image, log).map(|(built, _)| built)
    }

    /// Merges image-level and subject-level runtime `env`, plus the declared `PORT`.
    fn pod_env_for(&self, subject: &str, image: Option<&str>, port: Option<u16>) -> Vec<PodEnv> {
        merge_env(&self.resolved_env, subject, image, port)
    }

    fn run_once(&self, step: &BootStep, log: &mut BootSink) -> Result<String, BootFailure> {
        if step.uses_env {
            return Err(BootFailure::with_remedy(
                "this step reads `$env.`, which this build cannot resolve",
                "replace `$env.<KEY>` with a literal value, or export the variable inside the step's own `run:` shell",
            ));
        }
        let command = step
            .command
            .clone()
            .ok_or_else(|| BootFailure::new(format!("`{}` has no command", step.label)))?;
        let built = self
            .built_for(step.image.as_ref(), log)
            .map_err(BootFailure::new)?;
        let rootfs = self
            .clone_rootfs(&built.rootfs, &format!("boot{}", step.line), false)
            .map_err(BootFailure::new)?;
        let seq = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let spec = PodSpec {
            id: format!("boot-{seq}"),
            rootfs,
            workdir: built.workdir,
            argv: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                shell_command(&command),
            ],
            inherited_env: built.inherited_env,
            env: self.pod_env_for(&step.subject, step.image.as_deref(), None),
            binds: project_binds(&self.features, &self.project),
            files: Vec::new(),
        };
        // Setup and hook pods run to completion over `control.sock` with their pipes attached, so
        // independent nodes never stall behind a long `pre-deploy` seed.
        let mut tail: VecDeque<String> = VecDeque::new();
        let code = self
            .run_pod_once(&spec, &mut |stream, line| {
                log.append_stream(stream, line);
                if tail.len() == 10 {
                    tail.pop_front();
                }
                tail.push_back(line.to_owned());
            })
            .map_err(|error| BootFailure::new(error.to_string()))?;
        if code == 0 {
            Ok("ok".to_owned())
        } else {
            // A command that printed nothing still leaves one real line in its run capture, so a
            // failed step is never reported against an empty `slipfork logs -r <id>`.
            let line = exit_line(&command, code);
            log.append_stream(Stream::Stdout, &line);
            let mut message = line;
            for line in tail {
                message.push_str("\n  ");
                message.push_str(&line);
            }
            Err(BootFailure::new(message))
        }
    }

    fn start_service(&self, step: &BootStep, log: &mut BootSink) -> Result<Staged, String> {
        let sigma = step.subject.clone();
        let built = self.built_for(step.image.as_ref(), log)?;
        let rootfs = self.clone_rootfs(&built.rootfs, &format!("svc-{sigma}"), true)?;
        let port = self
            .services
            .iter()
            .find(|service| service.id == sigma)
            .and_then(|service| service.port);
        // Build the merged pod env before resolving the launch so a `Forkfile`/image `PATH` is
        // the base the shim directory is prepended to — never something we overwrite.
        let env = self.pod_env_for(&step.subject, step.image.as_deref(), port);
        let path = merge_path(&env, &built.inherited_env);
        let mut launch_env = Vec::new();
        let (argv, display) = if let Some(command) = &step.command {
            let launch =
                ServiceLaunch::resolve(&self.project, &built.workdir, &rootfs, &path, command);
            launch_env = launch.env;
            (launch.argv, command.clone())
        } else if built.argv.is_empty() {
            return Err(format!(
                "service `{sigma}` declares no `run:` and its image has no `ENTRYPOINT`/`CMD`"
            ));
        } else {
            (built.argv.clone(), built.argv.join(" "))
        };
        let mut spec = PodSpec {
            id: sigma,
            rootfs,
            workdir: built.workdir,
            argv,
            inherited_env: built.inherited_env,
            env,
            binds: project_binds(&self.features, &self.project),
            files: Vec::new(),
        };
        upsert_env(&mut spec.env, launch_env);
        self.launch_pod(&spec)?;
        let ready = match port {
            Some(port) => Ready::Service {
                pods_dir: self.pods_dir.clone(),
                port,
                label: display,
                service: spec.id.clone(),
            },
            None => Ready::Portless {
                pods_dir: self.pods_dir.clone(),
                service: spec.id.clone(),
            },
        };
        Ok(Staged {
            state: BootState::Ready,
            detail: "started".to_owned(),
            ready,
            progress: None,
        })
    }

    /// The injected store layout (used to resolve sibling resource clones).
    pub(super) fn store_layout(&self) -> StoreLayout {
        self.layout.clone()
    }

    /// `<mount>/.slipfork-store/worlds/<world>`.
    pub(super) fn world_dir(&self) -> PathBuf {
        self.layout.world(&self.world)
    }

    /// Resolved rootfs + runtime facts per image id, recorded in the world stamp.
    pub(super) fn image_stamps(&self) -> BTreeMap<String, ImageStamp> {
        self.images.stamps()
    }

    /// Stops an empty world immediately and sweeps ephemeral pod rootfs clones.
    pub(super) fn stop_and_cleanup(&self) -> Result<(), String> {
        let stopped = self
            .supervisor
            .lock()
            .map_err(|_| "world supervisor lock poisoned".to_owned())?
            .stop()
            .map_err(|error| error.to_string());
        self.sweep_clones();
        stopped
    }

    /// Blocks until the world init exits, then sweeps ephemeral pod rootfs clones.
    pub(super) fn await_exit_and_cleanup(&self) -> Result<(), String> {
        let awaited = self
            .supervisor
            .lock()
            .map_err(|_| "world supervisor lock poisoned".to_owned())?
            .await_exit()
            .map_err(|error| error.to_string());
        self.sweep_clones();
        awaited
    }

    fn sweep_clones(&self) {
        let clones = self
            .clones
            .lock()
            .map(|mut guard| std::mem::take(&mut *guard))
            .unwrap_or_default();
        for clone in clones {
            let _ = self.engine.destroy(&clone, Dirtiness::Clean, true);
        }
        let _ = fs::remove_dir(self.layout.world_pods(&self.world));
        let _ = fs::remove_dir(self.layout.pods_root());
    }
}

/// Step wall time in whole milliseconds, clamped for a pathological clock jump.
fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
