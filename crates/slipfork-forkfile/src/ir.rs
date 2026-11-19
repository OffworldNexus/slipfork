//! Canonical evaluated-manifest IR shared with `slipforkd` and Kerfufoo.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::syntax::{Argv, EnvAssignment, HookPhase};
use crate::vault::{EntropySource, SecretProvider};

/// A resolved property value supplied by the daemon for one entity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsValue {
    /// The resolved text.
    pub value: String,
    /// Whether the value must be redacted everywhere it is displayed.
    pub secret: bool,
}
/// Dotted property paths (`url`, `env.branch.name`) to their resolved values.
pub type PropertyMap = BTreeMap<String, FactsValue>;
/// Every concrete world fact an expression can reference.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    /// `app.*`
    pub app: PropertyMap,
    /// `deployment.*`
    pub deployment: PropertyMap,
    /// Postgres property maps keyed by database id.
    pub postgres: BTreeMap<String, PropertyMap>,
    /// Redis property maps keyed by cluster id.
    pub redis: BTreeMap<String, PropertyMap>,
    /// S3 property maps keyed by bucket id.
    pub s3: BTreeMap<String, PropertyMap>,
    /// Domain property maps keyed by domain id.
    pub domains: BTreeMap<String, PropertyMap>,
    /// Service property maps keyed by service id.
    pub services: BTreeMap<String, PropertyMap>,
}
/// The kind of one concrete manifest entity an expression can reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    /// A `postgres <cluster>` declaration.
    PgCluster,
    /// One `database <id>` inside a cluster.
    PgDatabase,
    /// A `redis <cluster>` declaration.
    Redis,
    /// An `s3 <bucket>` declaration.
    S3,
    /// A `domain <id>` declaration.
    Domain,
    /// A `service <id>` declaration.
    Service,
}
impl EntityKind {
    /// The `claims` namespace a client uses to own this entity by id.
    ///
    /// `None` means the entity is not individually claimable: a postgres cluster is owned through
    /// its databases, and `app`/`deployment` live in their own namespaces.
    pub const fn claim_namespace(self) -> Option<&'static str> {
        match self {
            Self::PgCluster => None,
            Self::PgDatabase => Some("pg_databases"),
            Self::Redis => Some("redis_clusters"),
            Self::S3 => Some("s3_buckets"),
            Self::Domain => Some("domains"),
            Self::Service => Some("components"),
        }
    }
}
impl Facts {
    pub(crate) fn lookup(&self, kind: EntityKind, id: &str, property: &str) -> Option<&FactsValue> {
        let table = match kind {
            EntityKind::PgDatabase => &self.postgres,
            EntityKind::Redis => &self.redis,
            EntityKind::S3 => &self.s3,
            EntityKind::Domain => &self.domains,
            EntityKind::Service => &self.services,
            EntityKind::PgCluster => return None,
        };
        table
            .get(id)
            .and_then(|properties| properties.get(property))
    }
}
/// A resolved string that redacts itself whenever a secret contributed to it.
#[derive(Clone, PartialEq, Eq)]
pub struct TaintedString {
    value: String,
    tainted: bool,
}
impl TaintedString {
    /// A value built only from non-secret inputs.
    pub fn plain(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            tainted: false,
        }
    }

    /// A value that any `secret(...)` token contaminated.
    pub fn tainted(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            tainted: true,
        }
    }

    /// Whether the value must be redacted in logs and diagnostics.
    pub const fn is_tainted(&self) -> bool {
        self.tainted
    }

    /// Explicitly expose the plaintext at an execution or adapter boundary.
    pub fn expose(&self) -> &str {
        &self.value
    }

    /// `[REDACTED]` for secret-tainted values, the raw text otherwise.
    pub fn redacted(&self) -> &str {
        if self.tainted {
            "[REDACTED]"
        } else {
            &self.value
        }
    }
}
impl std::fmt::Debug for TaintedString {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "TaintedString({})", self.redacted())
    }
}
impl std::fmt::Display for TaintedString {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.redacted())
    }
}

impl Serialize for TaintedString {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.redacted())
    }
}

impl<'de> Deserialize<'de> for TaintedString {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::plain(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializing_a_tainted_string_never_emits_the_secret() {
        let json = serde_json::to_string(&TaintedString::tainted("hunter2")).expect("serialize");
        assert!(!json.contains("hunter2"), "secret leaked into {json}");
        assert_eq!(json, "\"[REDACTED]\"");

        let restored: TaintedString = serde_json::from_str("\"plain\"").expect("deserialize");
        assert_eq!(restored.redacted(), "plain");
        assert!(!restored.is_tainted());
    }
}
/// The injected ports and concrete facts used by [`EvaluatedFork::resolve_env`].
pub struct Resolvers<'a> {
    /// Local `0600` vault (or a read-only OSPP-backed provider).
    pub provider: &'a dyn SecretProvider,
    /// CSPRNG used by write-once generators.
    pub entropy: &'a dyn EntropySource,
    /// Concrete resource/service facts resolved by the daemon.
    pub facts: &'a Facts,
}
impl<'a> Resolvers<'a> {
    /// Assemble the resolver bundle.
    pub fn new(
        provider: &'a dyn SecretProvider,
        entropy: &'a dyn EntropySource,
        facts: &'a Facts,
    ) -> Self {
        Self {
            provider,
            entropy,
            facts,
        }
    }
}
/// One evaluated lifecycle command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRun {
    /// The lifecycle phase.
    pub phase: HookPhase,
    /// The command line.
    pub argv: Argv,
}
/// A service's evaluated container source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvaluatedImage {
    /// Built from `<context_dir>/<dockerfile>` at `<stage>`.
    Dockerfile {
        /// The build context, relative to the worktree root.
        context_dir: String,
        /// The Dockerfile path (default `Dockerfile`).
        dockerfile: String,
        /// The optional build stage.
        stage: Option<String>,
        /// Build-time environment.
        build_env: Vec<EnvAssignment>,
    },
    /// Pulled directly from an OCI registry.
    OciImage {
        /// The image reference.
        image_ref: String,
    },
}
/// One evaluated postgres database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluatedPgDatabase {
    /// The database id.
    pub id: String,
    /// Whether the database is pooled (`true` by default).
    pub pool: bool,
}
/// One evaluated postgres cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluatedPg {
    /// The cluster id.
    pub id: String,
    /// The PostgreSQL major version.
    pub version: String,
    /// The databases in declaration order.
    pub databases: Vec<EvaluatedPgDatabase>,
}
/// One evaluated redis cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluatedRedis {
    /// The cluster id.
    pub id: String,
    /// The Redis major version.
    pub version: String,
}
/// One evaluated reusable env group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluatedEnvGroup {
    /// The group name.
    pub name: String,
    /// Variables in declaration order.
    pub vars: Vec<EnvAssignment>,
}
/// One evaluated service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluatedService {
    /// The service id.
    pub id: String,
    /// The container source.
    pub image: EvaluatedImage,
    /// The single exposed port, when declared.
    pub port: Option<u32>,
    /// Additional container ports.
    pub internal_ports: Vec<u32>,
    /// Routes in declaration order.
    pub routes: Vec<String>,
    /// CoW-mounted volume paths.
    pub volumes: Vec<String>,
    /// The size hint (`*` by default).
    pub size: String,
    /// The environment layers, merged in order.
    pub env: Vec<EnvAssignment>,
    /// The single supervised command.
    pub run: Option<Argv>,
    /// Lifecycle hooks in declaration order.
    pub hooks: Vec<HookRun>,
}
/// One evaluated task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluatedTask {
    /// The task name.
    pub name: String,
    /// The service the task runs inside.
    pub service: String,
    /// The command lines.
    pub commands: Vec<Argv>,
    /// Whether the task was a multi-line block (and therefore rejects extra args).
    pub multi: bool,
}
/// The feature-evaluated manifest: exactly what `slipforkd` subscribes to OSPP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluatedFork {
    /// The optional `project <slug>` override.
    pub project: Option<String>,
    /// The resolved active feature set, sorted.
    pub features: Vec<String>,
    /// Postgres clusters in declaration order.
    pub postgres: Vec<EvaluatedPg>,
    /// Redis clusters in declaration order.
    pub redis: Vec<EvaluatedRedis>,
    /// S3 bucket ids.
    pub s3: Vec<String>,
    /// Domain ids.
    pub domains: Vec<String>,
    /// Reusable env groups.
    pub env_groups: Vec<EvaluatedEnvGroup>,
    /// Services in declaration order.
    pub services: Vec<EvaluatedService>,
    /// Tasks in declaration order.
    pub tasks: Vec<EvaluatedTask>,
}
