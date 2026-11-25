//! Typed failure rendering and feature resolution shared by `up`, `down`, `restart`, and `fork`.

use std::process::ExitCode;

use slipfork_forkfile::Diagnostics;

use crate::commands::Session;
use crate::ui::report::{Failure, Report};
use crate::world::TargetError;
use crate::world::features::WorldFeatures;

impl Session {
    /// Reads, compiles, and verifies `<project>/Forkfile` with `request` merged over the persisted
    /// `.slipfork/features.json`; the returned configuration is exactly what a boot will read.
    ///
    /// Persistence belongs to the caller: `up`/`fork` save, `env` evaluates only.
    pub(in crate::world) fn project_features(
        &self,
        project: &std::path::Path,
        request: &WorldFeatures,
        json: bool,
    ) -> Result<WorldFeatures, ExitCode> {
        let path = project.join("Forkfile");
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                return Err(self.report(
                    "forkfile",
                    &format!("cannot read `{}`: {error}", path.display()),
                    "create a `Forkfile` in this directory, then run `slipfork up`",
                    json,
                ));
            }
        };
        let checked = match slipfork_forkfile::compile(&source) {
            Ok(checked) => checked,
            Err(diagnostics) => return Err(self.forkfile_failure(&diagnostics, json)),
        };
        let config = match WorldFeatures::merged(project, request) {
            Ok(config) => config,
            Err(error) => {
                return Err(self.report("feature", &error, WorldFeatures::REMEDY, json));
            }
        };
        if let Err(diagnostic) = config.resolve(&checked.document) {
            return Err(self.feature_failure(&diagnostic, json));
        }
        Ok(config)
    }

    /// The one refusal for an unresolvable positional target.
    pub(in crate::world) fn target_failure(
        &self,
        error: &TargetError,
        json_mode: bool,
    ) -> ExitCode {
        self.lifecycle_failure(
            Failure::remedied("no-target", error.message.clone(), error.next),
            json_mode,
            false,
        )
    }

    /// Renders the first compiler diagnostic as `Forkfile:line:col`, plus its one `next:`.
    pub(in crate::world) fn forkfile_failure(
        &self,
        diagnostics: &Diagnostics,
        json_mode: bool,
    ) -> ExitCode {
        let first = diagnostics.first();
        let message = first.map_or_else(
            || "invalid Forkfile".to_owned(),
            |item| {
                format!(
                    "Forkfile:{}:{}: {}",
                    item.span.line(),
                    item.span.col(),
                    item.message
                )
            },
        );
        let next = first.map_or("fix the Forkfile", |item| item.next.as_str());

        self.lifecycle_failure(
            Failure::remedied("forkfile", message, next),
            json_mode,
            false,
        )
    }

    /// Prints exactly one `next:` remedy, or one JSON error object under `--json`.
    pub(in crate::world) fn lifecycle_failure(
        &self,
        failure: Failure,
        json_mode: bool,
        already_rendered: bool,
    ) -> ExitCode {
        if json_mode {
            let _ = self.renderer().emit_json(&failure.json());
            return ExitCode::FAILURE;
        }
        if already_rendered {
            return ExitCode::FAILURE;
        }
        let _ = self.renderer().emit(&Report::failure(failure));
        ExitCode::FAILURE
    }
}
