//! The five-state live-row machine one stage's reconcile observer drives.
//!
//! `StageRows` owns the cursor and the identity of the row a clone opened; every method is one
//! typed transition, so the builder's reconcile call site never re-derives row layout.

use std::time::Duration;

use slipfork_world::snapshot::{PhaseTiming, TailPhase};

use crate::plan::PlannedStage;
use crate::step_log::{StepLog, StepPlace};

use super::phase_note;

/// The five-state row machine one stage's reconcile observer drives.
#[derive(Debug, Default)]
pub(super) struct StageRows {
    pub(super) cursor: usize,
    open: Option<slipfork_world::NodeKey>,
    open_place: Option<StepPlace>,
}

impl StageRows {
    /// A sealed node's row is published and settled at once, without a live clock.
    pub(super) fn reused(
        &mut self,
        stage: &PlannedStage,
        key: slipfork_world::NodeKey,
        log: &mut StepLog<'_>,
    ) {
        let Some(index) = stage.row_index_from(self.cursor, key) else {
            return;
        };
        stage.flush_metadata(self.cursor, index, log);
        self.start(stage, index, log);
        log.done(Duration::ZERO);
        self.open = None;
        self.open_place = None;
    }

    /// A clone opens its row here (the slot does not exist yet); later tails only relabel it.
    pub(super) fn tail(
        &mut self,
        stage: &PlannedStage,
        key: slipfork_world::NodeKey,
        phase: TailPhase,
        log: &mut StepLog<'_>,
    ) {
        if self.open == Some(key) {
            log.tail(phase);
            return;
        }
        let Some(index) = stage.row_index_from(self.cursor, key) else {
            return;
        };
        stage.flush_metadata(self.cursor, index, log);
        self.start(stage, index, log);
        self.open = Some(key);
        log.tail(phase);
    }

    /// Execution keeps the clone row (dropping its label) or opens a fresh one.
    pub(super) fn building(
        &mut self,
        stage: &PlannedStage,
        key: slipfork_world::NodeKey,
        log: &mut StepLog<'_>,
    ) {
        if self.open == Some(key) {
            log.tail(TailPhase::Executing);
            return;
        }
        let Some(index) = stage.row_index_from(self.cursor, key) else {
            return;
        };
        stage.flush_metadata(self.cursor, index, log);
        self.start(stage, index, log);
        self.open = Some(key);
    }

    /// Settles the open row with the measured split and its one `-v` timing note.
    pub(super) fn built(
        &mut self,
        key: slipfork_world::NodeKey,
        timing: PhaseTiming,
        log: &mut StepLog<'_>,
    ) {
        if self.open != Some(key) {
            return;
        }
        self.open = None;
        log.done(timing.total());
        if let Some(place) = self.open_place.take() {
            log.note(&phase_note(place, timing));
        }
    }

    /// Settles the open row as failed with the materializer's exit code.
    pub(super) fn failed(
        &mut self,
        key: slipfork_world::NodeKey,
        elapsed: Duration,
        code: Option<i32>,
        log: &mut StepLog<'_>,
    ) {
        if self.open == Some(key) {
            self.close_open(elapsed, code, log);
        }
    }

    /// Closes a row left open by a reconcile failure; no stale live row survives the verdict.
    pub(super) fn close_open(
        &mut self,
        elapsed: Duration,
        code: Option<i32>,
        log: &mut StepLog<'_>,
    ) {
        if self.open.take().is_some() {
            self.open_place = None;
            log.failed(elapsed, code);
        }
    }

    /// Opens `index`'s row and advances the cursor past it.
    fn start(&mut self, stage: &PlannedStage, index: usize, log: &mut StepLog<'_>) {
        let (place, keyword, args) = stage.row_parts(index);
        log.started(place, keyword, args);
        self.cursor = index + 1;
        self.open_place = Some(place);
    }
}
