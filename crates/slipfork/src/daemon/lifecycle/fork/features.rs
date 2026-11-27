//! Feature overrides on a fork checkout, validated against the fork's own `Forkfile`.

use std::path::Path;

use super::super::core::Core;
use super::super::plan;
use crate::daemon::Failure;
use crate::world::features::WorldFeatures;

/// Announced plan row explaining why a feature-overridden fork rebuilds instead of adopting
/// the parent's sealed images in 0ms.
pub(super) const FEATURE_REBUILD_ROW: (&str, &str) =
    ("reuse", "rebuilding images (feature set changed)");

impl Core {
    /// Builds the complete announced row list for one fork, inserting the reuse-declined row
    /// whenever the request carries feature overrides.
    pub(super) fn planned_fork_rows(
        project: &Path,
        raw_name: &str,
        request: Option<&WorldFeatures>,
    ) -> Vec<plan::PlannedRow> {
        let mut rows = plan::fork_rows(raw_name);
        if request.is_some() {
            rows.push((
                FEATURE_REBUILD_ROW.0.to_owned(),
                FEATURE_REBUILD_ROW.1.to_owned(),
            ));
        }
        rows.extend(plan::world_rows_with(project, request));
        rows
    }

    /// Validates `request` merged over `<project>/.slipfork/features.json` without mutating disk.
    pub(super) fn validate_features(
        project: &Path,
        request: &WorldFeatures,
    ) -> Result<WorldFeatures, Failure> {
        let config = WorldFeatures::merged(project, request).map_err(|error| {
            Failure::new(
                "feature",
                format!("{error}\nnext: {}", WorldFeatures::REMEDY),
            )
        })?;
        let path = project.join("Forkfile");
        let source = std::fs::read_to_string(&path).map_err(|_| {
            Failure::new(
                "forkfile",
                "cannot read `Forkfile`\nnext: restore the file, then retry `slipfork fork`",
            )
        })?;
        let checked = slipfork_forkfile::compile(&source).map_err(|diagnostics| {
            Failure::new(
                "feature",
                diagnostics
                    .first()
                    .map_or("invalid Forkfile".to_owned(), |item| {
                        format!("Forkfile:{}: {}", item.span.line(), item.message)
                    }),
            )
        })?;
        config.resolve(&checked.document).map_err(|diagnostic| {
            Failure::new(
                "feature",
                format!(
                    "Forkfile:{}: {}\nnext: {}",
                    diagnostic.span.line(),
                    diagnostic.message,
                    diagnostic.next
                ),
            )
        })?;
        Ok(config)
    }

    /// Applies a fork request's edits over the snapshot's inherited configuration, verifies it
    /// resolves against the fork checkout's `Forkfile`, and persists `.slipfork/features.json`.
    pub(super) fn apply_features(
        project: &Path,
        request: WorldFeatures,
    ) -> Result<WorldFeatures, Failure> {
        let config = Self::validate_features(project, &request)?;
        config.save(project).map_err(|error| {
            Failure::new(
                "feature",
                format!(
                    "cannot save `{}`: {error}\nnext: check that the fork's `.slipfork` directory is writable",
                    WorldFeatures::path(project).display()
                ),
            )
        })?;
        Ok(config)
    }
}
