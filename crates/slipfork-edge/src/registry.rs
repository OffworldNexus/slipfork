//! Canonical `WorldRegistry`: `(ProjectSlug, WorldTarget)` -> `WorldOrigin` routing, the
//! `IngressRoute` prefix table, and the in-process world handles.
//!
//! Worlds are attached and released in-process (never through a cross-process handle,
//! `H-010` #1). The registry is the single owner of the `(project, target)` -> backend mapping:
//!
//! - [`WorldRegistry::mint_origin`] renders the world's single public `WorldOrigin::Local`.
//! - [`WorldRegistry::attach_world`] registers one live [`WorldHandle`] with the manifest's
//!   `snow.yml` `domains[].id` set and its `ingresses` prefix table. The **caller** supplies that
//!   table: the declared `snow.yml` prefixes plus the empty-prefix catch-all built from the first
//!   component `internal_ports[0]` (it matches every path and loses every longest-prefix race).
//! - [`WorldRegistry::route`] parses the request `Host` with
//!   [`slipfork_world::LocalHostRoute::parse`] and resolves the backend port by longest matching
//!   prefix; a request for an unattached world or an unmatched path is a `404`.
//! - [`WorldRegistry::dial`] opens the world-local backend stream through
//!   `WorldEngine::dial_world_tcp`.
//! - [`WorldRegistry::release_world`] destroys the in-process world.
//!
//! Every request-path error names the world id and the `Host` header.

use std::net::TcpStream;
use std::sync::{Arc, Mutex, MutexGuard};

use slipfork_engine::{WorldEngine, WorldHandle};
use slipfork_world::{LocalHostRoute, ProjectSlug, WorldOrigin, WorldTarget};

use crate::EdgeError;

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

/// One resolved request target: the owning world, its backend port, and the diagnostic identity.
#[derive(Debug, Clone)]
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
}

/// Canonical in-process owner of every live world and its `.localhost` routing table.
///
/// The attached-world index is a small linear `Vec` under a [`Mutex`]: a developer host owns a
/// handful of worlds, and `WorldTarget` deliberately exposes no hash/ordering key.
pub struct WorldRegistry {
    engine: Arc<dyn WorldEngine>,
    worlds: Mutex<Vec<AttachedWorld>>,
}

impl WorldRegistry {
    /// Construct the registry over the daemon's single in-process [`WorldEngine`].
    pub fn new(engine: Arc<dyn WorldEngine>) -> Self {
        Self {
            engine,
            worlds: Mutex::new(Vec::new()),
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
        let mut worlds = self.lock("attach_world")?;
        if worlds
            .iter()
            .any(|entry| entry.project == *project && entry.target == *target)
        {
            return Err(EdgeError::WorldAlreadyRegistered { world: id });
        }
        worlds.push(AttachedWorld {
            project: project.clone(),
            target: target.clone(),
            snow_domains,
            world,
            ingresses,
        });
        Ok(())
    }

    /// Release a world: remove it from the routing table and destroy its in-process handle.
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
            let mut worlds = self.lock("release_world")?;
            let index = worlds
                .iter()
                .position(|entry| entry.project == *project && entry.target == *target);
            index.map(|index| worlds.remove(index))
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

    /// Resolve one request `Host` + `path` to the owning world and its backend port.
    ///
    /// Unknown hosts/targets with no attached world are [`EdgeError::UnknownHost`]; an attached
    /// world whose ingress table has no matching prefix is [`EdgeError::UnknownIngressPath`]. Both
    /// dispatch as `404`. The routing table is borrowed only long enough to resolve, so the caller
    /// may dial the world without holding the registry lock.
    pub fn route(&self, host: &str, path: &str) -> Result<RoutedWorld, EdgeError> {
        let worlds = self.lock("route")?;
        for entry in worlds.iter() {
            let snow_domains: Vec<&str> = entry.snow_domains.iter().map(String::as_str).collect();
            let Some(route) = LocalHostRoute::parse(&entry.project, &snow_domains, host) else {
                continue;
            };
            if route.target() != &entry.target {
                continue;
            }
            let id = entry.id();
            let port = entry
                .resolve_port(path)
                .ok_or_else(|| EdgeError::UnknownIngressPath {
                    world: id.clone(),
                    host: host.to_owned(),
                    path: path.to_owned(),
                })?;
            return Ok(RoutedWorld {
                world: entry.world.clone(),
                port,
                host: host.to_owned(),
                id,
            });
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

    fn lock(
        &self,
        operation: &'static str,
    ) -> Result<MutexGuard<'_, Vec<AttachedWorld>>, EdgeError> {
        self.worlds
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
