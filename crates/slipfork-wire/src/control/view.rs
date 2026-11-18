//! Control view models: daemon status, world rows, process reports, and streamed boot events.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use slipfork_world::WorldOrigin;

use super::lineage::{WorldLineage, WorldServiceInfo};
use super::stream::StreamChunk;
use super::{DaemonError, DaemonErrorKind};

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
    pub fn new(addrs: Vec<SocketAddr>) -> Result<Self, IngressError> {
        let mut listeners = addrs.iter();
        let Some(first) = listeners.next() else {
            return Err(IngressError::Missing);
        };
        let port = first.port();
        if port == 0 {
            return Err(IngressError::Invalid {
                reason: format!("the HTTP listener {first} carries the unassigned port :0"),
            });
        }
        for addr in listeners {
            if addr.port() != port {
                return Err(IngressError::Invalid {
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
    type Error = IngressError;

    fn try_from(frame: IngressInfoFrame) -> Result<Self, Self::Error> {
        let info = Self::new(frame.addrs)?;
        if info.port != frame.port {
            return Err(IngressError::Invalid {
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
    ) -> Result<Self, DaemonError> {
        let mut seen = BTreeSet::new();
        for service in &services {
            if !seen.insert(service.info.id.as_str()) {
                return Err(DaemonError::new(
                    DaemonErrorKind::Internal,
                    format!(
                        "world '{}' reported two services with the id '{}'; process rows must have unique ids",
                        service.world, service.info.id
                    ),
                ));
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

/// Structural refusal of an `IngressInfo` listener set (a view-model invariant, no CLI remedy).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IngressError {
    /// No bound HTTP ingress listener was reported.
    #[error("the daemon reported no bound HTTP ingress listener")]
    Missing,
    /// The reported listener set cannot name one real, bound origin port.
    #[error("{reason}")]
    Invalid {
        /// What is wrong with the reported listener set.
        reason: String,
    },
}
