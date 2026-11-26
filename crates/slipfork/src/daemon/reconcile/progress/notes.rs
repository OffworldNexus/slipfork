//! Note, failure, label, and settlement builders one image attempt publishes.

use std::path::{Path, PathBuf};

use slipfork_image::{StepEvent, StepTrace};
use slipfork_world::ImageNode;
use slipfork_world::run_log::RunNote;

use super::ImageSettle;
use crate::ui::report::{Done, StepSettlement};

/// Splits an engine message into its body and the one `next:` remedy it already carries.
pub(super) fn failure_parts(message: &str) -> (String, String) {
    let (body, next) = crate::ui::report::split_remedy(message);
    (
        body,
        next.unwrap_or_else(|| "fix the failing step and retry `slipfork up`".to_owned()),
    )
}
/// The exact settlement one finished attempt publishes; zero executed steps stay `cached`.
pub(super) fn settlement(
    settle: ImageSettle,
    duration_ms: u64,
    run_id: Option<&str>,
) -> StepSettlement {
    match settle {
        ImageSettle::Cached => StepSettlement::Cached { duration_ms },
        ImageSettle::Pulled => StepSettlement::Completed {
            done: Done::Pulled,
            duration_ms,
            run_id: run_id.map(str::to_owned),
        },
        ImageSettle::Built(steps) => StepSettlement::Completed {
            done: Done::Built { steps },
            duration_ms,
            run_id: run_id.map(str::to_owned),
        },
    }
}
/// One failed attempt: the user message plus the typed facts its run record still needs.
pub(in crate::daemon::reconcile) struct AttemptFailure {
    /// `1 line + next:` message printed by `up`; never appended to the capture.
    pub(super) message: String,
    /// The failing instruction when the attempt already knows it (`FROM`, a parsed pull).
    pub(super) note: Option<Box<RunNote>>,
    /// Typed child exit code the step carried, when it carried one.
    pub(super) exit_code: Option<i32>,
    /// The engine's raw one-line cause, so a comment-less replay explains itself.
    pub(super) reason: Option<String>,
}

impl AttemptFailure {
    /// A failure with no typed engine facts (a missing `Dockerfile`, an unreadable context).
    pub(in crate::daemon::reconcile) fn bare(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            note: None,
            exit_code: None,
            reason: None,
        }
    }

    /// A failure the image engine reported, with its typed exit code and one-line cause.
    pub(in crate::daemon::reconcile) fn engine(
        message: impl Into<String>,
        error: &slipfork_image::Error,
        note: Option<RunNote>,
    ) -> Self {
        Self {
            message: message.into(),
            note: note.map(Box::new),
            exit_code: error.exit_code(),
            reason: Some(error.cause()),
        }
    }
}
/// One instruction's capture row: `<index>/<total> <KEYWORD> <args>`, ANSI-free and single-line.
pub(super) fn step_label(event: &StepEvent<'_>) -> String {
    let raw = format!(
        "{}/{} {} {}",
        event.index, event.total, event.instruction, event.args
    );
    let raw = crate::ui::strip(&raw);
    let mut label = String::with_capacity(raw.len());
    let mut gap = false;
    for ch in raw.chars() {
        if matches!(ch, '\n' | '\r' | '\t') {
            gap = true;
            continue;
        }
        if gap {
            label.push(' ');
            gap = false;
        }
        label.push(ch);
    }
    label.trim_end().to_owned()
}

/// The instruction a build announced at START, so a failure always leaves the right header.
///
/// `line` is the instruction's `Dockerfile` source line; a step with no source line (an OCI pull)
/// keeps its phase and command and simply carries no `source`.
pub(super) fn started_note(dockerfile: Option<&Path>, event: &StepEvent<'_>) -> RunNote {
    RunNote {
        phase: Some(event.instruction.to_ascii_lowercase()),
        instruction: (!event.args.is_empty()).then(|| event.args.to_owned()),
        source: dockerfile
            .filter(|_| event.line > 0)
            .map(|dockerfile| format!("{}:{}", file_label(dockerfile), event.line)),
        ..RunNote::default()
    }
}

/// The note for a failure that ran no instruction at all: it names only the `Dockerfile`, so the
/// verdict — never an invented step row — carries the reason.
pub(super) fn failed_note(dockerfile: &Path) -> RunNote {
    RunNote {
        source: Some(file_label(dockerfile)),
        ..RunNote::default()
    }
}

/// A `from:` image pull records the reference it fetched, under the one `FROM` verb.
fn from_note(reference: &str) -> RunNote {
    RunNote {
        phase: Some("from".to_owned()),
        instruction: Some(reference.to_owned()),
        ..RunNote::default()
    }
}

/// A successful build names the last instruction it applied, or its Dockerfile when it applied
/// none, or the pulled reference.
pub(super) fn success_note(nodes: &[ImageNode], image: &str, last: Option<&StepTrace>) -> RunNote {
    let Some(node) = nodes.iter().find(|node| node.id == image) else {
        return RunNote::default();
    };
    if let Some(reference) = &node.from {
        return from_note(reference);
    }
    let dockerfile = node
        .dockerfile
        .clone()
        .unwrap_or_else(|| PathBuf::from("Dockerfile"));
    let label = file_label(&dockerfile);
    match last {
        Some(last) => RunNote {
            phase: Some(last.phase.to_owned()),
            instruction: Some(last.command.clone()),
            source: Some(format!("{label}:{}", last.line)),
            exit_code: Some(0),
            reason: None,
        },
        None => RunNote {
            phase: Some(label.clone()),
            instruction: None,
            source: Some(label),
            exit_code: Some(0),
            reason: None,
        },
    }
}

/// The bare file name a source location starts with (`Dockerfile`, not `/ctx/Dockerfile`).
pub(super) fn file_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}
