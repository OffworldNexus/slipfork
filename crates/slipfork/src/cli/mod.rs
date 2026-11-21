//! The `clap` derive tree and its dispatcher; never a second argv parser.

mod dispatch;
mod manpage;
pub(crate) mod verbs;

pub use dispatch::run;
