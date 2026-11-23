//! Boot-graph construction: node emission, dependency edges, and in-degree bookkeeping.

use std::collections::{BTreeMap, VecDeque};

use slipfork_forkfile::ast::Forkfile;
use slipfork_forkfile::boot::boot_plan_with;

use super::plan::{Parts, image_of, lookup};
use super::stamp::{StepStamp, stamp_key};
use super::{BootKind, BootStep, FailedHook, HookPhase, WorldPlan};
use crate::error::{Error, Result};
use crate::resources::ResourceKind;

/// Emits one [`BootStep`] per declaration and wires every dependency edge.
pub(super) fn finish(doc: &Forkfile, mut parts: Parts<'_>) -> Result<WorldPlan> {
    let mut image_steps = Vec::new();
    for image in &parts.images {
        parts.steps.push(BootStep {
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
        image_steps.push(parts.steps.len() - 1);
    }

    let mut resource_steps = Vec::new();
    let mut first_s3: Option<usize> = None;
    for resource in &parts.resources {
        let mut deps = Vec::new();
        if resource.resource == ResourceKind::S3 {
            // One native S3 server owns every bucket: the first bucket node configures the union
            // and later bucket rows wait for it, so a `ConfigureS3` never replaces another.
            match first_s3 {
                Some(first) => deps.push(first),
                None => first_s3 = Some(parts.steps.len()),
            }
        }
        parts.steps.push(BootStep {
            kind: BootKind::Resource,
            image: None,
            subject: resource.id.clone(),
            phase: None,
            label: format!("Resource {}", resource.id),
            command: None,
            stamp: None,
            uses_env: false,
            line: resource.line,
            deps,
        });
        resource_steps.push(parts.steps.len() - 1);
    }

    for node in boot_plan_with(doc, parts.root, parts.features).nodes {
        let image_idx = image_of(&parts.images, &image_steps, &node.image);
        let (phase, subject, line) = match lookup(&parts.hooks, &node) {
            Some((HookPhase::FailedDeploy, owner, image, line)) => {
                parts.failed_deploy.push(FailedHook {
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
        parts.steps.push(BootStep {
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

    let setup_range = (image_steps.len() + resource_steps.len())..parts.steps.len();
    let mut service_steps = Vec::new();
    for service in &parts.services {
        // `specs/03` §3 rule 2: a service waits for its image and its own hook edges only.
        // Resources stay readiness barriers for `:80` swap, `✓ Ready`, and `post-deploy`, never
        // for the pod spawn itself, so services boot in parallel with `pg`/`redis`.
        let mut deps = image_of(&parts.images, &image_steps, &service.image)
            .into_iter()
            .collect::<Vec<_>>();
        for index in setup_range.clone() {
            let step = &parts.steps[index];
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
        parts.steps.push(BootStep {
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
        service_steps.push(parts.steps.len() - 1);
    }

    let mut gated = BTreeMap::new();
    for index in setup_range.clone() {
        if parts.steps[index].phase != Some(HookPhase::PreDeploy) {
            continue;
        }
        let image = parts.steps[index].image.clone();
        let deps = image
            .as_deref()
            .and_then(|id| image_of(&parts.images, &image_steps, id))
            .into_iter()
            .collect::<Vec<_>>();
        let mut deps = deps;
        for candidate in setup_range.clone() {
            let step = &parts.steps[candidate];
            let before_start = matches!(step.phase, None | Some(HookPhase::BeforeStart));
            if before_start && step.image == image {
                deps.push(candidate);
            }
        }
        deps.sort_unstable();
        deps.dedup();
        parts.steps[index].deps = deps;
        // The resource barrier is conditional: it applies on a cold/stale epoch and is released
        // in 0ms when the `pre-deploy` stamp is current (the scheduler decides at release time).
        if !resource_steps.is_empty() {
            gated.insert(index, resource_steps.clone());
        }
    }

    for index in setup_range {
        if parts.steps[index].phase == Some(HookPhase::PostDeploy) {
            // `post-deploy` runs only once the whole world is observed ready: every resource and
            // every service.
            let mut deps = service_steps.clone();
            deps.extend(resource_steps.iter().copied());
            deps.sort_unstable();
            deps.dedup();
            parts.steps[index].deps = deps;
        }
    }

    Ok(WorldPlan {
        steps: parts.steps,
        failed_deploy: parts.failed_deploy,
        images: parts.images,
        resources: parts.resources,
        services: parts.services,
        gated,
    })
}

/// A validated acyclic boot graph: per-node edges plus in-degree bookkeeping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootGraph {
    deps: Vec<Vec<usize>>,
    gated: Vec<Vec<usize>>,
    downstream: Vec<Vec<usize>>,
}

impl BootGraph {
    /// Validates the unconditional edge set; convenience for plans without stamp-gated edges.
    pub fn new(steps: &[BootStep]) -> Result<Self> {
        Self::with_gated(steps, &BTreeMap::new())
    }

    /// Validates the edge set (unconditional **and** stamp-gated) and derives downstream
    /// adjacency; a cycle anywhere is refused here.
    pub fn with_gated(steps: &[BootStep], gated: &BTreeMap<usize, Vec<usize>>) -> Result<Self> {
        let mut downstream = vec![Vec::new(); steps.len()];
        let mut indegree = vec![0_usize; steps.len()];
        for (index, step) in steps.iter().enumerate() {
            for &dep in step
                .deps
                .iter()
                .chain(gated.get(&index).into_iter().flatten())
            {
                if dep >= steps.len() {
                    return Err(Error::BootCycle {
                        steps: vec![step.label.clone()],
                    });
                }
                // One truth for both cycle detection and the accessor: `dep` releases `index`.
                downstream[dep].push(index);
                indegree[index] += 1;
            }
        }
        let mut remaining = indegree;
        let mut ready: VecDeque<usize> = (0..steps.len())
            .filter(|&index| remaining[index] == 0)
            .collect();
        let mut released = 0;
        while let Some(node) = ready.pop_front() {
            released += 1;
            for &next in &downstream[node] {
                remaining[next] -= 1;
                if remaining[next] == 0 {
                    ready.push_back(next);
                }
            }
        }
        if released != steps.len() {
            let steps = (0..steps.len())
                .filter(|&index| remaining[index] > 0)
                .map(|index| steps[index].label.clone())
                .collect();
            return Err(Error::BootCycle { steps });
        }
        let mut resolved = vec![Vec::new(); steps.len()];
        for (&index, deps) in gated {
            if let Some(slot) = resolved.get_mut(index) {
                *slot = deps.clone();
            }
        }
        Ok(Self {
            deps: steps.iter().map(|step| step.deps.clone()).collect(),
            gated: resolved,
            downstream,
        })
    }

    /// The number of nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.deps.len()
    }

    /// Whether the graph has no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.deps.is_empty()
    }

    /// Upstream edges of `node`.
    #[must_use]
    pub fn deps(&self, node: usize) -> &[usize] {
        self.deps.get(node).map_or(&[], Vec::as_slice)
    }

    /// Stamp-gated upstream edges of `node`: awaited only while the node's stamp is stale.
    #[must_use]
    pub fn gated_deps(&self, node: usize) -> &[usize] {
        self.gated.get(node).map_or(&[], Vec::as_slice)
    }

    /// Nodes released when `node` completes.
    #[must_use]
    pub fn downstream(&self, node: usize) -> &[usize] {
        self.downstream.get(node).map_or(&[], Vec::as_slice)
    }
}
