//! World lifecycle inside the daemon: placeholders, up/down/restart, teardown, and `ps`.

pub(super) mod core;
mod down;
mod fork;
mod logs;
pub(crate) mod plan;
mod ps;
pub(super) mod pty;
pub(super) mod stream;
mod streaming;
pub(super) mod up;
