//! Streamed-output chunks, the child-stream source, service lifecycle state, and the service exec
//! recipe — every value the control wire carries verbatim.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Lifecycle state of one world-owned service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    /// The service process is alive.
    Running,
    /// The service process exited on its own (crash or clean exit); it is never restarted.
    Exited,
    /// An engine stop (or a parent world teardown/freeze) stopped it.
    Stopped,
}

impl ServiceState {
    /// Decode the bit representation an engine adapter stores in its supervision cell.
    ///
    /// Unknown bits fail closed to [`ServiceState::Stopped`]: a supervision cell must never be
    /// reported as a live service just because its value is unrecognized.
    pub fn from_bits(bits: u8) -> Self {
        match bits {
            0 => Self::Running,
            1 => Self::Exited,
            _ => Self::Stopped,
        }
    }

    /// The bit representation an engine adapter stores in its supervision cell.
    pub fn to_bits(self) -> u8 {
        match self {
            Self::Running => 0,
            Self::Exited => 1,
            Self::Stopped => 2,
        }
    }
}

/// The thin `slipfork` CLI spawns `program` with `args_prefix` followed by the target argv, from
/// `/`, with native `Stdio::inherit()` — zero `SCM_RIGHTS` descriptors cross the CLI <->
/// `slipforkd` boundary (`H-010` #1).
///
/// The recipe always names a **running service container** (`crun exec` into a pod or a
/// backing-service container); the container's own bundle owns its working directory and complete
/// environment, so the spawner only chooses `/` as its host chdir.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceExec {
    /// Executable of the engine's entry program (for example `nsenter`).
    pub program: PathBuf,
    /// Arguments that must precede the target argv.
    pub args_prefix: Vec<String>,
}

/// Which child stream a chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamSource {
    /// Child stdout.
    Stdout,
    /// Child stderr.
    Stderr,
}

/// One streamed chunk of child output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamChunk {
    /// Originating stream.
    pub source: StreamSource,
    /// Raw bytes (not necessarily UTF-8).
    pub bytes: Vec<u8>,
}

/// Version of the retained-log wire shape (`LogChunk` + the `logs` NDJSON document).
///
/// v2 added the capture-time `ts_ms`, the typed `stream` (`stdout` / `stderr` / `event`), and the
/// `restarted` divider event. A peer that speaks any other version fails closed: `LogChunk` refuses
/// to deserialize rather than silently rendering a chunk whose time or stream it cannot trust.
pub const CONTROL_PROTOCOL_VERSION: u32 = 2;

/// Milliseconds since the Unix epoch, read once at capture time.
pub fn epoch_millis_now() -> u64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        // A clock set before the epoch is still a real instant: saturation keeps it monotonic
        // rather than silently stamping `0` (which would sort a chunk before every real one).
        .unwrap_or_default();
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// The kind of line a retained [`LogChunk`] carries: one child stream, or a daemon lifecycle event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    /// Child stdout.
    Stdout,
    /// Child stderr.
    Stderr,
    /// A daemon-emitted lifecycle event (`── restarted <time> ──`), never child bytes.
    Event,
}

impl From<StreamSource> for LogStream {
    fn from(source: StreamSource) -> Self {
        match source {
            StreamSource::Stdout => Self::Stdout,
            StreamSource::Stderr => Self::Stderr,
        }
    }
}

/// One attributed chunk of a **retained** component/hook log.
///
/// [`StreamChunk`] is the un-attributed stream a live exec or hook run yields; a retained log chunk
/// also names the component or hook id it came from and the wall-clock instant its bytes were read,
/// so `slipfork logs` orders interleaved output across every service and hook without the renderer
/// guessing which service produced which line or when. The id is the engine's own record
/// (`ServiceLogs::target`) for a service and the daemon's hook record for a hook — never the
/// caller's request string.
///
/// `stream == LogStream::Event` is the daemon's restart marker: `target` is empty and `bytes` are
/// deliberately empty (the renderer formats `── restarted <time> ──`), so an event can never be
/// mistaken for child output or carry bytes a prefix could shear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogChunk {
    /// Wire shape version this chunk was produced for (always [`CONTROL_PROTOCOL_VERSION`]).
    pub protocol: u32,
    /// Capture time in milliseconds since the Unix epoch, stamped by the reader that read the bytes.
    pub ts_ms: u64,
    /// Component id (`api`) or hook id (`api:pre-deploy`); empty for a [`LogStream::Event`].
    pub target: String,
    /// Which stream (or daemon event) this chunk belongs to.
    pub stream: LogStream,
    /// Raw bytes (not necessarily UTF-8); empty for an event.
    pub bytes: Vec<u8>,
}

/// Wire shape [`LogChunk`] deserializes through, so an unknown protocol version is refused.
#[derive(Deserialize)]
struct LogChunkFrame {
    protocol: u32,
    ts_ms: u64,
    target: String,
    stream: LogStream,
    bytes: Vec<u8>,
}

impl<'de> Deserialize<'de> for LogChunk {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let frame = LogChunkFrame::deserialize(deserializer)?;
        if frame.protocol != CONTROL_PROTOCOL_VERSION {
            return Err(serde::de::Error::custom(format!(
                "unsupported logs protocol v{}: this build speaks v{CONTROL_PROTOCOL_VERSION}",
                frame.protocol
            )));
        }
        Ok(Self {
            protocol: frame.protocol,
            ts_ms: frame.ts_ms,
            target: frame.target,
            stream: frame.stream,
            bytes: frame.bytes,
        })
    }
}

impl LogChunk {
    /// Attribute one capture-time [`StreamChunk`] to the component/hook `target` that produced it.
    pub fn output(target: impl Into<String>, ts_ms: u64, chunk: StreamChunk) -> Self {
        Self {
            protocol: CONTROL_PROTOCOL_VERSION,
            ts_ms,
            target: target.into(),
            stream: chunk.source.into(),
            bytes: chunk.bytes,
        }
    }

    /// The daemon's restart/down+up epoch marker; the renderer owns its human formatting.
    pub fn divider(ts_ms: u64) -> Self {
        Self {
            protocol: CONTROL_PROTOCOL_VERSION,
            ts_ms,
            target: String::new(),
            stream: LogStream::Event,
            bytes: Vec::new(),
        }
    }

    /// `true` for a restart/down+up epoch marker.
    pub fn is_event(&self) -> bool {
        self.stream == LogStream::Event
    }
}
