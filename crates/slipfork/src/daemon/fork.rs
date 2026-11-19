//! The daemon's `Forkfile` entry point and the projection facts it reads.
//!
//! `slipforkd` reads `<worktree>/Forkfile` through [`crate::project::load_project_fork`], evaluates
//! the active world feature into an [`EvaluatedFork`], and thereafter reads only that projection. It
//! never scans `${{` expressions, re-derives resource coordinates, or parses a second manifest.

use std::path::Path;

use slipfork_edge::EdgeError;
use slipfork_forkfile::{EvaluatedFork, TaintedString, WorldFact};

use super::rejected;

/// Load, statically check, and feature-evaluate the project `Forkfile`.
///
/// A malformed or missing manifest is a user-authored refusal, never an internal failure: the
/// single `crate::project` formatter already supplies the `path:line:col` and the `next:` remedy.
pub(super) fn load_project_fork(worktree: &Path) -> Result<EvaluatedFork, EdgeError> {
    crate::project::load_project_fork(worktree).map_err(rejected)
}

/// One `<entity>.<property>` fact of an already-built projection.
pub(super) fn fact<'a>(
    facts: &'a [WorldFact],
    entity: &str,
    property: &str,
) -> Option<&'a TaintedString> {
    facts
        .iter()
        .find(|fact| fact.entity == entity && fact.property == property)
        .map(|fact| &fact.value)
}

/// The published TCP port of one backing service, or `None` when it publishes no `port`.
pub(super) fn fact_port(facts: &[WorldFact], entity: &str) -> Option<u16> {
    fact(facts, entity, "port")?.expose().parse().ok()
}
