//! The `plan` frame: every row a lifecycle verb will print, announced before the first `Started`.
//!
//! The CLI measures its six columns from this single frame, so a name or a state that arrives
//! later can never move a row that is already on screen. Kinds reuse [`kind_name`], the supervisor's
//! own spelling, so a planned row always matches the `Started`/`Finished` frames that follow.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use slipfork_world::world_plan_with;

use crate::world::features::WorldFeatures;

/// One planned row: `(kind, name)` exactly as the later frames spell them.
pub(crate) type PlannedRow = (String, String);

/// Every step of `project`'s world plan; empty when its `Forkfile` does not compile.
pub(crate) fn world_rows(project: &Path) -> Vec<PlannedRow> {
    world_rows_with(project, None)
}

/// Every step of `project`'s world plan with an optional `fork` feature override evaluated in
/// memory over `<project>/.slipfork/features.json` without mutating the parent checkout.
pub(crate) fn world_rows_with(project: &Path, request: Option<&WorldFeatures>) -> Vec<PlannedRow> {
    let Ok(source) = fs::read_to_string(project.join("Forkfile")) else {
        return Vec::new();
    };
    let Ok(checked) = slipfork_forkfile::compile(&source) else {
        return Vec::new();
    };
    let config = match request {
        Some(override_request) => WorldFeatures::merged(project, override_request),
        None => WorldFeatures::load(project),
    };
    let Ok(config) = config else {
        return Vec::new();
    };
    let Ok(features) = config.resolve(&checked.document) else {
        return Vec::new();
    };
    let Ok(plan) = world_plan_with(&checked.document, project, &features) else {
        return Vec::new();
    };
    plan.steps
        .iter()
        .map(|step| (step.kind.as_str().to_owned(), step.node_name()))
        .collect()
}

/// The fork's own prep rows, ahead of the world steps it boots.
pub(crate) fn fork_rows(branch: &str) -> Vec<PlannedRow> {
    vec![
        ("fork".to_owned(), "snapshot".to_owned()),
        ("git".to_owned(), format!("branch {branch}")),
    ]
}

/// The merged plan's row count, as every per-row `step`/`total` frame echoes it.
fn total(rows: &[PlannedRow]) -> u64 {
    u64::try_from(rows.len()).unwrap_or(u64::MAX)
}

/// The 1-based position of a planned row, matched by the exact `kind` and `name` the later
/// `started`/`finished` frames carry.
fn index(rows: &[PlannedRow], kind: &str, name: &str) -> Option<u64> {
    rows.iter()
        .position(|(planned_kind, planned_name)| planned_kind == kind && planned_name == name)
        .and_then(|position| u64::try_from(position).ok())
        .map(|position| position + 1)
}

/// One fork-prep frame: the `fork` snapshot, the `git` branch switch, or a `drop` teardown row.
///
/// Its counter is resolved once. A planned row takes its 1-based position and the merged plan's
/// total, so `--json` can never disagree with the already latched `plan` frame; a teardown row of
/// a verb that announces no plan takes an explicit pair.
pub(crate) struct PrepFrame<'a> {
    kind: &'a str,
    name: Option<&'a str>,
    step: u64,
    total: u64,
}

impl<'a> PrepFrame<'a> {
    /// A prep row announced in the merged plan, falling back only if the row was absent.
    pub(crate) fn planned(
        rows: &[PlannedRow],
        kind: &'a str,
        name: &'a str,
        position: u64,
    ) -> Self {
        Self {
            kind,
            name: Some(name),
            step: index(rows, kind, name).unwrap_or(position),
            total: total(rows),
        }
    }

    /// A prep row of a verb that announces no plan (`drop`): an explicit `step`/`total` pair.
    pub(crate) fn fixed(kind: &'a str, step: u64, total: u64) -> Self {
        Self {
            kind,
            name: None,
            step,
            total,
        }
    }

    /// The `started` frame, carrying the phrase shown while the row runs.
    pub(crate) fn started(&self, label: String) -> Value {
        self.frame("started", label, None, None)
    }

    /// The settled `finished` frame, with its plain detail cell and elapsed time.
    pub(crate) fn finished(&self, label: String, detail: &str, elapsed_ms: u64) -> Value {
        self.frame("finished", label, Some(detail), Some(elapsed_ms))
    }

    fn frame(
        &self,
        event: &str,
        label: String,
        detail: Option<&str>,
        elapsed_ms: Option<u64>,
    ) -> Value {
        let mut frame = json!({
            "event": event,
            "step": self.step,
            "total": self.total,
            "kind": self.kind,
            "label": label,
        });
        if let Some(name) = self.name {
            frame["name"] = Value::String(name.to_owned());
        }
        if let Some(detail) = detail {
            frame["detail"] = Value::String(detail.to_owned());
        }
        if let Some(elapsed_ms) = elapsed_ms {
            frame["elapsed_ms"] = Value::from(elapsed_ms);
        }
        frame
    }
}

/// Emits the one `plan` frame; `total` is how many rows the verb will finish (0 means all rows).
pub(crate) fn emit(rows: &[PlannedRow], total: usize, emit: &mut dyn FnMut(Value)) {
    if rows.is_empty() {
        return;
    }
    let total = if total == 0 { rows.len() } else { total };
    emit(json!({
        "event": "plan",
        "rows": rows
            .iter()
            .map(|(kind, name)| json!({ "kind": kind, "name": name }))
            .collect::<Vec<_>>(),
        "total": total,
    }));
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{fork_rows, world_rows, world_rows_with};
    use crate::world::features::WorldFeatures;
    use slipfork_forkfile::FeatureEdit;

    #[test]
    fn world_rows_list_every_declared_step_and_a_bad_forkfile_lists_nothing() {
        let root = tempfile::Builder::new()
            .prefix("slipfork-plan-rows")
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory");
        fs::write(
            root.path().join("Forkfile"),
            "# forkfile: 0.1\nimage base from python:3.13-slim {\n  service api {\n    run: sleep 3600\n  }\n}\n",
        )
        .expect("write a Forkfile");
        let rows = world_rows(root.path());
        let rendered: Vec<String> = rows
            .iter()
            .map(|(kind, name)| format!("{kind} {name}"))
            .collect();
        assert!(
            rendered.iter().any(|row| row == "service api"),
            "{rendered:?}"
        );
        assert!(
            rendered.iter().any(|row| row == "image base"),
            "{rendered:?}"
        );

        fs::write(root.path().join("Forkfile"), "not a forkfile\n").expect("write garbage");
        assert!(world_rows(root.path()).is_empty());
        assert!(world_rows(&root.path().join("missing")).is_empty());
    }

    #[test]
    fn fork_rows_name_the_two_prep_steps_before_the_world_boots() {
        assert_eq!(
            fork_rows("feat-auth"),
            vec![
                ("fork".to_owned(), "snapshot".to_owned()),
                ("git".to_owned(), "branch feat-auth".to_owned()),
            ]
        );
    }

    #[test]
    fn world_rows_with_evaluates_fork_feature_overrides_without_touching_parent() {
        let root = tempfile::Builder::new()
            .prefix("slipfork-plan-override")
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory");
        fs::write(
            root.path().join("Forkfile"),
            "# forkfile: 0.1\nfeatures {\n  dev = [mail]\n  mail\n}\nimage base from python:3.13-slim {\n  service api {\n    run: sleep 3600\n  }\n}\non mail {\n  image mailpit from axllent/mailpit:latest {\n    service mailpit {\n      port 8025\n    }\n  }\n}\n",
        )
        .expect("write a Forkfile");
        let default_rows = world_rows(root.path());
        assert!(default_rows.iter().any(|(_, name)| name == "mailpit"));

        let prod_override = WorldFeatures::new(false, vec![FeatureEdit::Disable("dev".to_owned())]);
        let prod_rows = world_rows_with(root.path(), Some(&prod_override));
        assert!(
            prod_rows.iter().all(|(_, name)| name != "mailpit"),
            "`-F -dev` must omit `mailpit` from the fork's announced plan: {prod_rows:?}"
        );
        assert!(
            !WorldFeatures::path(root.path()).exists(),
            "evaluating the fork's plan must never write the parent's features.json"
        );
    }
}
