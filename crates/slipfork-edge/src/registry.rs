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
//! The `Host` port-strip and DNS-label grammar is owned by `slipfork_world`; the table grammar and
//! longest-prefix resolution live in [`ingress_table`], so this module only owns the world index.
//!
//! Every request-path error names the world id and the `Host` header.

mod ingress_table;

use std::net::TcpStream;
use std::sync::{Arc, Mutex, MutexGuard};

use slipfork_world::{LocalHostRoute, ProjectSlug, WorldOrigin, WorldTarget, normalize_host};
use slipfork_world::{WorldEngine, WorldHandle};

use crate::EdgeError;
use crate::forward::{ForwardBridge, ForwardTargetSpec};
use ingress_table::IngressTable;

/// One ingress route mapped to a backend port inside the world.
///
/// Two spellings, both supplied by the caller (the edge holds no CLI routing tables):
///
/// - a path prefix (`"/api"`, `"/"`, or `""` for the `internal_ports[0]` catch-all), matched
///   longest-prefix-first and only after the host is validated; or
/// - `<label>.*` (`"mail.*"`), the `label.<world>.localhost` / `label.<fork>.<world>.localhost`
///   host family, which owns every path on that host.
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
    ingresses: IngressTable,
}

impl AttachedWorld {
    fn id(&self) -> String {
        world_id(&self.project, &self.target)
    }

    /// The path-prefix route for `path`, or a typed missing-route refusal.
    fn routed_by_path(&self, host: &str, path: &str) -> Result<RoutedWorld, EdgeError> {
        let port = self
            .ingresses
            .path_port(path)
            .ok_or_else(|| EdgeError::UnknownIngressPath {
                world: self.id(),
                host: host.to_owned(),
                path: path.to_owned(),
            })?;
        Ok(self.routed(host, port))
    }

    /// A resolved backend port on this world, carrying the diagnostic identity.
    fn routed(&self, host: &str, port: u16) -> RoutedWorld {
        RoutedWorld {
            world: self.world.clone(),
            port,
            host: host.to_owned(),
            id: self.id(),
        }
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
        let ingresses = IngressTable::new(ingresses);
        ingresses.validate(&id)?;
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
        let ingresses = IngressTable::new(ingresses);
        ingresses.validate(&id)?;
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
    /// A `*.localhost` host of an attached world is proxied; any `:<port>` suffix is stripped
    /// first, so `demo.localhost:8080` (an `ssh -L` tunnel) routes exactly like `demo.localhost`.
    /// A route table entry of the form `<label>.*` matches the `label.<world>.localhost` host
    /// family; `/`, `/api`, and the empty catch-all remain path prefixes. Path traversal, encoded
    /// separators, and hosts outside `.localhost` fail closed. The routing table is borrowed only
    /// long enough to resolve, so the caller may dial the world without holding the registry lock.
    pub fn resolve(&self, host: &str, path: &str) -> Result<RoutedWorld, EdgeError> {
        let Some(normalized) = normalize_ingress_host(host) else {
            return Err(EdgeError::HostOutsideLocalhost {
                host: host.to_owned(),
            });
        };
        if path_is_traversal(path) {
            return Err(EdgeError::IngressTraversal {
                host: host.to_owned(),
                path: path.to_owned(),
            });
        }
        let state = self.lock("route")?;
        for entry in state.worlds.iter() {
            let snow_domains: Vec<&str> = entry.snow_domains.iter().map(String::as_str).collect();
            // An explicit `<label>.*` route owns that subdomain's host family; the remaining
            // labels are validated as the world's own identity (canonical or fork).
            if let Some((port, remainder)) = entry.ingresses.subdomain_route(&normalized) {
                let Some(route) = LocalHostRoute::parse(&entry.project, &snow_domains, remainder)
                else {
                    continue;
                };
                if route.target() != &entry.target {
                    continue;
                }
                return Ok(entry.routed(host, port));
            }
            let Some(route) = LocalHostRoute::parse(&entry.project, &snow_domains, &normalized)
            else {
                continue;
            };
            if route.target() != &entry.target {
                continue;
            }
            return entry.routed_by_path(host, path);
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

/// Normalize one `Host` header for routing and assert the `.localhost` namespace.
///
/// The port-strip, bracket, trailing-dot, and lowercase grammar is owned by
/// [`slipfork_world::normalize_host`]; this daemon only adds the one namespace assertion, so
/// `demo.localhost:8080` and `demo.localhost` name the same world and `demo.localhost.evil.com`
/// is refused.
fn normalize_ingress_host(header: &str) -> Option<String> {
    let host = normalize_host(header)?;
    host.ends_with(".localhost").then_some(host)
}

/// `true` when any raw or percent-encoded path segment tries to climb out of the route.
///
/// Raw `..` segments are refused, and so is any segment carrying an encoded separator (`%2e`,
/// `%2f`, `%5c`) or a backslash, so `/a/..%2fetc` and `/a/%2e%2e%2fb` never reach a backend.
fn path_is_traversal(path: &str) -> bool {
    let path = path.split(['?', '#']).next().unwrap_or("");
    path.split('/').any(|segment| {
        let segment = segment.to_ascii_lowercase();
        matches!(segment.as_str(), ".." | "%2e%2e" | "%2e." | ".%2e")
            || segment.contains("%2e")
            || segment.contains("%2f")
            || segment.contains("%5c")
            || segment.contains('\\')
    })
}

#[cfg(test)]
mod routing_tests {
    use super::{normalize_ingress_host, path_is_traversal};

    #[test]
    fn a_localhost_port_is_stripped_before_matching() {
        assert_eq!(
            normalize_ingress_host("demo.localhost:8080").as_deref(),
            Some("demo.localhost")
        );
        assert_eq!(
            normalize_ingress_host("DEMO.localhost.:80").as_deref(),
            Some("demo.localhost")
        );
        assert_eq!(
            normalize_ingress_host("demo.localhost.evil.com"),
            None,
            "a foreign host is never a .localhost world"
        );
        assert_eq!(normalize_ingress_host("demo.localhost:http"), None);
        assert_eq!(normalize_ingress_host("[::1]:80"), None);
    }

    #[test]
    fn traversal_is_refused() {
        assert!(path_is_traversal("../etc/passwd"));
        assert!(path_is_traversal("/api/../secret"));
        assert!(path_is_traversal("/a/%2E%2E/b"));
        assert!(path_is_traversal("/a/..%2fetc"));
        assert!(path_is_traversal("/a/%2e%2e%2fb"));
        assert!(path_is_traversal("/a/%5c..%5cetc"));
        assert!(path_is_traversal(r"/a\..\etc"));
        assert!(!path_is_traversal("/api/..b"));
        assert!(!path_is_traversal("/api/v1/users"));
    }
}
