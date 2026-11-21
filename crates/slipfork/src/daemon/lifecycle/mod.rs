//! World lifecycle inside the daemon: placeholders, up/down/restart, teardown, and `ps`.

pub(super) mod core;
mod down;
mod ps;
mod up;
