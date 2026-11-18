//! `slipfork-wire`: the neutral, dependency-light wire crate shared by every Slipfork process.
//!
//! Nothing here reads ambient OS state, spawns a process, or formats a CLI table. The crate owns
//! exactly two things:
//!
//! - [`control`]: the typed `DaemonControl` contract the thin `slipfork` CLI and `slipforkd` speak
//!   over `<runtime_dir>/daemon.sock` (one `\n`-terminated JSON frame per message, hard-capped at
//!   64 KiB), the `0600` pathname-socket budget both daemon sockets share, and the redacting
//!   [`control::ExecEnv`] / lineage view values those frames carry.
//! - [`relay`]: the single bounded 16 KiB whole-duplex splice (`Duplex`, `pipe`) used by the edge
//!   ingress, the daemon's `forward` bridge, and `slipfork-world-init`.
//!
//! ```text
//! slipfork-world  <--  slipfork-wire  <--  slipfork-engine / slipfork-edge / slipfork
//! ```
//!
//! The only trait here is [`relay::Duplex`], the genuine two-endpoint port the splice needs (a
//! `TcpStream` and a `UnixStream` implement it); the `DaemonControl` implementation port stays in
//! `slipfork-edge`, which only speaks these contract values.

#![forbid(unsafe_code)]

pub mod control;
pub mod relay;
