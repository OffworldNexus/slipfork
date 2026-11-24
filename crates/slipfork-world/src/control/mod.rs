//! NDJSON control protocol between [`WorldSupervisor`](crate::WorldSupervisor) and
//! `slipfork-world-init`.
//!
//! One newline-delimited JSON frame travels over the init's stdin/stdout. stdin is also the
//! `O_CLOEXEC` lifeline: when the supervisor closes it (or exits), PID 1 stops every pod and exits.
//! The same frames are also served over `<pods>/control.sock` (mode `0600`) so a separate
//! `slipfork down` / `slipfork restart` process can reach the world without the supervisor.

mod config;
mod dial;
mod frames;
mod stream;

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use slipfork_forkfile::{BucketPolicy, SecretValue};

pub use config::{configure_s3, launch_pod, list_pods, run_once};
pub use dial::dial_tcp;
pub use frames::{
    ControlError, FrameLines, InitFrame, OutputKind, PROTOCOL_VERSION, Stream, check_version,
    decode_init_frame, encode,
};
pub use stream::{
    OutputEvent, StreamClient, StreamFrame, StreamInput, exec, exit_code, finish, logs, logs_window,
};

use crate::error::{Error, Result};
use crate::pod::PodSpec;

/// Socket name inside the world's tmpfs pod directory.
pub const CONTROL_SOCKET: &str = "control.sock";

/// User remedy for a violated or version-skewed NDJSON control frame.
pub const REMEDY_PROTOCOL: &str = "run `slipfork down` then `slipfork up`, or restart the daemon with `systemctl --user restart slipfork`";

/// `<pods>/control.sock`, refused before `bind(2)` when `sun_path` would overflow.
pub fn control_socket_path(pods_root: &Path) -> Result<PathBuf> {
    unix_path(pods_root.join(CONTROL_SOCKET), "control socket")
}

/// `<pods>/pty-<seq>.sock`: the `--console-socket` path handed to one `crun exec --tty`.
pub fn console_socket_path(pods_root: &Path, seq: u64) -> Result<PathBuf> {
    unix_path(
        pods_root.join(format!("pty-{seq}.sock")),
        "PTY console socket",
    )
}

fn unix_path(path: PathBuf, label: &str) -> Result<PathBuf> {
    // `sockaddr_un.sun_path` is 108 bytes including the trailing NUL; keep a safety margin.
    if path.as_os_str().as_encoded_bytes().len() >= 100 {
        return Err(Error::Protocol {
            message: format!(
                "{label} `{}` exceeds the AF_UNIX path limit",
                path.display()
            ),
        });
    }
    Ok(path)
}

/// One bucket the native S3 server must serve.
#[derive(Debug, Clone)]
pub struct S3BucketSpec {
    /// Validated bucket name.
    pub name: String,
    /// Anonymous-read policy.
    pub policy: BucketPolicy,
    /// SigV4 access key id.
    pub access_key: String,
    /// SigV4 signing secret; never rendered.
    pub secret_key: SecretValue,
}

impl PartialEq for S3BucketSpec {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.policy == other.policy
            && self.access_key == other.access_key
            && self.secret_key.expose_secret() == other.secret_key.expose_secret()
    }
}

impl Serialize for S3BucketSpec {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut row = serializer.serialize_struct("S3BucketSpec", 4)?;
        row.serialize_field("name", &self.name)?;
        row.serialize_field("policy", self.policy.keyword())?;
        row.serialize_field("access_key", &self.access_key)?;
        row.serialize_field("secret_key", self.secret_key.expose_secret())?;
        row.end()
    }
}

impl<'de> Deserialize<'de> for S3BucketSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct SpecVisitor;

        impl<'de> Visitor<'de> for SpecVisitor {
            type Value = S3BucketSpec;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an S3 bucket spec with name, policy, and credentials")
            }

            fn visit_map<A: de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<S3BucketSpec, A::Error> {
                let mut name = None;
                let mut policy = None;
                let mut access_key = None;
                let mut secret_key = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "name" => name = Some(map.next_value::<String>()?),
                        "policy" => policy = Some(map.next_value::<String>()?),
                        "access_key" => access_key = Some(map.next_value::<String>()?),
                        "secret_key" => secret_key = Some(map.next_value::<String>()?),
                        other => {
                            let _ = map.next_value::<de::IgnoredAny>()?;
                            return Err(de::Error::unknown_field(
                                other,
                                &["name", "policy", "access_key", "secret_key"],
                            ));
                        }
                    }
                }
                let name = name.ok_or_else(|| de::Error::missing_field("name"))?;
                let policy = policy.ok_or_else(|| de::Error::missing_field("policy"))?;
                let policy = BucketPolicy::parse(&policy).ok_or_else(|| {
                    de::Error::custom(format!("unknown bucket policy `{policy}`"))
                })?;
                Ok(S3BucketSpec {
                    name,
                    policy,
                    access_key: access_key.ok_or_else(|| de::Error::missing_field("access_key"))?,
                    secret_key: SecretValue::new(
                        secret_key.ok_or_else(|| de::Error::missing_field("secret_key"))?,
                    ),
                })
            }
        }

        deserializer.deserialize_map(SpecVisitor)
    }
}

/// Everything the world's native S3 server needs to bind and serve one world.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct S3WorldConfig {
    /// World-loopback address the init binds exactly once (`127.0.0.1:8333`).
    pub listen_addr: SocketAddr,
    /// `<world>/s3` root; objects live under `<root>/<bucket>/objects`.
    pub s3_root: PathBuf,
    /// Buckets and their credentials.
    pub buckets: Vec<S3BucketSpec>,
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
    /// Bind the native S3 server on the injected address and serve the declared buckets.
    ConfigureS3 {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
        /// Listener address plus the world's S3 root and buckets.
        config: S3WorldConfig,
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
    /// Stop one tracked pod: kill its process group, `crun delete` it, and remove its bundle.
    Stop {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
        /// Pod id to stop; must be a live tracked pod.
        pod: String,
    },
    /// Replay captured pod output, optionally following it live.
    Logs {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the reply.
        id: String,
        /// Only this pod id; `None` merges `pods` (or every pod when `pods` is `None`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service: Option<String>,
        /// Optional pod allowlist when `service` is `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pods: Option<Vec<String>>,
        /// Keep the stream open after the current tail until a stop or client close.
        #[serde(default)]
        follow: bool,
        /// Only replay lines at or after this epoch-millisecond timestamp.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since_ms: Option<u64>,
        /// Only replay lines at or before this epoch-millisecond timestamp.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until_ms: Option<u64>,
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
    /// Allocate a real kernel PTY inside a live pod and hand its master over `SCM_RIGHTS`.
    ///
    /// The init answers with one `sendmsg` carrying payload `pty` plus the `/dev/pts/N` master
    /// descriptor, then keeps the descriptor open until the exec child exits; the caller owns
    /// raw terminal I/O on the received descriptor.
    ExecPty {
        /// Protocol version.
        v: u32,
        /// Request id echoed back in the final reply and matched by [`Request::ExecResize`].
        id: String,
        /// Pod id to exec into.
        pod: String,
        /// Command and arguments.
        argv: Vec<String>,
        /// Initial terminal columns.
        cols: u16,
        /// Initial terminal rows.
        rows: u16,
    },
    /// Resize the PTY of a live [`Request::ExecPty`] with `ioctl(TIOCSWINSZ)`.
    ExecResize {
        /// Protocol version.
        v: u32,
        /// Request id of the owning `ExecPty`.
        id: String,
        /// New terminal columns.
        cols: u16,
        /// New terminal rows.
        rows: u16,
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
            | Self::Stop { id, .. }
            | Self::Dial { id, .. }
            | Self::ConfigureS3 { id, .. }
            | Self::List { id, .. }
            | Self::RunOnce { id, .. }
            | Self::Wait { id, .. }
            | Self::Logs { id, .. }
            | Self::Exec { id, .. }
            | Self::ExecInput { id, .. }
            | Self::ExecPty { id, .. }
            | Self::ExecResize { id, .. } => id,
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
        | Request::Stop { v, .. }
        | Request::Dial { v, .. }
        | Request::ConfigureS3 { v, .. }
        | Request::List { v, .. }
        | Request::RunOnce { v, .. }
        | Request::Wait { v, .. }
        | Request::Logs { v, .. }
        | Request::Exec { v, .. }
        | Request::ExecInput { v, .. }
        | Request::ExecPty { v, .. }
        | Request::ExecResize { v, .. } => *v,
    };
    check_version(version)?;
    Ok(request)
}
