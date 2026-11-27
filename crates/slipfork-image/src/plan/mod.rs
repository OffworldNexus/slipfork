//! The pure build plan: a walked `Dockerfile` becomes one `BuildGraph` per stage.
//!
//! `refs` owns the tag → digest map, `sources` owns per-`COPY` Merkles, `expand` owns instruction
//! expansion and `--from` resolution, and `planner` schedules one graph per stage. Nothing here
//! writes the snapshots store; the builder only materializes the plan.

mod expand;
mod planner;
mod refs;
mod sources;

#[cfg(test)]
mod tests;

pub use planner::StepTrace;
pub(crate) use planner::{BuildPlanner, CopyOrigin, FsKind, PlannedStage, StepTask};
pub(crate) use refs::RefResolver;
