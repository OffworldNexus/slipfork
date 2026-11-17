//! Canonical `WorldRegistry`: `(ProjectSlug, WorldTarget)` -> `WorldOrigin` routing, the
//! `IngressRoute` prefix table, shared tunnel hostnames, and the in-process world handles.
//!
//! Worlds are attached and released in-process (never through a cross-process handle,
//! `H-010` #1). The registry is the single owner of the `(project, target)` -> backend mapping:
//!
//! - [`WorldRegistry::mint_origin`] renders the world's single public `WorldOrigin::Local`.
//! - [`WorldRegistry::attach_world`] registers one live [`WorldHandle`] with the manifest's
//!   `snow.yml` `domains[].id` set and its `ingresses` prefix table. The **caller** supplies that
//!   table: the declared `snow.yml` prefixes plus the empty-prefix catch-all built from the first
//!   component `internal_ports[0]` (it matches every path and loses every longest-prefix race).
//! - [`WorldRegistry::share_world`] mints a `WorldOrigin::Shared` lease from the injected
//!   [`TunnelRegistrar`] against `slipforkd`'s planned static forward port and records the public
//!   hostname in `shared_hosts`; [`WorldRegistry::unshare_world`] drops the lease and is an
//!   idempotent success no-op returning `WorldOrigin::Local` (`ADR-0005` §2).
//! - [`WorldRegistry::resolve`] decides one incoming request: proxy it (`shared_hosts` public
//!   hostnames, or a `Local` `*.localhost` world) or answer `308`/`421` because the `*.localhost`
//!   world is shared (`ADR-0005` §1).
//! - [`WorldRegistry::dial`] / [`WorldRegistry::forward`] open the world-local backend through
//!   `WorldEngine::dial_world_tcp`; the stream never crosses the CLI <-> `slipforkd` boundary.
//! - [`WorldRegistry::release_world`] destroys the in-process world and clears its share.
//!
//! Every request-path error names the world id and the `Host` header.

use std::collections::BTreeMap;
use std::net::TcpStream;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use slipfork_engine::{WorldEngine, WorldHandle};
use slipfork_world::{
    LocalHostRoute, ProjectSlug, TunnelLease, TunnelRegistrar, TunnelRequest, WorldOrigin,
    WorldTarget,
};

use crate::EdgeError;
use crate::forward::{ForwardBridge, ForwardTargetSpec};

/// One `snow.yml` ingress prefix mapped to a backend port inside the world.
///
/// The empty prefix is the canonical catch-all: it matches every path and loses every
/// longest-prefix competition, so it behaves exactly like `internal_ports[0]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressRoute {
    /// Request-path prefix (`"/api"`), or `""` for the `internal_ports[0]` catch-all.
    pub prefix: String,
    /// Backend port bound on `127.0.0.1` inside the world network namespace.
    pub port: u16,
}

/// One resolved proxy target: the owning world, its backend port, and the diagnostic identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedWorld {
    world: WorldHandle,
    port: u16,
    host: String,
    id: String,
}

impl RoutedWorld {
    /// Backend port inside the world.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `Host` header the request was routed with.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// World identity used in diagnostics (`<project>` or `<project>:<fork>`).
    pub fn world_id(&self) -> &str {
        &self.id
    }
}

/// One resolved decision for an incoming HTTP request.
///
/// Routing semantics live here, not in [`crate::ingress`]: the ingress only renders the decision
/// as an HTTP response or relays it to the world backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    /// Proxy the request to the world backend (`shared_hosts` public host or a `Local` world).
    Forward(RoutedWorld),
    /// The `*.localhost` world is shared: `308 Permanent Redirect` to the shared origin
    /// (`ADR-0005` §1 rule 1), for every method.
    Redirect {
        /// World id the host resolved to.
        world: String,
        /// Absolute `Location` (`<shared_url><path_and_query>`).
        location: String,
    },
    /// The `*.localhost` world is shared and the request asks for an `Upgrade`: `421 Misdirected
    /// Request` naming the shared origin (`ADR-0005` §1 rule 2).
    Misdirected {
        /// World id the host resolved to.
        world: String,
        /// Public `WorldOrigin::Shared` URL.
        shared_url: String,
    },
}

/// The single public origin and opaque lease identity of one shared world.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedWorldInfo {
    /// `WorldOrigin::Shared` minted from the lease URL before materialization.
    pub origin: WorldOrigin,
    /// Opaque daemon-issued lease identifier.
    pub lease_id: String,
}

impl SharedWorldInfo {
    /// Public tunnel URL (`origin.url()`), the world's only reachable origin while shared.
    pub fn public_url(&self) -> &str {
        self.origin.url()
    }
}

/// One attached, live world.
struct AttachedWorld {
    project: ProjectSlug,
    target: WorldTarget,
    snow_domains: Vec<String>,
    world: WorldHandle,
    ingresses: Vec<IngressRoute>,
}

impl AttachedWorld {
    fn id(&self) -> String {
        world_id(&self.project, &self.target)
    }

    fn resolve_port(&self, path: &str) -> Option<u16> {
        self.ingresses
            .iter()
            .filter(|route| prefix_matches(&route.prefix, path))
            .max_by_key(|route| route.prefix.len())
            .map(|route| route.port)
    }

    fn routed(&self, host: &str, path: &str) -> Result<RoutedWorld, EdgeError> {
        let port = self
            .resolve_port(path)
            .ok_or_else(|| EdgeError::UnknownIngressPath {
                world: self.id(),
                host: host.to_owned(),
                path: path.to_owned(),
            })?;
        Ok(RoutedWorld {
            world: self.world.clone(),
            port,
            host: host.to_owned(),
            id: self.id(),
        })
    }
}

/// The (project, target) -> [`IngressRoute`] mapping of one shared world (`ADR-0005` §2).
struct ActiveShare {
    project: ProjectSlug,
    target: WorldTarget,
    origin: WorldOrigin,
    /// RAII guard: dropping the share revokes the tunnel (`TunnelLease` contract).
    _lease: Box<dyn TunnelLease>,
}

impl ActiveShare {
    fn id(&self) -> String {
        world_id(&self.project, &self.target)
    }

    fn host_key(&self) -> String {
        normalize_shared_host(self.origin.host())
    }
}

/// Every mutable registry index, mutated together under one lock.
#[derive(Default)]
struct RegistryState {
    worlds: Vec<AttachedWorld>,
    /// Public tunnel hostname -> owning world (exact, case-insensitive).
    shared_hosts: BTreeMap<String, (ProjectSlug, WorldTarget)>,
    /// One record per shared world: public origin plus the RAII tunnel lease.
    shares: Vec<ActiveShare>,
}

impl RegistryState {
    fn share(&self, project: &ProjectSlug, target: &WorldTarget) -> Option<&ActiveShare> {
        self.shares
            .iter()
            .find(|share| share.project == *project && share.target == *target)
    }

    fn take_share(&mut self, project: &ProjectSlug, target: &WorldTarget) -> Option<ActiveShare> {
        let index = self
            .shares
            .iter()
            .position(|share| share.project == *project && share.target == *target)?;
        let share = self.shares.remove(index);
        let host = share.host_key();
        if self.shared_hosts.get(&host) == Some(&(project.clone(), target.clone())) {
            self.shared_hosts.remove(&host);
        }
        Some(share)
    }

    /// Record `share`, returning (and revoking) the superseded record of the same world.
    ///
    /// The conflict check runs **before** any mutation, so a registrar that hands out a hostname
    /// another world already owns fails closed with the previous state fully intact.
    fn insert_share(&mut self, share: ActiveShare) -> Result<Option<ActiveShare>, EdgeError> {
        let host = share.host_key();
        if let Some((owner, owner_target)) = self.shared_hosts.get(&host)
            && (*owner != share.project || *owner_target != share.target)
        {
            return Err(EdgeError::SharedHostConflict {
                world: share.id(),
                host,
            });
        }
        let previous = self.take_share(&share.project, &share.target);
        self.shared_hosts
            .insert(host, (share.project.clone(), share.target.clone()));
        self.shares.push(share);
        Ok(previous)
    }
}

/// Canonical in-process owner of every live world and its `.localhost` routing table.
///
/// The attached-world index is a small linear `Vec` under a [`Mutex`]: a developer host owns a
/// handful of worlds, and `WorldTarget` deliberately exposes no hash/ordering key.
pub struct WorldRegistry {
    engine: Arc<dyn WorldEngine>,
    registrar: Arc<dyn TunnelRegistrar>,
    http_ingress_port: u16,
    planned_forward_port: Option<u16>,
    state: Mutex<RegistryState>,
}

impl WorldRegistry {
    /// Construct the registry over the daemon's single in-process collaborators.
    ///
    /// `http_ingress_port` is the bound TCP HTTP ingress port rendered into every
    /// `WorldOrigin::Local`; `planned_forward_port` is `Some(port)` only when a forward listener is
    /// bound, and `share_world` fails closed without it.
    pub fn new(
        engine: Arc<dyn WorldEngine>,
        registrar: Arc<dyn TunnelRegistrar>,
        http_ingress_port: u16,
        planned_forward_port: Option<u16>,
    ) -> Self {
        Self {
            engine,
            registrar,
            http_ingress_port,
            planned_forward_port,
            state: Mutex::new(RegistryState::default()),
        }
    }

    /// Planned static forward port `weaved` forwards shared hostnames to, when bound (`H-008`).
    pub fn planned_forward_port(&self) -> Option<u16> {
        self.planned_forward_port
    }

    /// Mint the world's single public origin for its `.localhost` route (`H-010` #2).
    ///
    /// Port 80 renders `http://<host>`; every other ingress port renders `http://<host>:<port>`.
    /// A route scoped to a `snow.yml` `domains[].id` is not the default ingress and fails closed.
    pub fn mint_origin(
        &self,
        route: &LocalHostRoute,
        ingress_port: u16,
    ) -> Result<WorldOrigin, EdgeError> {
        WorldOrigin::local(route, ingress_port).map_err(|err| EdgeError::Origin {
            world: world_id(route.project(), route.target()),
            host: route.host(),
            reason: err.to_string(),
        })
    }

    /// Register one live world handle with its `snow.yml` domains and ingress prefix table.
    ///
    /// A second attach for an already-registered `(project, target)` fails closed with
    /// [`EdgeError::WorldAlreadyRegistered`]; it never replaces or leaks the previous handle.
    pub fn attach_world(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
        snow_domains: Vec<String>,
        world: WorldHandle,
        ingresses: Vec<IngressRoute>,
    ) -> Result<(), EdgeError> {
        let id = world_id(project, target);
        validate_ingresses(&id, &ingresses)?;
        let mut state = self.lock("attach_world")?;
        if state
            .worlds
            .iter()
            .any(|entry| entry.project == *project && entry.target == *target)
        {
            return Err(EdgeError::WorldAlreadyRegistered { world: id });
        }
        state.worlds.push(AttachedWorld {
            project: project.clone(),
            target: target.clone(),
            snow_domains,
            world,
            ingresses,
        });
        Ok(())
    }

    /// Replace an attached world's `snow.yml`-derived domains and ingress table **in place**.
    ///
    /// `slipfork restart` re-reads `ingresses:`/`domains:`/`internal_ports` and applies them to the
    /// live world without touching its engine handle (the CoW state must survive). The world must
    /// already be attached: an unknown `(project, target)` fails closed with
    /// [`EdgeError::WorldNotAttached`] rather than silently creating a route table for a world this
    /// daemon does not hold. The world's share (if any) is left untouched.
    pub fn replace_world(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
        snow_domains: Vec<String>,
        ingresses: Vec<IngressRoute>,
    ) -> Result<(), EdgeError> {
        let id = world_id(project, target);
        validate_ingresses(&id, &ingresses)?;
        let mut state = self.lock("replace_world")?;
        let Some(entry) = state
            .worlds
            .iter_mut()
            .find(|entry| entry.project == *project && entry.target == *target)
        else {
            return Err(EdgeError::WorldNotAttached { world: id });
        };
        entry.snow_domains = snow_domains;
        entry.ingresses = ingresses;
        Ok(())
    }

    /// The world's single public origin: its share when shared, otherwise its `.localhost` origin.
    pub fn origin(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
    ) -> Result<WorldOrigin, EdgeError> {
        let shared = {
            let state = self.lock("origin")?;
            state
                .share(project, target)
                .map(|share| share.origin.clone())
        };
        match shared {
            Some(origin) => Ok(origin),
            None => self.local_origin(project, target),
        }
    }

    /// Mint a `WorldOrigin::Shared` lease for `(project, target)` (`ADR-0005` §2).
    ///
    /// Requires a bound planned forward port (`H-008`: `weaved` forwards the public hostname to
    /// `127.0.0.1:<forward_port>`, never to `*.localhost`), so a daemon without one fails closed
    /// with [`EdgeError::NoForwardListener`]. The lease is registered against that port and the
    /// public hostname is recorded in `shared_hosts`; a superseded lease of the same world is
    /// dropped (revoked) after the swap, and a registrar failure leaves the previous state intact.
    pub fn share_world(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
    ) -> Result<SharedWorldInfo, EdgeError> {
        let id = world_id(project, target);
        let local_port = self
            .planned_forward_port
            .ok_or(EdgeError::NoForwardListener)?;
        let route =
            LocalHostRoute::new(project.clone(), target.clone(), None, &[]).map_err(|err| {
                EdgeError::WorldRoute {
                    world: id.clone(),
                    reason: err.to_string(),
                }
            })?;
        let lease = self
            .registrar
            .register(&TunnelRequest {
                route: &route,
                local_port,
            })
            .map_err(|err| EdgeError::TunnelUnavailable {
                world: id.clone(),
                reason: err.to_string(),
            })?;
        let origin = WorldOrigin::shared(lease.public_url()).map_err(|err| EdgeError::Origin {
            world: id.clone(),
            host: lease.public_url().to_owned(),
            reason: err.to_string(),
        })?;
        // `weaved` must forward a public hostname to `slipforkd`, never a `*.localhost` one: such a
        // lease would make every shared request redirect-loop back into this ingress (`H-008`).
        if is_localhost_host(origin.host()) {
            return Err(EdgeError::TunnelUnavailable {
                world: id,
                reason: format!(
                    "the tunnel lease returned '{}', which points at slipforkd's own .localhost ingress and would redirect-loop",
                    origin.url()
                ),
            });
        }
        let info = SharedWorldInfo {
            origin: origin.clone(),
            lease_id: lease.lease_id().to_owned(),
        };
        let superseded = {
            let mut state = self.lock("share_world")?;
            state.insert_share(ActiveShare {
                project: project.clone(),
                target: target.clone(),
                origin,
                _lease: lease,
            })?
        };
        drop(superseded);
        Ok(info)
    }

    /// Drop any tunnel lease and return the world's `.localhost` origin (`ADR-0005` §2).
    ///
    /// Idempotent: unsharing an already-`Local` world with no lease succeeds and changes nothing.
    pub fn unshare_world(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
    ) -> Result<WorldOrigin, EdgeError> {
        let dropped = {
            let mut state = self.lock("unshare_world")?;
            state.take_share(project, target)
        };
        drop(dropped);
        self.local_origin(project, target)
    }

    /// Release a world: remove it from the routing table, clear its share, and destroy its handle.
    ///
    /// Returns `false` when no world is registered for `(project, target)`. The entry is removed
    /// **before** destruction, so a failed destroy never leaves the registry routing traffic to a
    /// half-dead world.
    pub fn release_world(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
    ) -> Result<bool, EdgeError> {
        let released = {
            let mut state = self.lock("release_world")?;
            let index = state
                .worlds
                .iter()
                .position(|entry| entry.project == *project && entry.target == *target);
            let share = state.take_share(project, target);
            (index.map(|index| state.worlds.remove(index)), share)
        };
        let (released, share) = released;
        drop(share);
        let Some(released) = released else {
            return Ok(false);
        };
        let id = released.id();
        self.engine
            .destroy_world(released.world)
            .map_err(|err| EdgeError::Release {
                world: id,
                reason: err.to_string(),
            })?;
        Ok(true)
    }

    /// Decide how one incoming request is served.
    ///
    /// 1. An exact (case-insensitive) `shared_hosts` public hostname is **proxied** to its world,
    ///    preserving the public `Host` — a shared world is never redirected (`H-008`).
    /// 2. A `*.localhost` host resolving to a **shared** world answers `421` when `upgrade` is set
    ///    and `308` otherwise, before any backend is dialed (`ADR-0005` §1).
    /// 3. Any other `*.localhost` host of an attached `Local` world is proxied.
    ///
    /// Unattached hosts are [`EdgeError::UnknownHost`]; an unmatched path of an attached local
    /// world is [`EdgeError::UnknownIngressPath`]. The routing table is borrowed only long enough
    /// to resolve, so the caller may dial the world without holding the registry lock.
    pub fn resolve(
        &self,
        host: &str,
        path: &str,
        upgrade: bool,
    ) -> Result<RouteDecision, EdgeError> {
        let state = self.lock("route")?;
        let host_key = normalize_shared_host(host);
        if let Some((project, target)) = state.shared_hosts.get(&host_key) {
            let entry = state
                .worlds
                .iter()
                .find(|entry| entry.project == *project && entry.target == *target)
                .ok_or_else(|| EdgeError::UnknownHost {
                    host: host.to_owned(),
                })?;
            return entry.routed(host, path).map(RouteDecision::Forward);
        }

        for entry in state.worlds.iter() {
            let snow_domains: Vec<&str> = entry.snow_domains.iter().map(String::as_str).collect();
            let Some(route) = LocalHostRoute::parse(&entry.project, &snow_domains, host) else {
                continue;
            };
            if route.target() != &entry.target {
                continue;
            }
            if let Some(share) = state.share(&entry.project, &entry.target) {
                let shared_url = share.origin.url().trim_end_matches('/').to_owned();
                return if upgrade {
                    Ok(RouteDecision::Misdirected {
                        world: entry.id(),
                        shared_url,
                    })
                } else {
                    Ok(RouteDecision::Redirect {
                        world: entry.id(),
                        location: format!("{shared_url}{path}"),
                    })
                };
            }
            return entry.routed(host, path).map(RouteDecision::Forward);
        }
        Err(EdgeError::UnknownHost {
            host: host.to_owned(),
        })
    }

    /// Open the resolved world-local backend stream through `WorldEngine::dial_world_tcp`.
    ///
    /// Daemon-internal only: the stream never crosses the CLI <-> `slipforkd` boundary.
    pub fn dial(&self, routed: &RoutedWorld) -> Result<TcpStream, EdgeError> {
        self.engine
            .dial_world_tcp(&routed.world, routed.port)
            .map_err(|err| EdgeError::UpstreamDial {
                world: routed.id.clone(),
                host: routed.host.clone(),
                port: routed.port,
                reason: err.to_string(),
            })
    }

    /// Open one specific world-local port (daemon-owned `forward` targets, `H-010` #1).
    pub fn dial_port(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
        port: u16,
        host: &str,
    ) -> Result<TcpStream, EdgeError> {
        let (world, id) = {
            let state = self.lock("dial_port")?;
            let entry = state
                .worlds
                .iter()
                .find(|entry| entry.project == *project && entry.target == *target)
                .ok_or_else(|| EdgeError::UnknownHost {
                    host: host.to_owned(),
                })?;
            (entry.world.clone(), entry.id())
        };
        self.engine
            .dial_world_tcp(&world, port)
            .map_err(|err| EdgeError::UpstreamDial {
                world: id,
                host: host.to_owned(),
                port,
                reason: err.to_string(),
            })
    }

    /// Bind one host `127.0.0.1:0` listener per target and bridge it into the world.
    ///
    /// The listener set lives exactly as long as the returned [`ForwardBridge`]; dropping it stops
    /// the accept loops and closes every host port, so a disconnected control client can never
    /// leave a forward behind (`H-010` #1).
    pub fn forward(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
        specs: &[ForwardTargetSpec],
    ) -> Result<ForwardBridge, EdgeError> {
        if specs.is_empty() {
            return Err(EdgeError::ForwardSpec {
                reason: "at least one forward target is required".to_owned(),
            });
        }
        for spec in specs {
            spec.validate()?;
        }
        let (world, id) = {
            let state = self.lock("forward")?;
            let entry = state
                .worlds
                .iter()
                .find(|entry| entry.project == *project && entry.target == *target)
                .ok_or_else(|| EdgeError::UnknownHost {
                    host: world_id(project, target),
                })?;
            (entry.world.clone(), entry.id())
        };
        ForwardBridge::bind(Arc::clone(&self.engine), world, id, specs)
    }

    fn local_origin(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
    ) -> Result<WorldOrigin, EdgeError> {
        let id = world_id(project, target);
        let route =
            LocalHostRoute::new(project.clone(), target.clone(), None, &[]).map_err(|err| {
                EdgeError::WorldRoute {
                    world: id.clone(),
                    reason: err.to_string(),
                }
            })?;
        self.mint_origin(&route, self.http_ingress_port)
    }

    fn lock(&self, operation: &'static str) -> Result<MutexGuard<'_, RegistryState>, EdgeError> {
        self.state
            .lock()
            .map_err(|_| EdgeError::RegistryUnavailable { operation })
    }
}

/// World identity used in every diagnostic: `<project>` or `<project>:<target-label>`.
pub fn world_id(project: &ProjectSlug, target: &WorldTarget) -> String {
    match target {
        WorldTarget::Canonical => project.to_string(),
        WorldTarget::UserFork(fork) => format!("{project}:{fork}"),
        WorldTarget::TestSlot(slot) => format!("{project}:test-{slot}"),
    }
}

/// Canonical parent world id of `target`: `None` for the canonical world, `Some(<project>)` for a
/// fork (and for an internal test slot).
///
/// The single owner of the target -> parent relation: `slipfork status` draws its tree from this
/// value and never splits a world id to find the project.
pub fn parent_world(project: &ProjectSlug, target: &WorldTarget) -> Option<String> {
    match target {
        WorldTarget::Canonical => None,
        WorldTarget::UserFork(_) | WorldTarget::TestSlot(_) => Some(project.to_string()),
    }
}

/// `true` for a host inside the reserved loopback `.localhost` namespace.
fn is_localhost_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".localhost")
}

/// Normalize a public tunnel `Host` for the exact, case-insensitive `shared_hosts` lookup.
///
/// A trailing root dot and a numeric `:port` are dropped (a tunnel hostname arrives as
/// `<public-host>` or `<public-host>:443`); no suffix or wildcard matching is performed.
fn normalize_shared_host(host: &str) -> String {
    let host = host.trim();
    let host = match host.rsplit_once(':') {
        Some((name, port))
            if !name.is_empty() && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            name
        }
        _ => host,
    };
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// `true` when `prefix` covers `path` as a URL path segment boundary.
///
/// The empty prefix (the `internal_ports[0]` catch-all) covers everything; `"/api"` covers
/// `"/api"`, `"/api/users"`, and `"/api?x=1"`, but never `"/apifoo"`.
fn prefix_matches(prefix: &str, path: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    let Some(rest) = path.strip_prefix(prefix) else {
        return false;
    };
    prefix.ends_with('/') || rest.is_empty() || rest.starts_with('/') || rest.starts_with('?')
}

/// Fail closed on an ingress table that cannot be routed deterministically.
fn validate_ingresses(world: &str, ingresses: &[IngressRoute]) -> Result<(), EdgeError> {
    if ingresses.is_empty() {
        return Err(EdgeError::InvalidIngressRoute {
            world: world.to_owned(),
            reason: "at least one route (the internal_ports[0] catch-all) is required".to_owned(),
        });
    }
    for (index, route) in ingresses.iter().enumerate() {
        if route.port == 0 {
            return Err(EdgeError::InvalidIngressRoute {
                world: world.to_owned(),
                reason: format!("prefix '{}' maps to port 0", route.prefix),
            });
        }
        if !route.prefix.is_empty()
            && (!route.prefix.starts_with('/')
                || route.prefix.contains(char::is_whitespace)
                || route.prefix.contains('?')
                || route.prefix.contains('#'))
        {
            return Err(EdgeError::InvalidIngressRoute {
                world: world.to_owned(),
                reason: format!(
                    "prefix '{}' must be empty or start with '/' and contain no whitespace, '?', or '#'",
                    route.prefix
                ),
            });
        }
        if ingresses[..index]
            .iter()
            .any(|previous| previous.prefix == route.prefix)
        {
            return Err(EdgeError::InvalidIngressRoute {
                world: world.to_owned(),
                reason: format!("duplicate ingress prefix '{}'", route.prefix),
            });
        }
    }
    Ok(())
}
