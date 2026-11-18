//! The complete typed `DaemonControl` port the thin `slipfork` CLI drives (`ADR-0008` §4).
//!
//! `slipforkd` implements [`DaemonControl`] in-process; [`client::DaemonControlClient`] speaks the
//! same trait over `<runtime_dir>/daemon.sock` (`0600`, bounded 64 KiB JSON-line frames) and
//! [`server::DaemonControlServer`] exposes an injected implementation on that socket.
//!
//! Every `cwd`-sensitive request (`UpRequest`, `WorldSelector`, `ForkRequest`, `LogsRequest`,
//! `HookRunRequest`, `ExecTargetRequest`, `ForwardRequest`) carries an explicit `cwd: PathBuf`, so
//! the daemon never calls `current_dir()`. [`DaemonControl::exec_target`]'s [`WorldExecTarget`] is
//! the **only** reply permitted to carry a plaintext [`ExecEnv`]; every error rendered by this port
//! is a [`DaemonError`] with a redacted message.
//!
//! The wire schema below is the single home of the CLI <-> daemon contract: one request frame per
//! connection, zero stream upgrades, and zero file descriptors crossing the socket.

pub mod client;
pub mod server;
pub(crate) mod wire;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use slipfork_engine::{
    BaseLineage, CacheOutcome, RootfsLineage, SeedLineage, ServiceImageLineage, ServiceKind,
    WorldLineage, WorldServiceInfo,
};
use slipfork_engine::{ExecEnv, LogChunk, StreamChunk, WorldEntryCommand};
use slipfork_world::WorldOrigin;

use crate::EdgeError;

/// File name of the user control socket inside `slipforkd`'s explicit `--runtime-dir`.
pub const CONTROL_SOCKET_FILE: &str = "daemon.sock";

/// Hard cap on one JSON-line control frame (request or reply).
pub const MAX_CONTROL_FRAME: usize = 64 * 1024;

/// Largest streamed chunk payload (a `StreamChunk` or `LogChunk`'s bytes) emitted per frame, so
/// JSON escaping stays under the frame cap.
pub const MAX_STREAM_CHUNK: usize = 8 * 1024;

/// The HTTP ingress listeners `slipforkd` actually serves, plus the one port every minted
/// `WorldOrigin` carries (`docs/design/dx-0.1.0-ux.md` §3).
///
/// `addrs` is the bound (or `LISTEN_FDS`-adopted) listener set — `127.0.0.1:80` **and**
/// `[::1]:80` under socket activation, or one ephemeral `127.0.0.1:<port>` under
/// `--listen 127.0.0.1:0`. Its order is **non-semantic**. `port` is never stored independently:
/// [`IngressInfo::new`] derives it from the listeners and refuses an empty set, an unbound `:0`,
/// or listeners that disagree on the port, so a daemon can never report a guessed `:0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "IngressInfoFrame")]
pub struct IngressInfo {
    addrs: Vec<SocketAddr>,
    port: u16,
}

/// Wire shape [`IngressInfo`] deserializes through, so a forged frame is re-validated.
#[derive(Deserialize)]
struct IngressInfoFrame {
    addrs: Vec<SocketAddr>,
    port: u16,
}

impl IngressInfo {
    /// Derive the single ingress port from the bound listener addresses.
    pub fn new(addrs: Vec<SocketAddr>) -> Result<Self, EdgeError> {
        let mut listeners = addrs.iter();
        let Some(first) = listeners.next() else {
            return Err(EdgeError::IngressMissing);
        };
        let port = first.port();
        if port == 0 {
            return Err(EdgeError::IngressPortInvalid {
                reason: format!("the HTTP listener {first} carries the unassigned port :0"),
            });
        }
        for addr in listeners {
            if addr.port() != port {
                return Err(EdgeError::IngressPortInvalid {
                    reason: format!(
                        "the HTTP listeners {first} and {addr} disagree on the ingress port"
                    ),
                });
            }
        }
        Ok(Self { addrs, port })
    }

    /// Bound HTTP ingress listeners (order is non-semantic).
    pub fn addrs(&self) -> &[SocketAddr] {
        &self.addrs
    }

    /// The single port every `WorldOrigin` minted by this daemon carries.
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl TryFrom<IngressInfoFrame> for IngressInfo {
    type Error = EdgeError;

    fn try_from(frame: IngressInfoFrame) -> Result<Self, Self::Error> {
        let info = Self::new(frame.addrs)?;
        if info.port != frame.port {
            return Err(EdgeError::IngressPortInvalid {
                reason: format!(
                    "the reported origin port {} is not carried by any bound listener ({})",
                    frame.port,
                    info.addrs
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        Ok(info)
    }
}

/// Process-level view of `slipforkd` and its live worlds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    /// Pid of the daemon answering the control socket.
    pub pid: u32,
    /// Bound HTTP ingress listeners and the single origin port they serve.
    pub ingress: IngressInfo,
    /// Planned static forward port, when a forward listener is bound.
    pub forward_port: Option<u16>,
    /// Every world the daemon currently knows about, each with exactly one origin URL.
    pub worlds: Vec<WorldSummary>,
}

/// Lifecycle state of one world as reported by `slipfork list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldState {
    /// World services are running.
    Running,
    /// Boot failed; the world is preserved for inspection.
    Failed,
    /// World services were stopped (`slipfork down`).
    Stopped,
}

/// One row of `slipfork list` / `slipfork status` (`docs/design/dx-0.1.0-ux.md` §3).
///
/// `world` is the canonical `<project>` / `<project>:<fork>` id printed by every command and
/// pasteable back into `--world`; the typed `WorldId` newtype is the `0.2.0` target state
/// (`ADR-0005` §3), so `0.1.0` carries the `registry::world_id` string form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldSummary {
    /// Canonical world id (`<project>` or `<project>:<fork>`).
    pub world: String,
    /// Canonical world: `None`; fork: `Some(<project>)`. Never derived by splitting `world`.
    pub parent: Option<String>,
    /// Lifecycle state.
    pub state: WorldState,
    /// The world's single public origin.
    pub origin: WorldOrigin,
    /// Host worktree backing the world.
    pub worktree: PathBuf,
    /// This world's own build lineage — never another world's facts.
    pub lineage: WorldLineage,
}

/// One `[resources]` diagnostic line (`docs/design/target-dx.md` §1.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceSummary {
    /// Declared resource id (`main`, `media`) — the bare `ResourceDiagnostic::id`, never a
    /// kind-qualified `pg_databases.<id>` service id.
    pub id: String,
    /// Effective `ResourceStrategy`.
    pub strategy: String,
    /// `remote` or `local`.
    pub resolved: String,
    /// Why the effective strategy was chosen.
    pub reason: String,
}

/// Result of `slipfork up` / `slipfork restart` / `slipfork fork` (`docs/design/dx-0.1.0-ux.md` §4).
///
/// `changed` and `restarted` are the two facts that tell the caller whether anything happened:
/// an idempotent `up` of a `Running` world is `changed: false` with nothing touched; every boot is
/// `changed: true`; `restart` is `changed: true` only when it started a service or ran a hook.
/// `restarted` names only the ids this run brought to `Running` — each one is a `services[].id` of
/// this same world, so the join is sound — and is empty when `changed` is false.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldUpSummary {
    /// World id (`<project>` or `<project>:<fork>`).
    pub world: String,
    /// The world's lifecycle state after the call.
    pub state: WorldState,
    /// `false` only when the world was already `Running` and nothing was rebuilt or restarted.
    pub changed: bool,
    /// Ids of this world's services this call started, unique, empty when `changed: false`.
    pub restarted: Vec<String>,
    /// Host worktree backing the world.
    pub worktree: PathBuf,
    /// The world's single public origin.
    pub origin: WorldOrigin,
    /// This world's own build lineage.
    pub lineage: WorldLineage,
    /// Every component and backing-service row of the world, so no second call is needed.
    pub services: Vec<WorldServiceSummary>,
    /// One diagnostic line per resolved resource.
    pub resources: Vec<ResourceSummary>,
    /// This world state's `next` commands (`docs/design/dx-0.1.0-ux.md` §1).
    pub next: Vec<String>,
}

/// The `next` commands of one world state (`docs/design/dx-0.1.0-ux.md` §1 is the only table).
///
/// Rows are the bare `<verb>` commands of the state; a client that renders a world other than the
/// cwd-selected one appends its own `--world <WORLD>`, so the daemon never guesses a selector.
#[must_use]
pub fn world_next_commands(state: WorldState) -> Vec<String> {
    let verbs: &[&str] = match state {
        WorldState::Running => &["open", "db", "logs", "restart", "down"],
        WorldState::Failed => &["logs", "restart", "down"],
        WorldState::Stopped => &["up", "logs", "down"],
    };
    verbs
        .iter()
        .map(|verb| format!("slipfork {verb}"))
        .collect()
}

/// One row of the per-world process table `slipfork ps` renders (`docs/design/dx-0.1.0-ux.md` §5).
///
/// The engine's [`WorldServiceInfo`] carries the row's identity, kind, lifecycle state, and
/// measured facts; `world` names the world the row belongs to, so one report can never mix worlds.
/// `info.id` is unique within one world's row set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldServiceSummary {
    /// Canonical world id the row belongs to.
    pub world: String,
    /// The engine's record for this service, flattened into the same JSON object.
    #[serde(flatten)]
    pub info: WorldServiceInfo,
}

/// One world's process view: the world state, its lineage, and every service row — read once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldProcessReport {
    /// The world's own lifecycle state, read by the same call that read the rows.
    pub state: WorldState,
    /// This world's build lineage.
    pub lineage: WorldLineage,
    /// Every component and backing-service row of this world, ids unique.
    pub services: Vec<WorldServiceSummary>,
}

impl WorldProcessReport {
    /// Assemble one world's report, refusing a row set with a duplicated service id so
    /// `services[].id` can never stop being a 1:1 join key.
    pub fn new(
        state: WorldState,
        lineage: WorldLineage,
        services: Vec<WorldServiceSummary>,
    ) -> Result<Self, EdgeError> {
        let mut seen = BTreeSet::new();
        for service in &services {
            if !seen.insert(service.info.id.as_str()) {
                return Err(EdgeError::Control(DaemonError::new(
                    DaemonErrorKind::Internal,
                    format!(
                        "world '{}' reported two services with the id '{}'; process rows must have unique ids",
                        service.world, service.info.id
                    ),
                )));
            }
        }
        Ok(Self {
            state,
            lineage,
            services,
        })
    }
}
/// One live boot event streamed before [`WorldUpSummary`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum BootEvent {
    /// One `[resources]` decision line.
    Resources {
        /// The decided resource.
        resource: ResourceSummary,
    },
    /// A `<component>:<phase>` hook started.
    Hook {
        /// Hook id (`api:pre-deploy`).
        id: String,
        /// Exact argv being run.
        argv: Vec<String>,
    },
    /// A live chunk of hook output.
    HookOutput {
        /// Hook id.
        id: String,
        /// Captured output chunk.
        chunk: StreamChunk,
    },
    /// A hook exited non-zero; the world is kept alive for inspection (`ADR-0008` §4).
    HookFailed {
        /// Hook id (`api:pre-deploy`).
        id: String,
        /// Exact argv that failed.
        argv: Vec<String>,
        /// Process exit code.
        exit_code: i32,
        /// World id the hook ran in.
        world: String,
        /// Retained log file holding the full un-truncated output.
        log_path: PathBuf,
    },
    /// A component's `internal_ports` entry accepts connections.
    Ready {
        /// Component id.
        component: String,
        /// In-world port that became ready.
        port: u16,
    },
}

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
/// `redis`). A stopped container fails closed with [`DaemonErrorKind::ServiceNotRunning`]; there
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
/// This is the **only** [`DaemonControl`] reply permitted to carry a plaintext [`ExecEnv`].
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

/// Typed reason a control call failed, preserved across the socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonErrorKind {
    /// A request that would need the HTTP ingress cannot name one bound port.
    IngressMissing,
    /// The daemon's ingress report has no single real, bound origin port.
    IngressPortInvalid,
    /// The requested world does not exist.
    UnknownWorld,
    /// `restart` was asked for a world this daemon never booted (or already stopped).
    NotRunning,
    /// A requested backing service is not running as a container.
    ServiceNotRunning,
    /// The caller sent a malformed request.
    BadRequest,
    /// The caller sent a request that does not fit the daemon's world model.
    Rejected,
    /// A frame exceeded the 64 KiB cap.
    FrameTooLarge,
    /// The daemon hit an engine or I/O failure.
    Internal,
}

/// A control-call failure with a diagnostic that never contains a secret value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonError {
    /// Typed reason, stable across the wire.
    pub kind: DaemonErrorKind,
    /// Actionable, secret-free diagnostic.
    pub message: String,
}

impl DaemonError {
    /// Build one typed error.
    pub fn new(kind: DaemonErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<&EdgeError> for DaemonError {
    fn from(err: &EdgeError) -> Self {
        if let EdgeError::Control(error) = err {
            return error.clone();
        }
        let kind = match err {
            EdgeError::IngressMissing => DaemonErrorKind::IngressMissing,
            EdgeError::IngressPortInvalid { .. } => DaemonErrorKind::IngressPortInvalid,
            EdgeError::UnknownHost { .. } | EdgeError::UnknownIngressPath { .. } => {
                DaemonErrorKind::UnknownWorld
            }
            EdgeError::ForwardSpec { .. } => DaemonErrorKind::Rejected,
            EdgeError::ControlFrame { .. } => DaemonErrorKind::FrameTooLarge,
            EdgeError::ControlProtocol { .. } => DaemonErrorKind::BadRequest,
            _ => DaemonErrorKind::Internal,
        };
        Self::new(kind, err.to_string())
    }
}

impl From<EdgeError> for DaemonError {
    fn from(err: EdgeError) -> Self {
        Self::from(&err)
    }
}

/// One request frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum DaemonRequest {
    /// Daemon process status plus every world (each with exactly one origin).
    Status,
    /// Validate/resolve `snow.yml` through the daemon-owned `0600` vault.
    SnowResolve(SnowResolveRequest),
    /// Boot a world; [`DaemonReply::Event`] frames precede the [`DaemonReply::World`] reply.
    Up(UpRequest),
    /// Stop the selected world's components and `crun` services in place, then boot it again;
    /// [`DaemonReply::Event`] frames precede the terminal [`DaemonReply::World`] reply.
    Restart(WorldSelector),
    /// Stop a world's services while keeping its worktree.
    Down(WorldSelector),
    /// Create a fork worktree plus its CoW world.
    Fork(ForkRequest),
    /// Destroy a fork world and its worktree.
    DropFork(DropForkRequest),
    /// List worlds, optionally scoped to one repository root.
    List {
        /// Repository root to scope the listing to; `None` lists every world.
        repo_root: Option<PathBuf>,
    },
    /// Read one world's state, lineage, and process rows in a single call (`slipfork ps`).
    WorldProcesses(WorldSelector),
    /// Preview or reap orphaned worlds and snapshots.
    Prune {
        /// `true` for `--dry-run`.
        dry_run: bool,
    },
    /// Store a `0600` vault secret, refusing to overwrite without `force`.
    SecretSet {
        /// Repository root owning the vault.
        repo_root: PathBuf,
        /// Secret key name.
        key: String,
        /// Secret value (redacted by `Debug`).
        value: ControlSecret,
        /// Rotate an existing value.
        force: bool,
    },
    /// Remove a `0600` vault secret.
    SecretUnset {
        /// Repository root owning the vault.
        repo_root: PathBuf,
        /// Secret key name.
        key: String,
    },
    /// List `0600` vault secret key names.
    SecretList {
        /// Repository root owning the vault.
        repo_root: PathBuf,
    },
    /// Read or follow retained component/hook logs ([`DaemonReply::Log`] frames).
    Logs(LogsRequest),
    /// Fetch the direct-execution target for a world (or a running backing-service container).
    ExecTarget(ExecTargetRequest),
    /// Fetch the direct-execution target for an interactive `hook run` (native TTY).
    HookExecTarget(HookRunRequest),
    /// Enumerate a live fork world's two advertised refs and commit ids (`git-remote-slipfork`).
    WorkspaceRefs(WorkspaceRefsRequest),
    /// Re-run a hook without a TTY ([`DaemonReply::Chunk`] frames, then the exit code).
    HookRun(HookRunRequest),
    /// Bind host listeners and bridge them into a world for this connection's lifetime.
    Forward(ForwardRequest),
    /// Stop serving: the daemon releases `daemon.sock` and exits.
    ///
    /// Used only by `slipfork daemon install` to hand a zero-setup `:0` daemon over to
    /// `slipforkd.socket` (`ADR-0011` §3).
    Shutdown,
    /// Pair this machine against the daemon's boot-injected OSPP endpoint.
    AuthLogin {
        /// Optional endpoint URL from the CLI; the daemon refuses any endpoint that differs
        /// from the one injected at boot (one server identity per daemon).
        endpoint_url: Option<String>,
    },
}

/// One reply frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum DaemonReply {
    /// Reply to [`DaemonRequest::Status`].
    Status {
        /// Daemon status.
        status: DaemonStatus,
    },
    /// Reply to [`DaemonRequest::SnowResolve`].
    SnowResolved {
        /// Resolved variables (redacted unless the request asked for plaintext).
        summary: SnowResolveSummary,
    },
    /// A live boot event, streamed before the terminal [`DaemonReply::World`] reply.
    Event {
        /// The boot event.
        event: BootEvent,
    },
    /// Terminal reply to [`DaemonRequest::Up`] / [`DaemonRequest::Restart`] /
    /// [`DaemonRequest::Fork`].
    World {
        /// The booted world.
        world: WorldUpSummary,
    },
    /// Reply to [`DaemonRequest::Down`] / [`DaemonRequest::DropFork`].
    Released {
        /// `true` when a world was released.
        released: bool,
    },
    /// Reply to [`DaemonRequest::List`].
    Worlds {
        /// Every matching world.
        worlds: Vec<WorldSummary>,
    },
    /// Reply to [`DaemonRequest::WorldProcesses`].
    Processes {
        /// The world's state, lineage, and process rows.
        report: WorldProcessReport,
    },
    /// Reply to [`DaemonRequest::Prune`].
    Prune {
        /// What was (or would be) reaped.
        report: PruneReport,
    },
    /// Reply to [`DaemonRequest::SecretSet`].
    SecretSet,
    /// Reply to [`DaemonRequest::SecretUnset`].
    SecretUnset {
        /// `true` when a key was removed.
        removed: bool,
    },
    /// Reply to [`DaemonRequest::SecretList`].
    SecretKeys {
        /// Stored secret key names (never values).
        keys: Vec<String>,
    },
    /// One streamed `hook run` output chunk.
    Chunk {
        /// Captured output chunk.
        chunk: StreamChunk,
    },
    /// One streamed retained log chunk, attributed to its component or hook id.
    Log {
        /// Attributed log chunk.
        log: LogChunk,
    },
    /// Terminal reply to [`DaemonRequest::Logs`].
    Done,
    /// Terminal reply to [`DaemonRequest::HookRun`].
    HookExit {
        /// Hook process exit code.
        exit_code: i32,
    },
    /// Reply to [`DaemonRequest::ExecTarget`].
    ExecTarget {
        /// The direct-execution target.
        target: WorldExecTarget,
    },
    /// Reply to [`DaemonRequest::WorkspaceRefs`].
    WorkspaceRefs {
        /// Advertised refs and their commit ids.
        refs: WorkspaceRefsSummary,
    },
    /// Reply to [`DaemonRequest::Forward`]; the connection then stays open for the bridge.
    Forward {
        /// Bound host listeners.
        summary: ForwardSummary,
    },
    /// Reply to [`DaemonRequest::Shutdown`]; the server stops accepting after this frame.
    Shutdown,
    /// Reply to [`DaemonRequest::AuthLogin`].
    Auth {
        /// Pairing outcome.
        auth: AuthSummary,
    },
    /// The call failed; the connection is closed after this frame.
    Error {
        /// Typed, secret-free failure.
        error: DaemonError,
    },
}

/// A secret value carried by exactly one request variant, with a redacting [`fmt::Debug`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ControlSecret(String);

impl ControlSecret {
    /// Wrap a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the secret value (only the daemon's secret-store call should).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ControlSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// The complete typed CLI <-> `slipforkd` control contract (`ARCHITECTURE.md` §3.4).
///
/// Implemented in-process by `slipforkd`; [`client::DaemonControlClient`] speaks the identical
/// trait over `<runtime_dir>/daemon.sock`. Streaming calls report progress through their callback
/// and return the terminal value.
pub trait DaemonControl: Send + Sync {
    /// Daemon status plus every world, each with exactly one origin URL.
    fn status(&self) -> Result<DaemonStatus, EdgeError>;

    /// Validate/resolve `snow.yml` through the daemon-owned `0600` vault.
    ///
    /// The shipped daemon and client implement this; a control backend that does not own a vault
    /// fails closed instead of pretending to resolve secrets.
    fn snow_resolve(&self, _req: &SnowResolveRequest) -> Result<SnowResolveSummary, EdgeError> {
        Err(EdgeError::ControlProtocol {
            reason: "this control backend does not implement `snow resolve`".to_owned(),
        })
    }

    /// Boot (or idempotently confirm) the world selected by `req.cwd` / `req.world`.
    fn up(
        &self,
        req: &UpRequest,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError>;

    /// Stop the selected world's components + `crun` backing services in place — keeping the
    /// worktree and the world's CoW state — then run the exact same boot pipeline as
    /// [`DaemonControl::up`] (`changed: true`, `restarted` naming what came back).
    ///
    /// A `Failed` world re-boots (the rescue path); a world this daemon never booted fails closed
    /// with [`DaemonErrorKind::NotRunning`] and one line naming `slipfork up`.
    fn restart(
        &self,
        req: &WorldSelector,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError>;

    /// Stop a world's services while keeping its worktree.
    fn down(&self, req: &WorldSelector) -> Result<bool, EdgeError>;

    /// Create `.worktrees/<fork>` plus its CoW world.
    fn fork(
        &self,
        req: &ForkRequest,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError>;

    /// Destroy a fork world and its worktree.
    fn drop_fork(&self, req: &DropForkRequest) -> Result<bool, EdgeError>;

    /// List worlds, optionally scoped to one repository root.
    fn list(&self, repo_root: Option<&Path>) -> Result<Vec<WorldSummary>, EdgeError>;

    /// Read one world's lifecycle state, build lineage, and every service row in one call.
    ///
    /// The world's `state` comes from the same read as the rows, so `slipfork ps` never needs a
    /// second status call. An unknown or never-booted world fails closed with a typed
    /// `UnknownWorld` error — never an empty report.
    fn world_processes(&self, req: &WorldSelector) -> Result<WorldProcessReport, EdgeError>;

    /// Preview or reap orphaned worlds and snapshots.
    fn prune(&self, dry_run: bool) -> Result<PruneReport, EdgeError>;

    /// Store a `0600` vault secret, refusing to overwrite without `force`.
    fn secret_set(
        &self,
        repo_root: &Path,
        key: &str,
        value: &str,
        force: bool,
    ) -> Result<(), EdgeError>;

    /// Remove a `0600` vault secret.
    fn secret_unset(&self, repo_root: &Path, key: &str) -> Result<bool, EdgeError>;

    /// List stored secret key names (never values).
    fn secret_list(&self, repo_root: &Path) -> Result<Vec<String>, EdgeError>;

    /// Read or follow retained logs, streaming attributed chunks to `on_chunk`.
    fn logs(&self, req: &LogsRequest, on_chunk: &mut dyn FnMut(LogChunk)) -> Result<(), EdgeError>;

    /// Fetch the direct-execution target (the only `ExecEnv`-bearing reply).
    ///
    /// With `req.service = None` this enters the world rootfs; with `Some(id)` it returns the
    /// `crun exec` entry for that running backing-service container and fails closed with
    /// [`DaemonErrorKind::ServiceNotRunning`] instead of falling back to the app rootfs.
    fn exec_target(&self, req: &ExecTargetRequest) -> Result<WorldExecTarget, EdgeError>;

    /// Fetch the direct-execution target for an interactive `hook run` (native TTY).
    fn hook_exec_target(&self, _req: &HookRunRequest) -> Result<WorldExecTarget, EdgeError> {
        Err(EdgeError::ControlProtocol {
            reason: "this control backend does not implement interactive hook entry".to_owned(),
        })
    }

    /// Enumerate a live fork world's refs (the read-only path `git-remote-slipfork` drives).
    fn workspace_refs(
        &self,
        _req: &WorkspaceRefsRequest,
    ) -> Result<WorkspaceRefsSummary, EdgeError> {
        Err(EdgeError::ControlProtocol {
            reason: "this control backend does not implement fork ref enumeration".to_owned(),
        })
    }

    /// Re-run a hook without a TTY, streaming chunks to `on_chunk`, returning its exit code.
    fn hook_run(
        &self,
        req: &HookRunRequest,
        on_chunk: &mut dyn FnMut(StreamChunk),
    ) -> Result<i32, EdgeError>;

    /// Bind host listeners in-process and bridge them into a world.
    ///
    /// `wait_disconnect` publishes the [`ForwardSummary`] to the caller and returns when the
    /// control connection closes, at which point the bridges are torn down.
    fn forward(
        &self,
        req: &ForwardRequest,
        wait_disconnect: &mut dyn FnMut(ForwardSummary),
    ) -> Result<(), EdgeError>;

    /// Pair this machine against the daemon's boot-injected OSPP endpoint.
    ///
    /// `endpoint_url` must be absent or byte-identical to the injected endpoint; the daemon never
    /// dials a second server identity.
    fn auth_login(&self, endpoint_url: Option<&str>) -> Result<AuthSummary, EdgeError>;

    /// Acknowledge a shutdown request; the transport server stops serving after the reply.
    ///
    /// A control backend that cannot stop its transport fails closed (it never pretends).
    fn shutdown(&self) -> Result<(), EdgeError> {
        Err(EdgeError::ControlProtocol {
            reason: "this control backend cannot shut down".to_owned(),
        })
    }
}
