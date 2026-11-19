//! The typed `DaemonControl` contract (`ADR-0008` §4).
//!
//! The thin `slipfork` CLI and `slipforkd` exchange exactly one `\n`-terminated JSON frame per
//! message over `<runtime_dir>/daemon.sock` (`0600`, never `$XDG_RUNTIME_DIR` guessed). The
//! `DaemonControl` implementation port lives in `slipfork-edge`; this module owns the contract it
//! speaks — requests, replies, view models, framing, and the socket-path budget — so the engine,
//! the edge, and the CLI share one definition with zero mapping structs.
//!
//! Every `cwd`-sensitive request carries an explicit `cwd: PathBuf`, so a daemon never calls
//! `current_dir()`. [`WorldExecTarget`] is the **only** reply permitted to carry a plaintext
//! [`slipfork_world::ExecEnv`]; every failure rendered by this port is a [`DaemonError`] with a redacted message.

pub mod frame;
pub mod protocol;
pub mod request;
pub mod socket;
pub mod view;

use thiserror::Error;

pub use frame::{FrameReader, unexpected_reply, write_chunk, write_log, write_reply};
pub use protocol::{ControlSecret, DaemonError, DaemonErrorKind, DaemonReply, DaemonRequest};
pub use request::{
    AuthSummary, DropForkRequest, DumpRequest, ExecService, ExecTargetRequest, FeatureSelection,
    ForkRequest, ForwardRequest, ForwardSummary, ForwardTarget, HookRunRequest, LogsRequest,
    PruneReport, ResourceRequest, RestartRequest, UpRequest, WorkspaceRefsRequest,
    WorkspaceRefsSummary, WorldExecTarget, WorldSelector, WorldSelectorRepo,
};
pub use socket::{CONTROL_SOCKET_FILE, control_socket_path};
pub use view::{
    BootEvent, CONTROL_VIEW_VERSION, DaemonStatus, IngressError, IngressInfo, ResourceSummary,
    WorldProcessReport, WorldServiceSummary, WorldState, WorldSummary, WorldUpSummary,
};

/// Version of the retained-log wire shape (`LogChunk`, the `logs` reply, and its NDJSON document).
///
/// v2 stamps a capture-time `ts_ms` on every chunk and types the stream as `stdout` / `stderr` /
/// `event` (the restart divider). A peer speaking another version fails closed: serde refuses the
/// chunk rather than rendering output it cannot order or attribute.
pub const CONTROL_PROTOCOL_VERSION: u32 = slipfork_world::CONTROL_PROTOCOL_VERSION;

/// Hard cap on one JSON-line control frame (request or reply).
pub const MAX_CONTROL_FRAME: usize = 64 * 1024;

/// Largest streamed chunk payload (a [`slipfork_world::StreamChunk`] or [`slipfork_world::LogChunk`]'s bytes) emitted per frame,
/// so JSON escaping stays under the frame cap.
pub const MAX_STREAM_CHUNK: usize = 8 * 1024;

/// A control call failure with a diagnostic that never contains a secret value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ControlError {
    /// The control socket could not be read, written, or connected.
    #[error("{operation} failed on the daemon control socket: {message}")]
    Io {
        /// What was attempted (`connect`, `read request`, `write reply`).
        operation: &'static str,
        /// Underlying OS error.
        message: String,
    },

    /// A control frame exceeded the 64 KiB JSON-line cap or could not be encoded.
    #[error("invalid daemon control frame: {reason}")]
    Frame {
        /// What is wrong with the frame.
        reason: String,
    },

    /// A control peer sent a frame that is not a valid request/reply.
    #[error("daemon control protocol error: {reason}")]
    Protocol {
        /// What is wrong with the message.
        reason: String,
    },

    /// The daemon answered with a typed remote error.
    #[error("{0}")]
    Remote(DaemonError),
}
