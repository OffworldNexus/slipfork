//! The typed `Report` layer: one vocabulary and one renderer for every CLI surface.
//!
//! `model` owns the report vocabulary, `row` owns typed row states, `failure` owns the one
//! `error:`/`next:` projection, `wall` owns the injected local clock, and `render` writes it all.

mod failure;
mod live;
mod model;
mod render;
mod row;
mod wall;

pub use failure::Failure;
pub use live::Pane;
pub use model::{Entry, Report, Section, StreamKind, Verdict, VerdictKind};
pub use render::ReportRenderer;
pub use row::{Counter, Done, FailureFacts, Outcome, Row, RowLevel, StepSettlement};
pub use wall::WallZone;
