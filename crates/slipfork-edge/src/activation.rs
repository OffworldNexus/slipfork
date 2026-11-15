//! Systemd socket activation boundary and the explicit fd-order listener set.
//!
//! This will be the **only** module in the crate allowed to read the process environment
//! (`LISTEN_PID` / `LISTEN_FDS`). It will adopt inherited descriptors at `fd >= 3` through safe
//! `listenfd`, set them non-blocking immediately, and fail closed on any listener shape that does
//! not match `UnitSpec`'s emitted order — never self-binding and never retrying `--listen`.
