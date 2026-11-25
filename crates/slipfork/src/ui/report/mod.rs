//! The typed `Report` layer: one vocabulary and one renderer for every CLI surface.
//!
//! `model` owns the report vocabulary, `row` owns typed row states, `runs` owns the recorded-run
//! catalog grid, `failure` owns the one `error:`/`next:` projection, `wall` owns the injected
//! local clock, and `render` writes it all.

mod exit;
mod failure;
mod live;
mod model;
mod render;
mod row;
mod runs;
mod stream;
mod wall;

pub use exit::emit_stdout;
pub use failure::{Failure, split_remedy};
pub use live::Pane;
pub use model::{Entry, Report, Section, StreamKind, Verdict, VerdictKind, digest_cell};
pub use render::ReportRenderer;
pub use row::{Counter, Done, FailureFacts, Outcome, Row, RowLevel, StepSettlement};
pub use runs::RunRow;
pub use stream::StreamRenderer;
pub use stream::visible_line;
pub use wall::WallZone;
