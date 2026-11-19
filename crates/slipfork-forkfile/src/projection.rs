//! The canonical, pure world projection that the daemon, the CLI, and OSPP all read.
//!
//! Everything here is derived from the active feature set alone: no process environment, no clock,
//! no filesystem, and no provider I/O. Coordinates are the canonical local-world ones — backing
//! services bind loopback with one port per cluster, and names are
//! `<entity>.<project>.localhost` — so two consumers can never disagree about what a world
//! declares. Secret properties (`password`, `secret_key`, …) are deliberately absent: they are
//! vault values reached through `secret(<KEY>)`, never projection text.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::diag::{Diagnostic, Span};
use crate::ir::{DeploymentFacts, EvaluatedFork, EvaluatedImage, Resolvers, TaintedString};
use crate::syntax::Expr;
use crate::vault::{Namespace, SecretProvider};

/// The loopback host every local backing service binds.
const LOOPBACK: &str = "127.0.0.1";
/// First postgres port; each additional cluster takes the next one.
const PG_BASE_PORT: u16 = 5432;
/// First redis port; each additional cluster takes the next one.
const REDIS_BASE_PORT: u16 = 6379;
/// First S3 endpoint port; each additional bucket takes the next one.
const S3_BASE_PORT: u16 = 9000;

/// One env-visible world fact: `<entity>.<property>` and the value the world publishes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldFact {
    /// The declared entity id (`app`, `app_db`, `cache`, `media`, `main`, `api`).
    pub entity: String,
    /// The dotted property path (`url`, `private_domain`).
    pub property: String,
    /// The published value; `[REDACTED]` whenever a secret contributed.
    pub value: TaintedString,
}

/// One database stage the world must provision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldPgDatabase {
    /// The owning cluster id.
    pub cluster: String,
    /// The PostgreSQL major version of that cluster.
    pub version: String,
    /// The database id.
    pub database: String,
    /// Whether the database is pooled (`true` by default).
    pub pool: bool,
}

/// One image stage the world must build, deduplicated by the engine on
/// `(context_dir, dockerfile, stage, build_env)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildImage {
    /// The service the stage belongs to.
    pub service: String,
    /// The build context, relative to the worktree root.
    pub context_dir: String,
    /// The Dockerfile path.
    pub dockerfile: String,
    /// The optional build stage.
    pub stage: Option<String>,
    /// The declared build-time environment keys (values resolve through
    /// [`EvaluatedFork::resolve_build_env`]).
    pub build_env: Vec<String>,
}

/// One `route` a service publishes through the single-origin ingress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngressRoute {
    /// The service that owns the route.
    pub service: String,
    /// The container port to dial, when the service declares one.
    pub port: Option<u32>,
    /// The route pattern (`/api`, `/`, or one lowercase label like `mail.*`).
    pub route: String,
}

impl EvaluatedFork {
    /// Every env-visible world fact, derived purely from the active feature set.
    ///
    /// Postgres databases expose `host`, `port`, `user`, `db_name`, `url`, and `jdbc`; redis
    /// clusters expose `host`, `port`, `user`, and `url`; S3 buckets expose `port`, `endpoint`,
    /// `bucket`, `region`, and `url`; domains expose `name` and `url`; services expose
    /// `private_domain`, `private_port`, `private_url`, `public_route_path`, and `public_url`;
    /// `app` exposes `id`, `domain`, and `url`.
    ///
    /// The `<entity>.<project>.localhost` and `app.*` facts are omitted when the manifest declares
    /// no `project <slug>`; everything else stays world-local. This method is the single source of
    /// `$app_db.url`-style references: [`Self::resolve_env`] and [`Self::resolve_build_env`] read
    /// `Self::world_projection`, which starts from these facts and adds the declared local
    /// passwords plus the caller's deployment metadata.
    pub fn world_facts(&self) -> Vec<WorldFact> {
        let mut facts = Vec::new();
        if let Some(project) = &self.project {
            let domain = format!("{project}.localhost");
            push_facts(
                &mut facts,
                "app",
                &[
                    ("id", project.clone()),
                    ("domain", domain.clone()),
                    ("url", format!("http://{domain}")),
                ],
            );
            for id in &self.domains {
                let name = format!("{id}.{project}.localhost");
                push_facts(
                    &mut facts,
                    id,
                    &[("name", name.clone()), ("url", format!("http://{name}"))],
                );
            }
            for service in &self.services {
                let private_domain = format!("{}.{}.localhost", service.id, project);
                let mut properties = vec![
                    ("private_domain", private_domain.clone()),
                    ("private_url", format!("http://{private_domain}")),
                ];
                if let Some(port) = service.port {
                    properties.push(("private_port", port.to_string()));
                }
                if let Some(route) = service.routes.first() {
                    properties.push(("public_route_path", route.clone()));
                    properties.push(("public_url", public_url(project, route)));
                }
                push_facts(&mut facts, &service.id, &properties);
            }
        }
        for (index, cluster) in self.postgres.iter().enumerate() {
            let port = PG_BASE_PORT + index as u16;
            for database in &cluster.databases {
                let id = &database.id;
                push_facts(
                    &mut facts,
                    id,
                    &[
                        ("host", LOOPBACK.to_owned()),
                        ("port", port.to_string()),
                        ("user", "slipfork".to_owned()),
                        ("db_name", id.clone()),
                        ("url", format!("postgres://slipfork@{LOOPBACK}:{port}/{id}")),
                        ("jdbc", format!("jdbc:postgresql://{LOOPBACK}:{port}/{id}")),
                    ],
                );
            }
        }
        for (index, cluster) in self.redis.iter().enumerate() {
            let port = REDIS_BASE_PORT + index as u16;
            push_facts(
                &mut facts,
                &cluster.id,
                &[
                    ("host", LOOPBACK.to_owned()),
                    ("port", port.to_string()),
                    ("user", "default".to_owned()),
                    ("url", format!("redis://{LOOPBACK}:{port}")),
                ],
            );
        }
        for (index, bucket) in self.s3.iter().enumerate() {
            let port = S3_BASE_PORT + index as u16;
            let endpoint = format!("http://{LOOPBACK}:{port}");
            push_facts(
                &mut facts,
                bucket,
                &[
                    ("port", port.to_string()),
                    ("endpoint", endpoint.clone()),
                    ("bucket", bucket.clone()),
                    ("region", "us-east-1".to_owned()),
                    ("url", endpoint),
                ],
            );
        }
        facts
    }

    /// The value of one `<entity>.<property>` fact, or `None` when the world does not publish it.
    pub(crate) fn world_projection(
        &self,
        resolvers: &Resolvers<'_>,
    ) -> Result<Vec<WorldFact>, Diagnostic> {
        let mut facts = self.world_facts();
        self.compose_local_passwords(&mut facts, resolvers.provider)?;
        facts.extend(deployment_facts(&resolvers.deployment));
        Ok(facts)
    }

    /// Compose every declared local backing-service password into its `url`/`jdbc` facts.
    ///
    /// A local password is declared by storing `<ID>_PASSWORD` in the vault (Tier 1 stores
    /// strictly local backing-service passwords); when it is present the coordinate carries it and
    /// the fact is secret-tainted, otherwise the coordinate stays passwordless plaintext.
    fn compose_local_passwords(
        &self,
        facts: &mut [WorldFact],
        provider: &dyn SecretProvider,
    ) -> Result<(), Diagnostic> {
        let mut passwords: BTreeMap<&str, (bool, String)> = BTreeMap::new();
        for cluster in &self.postgres {
            for database in &cluster.databases {
                if let Some(password) = local_password(provider, &database.id)? {
                    passwords.insert(database.id.as_str(), (true, password));
                }
            }
        }
        for cluster in &self.redis {
            if let Some(password) = local_password(provider, &cluster.id)? {
                passwords.insert(cluster.id.as_str(), (false, password));
            }
        }
        for fact in facts.iter_mut() {
            let Some((postgres, password)) = passwords.get(fact.entity.as_str()) else {
                continue;
            };
            let password = encode_component(password);
            let composed = match (fact.property.as_str(), postgres) {
                ("url", true) => Some(fact.value.expose().replacen(
                    "://slipfork@",
                    &format!("://slipfork:{password}@"),
                    1,
                )),
                ("jdbc", true) => Some(format!(
                    "{}?user=slipfork&password={password}",
                    fact.value.expose()
                )),
                ("url", false) => Some(fact.value.expose().replacen(
                    "redis://",
                    &format!("redis://:{password}@"),
                    1,
                )),
                _ => None,
            };
            if let Some(composed) = composed {
                fact.value = TaintedString::tainted(composed);
            }
        }
        Ok(())
    }

    /// Every `secret(<KEY>) ?? <generator>` key the world must generate exactly once.
    ///
    /// Keys are collected in sorted order from env groups, service environments, and build
    /// environments, so the vault can be pre-warmed before any container starts.
    pub fn generated_secret_keys(&self) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        for group in &self.env_groups {
            for assignment in &group.vars {
                collect_generated(&assignment.expr, &mut keys);
            }
        }
        for service in &self.services {
            for assignment in &service.env {
                collect_generated(&assignment.expr, &mut keys);
            }
            if let EvaluatedImage::Dockerfile { build_env, .. } = &service.image {
                for assignment in build_env {
                    collect_generated(&assignment.expr, &mut keys);
                }
            }
        }
        keys
    }

    /// Every Dockerfile stage the world must build, in service declaration order.
    pub fn build_images(&self) -> Vec<BuildImage> {
        self.services
            .iter()
            .filter_map(|service| match &service.image {
                EvaluatedImage::Dockerfile {
                    context_dir,
                    dockerfile,
                    stage,
                    build_env,
                } => Some(BuildImage {
                    service: service.id.clone(),
                    context_dir: context_dir.clone(),
                    dockerfile: dockerfile.clone(),
                    stage: stage.clone(),
                    build_env: build_env
                        .iter()
                        .map(|assignment| assignment.key.value.clone())
                        .collect(),
                }),
                EvaluatedImage::OciImage { .. } => None,
            })
            .collect()
    }

    /// Every postgres database to provision, in cluster then database declaration order.
    ///
    /// This is the whole resource declaration the daemon feeds into
    /// `slipfork_dag::ResourcePlan`; strategy overrides never live in the manifest.
    pub fn pg_databases(&self) -> Vec<WorldPgDatabase> {
        self.postgres
            .iter()
            .flat_map(|cluster| {
                cluster.databases.iter().map(|database| WorldPgDatabase {
                    cluster: cluster.id.clone(),
                    version: cluster.version.clone(),
                    database: database.id.clone(),
                    pool: database.pool,
                })
            })
            .collect()
    }

    /// Every `route` the single-origin ingress must serve, in service then route declaration order.
    pub fn ingress_routes(&self) -> Vec<IngressRoute> {
        self.services
            .iter()
            .flat_map(|service| {
                service.routes.iter().map(|route| IngressRoute {
                    service: service.id.clone(),
                    port: service.port,
                    route: route.clone(),
                })
            })
            .collect()
    }
}

fn push_facts(facts: &mut Vec<WorldFact>, entity: &str, properties: &[(&str, String)]) {
    facts.extend(properties.iter().map(|(property, value)| WorldFact {
        entity: entity.to_owned(),
        property: (*property).to_owned(),
        value: TaintedString::plain(value.clone()),
    }));
}

fn public_url(project: &str, route: &str) -> String {
    if let Some(label) = route.strip_suffix(".*") {
        return format!("http://{label}.{project}.localhost");
    }
    if route == "/" {
        return format!("http://{project}.localhost");
    }
    format!("http://{project}.localhost{route}")
}

fn collect_generated(expr: &Expr, keys: &mut BTreeSet<String>) {
    match expr {
        Expr::Fallback { lhs, rhs, .. } => {
            if let (Expr::Secret(key), Expr::Generator { .. }) = (&**lhs, &**rhs) {
                keys.insert(key.value.clone());
            }
            collect_generated(lhs, keys);
            collect_generated(rhs, keys);
        }
        Expr::Concat { parts, .. } => {
            for part in parts {
                collect_generated(part, keys);
            }
        }
        Expr::Str(_)
        | Expr::Int(_)
        | Expr::Bool(_)
        | Expr::Ref(_)
        | Expr::Var(_)
        | Expr::Secret(_)
        | Expr::Generator { .. } => {}
    }
}

/// Look one `<entity>.<property>` fact up in an already-built projection.
pub(crate) fn lookup<'a>(
    facts: &'a [WorldFact],
    entity: &str,
    property: &str,
) -> Option<&'a TaintedString> {
    facts
        .iter()
        .find(|fact| fact.entity == entity && fact.property == property)
        .map(|fact| &fact.value)
}

/// The vault key holding one local backing-service password (`app_db` -> `APP_DB_PASSWORD`).
pub(crate) fn password_key(entity: &str) -> String {
    let mut key = String::with_capacity(entity.len() + "_PASSWORD".len());
    for character in entity.chars() {
        key.push(if character.is_ascii_alphanumeric() {
            character.to_ascii_uppercase()
        } else {
            '_'
        });
    }
    key.push_str("_PASSWORD");
    key
}

/// Read one locally declared password, or `None` when the vault has no such entry.
fn local_password(
    provider: &dyn SecretProvider,
    entity: &str,
) -> Result<Option<String>, Diagnostic> {
    let key = password_key(entity);
    let stored = provider
        .get(Namespace::Secrets, &key)
        .map_err(|error| Diagnostic::new(Span::zero(), error.to_string()))?;
    Ok(stored.filter(|value| !value.is_empty()))
}

/// Percent-encode one URL userinfo component so a generated password cannot alter a coordinate.
fn encode_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// The fourth precedence tier: deployment metadata supplied by the caller, never by a manifest.
fn deployment_facts(deployment: &DeploymentFacts) -> Vec<WorldFact> {
    let mut facts = Vec::new();
    {
        let mut push = |property: &str, value: Option<String>| {
            if let Some(value) = value {
                facts.push(WorldFact {
                    entity: "deployment".to_owned(),
                    property: property.to_owned(),
                    value: TaintedString::plain(value),
                });
            }
        };
        push("id", deployment.id.clone());
        push("date_create", deployment.date_create.clone());
        push("date_event", deployment.date_event.clone());
        push("cause", deployment.cause.clone());
        push("commit_sha", deployment.commit_sha.clone());
        push(
            "is_production",
            deployment.is_production.map(|value| value.to_string()),
        );
        push(
            "is_leader",
            deployment.is_leader.map(|value| value.to_string()),
        );
        push("env.name", deployment.env_name.clone());
        push("env.branch.name", deployment.env_branch_name.clone());
        push("env.branch.repo", deployment.env_branch_repo.clone());
        push("product.name", deployment.product_name.clone());
        push("product.slug", deployment.product_slug.clone());
    }
    facts
}
