//! The `specs/02-cli-ux.md` status tree, rendered from the typed `ps` schema.
//!
//! Everything here is pure: the renderer receives the decoded `PsReply`, one clock reading, and the
//! caller's colour decision. Column widths are measured on the visible text, so a long fork or
//! service name widens its column instead of being sheared.

mod ledger;
mod model;
mod rows;
mod tree;

pub use ledger::offline;
pub use model::Options;
pub use tree::{no_world_line, parse, render, scope};
