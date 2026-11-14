//! `snow.yml` v2 schema, structural validation, `modes` inheritance (`modes.all` + `modes.<m>`),
//! and local/cloud `ResourceCoordinates`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::faceless::render_template;
use crate::provider::{EntropySource, ProviderError, SecretProvider, SecretValue};

/// Map of variable name to resolved [`SecretValue`].
pub type ResolvedEnvMap = BTreeMap<String, SecretValue>;

/// One missing required reference discovered during exhaustive resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissingRef {
    pub expression: String,
    pub referenced_by: String,
}

/// Errors produced when parsing, validating, or resolving `snow.yml`.
#[derive(Debug, Error)]
pub enum SnowError {
    #[error("invalid snow.yml YAML: {0}")]
    Yaml(String),
    #[error(
        "snow.yml cannot define both top-level 'envs' and 'modes'; migrate 'envs' into 'modes.all' / 'modes.<mode>'"
    )]
    EnvsAndModesMutuallyExclusive,
    #[error("mode '*' is forbidden in snow.yml; use 'all' for the universal base mode")]
    AsteriskModeForbidden,
    #[error("mode '{requested}' is not defined in snow.yml (available modes: {available})")]
    UnknownMode {
        requested: String,
        available: String,
    },
    #[error("snow.yml validation failed:\n  - {}", .0.join("\n  - "))]
    Validation(Vec<String>),
    #[error("unclosed '${{{{' expression in template: {0}")]
    UnclosedTemplate(String),
    #[error(
        "invalid namespace separator in '${{{{ {0} }}}}': use '::' (for example 'secrets::KEY')"
    )]
    InvalidNamespaceSeparator(String),
    #[error("invalid Faceless expression '${{{{ {0} }}}}'")]
    InvalidExpression(String),
    #[error("default value after '??' must be a valid JSON string literal, got: {0}")]
    InvalidDefaultLiteral(String),
    #[error(
        "invalid '!!' generator '{0}'; expected random_string(1..=512), random_hex(1..=256), random_base64(1..=256), or uuid4()"
    )]
    InvalidGenerator(String),
    #[error("'!!' generator is only allowed on single-key 'secrets::<KEY>', got '${{{{ {0} }}}}'")]
    EnsureOnlyAllowedOnSecrets(String),
    #[error("unknown resource/component/deployment path '${{{{ {0} }}}}'")]
    UnknownResourcePath(String),
    #[error(
        "missing required values for mode '{mode}':\n{}",
        format_missing(missing)
    )]
    MissingValues {
        mode: String,
        missing: Vec<MissingRef>,
    },
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

fn format_missing(items: &[MissingRef]) -> String {
    items
        .iter()
        .map(|m| format!("  - {} (referenced by {})", m.expression, m.referenced_by))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildImage {
    pub id: String,
    pub context: String,
    #[serde(default = "default_dockerfile")]
    pub dockerfile: String,
    #[serde(default)]
    pub envs: Vec<String>,
}

fn default_dockerfile() -> String {
    "Dockerfile".to_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Image {
    Build(BuildImage),
    DockerHub {
        id: String,
        registry: String,
        repository: String,
        #[serde(default = "default_tag")]
        tag: String,
    },
}

fn default_tag() -> String {
    "latest".to_owned()
}

impl Image {
    pub fn id(&self) -> &str {
        match self {
            Self::Build(b) => &b.id,
            Self::DockerHub { id, .. } => id,
        }
    }

    pub fn build_envs(&self) -> &[String] {
        match self {
            Self::Build(b) => &b.envs,
            Self::DockerHub { .. } => &[],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Component {
    pub id: String,
    pub image: String,
    #[serde(default)]
    pub run_command: String,
    #[serde(default = "default_size_hint")]
    pub size_hint: String,
    #[serde(default)]
    pub envs: Vec<String>,
    #[serde(default)]
    pub internal_ports: Vec<u16>,
}

fn default_size_hint() -> String {
    "*".to_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngressMatch {
    pub prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngressTarget {
    pub component: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ingress {
    #[serde(rename = "match")]
    pub match_rule: IngressMatch,
    pub target: IngressTarget,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum HookPhase {
    PreDeploy,
    PostDeploy,
    FailedDeploy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hook {
    pub component: String,
    pub when: HookPhase,
    #[serde(default)]
    pub run_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Domain {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PgCluster {
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PgDatabase {
    pub id: String,
    pub cluster: String,
    #[serde(default = "default_true")]
    pub pool: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RedisCluster {
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct S3Bucket {
    pub id: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Resources {
    #[serde(default)]
    pub pg_clusters: Vec<PgCluster>,
    #[serde(default)]
    pub pg_databases: Vec<PgDatabase>,
    #[serde(default)]
    pub redis_clusters: Vec<RedisCluster>,
    #[serde(default)]
    pub s3_buckets: Vec<S3Bucket>,
}

/// One mode entry inside `modes:` (`all`, `local`, `cloud`, etc.).
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModeSpec {
    #[serde(default)]
    pub envs: BTreeMap<String, BTreeMap<String, String>>,
}

/// Parsed `snow.yml` v2 document.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnowFile {
    #[serde(default)]
    pub images: Vec<Image>,
    #[serde(default)]
    pub components: Vec<Component>,
    #[serde(default)]
    pub ingresses: Vec<Ingress>,
    #[serde(default)]
    pub hooks: Vec<Hook>,
    #[serde(default)]
    pub domains: Vec<Domain>,
    #[serde(default)]
    pub resources: Resources,
    #[serde(default)]
    pub envs: Option<BTreeMap<String, BTreeMap<String, String>>>,
    #[serde(default)]
    pub modes: Option<BTreeMap<String, ModeSpec>>,
}

impl SnowFile {
    /// Parse and structurally validate a `snow.yml` document.
    pub fn parse_yaml(yaml: &str) -> Result<Self, SnowError> {
        let snow: Self =
            serde_saphyr::from_str(yaml).map_err(|e| SnowError::Yaml(e.to_string()))?;
        snow.validate_structure()?;
        Ok(snow)
    }

    /// Return the merged template map `group_id -> (var_name -> template)` for `mode`.
    pub fn effective_env_groups(
        &self,
        mode: &str,
    ) -> Result<BTreeMap<String, BTreeMap<String, String>>, SnowError> {
        if let Some(modes) = &self.modes {
            if mode == "all" {
                return Ok(modes.get("all").map(|m| m.envs.clone()).unwrap_or_default());
            }
            if !modes.contains_key(mode) {
                let available = modes
                    .keys()
                    .filter(|k| k.as_str() != "all")
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(SnowError::UnknownMode {
                    requested: mode.to_owned(),
                    available: if available.is_empty() {
                        "(none)".to_owned()
                    } else {
                        available
                    },
                });
            }
            let mut merged = modes.get("all").map(|m| m.envs.clone()).unwrap_or_default();
            if let Some(specific) = modes.get(mode) {
                for (group_id, vars) in &specific.envs {
                    let dst = merged.entry(group_id.clone()).or_default();
                    for (k, v) in vars {
                        dst.insert(k.clone(), v.clone());
                    }
                }
            }
            Ok(merged)
        } else {
            Ok(self.envs.clone().unwrap_or_default())
        }
    }

    /// Validate references, mutual exclusion (`envs` vs `modes`), and build/runtime conflicts.
    pub fn validate_structure(&self) -> Result<(), SnowError> {
        if self.envs.is_some() && self.modes.is_some() {
            return Err(SnowError::EnvsAndModesMutuallyExclusive);
        }
        if let Some(modes) = &self.modes
            && modes.contains_key("*")
        {
            return Err(SnowError::AsteriskModeForbidden);
        }

        let mut errs = Vec::new();
        let image_ids: BTreeSet<&str> = self.images.iter().map(Image::id).collect();
        let comp_ids: BTreeSet<&str> = self.components.iter().map(|c| c.id.as_str()).collect();
        let pg_clusters: BTreeSet<&str> = self
            .resources
            .pg_clusters
            .iter()
            .map(|c| c.id.as_str())
            .collect();

        for db in &self.resources.pg_databases {
            if !pg_clusters.contains(db.cluster.as_str()) {
                errs.push(format!(
                    "resources.pg_databases.{}: unknown cluster '{}'",
                    db.id, db.cluster
                ));
            }
        }
        for comp in &self.components {
            if !image_ids.contains(comp.image.as_str()) {
                errs.push(format!(
                    "components.{}: unknown image '{}'",
                    comp.id, comp.image
                ));
            }
        }
        for ing in &self.ingresses {
            if !comp_ids.contains(ing.target.component.as_str()) {
                errs.push(format!(
                    "ingresses.{}: unknown component '{}'",
                    ing.match_rule.prefix, ing.target.component
                ));
            }
        }
        for hook in &self.hooks {
            if !comp_ids.contains(hook.component.as_str()) {
                errs.push(format!("hooks: unknown component '{}'", hook.component));
            }
        }

        // Validate env group references and image/component template conflicts per concrete mode.
        let modes_to_check: Vec<String> = match &self.modes {
            Some(m) => {
                let concrete: Vec<String> =
                    m.keys().filter(|k| k.as_str() != "all").cloned().collect();
                if concrete.is_empty() {
                    vec!["all".to_owned()]
                } else {
                    concrete
                }
            }
            None => vec!["cloud".to_owned()],
        };

        for mode_name in modes_to_check {
            let groups = self.effective_env_groups(&mode_name)?;
            for img in &self.images {
                for g in img.build_envs() {
                    if !groups.contains_key(g) {
                        errs.push(format!(
                            "images.{}: env group '{}' not found in mode '{}'",
                            img.id(),
                            g,
                            mode_name
                        ));
                    }
                }
            }
            for comp in &self.components {
                for g in &comp.envs {
                    if !groups.contains_key(g) {
                        errs.push(format!(
                            "components.{}: env group '{}' not found in mode '{}'",
                            comp.id, g, mode_name
                        ));
                    }
                }
                if let Some(img) = self.images.iter().find(|i| i.id() == comp.image)
                    && !img.build_envs().is_empty()
                {
                    let img_vars = merge_group_list(&groups, img.build_envs());
                    let comp_vars = merge_group_list(&groups, &comp.envs);
                    for (k, iv) in &img_vars {
                        if let Some(cv) = comp_vars.get(k)
                            && iv != cv
                        {
                            errs.push(format!(
                                "mode '{}': variable '{}' conflicts between image '{}' and component '{}'",
                                mode_name, k, img.id(), comp.id
                            ));
                        }
                    }
                }
            }
        }

        if errs.is_empty() {
            Ok(())
        } else {
            Err(SnowError::Validation(errs))
        }
    }

    /// Resolve all environment variables for `component_id` (or every group if `None`) in `mode`.
    pub fn resolve_mode(
        &self,
        mode: &str,
        component_id: Option<&str>,
        provider: &mut dyn SecretProvider,
        resources: &ResourceCoordinates,
        entropy: &mut dyn EntropySource,
    ) -> Result<ResolvedEnvMap, SnowError> {
        let groups = self.effective_env_groups(mode)?;
        let selected_groups: Vec<String> = if let Some(cid) = component_id {
            let comp = self
                .components
                .iter()
                .find(|c| c.id == cid)
                .ok_or_else(|| {
                    SnowError::Validation(vec![format!("component '{cid}' not found")])
                })?;
            comp.envs.clone()
        } else {
            groups.keys().cloned().collect()
        };

        let mut missing = Vec::new();
        let mut out = ResolvedEnvMap::new();

        for group_id in selected_groups {
            let Some(vars) = groups.get(&group_id) else {
                continue;
            };
            for (var_name, tpl) in vars {
                let loc = if self.modes.is_some() {
                    format!("modes.{mode}.envs.{group_id}.{var_name}")
                } else {
                    format!("envs.{group_id}.{var_name}")
                };
                if let Some(val) =
                    render_template(tpl, &loc, provider, resources, entropy, &mut missing)?
                {
                    out.insert(var_name.clone(), val);
                }
            }
        }

        if !missing.is_empty() {
            return Err(SnowError::MissingValues {
                mode: mode.to_owned(),
                missing,
            });
        }
        Ok(out)
    }
}

fn merge_group_list<'a>(
    groups: &'a BTreeMap<String, BTreeMap<String, String>>,
    ids: &[String],
) -> BTreeMap<&'a str, &'a str> {
    let mut out = BTreeMap::new();
    for id in ids {
        if let Some(g) = groups.get(id) {
            for (k, v) in g {
                out.insert(k.as_str(), v.as_str());
            }
        }
    }
    out
}

/// Deterministic coordinates for `resources::*`, `components::*`, and `deployment::*`.
#[derive(Debug, Clone)]
pub struct ResourceCoordinates {
    pub app_url: String,
    pub app_domain: String,
    pub env_name: String,
    pub is_production: bool,
    pub pg_urls: BTreeMap<String, String>,
    pub redis_urls: BTreeMap<String, String>,
    pub s3_endpoints: BTreeMap<String, S3Coordinates>,
    pub component_urls: BTreeMap<String, (String, u16)>,
}

/// Coordinates for a local or cloud S3-compatible bucket.
#[derive(Debug, Clone)]
pub struct S3Coordinates {
    pub endpoint_url: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    pub region: String,
}

impl ResourceCoordinates {
    /// Construct canonical local-world coordinates (`127.0.0.1` inside the isolated CoW world)
    /// from the resources and ingresses declared in `snow`.
    pub fn local_defaults(snow: &SnowFile, project_slug: &str) -> Self {
        let mut pg_urls = BTreeMap::new();
        for db in &snow.resources.pg_databases {
            pg_urls.insert(
                db.id.clone(),
                format!("postgres://127.0.0.1:5432/{}", db.id),
            );
        }
        let mut redis_urls = BTreeMap::new();
        for (idx, rc) in snow.resources.redis_clusters.iter().enumerate() {
            redis_urls.insert(rc.id.clone(), format!("redis://127.0.0.1:6379/{idx}"));
        }
        let mut s3_endpoints = BTreeMap::new();
        for b in &snow.resources.s3_buckets {
            s3_endpoints.insert(
                b.id.clone(),
                S3Coordinates {
                    endpoint_url: "http://127.0.0.1:9000".to_owned(),
                    bucket: format!("{project_slug}-{}", b.id),
                    access_key: "slipfork-local".to_owned(),
                    secret_key: "slipfork-local-secret".to_owned(),
                    region: "us-east-1".to_owned(),
                },
            );
        }
        let mut component_urls = BTreeMap::new();
        for ing in &snow.ingresses {
            component_urls.insert(
                ing.target.component.clone(),
                (
                    format!("http://127.0.0.1:{}", ing.target.port),
                    ing.target.port,
                ),
            );
        }
        for comp in &snow.components {
            if !component_urls.contains_key(&comp.id) {
                let port = comp.internal_ports.first().copied().unwrap_or(8000);
                component_urls.insert(comp.id.clone(), (format!("http://127.0.0.1:{port}"), port));
            }
        }
        Self {
            app_domain: format!("{project_slug}.localtest.me"),
            app_url: format!("http://{project_slug}.localtest.me"),
            env_name: "local".to_owned(),
            is_production: false,
            pg_urls,
            redis_urls,
            s3_endpoints,
            component_urls,
        }
    }

    /// Resolve `(value, is_secret)` for `resources::*`, `components::*`, and `deployment::*`.
    pub fn resolve(&self, namespace: &str, path: &[String]) -> Option<(String, bool)> {
        let parts: Vec<&str> = path.iter().map(String::as_str).collect();
        match (namespace, parts.as_slice()) {
            ("resources", ["app", "url"]) => Some((self.app_url.clone(), false)),
            ("resources", ["app", "domain"]) => Some((self.app_domain.clone(), false)),
            ("resources", ["pg_databases" | "pg_cluster", id, "url"]) => {
                self.pg_urls.get(*id).map(|u| (u.clone(), false))
            }
            ("resources", ["redis_clusters", id, "url"]) => {
                self.redis_urls.get(*id).map(|u| (u.clone(), false))
            }
            ("resources", ["s3_buckets", id, prop]) => {
                let s3 = self.s3_endpoints.get(*id)?;
                match *prop {
                    "endpoint_url" => Some((s3.endpoint_url.clone(), false)),
                    "bucket" => Some((s3.bucket.clone(), false)),
                    "access_key" => Some((s3.access_key.clone(), false)),
                    "secret_key" => Some((s3.secret_key.clone(), true)),
                    "region" => Some((s3.region.clone(), false)),
                    _ => None,
                }
            }
            ("components", [id, prop]) => {
                let (url, port) = self.component_urls.get(*id)?;
                match *prop {
                    "private_url" | "public_url" => Some((url.clone(), false)),
                    "private_domain" => Some(("127.0.0.1".to_owned(), false)),
                    "private_port" => Some((port.to_string(), false)),
                    _ => None,
                }
            }
            ("deployment", ["env", "name"]) => Some((self.env_name.clone(), false)),
            ("deployment", ["is_production"]) => Some((
                if self.is_production { "true" } else { "false" }.to_owned(),
                false,
            )),
            _ => None,
        }
    }
}
