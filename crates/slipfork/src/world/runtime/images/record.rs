//! Run-record owner: the instruction a step actually executed, published onto its catalog run.
//!
//! A capture holds command output only. The instruction that ran, its source line, and its child
//! exit code are *record* facts: they are written once the run settles, under the catalog lock, so
//! `logs -r` can name the `RUN` that failed without polluting the capture itself.

use slipfork_world::run_log::RunNote;
use slipfork_world::{BootFailure, LogStore};

/// One failed image build: the human message plus the instruction that actually ran last.
#[derive(Debug, Clone)]
pub(crate) struct BuildFailure {
    /// One-line failure message, printed by `up` and never appended to the capture.
    pub(crate) message: String,
    /// The failing instruction, published onto the step's run record.
    pub(crate) note: RunNote,
}

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

    pub(crate) fn described(failure: BootFailure, note: RunNote) -> Self {
        Self {
            failure,
            note: Some(Box::new(note)),
        }
    }
}

/// Publishes the instruction that ran onto a step's already-finished catalog run.
pub(crate) struct RunNotes<'a> {
    store: &'a LogStore,
}

impl<'a> RunNotes<'a> {
    pub(crate) fn new(store: &'a LogStore) -> Self {
        Self { store }
    }

    /// A failed run names its failing instruction and the typed exit code the step carried.
    pub(crate) fn failed(
        &self,
        run_id: Option<&str>,
        note: Option<&RunNote>,
        exit_code: Option<i32>,
    ) -> Result<(), String> {
        let mut note = note.cloned().unwrap_or_default();
        note.exit_code = note.exit_code.or(exit_code);
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
