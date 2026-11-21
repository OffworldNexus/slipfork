//! One parallel world boot DAG: images, resources, deduplicated setup, hooks, services.
//!
//! Planning is pure data: [`world_plan`] consumes `slipfork_forkfile::boot_plan` for the
//! deduplicated `@<tool>` / `when changed(...)` nodes and walks the syntax tree only for phase
//! ordering. [`run_plan`] schedules ready nodes on `std::thread::scope` workers, skips stamped
//! setup steps, persists `<project>/.slipfork/<key>.stamp` after success, and streams typed
//! [`BootEvent`]s into an injected sink. Nothing here reads the environment or caches a global.

mod plan;
mod schedule;
mod stamp;

pub use plan::world_plan;
pub use schedule::{
    BootEvent, BootFailure, EventSink, ServiceHealth, StepExecutor, StepOutcome, classify_service,
    run_plan,
};
pub use stamp::{StampStore, StepStamp};

use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::resources::{ResourceKind, S3Bucket};

/// The kind of work one boot node performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootKind {
    /// Build or pull one `image`.
    Image,
    /// Start one backing resource pod (`postgres`, `redis`, `s3`).
    Resource,
    /// One deduplicated `@<tool>` / `when changed(...)` / `before-start` step.
    Setup,
    /// A `pre-deploy` or `post-deploy` hook.
    Hook,
    /// One long-running `service` pod.
    Service,
}

/// The lifecycle phase of a hook node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPhase {
    /// Runs right after its image; may be stamped by `when changed(...)`.
    BeforeStart,
    /// Runs after setups and resources, before services.
    PreDeploy,
    /// Runs after every service is up.
    PostDeploy,
    /// Runs only when another node fails.
    FailedDeploy,
}

impl HookPhase {
    /// The `Forkfile` keyword.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BeforeStart => "before-start",
            Self::PreDeploy => "pre-deploy",
            Self::PostDeploy => "post-deploy",
            Self::FailedDeploy => "failed-deploy",
        }
    }
}

/// One `image` declaration and where its rootfs comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageNode {
    /// Image id.
    pub id: String,
    /// Build context, relative to the project root.
    pub directory: PathBuf,
    /// Optional `dockerfile <path>`, relative to `directory`.
    pub dockerfile: Option<PathBuf>,
    /// The `from <ref>` OCI reference instead of a build context.
    pub from: Option<String>,
    /// `Forkfile` line of the `image` keyword.
    pub line: u32,
}

/// One backing-resource declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceNode {
    /// Resource id.
    pub id: String,
    /// Which real backend to start.
    pub resource: ResourceKind,
    /// Declared PostgreSQL child databases.
    pub databases: Vec<String>,
    /// Coalesced S3 buckets with private vs `public-read` policies.
    pub buckets: Vec<S3Bucket>,
    /// `Forkfile` line of the declaration.
    pub line: u32,
}

/// One service declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceNode {
    /// Service id (also its pod id).
    pub id: String,
    /// Enclosing image id.
    pub image: String,
    /// Public `port`, when declared.
    pub port: Option<u16>,
    /// Extra world-private `internal_ports`, when declared.
    pub internal_ports: Vec<u16>,
    /// The `run:` command with `$port` resolved, when declared.
    pub command: Option<String>,
    /// `Forkfile` line of the `service` keyword.
    pub line: u32,
}

/// A `failed-deploy` hook kept aside: it runs only when the boot actually fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedHook {
    /// Image or service that owns the hook.
    pub owner: String,
    /// Enclosing image id.
    pub image: String,
    /// The rendered command.
    pub command: String,
    /// `Forkfile` line.
    pub line: u32,
}

/// One node in the world boot DAG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootStep {
    /// What kind of work this node performs.
    pub kind: BootKind,
    /// Enclosing image id, when one applies.
    pub image: Option<String>,
    /// Image or service id the node belongs to.
    pub subject: String,
    /// Hook phase, when this node is a hook.
    pub phase: Option<HookPhase>,
    /// One-line `✓` label with no ANSI styling.
    pub label: String,
    /// Shell command for setup, hook, and service nodes.
    pub command: Option<String>,
    /// Conditional stamp for `@<tool>` / `when changed(...)` steps.
    pub stamp: Option<StepStamp>,
    /// The command references `$env.KEY`, which needs the daemon's env resolver.
    pub uses_env: bool,
    /// `Forkfile` line used by the `error:`/`next:` output.
    pub line: u32,
    /// Indexes of nodes that must finish first.
    pub deps: Vec<usize>,
}

/// The whole world boot plan, flattened into dependency-indexed steps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorldPlan {
    /// Nodes in declaration order.
    pub steps: Vec<BootStep>,
    /// Hooks that run only after a failure.
    pub failed_deploy: Vec<FailedHook>,
    /// Image declarations in declaration order.
    pub images: Vec<ImageNode>,
    /// Resource declarations in declaration order.
    pub resources: Vec<ResourceNode>,
    /// Service declarations in declaration order.
    pub services: Vec<ServiceNode>,
}

impl WorldPlan {
    /// The number of scheduled steps.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Whether the document declares no images, resources, hooks, or services.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Every `port` and `internal_ports` entry claimed by application services in this world.
    #[must_use]
    pub fn reserved_ports(&self) -> Vec<u16> {
        let mut ports: Vec<u16> = self
            .services
            .iter()
            .flat_map(|svc| {
                svc.port
                    .into_iter()
                    .chain(svc.internal_ports.iter().copied())
            })
            .collect();
        ports.sort_unstable();
        ports.dedup();
        ports
    }

    /// Groups steps into parallel waves; a dependency cycle is refused here.
    pub fn waves(&self) -> Result<Vec<Vec<usize>>> {
        let mut remaining = vec![0_usize; self.steps.len()];
        let mut downstream: Vec<Vec<usize>> = vec![Vec::new(); self.steps.len()];
        for (index, step) in self.steps.iter().enumerate() {
            remaining[index] = step.deps.len();
            for &dep in &step.deps {
                let Some(slot) = downstream.get_mut(dep) else {
                    return Err(Error::BootCycle {
                        steps: vec![step.label.clone()],
                    });
                };
                slot.push(index);
            }
        }
        let mut done = vec![false; self.steps.len()];
        let mut waves = Vec::new();
        let mut emitted = 0;
        while emitted < self.steps.len() {
            let wave: Vec<usize> = (0..self.steps.len())
                .filter(|&index| !done[index] && remaining[index] == 0)
                .collect();
            if wave.is_empty() {
                let steps = (0..self.steps.len())
                    .filter(|&index| !done[index])
                    .map(|index| self.steps[index].label.clone())
                    .collect();
                return Err(Error::BootCycle { steps });
            }
            for &index in &wave {
                done[index] = true;
                emitted += 1;
                for &next in &downstream[index] {
                    remaining[next] -= 1;
                }
            }
            waves.push(wave);
        }
        Ok(waves)
    }
}
