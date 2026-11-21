//! The injected runtime that executes one boot step: images, resources, setup, hooks, services.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slipfork_forkfile::ast::Forkfile;
use slipfork_image::{
    Architecture, BuildArgs, BuildRequest, Credentials, ImageLayout, ImageReference,
    NativeImageBuilder, OciPuller, PulledImage, StepHost, StepLayout,
};
use slipfork_ospp::{FileSecretVault, SecretVault};
use slipfork_world::{
    BootFailure, BootKind, BootState, BootStep, BtrfsEngine, Dirtiness, ImageNode, PodEnv, PodSpec,
    ServiceNode, StoreLayout, WorldEngine, WorldHost, WorldName, WorldPlan, WorldSupervisor,
};

use super::HostEnv;
use super::supervise::BootSink;

mod helpers;
mod ready;
mod resources;

use helpers::{merge_env, project_bind, shell_command, sweep_stale_pods};
use ready::{Ready, Staged};

/// One sealed image plus the OCI metadata every pod derived from it inherits.
#[derive(Clone)]
struct BuiltImage {
    rootfs: PathBuf,
    inherited_env: Vec<String>,
    /// `ENTRYPOINT` followed by `CMD`; empty when the image defines neither.
    argv: Vec<String>,
    workdir: String,
}

pub(super) struct WorldRuntime {
    engine: BtrfsEngine,
    layout: StoreLayout,
    world: WorldName,
    project: PathBuf,
    doc: Forkfile,
    resolved_env: BTreeMap<String, BTreeMap<String, (String, bool)>>,
    pub(super) supervisor: WorldSupervisor,
    puller: OciPuller,
    builder: NativeImageBuilder,
    images: Vec<ImageNode>,
    services: Vec<ServiceNode>,
    resources: Vec<slipfork_world::ResourceNode>,
    sealed: BTreeMap<String, BuiltImage>,
    pulled: BTreeMap<String, PulledImage>,
    clones: Vec<PathBuf>,
    counter: usize,
    total_steps: usize,
    log: RefCell<BootSink>,
}

impl WorldRuntime {
    pub(super) fn new(
        project: &Path,
        world: WorldName,
        doc: &Forkfile,
        plan: &WorldPlan,
        env: &HostEnv,
        log: BootSink,
    ) -> Result<Self, String> {
        let engine = BtrfsEngine::new(StoreLayout::at(env.store_root_for(project)));
        engine
            .adopt_project(project)
            .and_then(|()| engine.ensure_store())
            .map_err(|error| error.to_string())?;
        let layout = engine.layout().clone();
        let world_engine: Arc<dyn WorldEngine> = Arc::new(engine.clone());
        let architecture = Architecture::from_oci(env.arch)
            .ok_or_else(|| format!("unsupported host architecture `{}`", env.arch))?;
        let credentials = Credentials::Anonymous;
        let agent = format!("slipfork/{}", env!("CARGO_PKG_VERSION"));
        let puller = OciPuller::new(
            Arc::clone(&world_engine),
            ImageLayout::new(layout.images_root()),
            credentials.clone(),
            architecture,
            agent.clone(),
        );
        let build_puller = OciPuller::new(
            Arc::clone(&world_engine),
            ImageLayout::new(layout.images_root()),
            credentials,
            architecture,
            agent,
        );
        let crun = env.tool("crun")?;
        let host = StepHost::new(
            crun.clone(),
            env.resolv_conf.clone(),
            layout.store_root().join("build"),
            env.uid,
            env.gid,
        );
        let builder = NativeImageBuilder::new(
            world_engine,
            build_puller,
            StepLayout::new(layout.images_root()),
            architecture,
            host,
        );
        let host = WorldHost::new(
            crun,
            env.tool("unshare")?,
            env.init_binary()?,
            env.runtime_dir.clone(),
            env.uid,
            env.gid,
            Some(env.resolv_conf.clone()),
        );
        let supervisor = WorldSupervisor::boot(host, &world).map_err(|error| error.to_string())?;
        if !layout.world(&world).exists() {
            engine
                .create_world(&world)
                .map_err(|error| error.to_string())?;
        }
        sweep_stale_pods(&engine, &layout.world(&world).join("pods"))?;
        // The `Forkfile` compiler owns every `arg`/`env` expression; this composition root only
        // supplies the world name and the vault lookup the compiler asks for.
        let vault = FileSecretVault::new(env.state_dir.join("secrets.json"));
        let resolved_env = doc.eval_all_env(
            world.as_str(),
            &|component, key| {
                vault
                    .load(component, key)
                    .ok()
                    .flatten()
                    .map(|secret| secret.expose().to_owned())
            },
            &|key| std::env::var(key).ok(),
        );
        Ok(Self {
            engine,
            layout,
            world,
            project: project.to_path_buf(),
            doc: doc.clone(),
            resolved_env,
            supervisor,
            puller,
            builder,
            images: plan.images.clone(),
            services: plan.services.clone(),
            resources: plan.resources.clone(),
            sealed: BTreeMap::new(),
            pulled: BTreeMap::new(),
            clones: Vec::new(),
            counter: 0,
            total_steps: plan.steps.len(),
            log: RefCell::new(log),
        })
    }

    /// Prepares one ready step: builds/launches under the lock, then returns a readiness probe.
    ///
    /// The caller awaits [`Staged::await_ready`] after this returns, so a slow database start
    /// never holds the runtime mutex and independent nodes boot concurrently.
    pub(super) fn stage(&mut self, index: usize, step: &BootStep) -> Result<Staged, BootFailure> {
        self.log
            .get_mut()
            .focus(index + 1, self.total_steps, &step.label);
        let mut staged = match step.kind {
            BootKind::Image => self
                .build_image(&step.subject)
                .map(|(_, state)| Staged {
                    state,
                    detail: "built".to_owned(),
                    ready: Ready::None,
                    progress: None,
                })
                .map_err(BootFailure::new)?,
            BootKind::Resource => self
                .start_resource(&step.subject)
                .map_err(BootFailure::new)?,
            BootKind::Setup | BootKind::Hook => {
                let detail = self.run_once(step)?;
                Staged {
                    state: BootState::Ready,
                    detail,
                    ready: Ready::None,
                    progress: None,
                }
            }
            BootKind::Service => self.start_service(step).map_err(BootFailure::new)?,
        };
        staged.attach_progress(
            self.log.get_mut().teed(),
            index + 1,
            self.total_steps,
            &step.label,
        );
        Ok(staged)
    }

    fn build_image(&mut self, image: &str) -> Result<(BuiltImage, BootState), String> {
        if let Some(built) = self.sealed.get(image) {
            return Ok((built.clone(), BootState::Cached));
        }
        let node = self
            .images
            .iter()
            .find(|candidate| candidate.id == image)
            .cloned()
            .ok_or_else(|| format!("unknown image `{image}`"))?;
        let (built, state) = if let Some(reference) = &node.from {
            let parsed = ImageReference::parse(reference).map_err(|error| error.to_string())?;
            self.note(format!("pull {reference}"));
            let pulled = self
                .puller
                .pull_into(&parsed, &mut *self.log.get_mut())
                .map_err(|error| error.to_string())?;
            let mut argv = pulled.facts.entrypoint.clone();
            argv.extend(pulled.facts.cmd.iter().cloned());
            (
                BuiltImage {
                    rootfs: pulled.rootfs,
                    inherited_env: pulled.facts.env,
                    argv,
                    workdir: "/".to_owned(),
                },
                BootState::Pulled,
            )
        } else {
            let context = self.project.join(&node.directory);
            let dockerfile = context.join(
                node.dockerfile
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("Dockerfile")),
            );
            let mut args = BuildArgs::new();
            for (key, value) in self.doc.eval_image_args(self.world.as_str(), image) {
                args.declare(key, value);
            }
            let request = BuildRequest {
                context,
                dockerfile,
                args,
                target: self.doc.eval_image_stage(image),
            };
            let report = self
                .builder
                .build(&request, &mut *self.log.get_mut())
                .map_err(|error| error.to_string())?;
            self.note(format!(
                "dockerfile {image}: {} cached, {} built",
                report.cache_hits, report.cache_misses
            ));
            (
                BuiltImage {
                    rootfs: report.rootfs,
                    inherited_env: report
                        .config
                        .env
                        .iter()
                        .map(|(key, value)| format!("{key}={value}"))
                        .collect(),
                    argv: report.config.exec_argv(),
                    workdir: report.config.workdir,
                },
                if report.cache_misses == 0 && report.cache_hits > 0 {
                    BootState::Cached
                } else {
                    BootState::Built
                },
            )
        };
        self.sealed.insert(image.to_owned(), built.clone());
        Ok((built, state))
    }

    fn clone_rootfs(&mut self, sealed: &Path, prefix: &str) -> Result<PathBuf, String> {
        self.counter += 1;
        let dir = self.layout.world(&self.world).join("pods");
        fs::create_dir_all(&dir).map_err(|error| format!("create `{}`: {error}", dir.display()))?;
        let dest = dir.join(format!("{prefix}-{}", self.counter));
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
        self.clones.push(dest.clone());
        Ok(dest)
    }

    fn built_for(&mut self, image: Option<&String>) -> Result<BuiltImage, String> {
        let image = image.ok_or_else(|| "step has no enclosing image".to_owned())?;
        self.build_image(image).map(|(built, _)| built)
    }

    /// Merges image-level and subject-level runtime `env`, plus the declared `PORT`.
    fn pod_env_for(&self, subject: &str, image: Option<&str>, port: Option<u16>) -> Vec<PodEnv> {
        merge_env(&self.resolved_env, subject, image, port)
    }

    fn run_once(&mut self, step: &BootStep) -> Result<String, BootFailure> {
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
            .built_for(step.image.as_ref())
            .map_err(BootFailure::new)?;
        let rootfs = self
            .clone_rootfs(&built.rootfs, &format!("boot{}", step.line))
            .map_err(BootFailure::new)?;
        let spec = PodSpec {
            id: format!("boot-{}", self.counter),
            rootfs,
            workdir: built.workdir,
            argv: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                shell_command(&command),
            ],
            inherited_env: built.inherited_env,
            env: self.pod_env_for(&step.subject, step.image.as_deref(), None),
            binds: vec![project_bind(&self.project)],
            files: Vec::new(),
        };
        // Setup and hook pods run to completion with their pipes attached, so a failing
        // command's own diagnostics stream live and ride the failure message.
        let mut tail: VecDeque<String> = VecDeque::new();
        let log = self.log.get_mut();
        let code = self
            .supervisor
            .run_once(&spec, &mut |_stream, line| {
                let _ = writeln!(log, "{line}");
                let _ = log.flush();
                if tail.len() == 10 {
                    tail.pop_front();
                }
                tail.push_back(line.to_owned());
            })
            .map_err(|error| BootFailure::new(error.to_string()))?;
        if code == 0 {
            Ok("ok".to_owned())
        } else {
            let mut message = format!("`{command}` exited with {code}");
            for line in tail {
                message.push_str("\n  ");
                message.push_str(&line);
            }
            Err(BootFailure::new(message))
        }
    }

    fn start_service(&mut self, step: &BootStep) -> Result<Staged, String> {
        let sigma = step.subject.clone();
        let built = self.built_for(step.image.as_ref())?;
        let rootfs = self.clone_rootfs(&built.rootfs, &format!("svc-{sigma}"))?;
        let port = self
            .services
            .iter()
            .find(|service| service.id == sigma)
            .and_then(|service| service.port);
        let (argv, display) = if let Some(command) = &step.command {
            (
                vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    shell_command(command),
                ],
                command.clone(),
            )
        } else if built.argv.is_empty() {
            return Err(format!(
                "service `{sigma}` declares no `run:` and its image has no `ENTRYPOINT`/`CMD`"
            ));
        } else {
            (built.argv.clone(), built.argv.join(" "))
        };
        let spec = PodSpec {
            id: sigma,
            rootfs,
            workdir: built.workdir,
            argv,
            inherited_env: built.inherited_env,
            env: self.pod_env_for(&step.subject, step.image.as_deref(), port),
            binds: vec![project_bind(&self.project)],
            files: Vec::new(),
        };
        self.supervisor
            .launch(&spec)
            .map_err(|error| error.to_string())?;
        let ready = match port {
            Some(port) => Ready::Service {
                pods_dir: self.supervisor.pods_dir().to_path_buf(),
                port,
                label: display,
                service: spec.id.clone(),
            },
            None => Ready::None,
        };
        Ok(Staged {
            state: BootState::Ready,
            detail: "started".to_owned(),
            ready,
            progress: None,
        })
    }

    fn start_resource(&mut self, id: &str) -> Result<Staged, String> {
        resources::start_resource(self, id)
    }

    /// The injected store layout (used to resolve sibling resource clones).
    pub(super) fn store_layout(&self) -> StoreLayout {
        self.layout.clone()
    }

    /// `<mount>/.slipfork-store/worlds/<world>`.
    pub(super) fn world_dir(&self) -> PathBuf {
        self.layout.world(&self.world)
    }

    /// Resolved rootfs subvolume per image id, recorded in the world stamp.
    pub(super) fn image_paths(&self) -> BTreeMap<String, String> {
        self.sealed
            .iter()
            .map(|(image, built)| (image.clone(), built.rootfs.display().to_string()))
            .collect()
    }

    pub(super) fn cleanup(&mut self) {
        for clone in std::mem::take(&mut self.clones) {
            let _ = self.engine.destroy(&clone, Dirtiness::Clean, true);
        }
        let _ = fs::remove_dir(self.layout.world(&self.world).join("pods"));
    }

    /// Writes one already-formatted progress line and flushes it; sink errors are not fatal.
    fn note(&mut self, line: String) {
        let log = self.log.get_mut();
        let _ = writeln!(log, "{line}");
        let _ = log.flush();
    }
}
