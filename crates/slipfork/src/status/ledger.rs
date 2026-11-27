//! The daemonless ledger reader behind `status`/`ps` (`specs/03` §7.5).
//!
//! No daemon answers, so nothing may be converged or written: this opens `state.db` read-only,
//! projects the rows the ledger already owns through the shared [`assemble_reply`] tree, and hands
//! the pure renderer the same typed `PsReply` the daemon would send. A row the ledger calls `live`
//! renders as the dim `○ stopped` it really is without a supervisor, and a vanished checkout
//! becomes one `✗` row with one `!` remedy.

use std::collections::BTreeMap;
use std::path::Path;

use slipfork_world::{ForkLedger, LedgerError, LedgerRows, WorldState};

use crate::ps::{
    PsReply, PsWorld, PsWorldState, assemble_reply, checkout_problem, lineage, with_remedy,
};

/// Reads the on-disk ledger as the typed `ps` schema; a missing `state.db` is an empty reply.
///
/// # Errors
///
/// Returns the ledger diagnostic (a pending or newer schema, an unreadable database) so the caller
/// can print one line and its remedy.
pub fn offline(state_dir: &Path) -> Result<PsReply, LedgerError> {
    let ledger = ForkLedger::open_reader(state_dir)?;
    let rows = ledger.list()?;
    Ok(reply(&rows))
}

/// Projection of settled ledger rows: no supervisor can be proven, so nothing is `live`.
///
/// The one disk fact is whether a still-tracked checkout exists; a missing one is the row's one
/// honest problem, never an empty phantom.
fn reply(rows: &LedgerRows) -> PsReply {
    let fork_names: BTreeMap<i64, String> = rows
        .forks
        .iter()
        .map(|fork| (fork.world_id, fork.name.clone()))
        .collect();
    let mut built: BTreeMap<i64, PsWorld> = BTreeMap::new();
    for row in &rows.worlds {
        if matches!(row.state, WorldState::Creating | WorldState::Deleting) {
            continue;
        }
        let fork = fork_names.get(&row.id).map(String::as_str);
        let mut world = PsWorld::stopped(&row.checkout_path, &row.world_name, fork);
        world.problem = match (&row.problem, row.checkout_path.is_dir()) {
            (Some(problem), _) => Some(with_remedy(problem, fork, &row.world_name)),
            (None, false) => Some(checkout_problem(fork, &row.world_name)),
            (None, true) => None,
        };
        world.state = if row.state == WorldState::Broken || world.problem.is_some() {
            PsWorldState::Broken
        } else {
            PsWorldState::Stopped
        };
        world.lineage = lineage(rows, row.id);
        built.insert(row.id, world);
    }
    assemble_reply(rows, built)
}
