//! Pure world-boot planning: node collection, phase ordering, and dependency indexes.

use std::path::{Path, PathBuf};

use slipfork_forkfile::ast::*;
use slipfork_forkfile::boot::{SetupNode, boot_plan};
use slipfork_forkfile::{Command, CommandPart};

use super::stamp::{StepStamp, stamp_key};
use super::{
    BootKind, BootStep, FailedHook, HookPhase, ImageNode, ResourceNode, ServiceNode, WorldPlan,
};
use crate::resources::ResourceKind;

/// Builds the world boot plan from a checked `Forkfile` and its project root.
#[must_use]
pub fn world_plan(doc: &Forkfile, root: &Path) -> WorldPlan {
    let mut builder = PlanBuilder {
        root,
        images: Vec::new(),
        resources: Vec::new(),
        services: Vec::new(),
        hooks: Vec::new(),
        steps: Vec::new(),
        failed_deploy: Vec::new(),
    };
    for item in &doc.items.items {
        builder.top(item);
    }
    builder.finish(doc)
}

struct HookRecord {
    image: String,
    owner: String,
    phase: HookPhase,
    command: String,
    line: u32,
}

struct PlanBuilder<'a> {
    root: &'a Path,
    images: Vec<ImageNode>,
    resources: Vec<ResourceNode>,
    services: Vec<ServiceNode>,
    hooks: Vec<HookRecord>,
    steps: Vec<BootStep>,
    failed_deploy: Vec<FailedHook>,
}

impl PlanBuilder<'_> {
    fn top(&mut self, item: &TopItem) {
        match item {
            TopItem::Image(image) => {
                self.images.push(ImageNode {
                    id: image.id.clone(),
                    directory: image_directory(image),
                    dockerfile: image_dockerfile(image),
                    from: match &image.source {
                        ImageSource::From(reference) => Some(reference.clone()),
                        ImageSource::Dir(_) => None,
                    },
                    line: image.span.line(),
                });
                self.image_items(&image.id, &image.body.items);
            }
            TopItem::Postgres(postgres) => self.resources.push(ResourceNode {
                id: postgres.id.clone(),
                resource: ResourceKind::Postgres,
                line: postgres.span.line(),
            }),
            TopItem::Redis(redis) => self.resources.push(ResourceNode {
                id: redis.id.clone(),
                resource: ResourceKind::Redis,
                line: redis.span.line(),
            }),
            TopItem::S3(s3) => self.resources.push(ResourceNode {
                id: s3.bucket.clone(),
                resource: ResourceKind::Seaweedfs,
                line: s3.span.line(),
            }),
            TopItem::On(on) => match &on.body {
                OnBody::Inline(inner) => self.top(inner),
                OnBody::Block(block) => {
                    for item in &block.items {
                        self.top(item);
                    }
                }
            },
            _ => {}
        }
    }

    fn image_items(&mut self, image: &str, items: &[ImageItem]) {
        for item in items {
            match item {
                ImageItem::BeforeStart(hook) => {
                    self.hook(image, image, HookPhase::BeforeStart, hook);
                }
                ImageItem::PreDeploy(hook) => {
                    self.hook(image, image, HookPhase::PreDeploy, hook);
                }
                ImageItem::PostDeploy(hook) => {
                    self.hook(image, image, HookPhase::PostDeploy, hook);
                }
                ImageItem::FailedDeploy(hook) => {
                    self.hook(image, image, HookPhase::FailedDeploy, hook);
                }
                ImageItem::Service(service) => self.service(image, service),
                ImageItem::On(on) => match &on.body {
                    OnBody::Inline(inner) => {
                        self.image_items(image, std::slice::from_ref(inner.as_ref()));
                    }
                    OnBody::Block(block) => self.image_items(image, &block.items),
                },
                _ => {}
            }
        }
    }

    fn service_items(&mut self, image: &str, service: &str, items: &[ServiceItem]) {
        for item in items {
            match item {
                ServiceItem::BeforeStart(hook) => {
                    self.hook(image, service, HookPhase::BeforeStart, hook);
                }
                ServiceItem::PreDeploy(hook) => {
                    self.hook(image, service, HookPhase::PreDeploy, hook);
                }
                ServiceItem::PostDeploy(hook) => {
                    self.hook(image, service, HookPhase::PostDeploy, hook);
                }
                ServiceItem::FailedDeploy(hook) => {
                    self.hook(image, service, HookPhase::FailedDeploy, hook);
                }
                ServiceItem::On(on) => match &on.body {
                    OnBody::Inline(inner) => {
                        self.service_items(image, service, std::slice::from_ref(inner.as_ref()));
                    }
                    OnBody::Block(block) => self.service_items(image, service, &block.items),
                },
                _ => {}
            }
        }
    }

    fn service(&mut self, image: &str, service: &Service) {
        let mut port = None;
        let mut command = None;
        for item in &service.body.items {
            match item {
                ServiceItem::Port(value) => port = Some(*value),
                ServiceItem::Run(hook) => command = render_hook(hook, port),
                _ => {}
            }
        }
        self.services.push(ServiceNode {
            id: service.id.clone(),
            image: image.to_owned(),
            port,
            command,
            line: service.span.line(),
        });
        self.service_items(image, &service.id, &service.body.items);
    }

    fn hook(&mut self, image: &str, owner: &str, phase: HookPhase, hook: &Hook) {
        for command in hook_commands(hook) {
            self.hooks.push(HookRecord {
                image: image.to_owned(),
                owner: owner.to_owned(),
                phase,
                command: command.text(),
                line: hook.span.line(),
            });
        }
    }

    fn finish(mut self, doc: &Forkfile) -> WorldPlan {
        let mut image_steps = Vec::new();
        for image in &self.images {
            self.steps.push(BootStep {
                kind: BootKind::Image,
                image: Some(image.id.clone()),
                subject: image.id.clone(),
                phase: None,
                label: format!("Image {}", image.id),
                command: None,
                stamp: None,
                uses_env: false,
                line: image.line,
                deps: Vec::new(),
            });
            image_steps.push(self.steps.len() - 1);
        }

        let mut resource_steps = Vec::new();
        for resource in &self.resources {
            self.steps.push(BootStep {
                kind: BootKind::Resource,
                image: None,
                subject: resource.id.clone(),
                phase: None,
                label: format!("Resource {}", resource.id),
                command: None,
                stamp: None,
                uses_env: false,
                line: resource.line,
                deps: Vec::new(),
            });
            resource_steps.push(self.steps.len() - 1);
        }

        // The canonical deduplicated setup list comes from `boot_plan`; the AST walk only
        // supplies the phase and owner for each node it already produced.
        for node in boot_plan(doc, self.root).nodes {
            let (phase, subject, line) = match self.lookup(&node) {
                Some((HookPhase::FailedDeploy, owner, image, line)) => {
                    self.failed_deploy.push(FailedHook {
                        owner,
                        image,
                        command: node.command.clone(),
                        line,
                    });
                    continue;
                }
                Some((phase, owner, _image, line)) => (Some(phase), owner, line),
                None => (None, node.image.clone(), 1),
            };
            let conditional = phase.is_none_or(|phase| phase == HookPhase::BeforeStart)
                && (node.tool.is_some() || !node.patterns.is_empty());
            let image = image_of(&self.images, &image_steps, &node.image);
            let label = match phase {
                Some(HookPhase::BeforeStart) => format!("Hook {subject}:before-start"),
                Some(phase) => format!("Hook {subject}:{}", phase.as_str()),
                None => format!("Setup {}: {}", node.image, node.command),
            };
            self.steps.push(BootStep {
                kind: match phase {
                    Some(HookPhase::BeforeStart) | None => BootKind::Setup,
                    Some(_) => BootKind::Hook,
                },
                image: Some(node.image.clone()),
                subject,
                phase,
                label,
                command: Some(node.command.clone()),
                stamp: conditional.then(|| StepStamp {
                    key: stamp_key(&node),
                    digest: node.stamp.clone(),
                }),
                uses_env: node.command.contains("$env."),
                line,
                deps: image.into_iter().collect(),
            });
        }

        let setup_range = (image_steps.len() + resource_steps.len())..self.steps.len();

        // Services depend on this image's before-start/pre-deploy steps and every resource.
        let mut service_steps = Vec::new();
        for service in &self.services {
            let mut deps = resource_steps.clone();
            for index in setup_range.clone() {
                let step = &self.steps[index];
                if step.image.as_deref() != Some(service.image.as_str()) {
                    continue;
                }
                match step.phase {
                    None | Some(HookPhase::BeforeStart) => deps.push(index),
                    Some(HookPhase::PreDeploy) if step.subject == service.id => deps.push(index),
                    Some(HookPhase::PreDeploy) if step.subject == service.image => deps.push(index),
                    _ => {}
                }
            }
            deps.sort_unstable();
            deps.dedup();
            self.steps.push(BootStep {
                kind: BootKind::Service,
                image: Some(service.image.clone()),
                subject: service.id.clone(),
                phase: None,
                label: format!("Service {}", service.id),
                command: service.command.clone(),
                stamp: None,
                uses_env: false,
                line: service.line,
                deps,
            });
            service_steps.push(self.steps.len() - 1);
        }

        // Pre-deploy hooks need their image's setup nodes plus every resource.
        for index in setup_range.clone() {
            if self.steps[index].phase != Some(HookPhase::PreDeploy) {
                continue;
            }
            let image = self.steps[index].image.clone();
            let mut deps = resource_steps.clone();
            for candidate in setup_range.clone() {
                let step = &self.steps[candidate];
                let before_start = matches!(step.phase, None | Some(HookPhase::BeforeStart));
                if before_start && step.image == image {
                    deps.push(candidate);
                }
            }
            deps.sort_unstable();
            deps.dedup();
            self.steps[index].deps = deps;
        }

        // Post-deploy hooks run only after every service is up.
        for index in setup_range {
            if self.steps[index].phase == Some(HookPhase::PostDeploy) {
                self.steps[index].deps = service_steps.clone();
            }
        }

        WorldPlan {
            steps: self.steps,
            failed_deploy: self.failed_deploy,
            images: self.images,
            resources: self.resources,
            services: self.services,
        }
    }

    fn lookup(&self, node: &SetupNode) -> Option<(HookPhase, String, String, u32)> {
        self.hooks
            .iter()
            .find(|hook| hook.image == node.image && hook.command == node.command)
            .map(|hook| {
                (
                    hook.phase,
                    hook.owner.clone(),
                    hook.image.clone(),
                    hook.line,
                )
            })
    }
}

fn image_of(images: &[ImageNode], indexes: &[usize], image: &str) -> Option<usize> {
    images
        .iter()
        .position(|candidate| candidate.id == image)
        .and_then(|position| indexes.get(position).copied())
}

fn image_directory(image: &Image) -> PathBuf {
    match &image.source {
        ImageSource::Dir(directory) => PathBuf::from(directory),
        ImageSource::From(_) => PathBuf::from("."),
    }
}

fn image_dockerfile(image: &Image) -> Option<PathBuf> {
    image.body.items.iter().find_map(|item| match item {
        ImageItem::Dockerfile(path) => Some(PathBuf::from(path)),
        _ => None,
    })
}

fn hook_commands(hook: &Hook) -> Vec<&Command> {
    match &hook.body {
        HookBody::Inline(command) => vec![command],
        HookBody::Block(block) => block.items.iter().collect(),
    }
}

/// Renders a `run:` command, resolving `$port` from the service's declared port.
fn render_hook(hook: &Hook, port: Option<u16>) -> Option<String> {
    let command = match &hook.body {
        HookBody::Inline(command) => command,
        HookBody::Block(block) => block.items.last()?,
    };
    let mut out = String::new();
    if let Some(tool) = &command.tool {
        out.push('@');
        out.push_str(&tool.name);
        if !command.parts.is_empty() {
            out.push(' ');
        }
    }
    for part in &command.parts {
        match part {
            CommandPart::Text(text) => out.push_str(text),
            CommandPart::Port => out.push_str(&port.unwrap_or_default().to_string()),
            CommandPart::Env { key, .. } => {
                out.push_str("$env.");
                out.push_str(key);
            }
        }
    }
    Some(out)
}
