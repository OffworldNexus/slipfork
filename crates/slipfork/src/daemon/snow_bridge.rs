//! Transitional `snow.yml` → `Forkfile` bridge for the OSPP `subscribe` payload.
//!
//! `slipforkd` still boots from `snow.yml`; this module derives the feature-evaluated manifest
//! with the shipped migration bridge, evaluating the **active world mode** as a Forkfile feature so
//! the daemon subscribes to exactly the manifest its world runs. `M3.32.3.3` deletes this bridge and
//! the `slipfork-snow` dependency once the world pipeline reads the `Forkfile` directly.

use std::path::Path;

use slipfork_edge::EdgeError;
use slipfork_forkfile::{
    EvaluatedFork, FeatureSet, ForkFile, Stmt, migrate_snow_yaml, parse_forkfile,
};

use super::{WORLD_MODE, runtime_error};

/// The feature selection for one migrated manifest: the world mode when it declares one.
pub(super) fn active_features(file: &ForkFile) -> FeatureSet {
    if declares_feature(&file.statements, WORLD_MODE) {
        FeatureSet::with_features([WORLD_MODE])
    } else {
        FeatureSet::new()
    }
}

/// Migrate `snow.yml` and evaluate it with the active world-mode feature set.
pub(super) fn evaluated_forkfile(
    source: &str,
    worktree: &Path,
) -> Result<EvaluatedFork, EdgeError> {
    let path = worktree.join("snow.yml");
    let migrated = migrate_snow_yaml(source).map_err(|error| {
        runtime_error(format!(
            "{}: {}\n  next: run `slipfork migrate --check`",
            path.display(),
            error.message()
        ))
    })?;
    let parsed =
        parse_forkfile(&migrated).map_err(|diagnostics| refusal(&path, &diagnostics[0].message))?;
    let features = active_features(&parsed);
    parsed
        .evaluate(&features)
        .map_err(|diagnostics| refusal(&path, &diagnostics[0].message))
}

/// One actionable line plus the single remedy, with no synthesized-source rustc block.
fn refusal(path: &Path, message: &str) -> EdgeError {
    runtime_error(format!(
        "{}: {message}\n  next: run `slipfork migrate --check`",
        path.display()
    ))
}

fn declares_feature(statements: &[Stmt], name: &str) -> bool {
    statements.iter().any(|statement| match statement {
        Stmt::Features { definitions, .. } => definitions
            .iter()
            .any(|definition| definition.name.value == name),
        Stmt::On(block) => declares_feature(&block.body, name),
        _ => false,
    })
}
