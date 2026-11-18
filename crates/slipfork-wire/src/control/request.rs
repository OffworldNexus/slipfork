//! Control request bodies and their rendered summaries.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use slipfork_world::WorldOrigin;

use super::ControlSecret;
use super::env::ExecEnv;
use super::stream::WorldEntryCommand;

/// Request body of the local `slipfork snow resolve` diagnostic (daemon-owned vault access).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnowResolveRequest {
    /// Caller working directory; a relative `file` is resolved against it.
    pub cwd: PathBuf,
    /// Path to `snow.yml` (absolute, or relative to `cwd`).
    pub file: PathBuf,
    /// Mode to resolve (`local`, `cloud`, `all`, ...).
    pub mode: String,
    /// Optional component id whose environment groups are resolved.
    pub component: Option<String>,
    /// Return plaintext values instead of `[REDACTED]` (explicit caller request; `0600` socket).
    pub expose_secrets: bool,
}

/// Reply body of `snow_resolve`: already-rendered variables (redacted unless requested).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnowResolveSummary {
    /// One `VAR=value` entry per resolved variable; secret values are `[REDACTED]` by default.
    pub vars: BTreeMap<String, String>,
}

/// Endpoint selector shared by every world-scoped command (`--world <fork>` or explicit `cwd`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldSelector {
    /// Caller working directory; the daemon resolves the nearest world from it.
    pub cwd: PathBuf,
    /// Explicit `--world <fork>`; `None` means "resolve from `cwd`".
    pub world: Option<String>,
}

/// Seed-dump policy requested for `up`/`fork` (`--no-dump | --refresh-dump`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DumpRequest {
    /// Let the effective `ResourceStrategy` decide.
    Default,
    /// Skip seed dumps and run a clean `initdb` (`--no-dump`).
    NoDump,
    /// Require a live OSPP dump check (`--refresh-dump`).
    RefreshDump,
}

/// Resource strategy override requested for `up`/`fork` (`--resources local`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceRequest {
    /// Use the world-scoped `ResourceStrategy` defaults.
    Default,
    /// Force hermetic world-local resources (`--resources local`).
    Local,
}

/// Request body of `slipfork up`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpRequest {
    /// Caller working directory; never resolved by the daemon itself.
    pub cwd: PathBuf,
    /// Explicit `--world <fork>`; `None` means "resolve from `cwd`".
    pub world: Option<String>,
    /// Seed-dump flag.
    pub dump: DumpRequest,
    /// Resource override flag.
    pub resources: ResourceRequest,
    /// Mount the developer's host toolchain caches read-only after the ELF/libc ABI probe passes
    /// (`--host-caches`, `ADR-0009` §2.3). Never a `slipfork.toml` key.
    pub host_caches: bool,
}

/// Request body of `slipfork fork [FORK]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkRequest {
    /// Caller working directory; the fork worktree is created beneath its repository root.
    pub cwd: PathBuf,
    /// Fork name, or `None` to derive it from the requesting checkout's current branch.
    ///
    /// Only `slipforkd` resolves `None`: it asks `VcsWorkspaceFactory::current_branch`, sanitises
    /// the answer, and validates the result against the project's real `snow.yml` `domains[].id`
    /// set (`docs/design/dx-0.1.0-ux.md` §7.1). The client never runs `git` or reads `.git`.
    pub fork: Option<String>,
    /// Base revision for the new worktree (`None` uses the current branch).
    pub base: Option<String>,
    /// Seed-dump flag.
    pub dump: DumpRequest,
    /// Resource override flag.
    pub resources: ResourceRequest,
    /// Mount the developer's host toolchain caches read-only after the ABI probe passes.
    pub host_caches: bool,
}

/// Request body of `slipfork drop <fork>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DropForkRequest {
    /// Caller working directory; the fork is resolved beneath its repository root.
    pub cwd: PathBuf,
    /// Fork name.
    pub fork: String,
}

/// Outcome of `slipfork prune`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneReport {
    /// `true` when nothing was deleted (`--dry-run`).
    pub dry_run: bool,
    /// Orphaned worlds that were (or would be) reaped.
    pub worlds: Vec<String>,
    /// Unreferenced snapshots that were (or would be) reaped.
    pub snapshots: Vec<String>,
}

/// Request body of `slipfork logs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogsRequest {
    /// Caller working directory; the daemon resolves the nearest world from it.
    pub cwd: PathBuf,
    /// Explicit `--world <fork>`.
    pub world: Option<String>,
    /// Component/hook log to read (`None` means every retained log).
    pub target: Option<String>,
    /// Keep streaming new lines (`--follow`).
    pub follow: bool,
    /// Read the stderr stream instead of stdout.
    pub stderr: bool,
    /// Number of trailing lines to read.
    pub lines: Option<usize>,
}

/// Request body of `slipfork exec` / `shell` / `db` / `redis`.
///
/// `service` is the single typed selector between the two entry surfaces (`ADR-0009` §3):
/// `None` enters the world rootfs (`exec` / `shell`), while `Some("pg_databases.main")` enters
/// that running backing-service container through `WorldEngine::service_entry_command` (`db` /
/// `redis`). A stopped container fails closed with [`DaemonErrorKind::ServiceNotRunning`](super::DaemonErrorKind::ServiceNotRunning); there
/// is no app-rootfs fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecTargetRequest {
    /// Caller working directory; the daemon resolves the nearest world from it.
    pub cwd: PathBuf,
    /// Explicit `--world <fork>`.
    pub world: Option<String>,
    /// Declared backing-service id (`pg_databases.main`, `redis_clusters.cache`) to enter, if any.
    pub service: Option<String>,
    /// Argv the caller wants to run inside the world or service container.
    pub argv: Vec<String>,
}

/// Request body of `slipfork hook run --no-tty`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRunRequest {
    /// Caller working directory; the daemon resolves the nearest world from it.
    pub cwd: PathBuf,
    /// Explicit `--world <fork>`.
    pub world: Option<String>,
    /// `<component>:<phase>` hook id.
    pub hook: String,
}

/// Request body of `git-remote-slipfork` ref enumeration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRefsRequest {
    /// Caller working directory; the daemon resolves the repository from it.
    pub cwd: PathBuf,
    /// Fork whose two advertised refs are requested.
    pub fork: String,
}

/// The two advertised refs of a live fork world plus their resolved commit ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRefsSummary {
    /// Committed branch (`refs/heads/<fork>`).
    pub base: String,
    /// Synthesized dirty view (`refs/slipfork/<fork>/wip`).
    pub wip: String,
    /// Resolved base commit id.
    pub base_oid: String,
    /// Resolved dirty-view commit id.
    pub wip_oid: String,
    /// Host worktree backing the fork world.
    pub worktree: PathBuf,
}

/// Request body of `slipfork forward`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardRequest {
    /// Caller working directory; the daemon resolves the nearest world from it.
    pub cwd: PathBuf,
    /// Explicit `--world <fork>`.
    pub world: Option<String>,
    /// Target ids to forward (`pg_databases.main`); empty means every forwardable target.
    pub targets: Vec<String>,
}

/// One daemon-bound host listener.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardTarget {
    /// Stable target id (`pg_databases.main`).
    pub id: String,
    /// Bound host address (`127.0.0.1:43182`).
    pub host_addr: String,
    /// Printable URI (`postgres://127.0.0.1:43182/main`).
    pub uri: String,
}

/// Reply of a `forward` call; the connection then stays open for the bridge's lifetime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardSummary {
    /// World the hosts bridge into.
    pub world: String,
    /// Bound host listeners.
    pub targets: Vec<ForwardTarget>,
}

/// Result of `slipfork auth`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthSummary {
    /// Paired device key id (`p256:<66 hex>`).
    pub key_id: String,
    /// Signing backend (`software:file-0600`).
    pub backend: String,
    /// Account the remote provider reported, when it reports one.
    pub account: Option<String>,
    /// Machine name the server approved, when it reports one.
    pub machine: Option<String>,
    /// The server-supplied `<url>#handle=<handle>` pairing page that was opened (redacted by
    /// `Debug`; the single-use handle is zeroized when the owning `LivePairingUrl` drops).
    pub pairing_url: ControlSecret,
}

/// Everything the thin CLI needs to enter a world with inherited native stdio (`H-010` #1).
///
/// This is the **only** control-port reply permitted to carry a plaintext [`ExecEnv`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldExecTarget {
    /// World id (`<project>` or `<project>:<fork>`).
    pub world: String,
    /// The world's single public origin.
    pub origin: WorldOrigin,
    /// Child argv prefix of the engine's entry program (for example `nsenter`) to spawn.
    pub entry: WorldEntryCommand,
    /// Resolved world environment (redacted by `Debug`).
    pub env: ExecEnv,
    /// Argv to run inside the world after `entry`.
    pub target_argv: Vec<String>,
}
