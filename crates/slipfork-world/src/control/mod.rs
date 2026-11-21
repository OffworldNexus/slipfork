//! NDJSON control protocol between [`WorldSupervisor`](crate::WorldSupervisor) and
//! `slipfork-world-init`.
//!
//! One newline-delimited JSON frame travels over the init's stdin/stdout. stdin is also the
//! `O_CLOEXEC` lifeline: when the supervisor closes it (or exits), PID 1 stops every pod and exits.
//! The same frames are also served over `<pods>/control.sock` (mode `0600`) so a separate
//! `slipfork down` / `slipfork restart` process can reach the world without the supervisor.

mod dial;
mod frames;
mod stream;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use dial::dial_tcp;
pub use frames::{
    ControlError, FrameLines, InitFrame, OutputKind, PROTOCOL_VERSION, Stream, check_version,
    decode_init_frame, encode,
};
pub use stream::{
    OutputEvent, StreamClient, StreamFrame, StreamInput, exec, exit_code, finish, logs,
};

use crate::error::{Error, Result};
use crate::pod::PodSpec;

/// Socket name inside the world's tmpfs pod directory.
pub const CONTROL_SOCKET: &str = "control.sock";

/// `<pods>/control.sock`, refused before `bind(2)` when `sun_path` would overflow.
pub fn control_socket_path(pods_root: &Path) -> Result<PathBuf> {
    let path = pods_root.join(CONTROL_SOCKET);
    // `sockaddr_un.sun_path` is 108 bytes including the trailing NUL; keep a safety margin.
    if path.as_os_str().as_encoded_bytes().len() >= 100 {
        return Err(Error::Protocol {
            message: format!(
                "control socket `{}` exceeds the AF_UNIX path limit",
                path.display()
            ),
        });
    }
    Ok(path)
}

/// Requests the supervisor writes to the init's stdin or the control socket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "kebab-case")]
pub enum Request {
    /// Launch one OCI pod from an already-written tmpfs bundle.
    Launch {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
        /// Fully resolved pod value (config.json already written by the supervisor).
        pod: PodSpec,
    },
    /// Stop every pod, remove bundles, and reply before exiting on EOF.
    StopAll {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
    },
    /// Stop every pod and exit; `slipfork down` sends this over the pathname socket.
    Shutdown {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
    },
    /// Wait for one launched pod to exit and report its status code.
    Wait {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
        /// Host pid returned by `launch`.
        pid: i32,
        /// Upper bound in milliseconds.
        timeout_ms: u64,
    },
    /// Run one pod to completion (setup/hook step) while streaming its output.
    RunOnce {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
        /// Fully resolved pod value (config.json already written by the supervisor).
        pod: PodSpec,
        /// Upper bound in milliseconds for the whole run, enforced inside the world.
        timeout_ms: u64,
    },
    /// Dial `127.0.0.1:<port>` inside the world netns and hand the connection back over
    /// `control.sock` as one `SCM_RIGHTS` descriptor (status byte `0`) or a typed error (byte `1`).
    Dial {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
        /// World loopback port to connect to.
        port: u16,
    },
    /// Report every tracked pod and whether its process still exists.
    List {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
    },
    /// Restart one pod from its existing tmpfs bundle, or every pod when `service` is `None`.
    Restart {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
        /// Pod id to restart; `None` restarts every pod.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service: Option<String>,
    },
    /// Replay captured pod output, optionally following it live.
    Logs {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
        /// Only this pod id; `None` merges every pod.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service: Option<String>,
        /// Keep the stream open after the current tail until a stop or client close.
        #[serde(default)]
        follow: bool,
    },
    /// Run one command inside a live pod through `crun exec` from the init.
    Exec {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
        /// Pod id to exec into.
        pod: String,
        /// Command and arguments.
        argv: Vec<String>,
        /// Pipe stdin so the client can send [`Request::ExecInput`] frames.
        #[serde(default)]
        stdin: bool,
    },
    /// Bytes for a live `Exec` stdin; empty data closes the child's stdin.
    ExecInput {
        /// Protocol version.
        v: u32,
        /// Request id of the owning `Exec`.
        id: String,
        /// Base64 payload, or an empty string for EOF.
        data: String,
    },
}

impl Request {
    /// The request id echoed back by the init.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Launch { id, .. }
            | Self::StopAll { id, .. }
            | Self::Shutdown { id, .. }
            | Self::Restart { id, .. }
            | Self::Dial { id, .. }
            | Self::List { id, .. }
            | Self::RunOnce { id, .. }
            | Self::Wait { id, .. }
            | Self::Logs { id, .. }
            | Self::Exec { id, .. }
            | Self::ExecInput { id, .. } => id,
        }
    }
}

/// Decodes one supervisor request, rejecting an unknown version.
pub fn decode_request(line: &str) -> Result<Request> {
    let request: Request = serde_json::from_str(line).map_err(|source| Error::Protocol {
        message: format!("bad request frame: {source}"),
    })?;
    let version = match &request {
        Request::Launch { v, .. }
        | Request::StopAll { v, .. }
        | Request::Shutdown { v, .. }
        | Request::Restart { v, .. }
        | Request::Dial { v, .. }
        | Request::List { v, .. }
        | Request::RunOnce { v, .. }
        | Request::Wait { v, .. }
        | Request::Logs { v, .. }
        | Request::Exec { v, .. }
        | Request::ExecInput { v, .. } => *v,
    };
    check_version(version)?;
    Ok(request)
}
