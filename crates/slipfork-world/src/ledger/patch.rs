//! `worlds` update patch: `None` keeps a column, `Some(None)` clears it.

use super::entity::WorldState;

/// A partial `worlds` update built by the `mark_*` methods.
#[derive(Debug, Clone, Default)]
pub struct WorldPatch {
    /// New lifecycle state.
    pub state: Option<WorldState>,
    /// New supervisor pid.
    pub supervisor_pid: Option<Option<i64>>,
    /// New pod runtime directory.
    pub pods_dir: Option<Option<String>>,
    /// New boot start timestamp.
    pub started_at_ms: Option<Option<i64>>,
    /// New convergence problem.
    pub problem: Option<Option<String>>,
}

impl WorldPatch {
    /// Sets the lifecycle state.
    #[must_use]
    pub fn state(state: WorldState) -> Self {
        Self {
            state: Some(state),
            ..Self::default()
        }
    }

    /// Records a live supervisor pid.
    #[must_use]
    pub fn supervisor_pid(mut self, pid: i64) -> Self {
        self.supervisor_pid = Some(Some(pid));
        self
    }

    /// Records the world's pod runtime directory.
    #[must_use]
    pub fn pods_dir(mut self, dir: impl Into<String>) -> Self {
        self.pods_dir = Some(Some(dir.into()));
        self
    }

    /// Records when the boot published `live`.
    #[must_use]
    pub fn started_at_ms(mut self, ms: i64) -> Self {
        self.started_at_ms = Some(Some(ms));
        self
    }

    /// Records the convergence problem text.
    #[must_use]
    pub fn problem(mut self, problem: impl Into<String>) -> Self {
        self.problem = Some(Some(problem.into()));
        self
    }

    /// Forgets the supervisor pid.
    #[must_use]
    pub fn clear_supervisor(mut self) -> Self {
        self.supervisor_pid = Some(None);
        self
    }

    /// Forgets the pod runtime directory.
    #[must_use]
    pub fn clear_pods(mut self) -> Self {
        self.pods_dir = Some(None);
        self
    }

    /// Forgets the convergence problem.
    #[must_use]
    pub fn clear_problem(mut self) -> Self {
        self.problem = Some(None);
        self
    }
}
