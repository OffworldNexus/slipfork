//! Boot-graph construction: node emission, dependency edges, and in-degree bookkeeping.

use std::collections::VecDeque;

use slipfork_forkfile::ast::Forkfile;
use slipfork_forkfile::boot::boot_plan;

use super::plan::{Parts, image_of, lookup};
use super::stamp::{StepStamp, stamp_key};
use super::{BootKind, BootStep, FailedHook, HookPhase, WorldPlan};
use crate::error::{Error, Result};

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
    for resource in &parts.resources {
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
            deps: Vec::new(),
        });
        resource_steps.push(parts.steps.len() - 1);
    }

    for node in boot_plan(doc, parts.root).nodes {
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
        let mut deps = image_of(&parts.images, &image_steps, &service.image)
            .into_iter()
            .chain(resource_steps.iter().copied())
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

    for index in setup_range.clone() {
        if parts.steps[index].phase != Some(HookPhase::PreDeploy) {
            continue;
        }
        let image = parts.steps[index].image.clone();
        let mut deps = image
            .as_deref()
            .and_then(|id| image_of(&parts.images, &image_steps, id))
            .into_iter()
            .chain(resource_steps.iter().copied())
            .collect::<Vec<_>>();
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
    }

    for index in setup_range {
        if parts.steps[index].phase == Some(HookPhase::PostDeploy) {
            parts.steps[index].deps = service_steps.clone();
        }
    }

    Ok(WorldPlan {
        steps: parts.steps,
        failed_deploy: parts.failed_deploy,
        images: parts.images,
        resources: parts.resources,
        services: parts.services,
    })
}

/// A validated acyclic boot graph: per-node edges plus in-degree bookkeeping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootGraph {
    deps: Vec<Vec<usize>>,
    downstream: Vec<Vec<usize>>,
}

impl BootGraph {
    /// Validates the edge set and derives downstream adjacency; a cycle is refused here.
    pub fn new(steps: &[BootStep]) -> Result<Self> {
        let mut downstream = vec![Vec::new(); steps.len()];
        let mut indegree = vec![0_usize; steps.len()];
        for (index, step) in steps.iter().enumerate() {
            for &dep in &step.deps {
                let Some(slot) = downstream.get_mut(dep) else {
                    return Err(Error::BootCycle {
                        steps: vec![step.label.clone()],
                    });
                };
                slot.push(index);
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
        Ok(Self {
            deps: steps.iter().map(|step| step.deps.clone()).collect(),
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

    /// Nodes released when `node` completes.
    #[must_use]
    pub fn downstream(&self, node: usize) -> &[usize] {
        self.downstream.get(node).map_or(&[], Vec::as_slice)
    }
}
