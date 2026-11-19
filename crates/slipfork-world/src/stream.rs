//! Streamed-output chunks, the child-stream source, service lifecycle state, and the engine's
//! entry-command value — every value the control wire carries verbatim.

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

/// Engine-supplied argv prefix for entering a live world inheriting the caller's native stdio.
///
/// What the caller is about to do with the entry command.
///
/// `Exec` appends the requested argv to the bridge's prefix (hooks, one-shot `exec`, service
/// clients); `Interactive` bakes the single requested program into a PTY relay inside the world,
/// because a process in a child PID namespace cannot adopt the caller's terminal directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryKind {
    /// Non-interactive: append the target argv to the entry prefix.
    #[default]
    Exec,
    /// Interactive: the entry command carries the PTY relay and the single target program.
    Interactive,
}

/// The thin `slipfork` CLI spawns `program` with `args_prefix` followed by the target argv,
/// `cwd` as its working directory, and native `Stdio::inherit()` — zero `SCM_RIGHTS` descriptors
/// cross the CLI <-> `slipforkd` boundary (`H-010` #1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldEntryCommand {
    /// Executable of the engine's entry program (for example `nsenter`).
    pub program: PathBuf,
    /// Arguments that must precede the target argv.
    ///
    /// The entry program starts the target in a directory that exists **inside** the world: a
    /// host pathname is unreachable after the world's `pivot_root`, so the entry shell's first
    /// `getcwd(2)` would fail there.
    pub args_prefix: Vec<String>,
    /// Working directory of the process that spawns [`Self::program`].
    ///
    /// `/` on purpose: it is the one directory that exists both on the host and inside every
    /// world rootfs, so the spawner can `chdir` before `exec`ing the entry program without
    /// touching a path the world owns.
    pub cwd: PathBuf,
    /// Whether `args_prefix` already consumes the caller's target argv (e.g. an interactive relay).
    #[serde(default)]
    pub consumes_target_argv: bool,
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

/// One attributed chunk of a **retained** component/hook log.
///
/// [`StreamChunk`] is the un-attributed stream a live exec or hook run yields; a retained log chunk
/// also names the component or hook id it came from, so `slipfork logs` can attribute interleaved
/// output from several sources without the renderer guessing which service produced which line.
/// The id is the engine's own record (`ServiceLogs::target`) for a service and the daemon's hook
/// record for a hook — never the caller's request string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogChunk {
    /// Component id (`api`) or hook id (`api:pre-deploy`) that produced the bytes.
    pub target: String,
    /// Originating stream.
    pub source: StreamSource,
    /// Raw bytes (not necessarily UTF-8).
    pub bytes: Vec<u8>,
}

impl LogChunk {
    /// Attribute one retained [`StreamChunk`] to the component/hook `target` that produced it.
    pub fn attribute(target: impl Into<String>, chunk: StreamChunk) -> Self {
        Self {
            target: target.into(),
            source: chunk.source,
            bytes: chunk.bytes,
        }
    }
}
