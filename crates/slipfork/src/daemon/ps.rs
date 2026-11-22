//! The one wire schema of `ps`: the daemon serializes it and the CLI renderer deserializes it.
//!
//! Nothing downstream inspects `serde_json::Value` keys or sniffs authority strings: every field is
//! typed here, including the `Forkfile` domain filter mirror.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use slipfork_forkfile::BucketPolicy;

/// The whole `ps` reply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PsReply {
    /// Every owned world, with forks nested under their parent.
    #[serde(default)]
    pub worlds: Vec<PsWorld>,
}

/// One world or fork.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PsWorld {
    /// Project directory the world was booted from.
    pub project: PathBuf,
    /// Project directory, as the status tree prints it.
    pub path: PathBuf,
    /// World id (`demo`, or `<fork>--<world>` for a fork).
    pub world: String,
    /// Fork name, when this world is a fork.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<String>,
    /// Primary URL, `<world>.localhost`.
    pub url: String,
    /// Milliseconds since the Unix epoch when the world became ready; `0` when unknown.
    #[serde(default)]
    pub started_at_ms: u64,
    /// Supervisor process id.
    pub supervisor_pid: u32,
    /// The supervisor still owns its control socket.
    pub alive: bool,
    /// The world is still booting.
    pub booting: bool,
    /// Declared services and resources, in declaration order.
    #[serde(default)]
    pub services: Vec<PsService>,
    /// Forks nested under this world.
    #[serde(default)]
    pub forks: Vec<PsWorld>,
}

/// One service, image, or backing resource row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PsService {
    /// Service, resource, or bucket id.
    pub id: String,
    /// Which kind of row this is.
    pub kind: PsKind,
    /// The tracked pod is alive.
    pub running: bool,
    /// Public port, when declared or implied by the resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Explicit or default backing version (`18`, `7`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// CDN authority attached to an S3 bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cdn: Option<String>,
    /// Anonymous-read policy of an `s3` bucket; `None` for every other row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PsBucketPolicy>,
    /// Host-visible pod pid, when the world reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    /// Declared ingress routes.
    #[serde(default)]
    pub routes: Vec<PsRoute>,
    /// The tracked pod is alive but no longer declared by the current `Forkfile`.
    #[serde(default)]
    pub orphaned: bool,
    /// Enclosing `image <id>` for a service row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

/// The closed set of `ps` row kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PsKind {
    /// A `service` pod.
    Service,
    /// An image without its own service pod.
    Image,
    /// A `postgres` resource.
    Postgres,
    /// A `redis` resource.
    Redis,
    /// An `s3` bucket.
    S3,
    /// A CDN authority.
    Cdn,
}

/// One declared ingress route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PsRoute {
    /// Leading path prefix.
    pub prefix: String,
    /// Upstream port inside the world.
    pub port: u16,
    /// Prefix handling.
    pub mode: PsRouteMode,
    /// Authority filter, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<PsDomain>,
}

/// How a route treats its matched path prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PsRouteMode {
    /// Forward the path unchanged.
    Preserve,
    /// Strip the matched prefix.
    StripPrefix,
    /// Rewrite the prefix to another path.
    Rewrite,
    /// Match the prefix exactly.
    Exact,
}

/// A `domain <id>` or `domain <prefix>.*` authority filter, mirroring the `Forkfile` AST.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PsDomain {
    /// One exact declared domain id.
    Domain(String),
    /// A subdomain prefix such as `mail`.
    Prefix(String),
}

/// The anonymous-read policy of one native S3 bucket, as `ps --json` exposes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PsBucketPolicy {
    /// Anonymous reads are refused with `403`.
    Private,
    /// Anonymous `GET`/`HEAD` succeed; writes still need SigV4.
    PublicRead,
}

impl From<BucketPolicy> for PsBucketPolicy {
    fn from(policy: BucketPolicy) -> Self {
        match policy {
            BucketPolicy::Private => Self::Private,
            BucketPolicy::PublicRead => Self::PublicRead,
        }
    }
}
