//! Streaming control-socket requests: timestamp-merged `logs` replay and interactive `exec`.
//!
//! Both handlers run on a detached thread so the init loop keeps reaping pods and answering the
//! lifeline. They never touch `InitWorld` directly: [`WorldShared`] exposes only cloned facts.

mod exec;
mod lines;
mod tail;

pub(crate) use exec::serve_exec;
pub(crate) use tail::serve_logs;
