//! Canonical `WorldRegistry`: `(ProjectSlug, WorldTarget)` -> `WorldOrigin` routing, the
//! `IngressRoute` prefix table, and the in-process world handles.
//!
//! Worlds are attached and released in-process (never through a cross-process handle,
//! `H-010` #1). The registry is the single owner of the `(project, target)` -> backend mapping:
//!
//! - [`WorldRegistry::mint_origin`] renders the world's single public `WorldOrigin`.
//! - [`WorldRegistry::attach_world`] registers one live [`WorldHandle`] with the manifest's
//!   `snow.yml` `domains[].id` set and its `ingresses` prefix table. The **caller** supplies that
//!   table: the declared `snow.yml` prefixes plus the empty-prefix catch-all built from the first
//!   component `internal_ports[0]` (it matches every path and loses every longest-prefix race).
//! - [`WorldRegistry::resolve`] decides one incoming request: proxy its `*.localhost` world.
//! - [`WorldRegistry::dial`] / [`WorldRegistry::forward`] open the world-local backend through
//!   `WorldEngine::dial_world_tcp`; the stream never crosses the CLI <-> `slipforkd` boundary.
//! - [`WorldRegistry::release_world`] destroys the in-process world.
//!
//! Every request-path error names the world id and the `Host` header.

use std::net::TcpStream;
use std::sync::{Arc, Mutex, MutexGuard};

use slipfork_engine::{WorldEngine, WorldHandle};
use slipfork_world::{LocalHostRoute, ProjectSlug, WorldOrigin, WorldTarget};

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

/// Every mutable registry index, mutated together under one lock.
#[derive(Default)]
struct RegistryState {
    worlds: Vec<AttachedWorld>,
}

/// Canonical in-process owner of every live world and its `.localhost` routing table.
///
/// The attached-world index is a small linear `Vec` under a [`Mutex`]: a developer host owns a
/// handful of worlds, and `WorldTarget` deliberately exposes no hash/ordering key.
pub struct WorldRegistry {
    engine: Arc<dyn WorldEngine>,
    http_ingress_port: u16,
    state: Mutex<RegistryState>,
}

impl WorldRegistry {
    /// Construct the registry over the daemon's single in-process engine.
    ///
    /// `http_ingress_port` is the bound TCP HTTP ingress port rendered into every
    /// [`WorldOrigin`].
    pub fn new(engine: Arc<dyn WorldEngine>, http_ingress_port: u16) -> Self {
        Self {
            engine,
            http_ingress_port,
            state: Mutex::new(RegistryState::default()),
        }
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
    /// daemon does not hold.
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

    /// The world's single public `.localhost` origin.
    pub fn origin(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
    ) -> Result<WorldOrigin, EdgeError> {
        let id = world_id(project, target);
        let route =
            LocalHostRoute::new(project.clone(), target.clone(), None, &[]).map_err(|err| {
                EdgeError::WorldRoute {
                    world: id,
                    reason: err.to_string(),
                }
            })?;
        self.mint_origin(&route, self.http_ingress_port)
    }

    /// Release a world: remove it from the routing table and destroy its handle.
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
            index.map(|index| state.worlds.remove(index))
        };
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
    /// A `*.localhost` host of an attached world is proxied; every other host is
    /// [`EdgeError::UnknownHost`], and an unmatched path of an attached world is
    /// [`EdgeError::UnknownIngressPath`]. The routing table is borrowed only long enough to
    /// resolve, so the caller may dial the world without holding the registry lock.
    pub fn resolve(&self, host: &str, path: &str) -> Result<RoutedWorld, EdgeError> {
        let state = self.lock("route")?;
        for entry in state.worlds.iter() {
            let snow_domains: Vec<&str> = entry.snow_domains.iter().map(String::as_str).collect();
            let Some(route) = LocalHostRoute::parse(&entry.project, &snow_domains, host) else {
                continue;
            };
            if route.target() != &entry.target {
                continue;
            }
            return entry.routed(host, path);
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
