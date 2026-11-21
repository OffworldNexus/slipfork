//! Declaration walking: collects every `image`, resource, hook, and service into plain values.
//!
//! Node emission, edge wiring, and in-degree bookkeeping live in [`super::graph`]; this module
//! only consumes the checked syntax tree so the plan stays a pure data transform.
use super::graph;
use super::{BootStep, FailedHook, HookPhase, ImageNode, ResourceNode, ServiceNode, WorldPlan};
use crate::resources::{ResourceKind, S3Bucket};
use slipfork_forkfile::ast::*;
use slipfork_forkfile::boot::SetupNode;
use slipfork_forkfile::{Command, CommandPart, FeatureSet, each_enabled};
use std::path::{Path, PathBuf};
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
    };
    each_enabled(&doc.items.items, &features, &mut |item| {
        builder.top(item);
    });
    builder.finish(doc)
}
/// Declarations collected by [`PlanBuilder`], handed to [`graph::finish`].
pub(super) struct Parts<'a> {
    pub(super) root: &'a Path,
    pub(super) images: Vec<ImageNode>,
    pub(super) resources: Vec<ResourceNode>,
    pub(super) services: Vec<ServiceNode>,
    pub(super) hooks: Vec<HookRecord>,
    pub(super) steps: Vec<BootStep>,
    pub(super) failed_deploy: Vec<FailedHook>,
}
/// One declared hook command, kept for image/phase attribution during edge wiring.
pub(super) struct HookRecord {
    pub(super) image: String,
    pub(super) owner: String,
    pub(super) phase: HookPhase,
    pub(super) command: String,
    pub(super) line: u32,
}
struct PlanBuilder<'a> {
    features: &'a FeatureSet,
    error: Option<crate::Error>,
    root: &'a Path,
    images: Vec<ImageNode>,
    resources: Vec<ResourceNode>,
    services: Vec<ServiceNode>,
    hooks: Vec<HookRecord>,
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
                let policy = s3.policy_at(self.features.active());
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
                    .find(|node| node.resource == ResourceKind::S3)
                {
                    existing.buckets.push(bucket);
                } else {
                    self.resources.push(ResourceNode {
                        id: s3.bucket.clone(),
                        resource: ResourceKind::S3,
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
    fn finish(self, doc: &Forkfile) -> crate::Result<WorldPlan> {
        if let Some(error) = self.error {
            return Err(error);
        }
        graph::finish(
            doc,
            Parts {
                root: self.root,
                images: self.images,
                resources: self.resources,
                services: self.services,
                hooks: self.hooks,
                steps: Vec::new(),
                failed_deploy: Vec::new(),
            },
        )
    }
}
/// Matches one deduplicated setup node back to its declared hook command.
pub(super) fn lookup(
    hooks: &[HookRecord],
    node: &SetupNode,
) -> Option<(HookPhase, String, String, u32)> {
    hooks
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
/// The step index of `image`, when the plan declares it.
pub(super) fn image_of(images: &[ImageNode], indexes: &[usize], image: &str) -> Option<usize> {
    images
        .iter()
        .position(|candidate| candidate.id == image)
        .and_then(|position| indexes.get(position).copied())
}
fn collect_pg_databases(items: &[PostgresItem], features: &FeatureSet, out: &mut Vec<String>) {
    each_enabled(items, features, &mut |item| {
        if let PostgresItem::Database(db) = item {
            out.push(db.id.clone());
        }
    });
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
