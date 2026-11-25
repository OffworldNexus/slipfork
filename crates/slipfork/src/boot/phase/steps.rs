//! The live-step registry: running phases, their activity stamps, and lookup/take operations.
//!
//! Split out of [`super`] so the phase row stays a cohesive value while this module owns the
//! concurrent registry the build narrative and the status line share.

use super::{LiveKey, Phase};

/// One running step in [`LiveSteps`]: its phase row and activity stamp.
#[derive(Debug)]
pub(in crate::boot) struct LiveStep {
    pub(in crate::boot) phase: Phase,
    pub(in crate::boot) touched: u64,
}

/// The single live-step registry: owns every running phase and its last activity stamp.
#[derive(Debug, Default)]
pub(in crate::boot) struct LiveSteps {
    pub(in crate::boot) steps: Vec<LiveStep>,
    tick: u64,
}

impl LiveSteps {
    pub(in crate::boot) fn new() -> Self {
        Self::default()
    }

    /// Registers `phase` as running, replacing any stale entry with the same key.
    pub(in crate::boot) fn begin(&mut self, phase: Phase) {
        self.steps.retain(|step| step.phase.key != phase.key);
        self.tick = self.tick.wrapping_add(1);
        self.steps.push(LiveStep {
            phase,
            touched: self.tick,
        });
    }

    /// Marks `key` as most recently active.
    pub(in crate::boot) fn touch(&mut self, key: &LiveKey) -> Option<Phase> {
        let step = self.steps.iter_mut().find(|step| &step.phase.key == key)?;
        self.tick = self.tick.wrapping_add(1);
        step.touched = self.tick;
        Some(step.phase.clone())
    }

    /// Applies one tail phase to the live row with that exact key, keeping the elapsed clock.
    pub(in crate::boot) fn set_tail(
        &mut self,
        key: &LiveKey,
        phase: slipfork_world::snapshot::TailPhase,
    ) -> bool {
        let Some(step) = self.steps.iter_mut().find(|step| &step.phase.key == key) else {
            return false;
        };
        self.tick = self.tick.wrapping_add(1);
        step.touched = self.tick;
        step.phase.set_tail(phase);
        true
    }

    /// The running phase for `key`, if that row is still live.
    pub(in crate::boot) fn phase(&self, key: &LiveKey) -> Option<&Phase> {
        self.steps
            .iter()
            .find(|step| &step.phase.key == key)
            .map(|step| &step.phase)
    }

    /// Resolves the live step key from explicit frame keys, or `fallback` when none were pinned.
    pub(in crate::boot) fn resolve_key(
        &self,
        candidates: &[LiveKey],
        target: Option<&str>,
        fallback: Option<&LiveKey>,
    ) -> Option<LiveKey> {
        for key in candidates {
            if self.steps.iter().any(|step| &step.phase.key == key) {
                return Some(key.clone());
            }
        }
        if let Some(target) = target
            && let Some(step) = self
                .steps
                .iter()
                .rev()
                .find(|step| step.phase.target == target)
        {
            return Some(step.phase.key.clone());
        }
        if candidates.is_empty()
            && target.is_none()
            && let Some(key) = fallback
            && self.steps.iter().any(|step| &step.phase.key == key)
        {
            return Some(key.clone());
        }
        None
    }

    /// Removes and returns one settled step by exact key.
    pub(in crate::boot) fn take(&mut self, key: &LiveKey) -> Option<LiveStep> {
        let position = self.steps.iter().position(|step| &step.phase.key == key)?;
        Some(self.steps.remove(position))
    }

    /// Removes and returns a boot command step matching `(kind, name)` or `target == name`.
    pub(in crate::boot) fn take_boot(&mut self, kind: &str, name: &str) -> Option<LiveStep> {
        let position = self
            .steps
            .iter()
            .position(|step| {
                matches!(
                    &step.phase.key,
                    LiveKey::Boot { kind: planned, name: planned_name }
                        if planned == kind && planned_name == name
                )
            })
            .or_else(|| self.steps.iter().position(|step| step.phase.target == name))?;
        Some(self.steps.remove(position))
    }

    /// Removes the failing step, clears any sibling phases, and returns the failing step in full.
    pub(in crate::boot) fn take_failure(
        &mut self,
        target: Option<&str>,
        active: Option<&LiveKey>,
    ) -> Option<LiveStep> {
        let position = target
            .and_then(|target| {
                self.steps
                    .iter()
                    .rposition(|step| step.phase.target == target)
            })
            .or_else(|| {
                active.and_then(|key| self.steps.iter().rposition(|step| &step.phase.key == key))
            })
            .or_else(|| self.steps.len().checked_sub(1));
        let removed = position.map(|index| self.steps.remove(index));
        self.steps.clear();
        removed
    }

    /// Most recently started surviving step key.
    pub(in crate::boot) fn last_key(&self) -> Option<LiveKey> {
        self.steps.last().map(|step| step.phase.key.clone())
    }

    /// The one status-line text for the most recently active step, if any is still running.
    pub(in crate::boot) fn live_row(
        &self,
        spinner: char,
        width: usize,
        palette: crate::ui::report::Palette,
    ) -> Option<String> {
        let step = self.steps.iter().max_by_key(|step| step.touched)?;
        Some(
            step.phase
                .live_row(spinner, step.phase.elapsed_millis(), width, palette),
        )
    }
}
