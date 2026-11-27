//! Failure decoration and the one `-v` timing note for native image builds.
//!
//! Kept beside the orchestrator so `mod.rs` stays the plan-and-reconcile flow.

use std::path::Path;

use slipfork_world::snapshot::{PhaseTiming, TailPhase};

use crate::dockerfile::Instruction;
use crate::error::Error;
use crate::step_log::StepPlace;

/// The one `-v` note that proves where a settled step's seconds went.
pub(crate) fn phase_note(place: StepPlace, timing: PhaseTiming) -> String {
    let split = [
        (TailPhase::Cloning, timing.clone),
        (TailPhase::Executing, timing.exec),
        (TailPhase::Sealing, timing.seal),
        (TailPhase::Recording, timing.record),
        (TailPhase::Publishing, timing.publish),
    ]
    .iter()
    .map(|(phase, elapsed)| format!("{} {:.2}s", phase.as_str(), elapsed.as_secs_f64()))
    .collect::<Vec<_>>()
    .join(" ");
    format!(
        "step {}/{} {split} total {:.2}s",
        place.index,
        place.total,
        timing.total().as_secs_f64(),
    )
}

/// Folds a `FROM` resolution failure into a step failure naming that `FROM` row.
pub(crate) fn at_from(err: Error, line: usize, args: &str) -> Error {
    match err {
        Error::StepFailed { .. } | Error::Dockerfile { .. } => err,
        other => Error::StepFailed {
            line,
            phase: "from",
            instruction: args.into(),
            message: other.cause(),
            exit_code: other.exit_code(),
            runtime: None,
            next: other.remedy().unwrap_or_else(|| {
                "check the reference and registry access, then retry".to_owned()
            }),
        },
    }
}

/// Folds a step failure into a line-decorated `StepFailed` naming the instruction as written.
pub(crate) fn at_line(err: Error, line: usize, instruction: &Instruction) -> Error {
    let phase = instruction.phase();
    let command = instruction.command_text();
    match err {
        Error::Dockerfile { .. } => err,
        unknown @ Error::UnknownAccount { .. } => Error::StepFailed {
            line,
            phase,
            instruction: command.into(),
            message: unknown.cause(),
            exit_code: None,
            runtime: None,
            next: unknown.remedy().unwrap_or_default(),
        },
        Error::StepFailed {
            message,
            exit_code,
            runtime,
            next,
            ..
        } => Error::StepFailed {
            line,
            phase,
            instruction: command.into(),
            message,
            exit_code,
            runtime,
            next,
        },
        Error::Io {
            action,
            path,
            source,
        } => Error::StepFailed {
            line,
            phase,
            instruction: command.into(),
            message: format!("could not {action} `{}`: {source}", visible_path(&path)),
            exit_code: None,
            runtime: None,
            next: "check the build context permissions and free disk space, then retry".to_owned(),
        },
        Error::Http { url, status } => Error::StepFailed {
            line,
            phase,
            instruction: command.into(),
            message: format!("`{url}` returned HTTP {status}"),
            exit_code: None,
            runtime: None,
            next: "check the URL or the registry reference, then retry".to_owned(),
        },
        Error::Transport { url, message } => Error::StepFailed {
            line,
            phase,
            instruction: command.into(),
            message: format!("could not reach `{url}`: {message}"),
            exit_code: None,
            runtime: None,
            next: "check the URL and your network connection, then retry".to_owned(),
        },
        Error::CrunMissing { path } => Error::StepFailed {
            line,
            phase,
            instruction: command.into(),
            message: format!("`RUN` needs `crun`, but `{}` is missing", path.display()),
            exit_code: None,
            runtime: None,
            next:
                "install crun (`apt install crun`, `dnf install crun`, `pacman -S crun`) and retry"
                    .to_owned(),
        },
        Error::UnknownStage { name } => Error::StepFailed {
            line,
            phase,
            instruction: command.into(),
            message: format!("no build stage named `{name}`"),
            exit_code: None,
            runtime: None,
            next:
                "declare it above this stage with `FROM <image> AS <name>`, or use a full image reference"
                    .to_owned(),
        },
        other => other,
    }
}

/// Drops the internal `snapshots/<slot>` prefix so errors name container paths only.
fn visible_path(path: &Path) -> String {
    let components: Vec<String> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    match components
        .iter()
        .position(|component| component == "snapshots")
    {
        Some(index) if components.len() > index + 2 => {
            format!("/{}", components[index + 2..].join("/"))
        }
        _ => path.display().to_string(),
    }
}
