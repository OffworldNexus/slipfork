//! Run-record annotations: the instruction a step executed, published onto its catalog run.
//!
//! A capture holds command output only. The instruction that ran, its source line, its child
//! exit code, and (when no child printed the reason) the engine's one-line cause are *record*
//! facts written under the catalog lock, so `logs -r` can name the `RUN` that failed without
//! polluting the capture itself.

use slipfork_world::run_log::RunNote;
use slipfork_world::{BootFailure, LogStore};

/// One failed step plus the instruction detail its run record still needs.
pub(crate) struct StepFailure {
    pub(crate) failure: BootFailure,
    pub(crate) note: Option<Box<RunNote>>,
}

impl StepFailure {
    pub(crate) fn plain(failure: BootFailure) -> Self {
        Self {
            failure,
            note: None,
        }
    }
}

/// The one writer of a step's instruction facts onto its catalog run.
pub(crate) struct RunNotes {
    store: LogStore,
}

impl RunNotes {
    pub(crate) fn new(store: &LogStore) -> Self {
        Self {
            store: store.clone(),
        }
    }

    /// The instruction a step announced when it STARTED, so a crash or engine failure still names it.
    pub(crate) fn started(&self, run_id: Option<&str>, note: &RunNote) -> Result<(), String> {
        self.annotate(run_id, note)
    }

    /// A failed run names its failing instruction, the typed exit code, and the engine cause.
    pub(crate) fn failed(
        &self,
        run_id: Option<&str>,
        note: Option<&RunNote>,
        exit_code: Option<i32>,
        reason: Option<&str>,
    ) -> Result<(), String> {
        let mut note = note.cloned().unwrap_or_default();
        note.exit_code = note.exit_code.or(exit_code);
        note.reason = note.reason.or_else(|| reason.map(str::to_owned));
        self.annotate(run_id, &note)
    }

    /// A settled run names the last instruction it executed.
    pub(crate) fn succeeded(&self, run_id: Option<&str>, note: &RunNote) -> Result<(), String> {
        self.annotate(run_id, note)
    }

    fn annotate(&self, run_id: Option<&str>, note: &RunNote) -> Result<(), String> {
        let Some(run_id) = run_id else {
            return Ok(());
        };
        // A step that knows nothing (a service start) leaves the record untouched.
        if note.is_empty() {
            return Ok(());
        }
        self.store
            .annotate_run(run_id, note)
            .map_err(|error| error.to_string())
    }
}
