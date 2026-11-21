//! The injected runtime that executes one boot step: images, resources, setup, hooks, services.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use slipfork_image::{
    Architecture, BuildArgs, BuildRequest, Credentials, ImageLayout, ImageReference,
    NativeImageBuilder, OciPuller, PulledImage, StepHost, StepLayout,
};
use slipfork_world::{
    Bind, BindSource, BootFailure, BootKind, BootStep, BtrfsEngine, Dirtiness, ImageNode, PodEnv,
    PodSpec, ResourceKind, S3Credentials, SeaweedfsPlan, ServiceNode, StepOutcome, StoreLayout,
    WorldEngine, WorldHost, WorldName, WorldPlan, WorldSupervisor, postgres, redis, seaweedfs,
};

use super::HostEnv;
use super::supervise::BootSink;

pub(super) struct WorldRuntime {
    engine: BtrfsEngine,
    layout: StoreLayout,
    world: WorldName,
    project: PathBuf,
    pub(super) supervisor: WorldSupervisor,
    puller: OciPuller,
    builder: NativeImageBuilder,
    images: Vec<ImageNode>,
    services: Vec<ServiceNode>,
    resources: Vec<slipfork_world::ResourceNode>,
    sealed: BTreeMap<String, PathBuf>,
    pulled: BTreeMap<String, PulledImage>,
    clones: Vec<PathBuf>,
    counter: usize,
    total_steps: usize,
    log: BootSink,
}

impl WorldRuntime {
    pub(super) fn new(
        project: &Path,
        world: WorldName,
        plan: &WorldPlan,
        env: &HostEnv,
        log: BootSink,
    ) -> Result<Self, String> {
        let engine = BtrfsEngine::new(StoreLayout::new(project));
        engine
            .adopt_project(project)
            .and_then(|()| engine.ensure_store())
            .map_err(|error| error.to_string())?;
        let layout = engine.layout().clone();
        let world_engine: Arc<dyn WorldEngine> = Arc::new(engine.clone());
        let architecture = Architecture::from_oci(env.arch)
            .ok_or_else(|| format!("unsupported host architecture `{}`", env.arch))?;
        let credentials = Credentials::Anonymous;
        let puller = OciPuller::new(
            Arc::clone(&world_engine),
            ImageLayout::new(layout.images_root()),
            credentials.clone(),
            architecture,
            format!("slipfork/{}", env!("CARGO_PKG_VERSION")),
        );
        let build_puller = OciPuller::new(
            Arc::clone(&world_engine),
            ImageLayout::new(layout.images_root()),
            credentials,
            architecture,
            format!("slipfork/{}", env!("CARGO_PKG_VERSION")),
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
        Ok(Self {
            engine,
            layout,
            world,
            project: project.to_path_buf(),
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
            log,
        })
    }

    /// Executes one ready step, tagging every live log line with the step that produced it.
    pub(super) fn execute(
        &mut self,
        index: usize,
        step: &BootStep,
    ) -> Result<StepOutcome, BootFailure> {
        self.log.focus(index + 1, self.total_steps, &step.label);
        let outcome = match step.kind {
            BootKind::Image => self
                .build_image(&step.subject)
                .map(|_| "built".to_owned())
                .map_err(BootFailure::new),
            BootKind::Resource => self.start_resource(&step.subject).map_err(BootFailure::new),
            BootKind::Setup | BootKind::Hook => self.run_once(step),
            BootKind::Service => self.start_service(step).map_err(BootFailure::new),
        };
        outcome.map(|detail| StepOutcome { detail })
    }

    fn build_image(&mut self, image: &str) -> Result<PathBuf, String> {
        if let Some(rootfs) = self.sealed.get(image) {
            return Ok(rootfs.clone());
        }
        let node = self
            .images
            .iter()
            .find(|candidate| candidate.id == image)
            .cloned()
            .ok_or_else(|| format!("unknown image `{image}`"))?;
        let rootfs = if let Some(reference) = &node.from {
            let parsed = ImageReference::parse(reference).map_err(|error| error.to_string())?;
            self.note(format!("pull {reference}"));
            self.puller
                .pull_into(&parsed, &mut self.log)
                .map_err(|error| error.to_string())?
                .rootfs
        } else {
            let context = self.project.join(&node.directory);
            let dockerfile = context.join(
                node.dockerfile
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("Dockerfile")),
            );
            let request = BuildRequest {
                context,
                dockerfile,
                args: BuildArgs::default(),
                target: None,
            };
            self.builder
                .build(&request, &mut self.log)
                .map_err(|error| error.to_string())?
                .rootfs
        };
        self.sealed.insert(image.to_owned(), rootfs.clone());
        Ok(rootfs)
    }

    fn clone_rootfs(&mut self, sealed: &Path, prefix: &str) -> Result<PathBuf, String> {
        self.counter += 1;
        let dir = self.layout.world(&self.world).join("pods");
        fs::create_dir_all(&dir).map_err(|error| format!("create `{}`: {error}", dir.display()))?;
        let dest = dir.join(format!("{prefix}-{}", self.counter));
        self.engine
            .clone_subvolume(sealed, &dest)
            .map_err(|error| error.to_string())?;
        self.clones.push(dest.clone());
        Ok(dest)
    }

    fn image_rootfs(&mut self, image: Option<&String>) -> Result<PathBuf, String> {
        let image = image.ok_or_else(|| "step has no enclosing image".to_owned())?;
        self.build_image(image)
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
        let sealed = self
            .image_rootfs(step.image.as_ref())
            .map_err(BootFailure::new)?;
        let rootfs = self
            .clone_rootfs(&sealed, &format!("boot{}", step.line))
            .map_err(BootFailure::new)?;
        let spec = PodSpec {
            id: format!("boot-{}", self.counter),
            rootfs,
            workdir: "/project".to_owned(),
            argv: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                shell_command(&command),
            ],
            inherited_env: Vec::new(),
            env: Vec::new(),
            binds: vec![project_bind(&self.project)],
            files: Vec::new(),
        };
        // Setup and hook pods run to completion with their pipes attached, so a failing
        // command's own diagnostics stream live and ride the failure message.
        let mut tail: VecDeque<String> = VecDeque::new();
        let log = &mut self.log;
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

    fn start_service(&mut self, step: &BootStep) -> Result<String, String> {
        let sigma = step.subject.clone();
        let sealed = self.image_rootfs(step.image.as_ref())?;
        let rootfs = self.clone_rootfs(&sealed, &format!("svc-{sigma}"))?;
        let command = step.command.clone().unwrap_or_else(|| "true".to_owned());
        let mut env = Vec::new();
        if let Some(port) = self
            .services
            .iter()
            .find(|service| service.id == sigma)
            .and_then(|service| service.port)
        {
            env.push(PodEnv::new("PORT", &port.to_string()));
        }
        let spec = PodSpec {
            id: sigma,
            rootfs,
            workdir: "/project".to_owned(),
            argv: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                shell_command(&command),
            ],
            inherited_env: Vec::new(),
            env,
            binds: vec![project_bind(&self.project)],
            files: Vec::new(),
        };
        let pid = self
            .supervisor
            .launch(&spec)
            .map_err(|error| error.to_string())?;
        // A service that leaves inside the readiness window never started: surface it as failed
        // while the world stays up for inspection. A survivor is reported as running.
        let health = slipfork_world::classify_service(
            self.supervisor
                .wait(pid, Duration::from_millis(750))
                .map_err(|error| error.to_string())?,
        );
        match health {
            slipfork_world::ServiceHealth::Running => Ok("started".to_owned()),
            slipfork_world::ServiceHealth::Exited(code) => Err(format!(
                "`{command}` exited with {code} before it became ready"
            )),
        }
    }

    fn start_resource(&mut self, id: &str) -> Result<String, String> {
        let node = self
            .resources
            .iter()
            .find(|candidate| candidate.id == id)
            .cloned()
            .ok_or_else(|| format!("unknown resource `{id}`"))?;
        let reference = node.resource.image();
        let image = if let Some(image) = self.pulled.get(reference) {
            image.clone()
        } else {
            let parsed = ImageReference::parse(reference).map_err(|error| error.to_string())?;
            self.note(format!("pull {reference}"));
            let image = self
                .puller
                .pull_into(&parsed, &mut self.log)
                .map_err(|error| error.to_string())?;
            self.pulled.insert(reference.to_owned(), image.clone());
            image
        };
        let rootfs = self.clone_rootfs(&image.rootfs, &format!("res-{id}"))?;
        let facts = image.facts.clone();
        let data = self.layout.world(&self.world).join(id);
        if !data.exists() {
            self.engine
                .create_subvolume(&data)
                .map_err(|error| error.to_string())?;
        }
        let spec = match node.resource {
            ResourceKind::Postgres => postgres(id, &rootfs, &data, &facts),
            ResourceKind::Redis => redis(id, &rootfs, &data, &facts),
            ResourceKind::Seaweedfs => {
                let plan = SeaweedfsPlan::render(
                    &S3Credentials::new(&format!("slipfork-{id}"), &format!("slipfork-{id}")),
                    &[],
                )
                .map_err(|error| error.to_string())?;
                seaweedfs(id, &rootfs, &data, &plan, &facts)
            }
        };
        self.supervisor
            .launch(&spec)
            .map_err(|error| error.to_string())?;
        Ok("started".to_owned())
    }

    pub(super) fn cleanup(&mut self) {
        for clone in std::mem::take(&mut self.clones) {
            let _ = self.engine.destroy(&clone, Dirtiness::Clean, true);
        }
    }

    /// Writes one already-formatted progress line and flushes it; sink errors are not fatal.
    fn note(&mut self, line: String) {
        let _ = writeln!(self.log, "{line}");
        let _ = self.log.flush();
    }
}

fn project_bind(project: &Path) -> Bind {
    Bind {
        source: BindSource::Host(project.to_path_buf()),
        destination: "/project".to_owned(),
        read_only: false,
    }
}

fn shell_command(command: &str) -> String {
    command.strip_prefix('@').unwrap_or(command).to_owned()
}
