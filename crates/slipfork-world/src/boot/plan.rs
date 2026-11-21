//! Pure world-boot planning: node collection, phase ordering, and dependency indexes.

use std::path::{Path, PathBuf};

use slipfork_forkfile::ast::*;
use slipfork_forkfile::boot::{SetupNode, boot_plan};
use slipfork_forkfile::{Command, CommandPart, FeatureSet, each_enabled};

use super::stamp::{StepStamp, stamp_key};
use super::{
    BootKind, BootStep, FailedHook, HookPhase, ImageNode, ResourceNode, ServiceNode, WorldPlan,
};
use crate::resources::{ResourceKind, S3Bucket, S3Policy};

/// Builds the world boot plan from a checked `Forkfile` and its project root.
pub fn world_plan(doc: &Forkfile, root: &Path) -> crate::Result<WorldPlan> {
    let features = doc.active_features();
    let mut builder = PlanBuilder {
        features: &features,
        error: None,
        root,
        images: Vec::new(),
        resources: Vec::new(),
        services: Vec::new(),
        hooks: Vec::new(),
        steps: Vec::new(),
        failed_deploy: Vec::new(),
    };
    each_enabled(&doc.items.items, &features, &mut |item| {
        builder.top(item);
    });
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
    features: &'a FeatureSet,
    error: Option<crate::Error>,
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
            TopItem::Postgres(postgres) => {
                let mut databases = Vec::new();
                collect_pg_databases(&postgres.body.items, self.features, &mut databases);
                self.resources.push(ResourceNode {
                    id: postgres.id.clone(),
                    resource: ResourceKind::Postgres,
                    databases,
                    buckets: Vec::new(),
                    line: postgres.span.line(),
                });
            }
            TopItem::Redis(redis) => self.resources.push(ResourceNode {
                id: redis.id.clone(),
                resource: ResourceKind::Redis,
                databases: Vec::new(),
                buckets: Vec::new(),
                line: redis.span.line(),
            }),
            TopItem::S3(s3) => {
                let policy = if has_cdn(&s3.body.items, self.features) {
                    S3Policy::PublicRead
                } else {
                    S3Policy::Private
                };
                let bucket = match S3Bucket::new(&s3.bucket, policy) {
                    Ok(bucket) => bucket,
                    Err(error) => {
                        self.error.get_or_insert(error);
                        return;
                    }
                };
                if let Some(existing) = self
                    .resources
                    .iter_mut()
                    .find(|node| node.resource == ResourceKind::Seaweedfs)
                {
                    existing.buckets.push(bucket);
                } else {
                    self.resources.push(ResourceNode {
                        id: s3.bucket.clone(),
                        resource: ResourceKind::Seaweedfs,
                        databases: Vec::new(),
                        buckets: vec![bucket],
                        line: s3.span.line(),
                    });
                }
            }
            _ => {}
        }
    }

    fn image_items(&mut self, image: &str, items: &[ImageItem]) {
        let features = self.features;
        each_enabled(items, features, &mut |item| match item {
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
            _ => {}
        });
    }

    fn service_items(&mut self, image: &str, service: &str, items: &[ServiceItem]) {
        let features = self.features;
        each_enabled(items, features, &mut |item| match item {
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
            _ => {}
        });
    }

    fn service(&mut self, image: &str, service: &Service) {
        let mut port = None;
        let mut internal_ports = Vec::new();
        let mut raw_run: Option<&Hook> = None;
        collect_service_run(
            &service.body.items,
            self.features,
            &mut port,
            &mut internal_ports,
            &mut raw_run,
        );
        let command = raw_run.and_then(|hook| render_hook(hook, port));
        self.services.push(ServiceNode {
            id: service.id.clone(),
            image: image.to_owned(),
            port,
            internal_ports,
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

    fn finish(mut self, doc: &Forkfile) -> crate::Result<WorldPlan> {
        if let Some(error) = self.error {
            return Err(error);
        }
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

        for node in boot_plan(doc, self.root).nodes {
            let image_idx = image_of(&self.images, &image_steps, &node.image);
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
                uses_env: node.uses_env,
                line,
                deps: image_idx.into_iter().collect(),
            });
        }

        let setup_range = (image_steps.len() + resource_steps.len())..self.steps.len();
        let mut service_steps = Vec::new();
        for service in &self.services {
            let mut deps = image_of(&self.images, &image_steps, &service.image)
                .into_iter()
                .chain(resource_steps.iter().copied())
                .collect::<Vec<_>>();
            for index in setup_range.clone() {
                let step = &self.steps[index];
                if step.image.as_deref() != Some(service.image.as_str()) {
                    continue;
                }
                match step.phase {
                    None | Some(HookPhase::BeforeStart) => deps.push(index),
                    Some(HookPhase::PreDeploy)
                        if step.subject == service.id || step.subject == service.image =>
                    {
                        deps.push(index);
                    }
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

        for index in setup_range.clone() {
            if self.steps[index].phase != Some(HookPhase::PreDeploy) {
                continue;
            }
            let image = self.steps[index].image.clone();
            let mut deps = image
                .as_deref()
                .and_then(|id| image_of(&self.images, &image_steps, id))
                .into_iter()
                .chain(resource_steps.iter().copied())
                .collect::<Vec<_>>();
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

        for index in setup_range {
            if self.steps[index].phase == Some(HookPhase::PostDeploy) {
                self.steps[index].deps = service_steps.clone();
            }
        }

        Ok(WorldPlan {
            steps: self.steps,
            failed_deploy: self.failed_deploy,
            images: self.images,
            resources: self.resources,
            services: self.services,
        })
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

fn collect_pg_databases(items: &[PostgresItem], features: &FeatureSet, out: &mut Vec<String>) {
    each_enabled(items, features, &mut |item| {
        if let PostgresItem::Database(db) = item {
            out.push(db.id.clone());
        }
    });
}

fn has_cdn(items: &[S3Item], features: &FeatureSet) -> bool {
    let mut found = false;
    each_enabled(items, features, &mut |item| {
        if matches!(item, S3Item::Cdn(_)) {
            found = true;
        }
    });
    found
}

fn collect_service_run<'a>(
    items: &'a [ServiceItem],
    features: &FeatureSet,
    port: &mut Option<u16>,
    internal_ports: &mut Vec<u16>,
    run: &mut Option<&'a Hook>,
) {
    each_enabled(items, features, &mut |item| match item {
        ServiceItem::Port(value) => *port = Some(*value),
        ServiceItem::InternalPorts(ports) => internal_ports.extend(ports.iter().copied()),
        ServiceItem::Run(hook) => *run = Some(hook),
        _ => {}
    });
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
