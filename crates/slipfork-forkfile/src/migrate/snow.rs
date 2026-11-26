//! Typed `snow.yml` input schema. Unknown keys are refused by serde.

use std::collections::BTreeMap;

use serde::Deserialize;

pub(crate) type EnvMap = BTreeMap<String, BTreeMap<String, String>>;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snow {
    #[serde(default)]
    pub images: Vec<ImageSpec>,
    #[serde(default)]
    pub resources: Resources,
    #[serde(default)]
    pub domains: Vec<Id>,
    #[serde(default)]
    pub envs: EnvMap,
    #[serde(default)]
    pub components: Vec<Component>,
    #[serde(default)]
    pub ingresses: Vec<Ingress>,
    #[serde(default)]
    pub hooks: Vec<HookSpec>,
    #[serde(default)]
    pub modes: BTreeMap<String, Mode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum Id {
    Name(String),
    Object(IdObject),
}

impl Id {
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Name(id) => id,
            Self::Object(object) => &object.id,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdObject {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImageSpec {
    pub id: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub registry: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub dockerfile: Option<String>,
    #[serde(default)]
    pub envs: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Resources {
    #[serde(default)]
    pub pg_clusters: Vec<PgCluster>,
    #[serde(default)]
    pub pg_databases: Vec<PgDatabase>,
    #[serde(default)]
    pub redis_clusters: Vec<RedisCluster>,
    #[serde(default)]
    pub s3_buckets: Vec<Id>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PgCluster {
    pub id: String,
    #[serde(default)]
    pub version: Option<Version>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum Version {
    Number(u32),
    Text(String),
}

impl Version {
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Number(value) => value.to_string(),
            Self::Text(value) => value.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PgDatabase {
    pub id: String,
    pub cluster: String,
    /// Legacy `snow.yml` key accepted for `deny_unknown_fields`; never read.
    #[serde(default)]
    #[expect(dead_code, reason = "parsed for `deny_unknown_fields` only")]
    pub pool: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RedisCluster {
    pub id: String,
    #[serde(default)]
    pub version: Option<Version>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Component {
    pub id: String,
    pub image: String,
    pub run_command: String,
    #[serde(default)]
    pub envs: Vec<String>,
    #[serde(default)]
    pub internal_ports: Vec<u16>,
    /// Legacy `snow.yml` key accepted for `deny_unknown_fields`; never read.
    #[serde(default)]
    #[expect(dead_code, reason = "parsed for `deny_unknown_fields` only")]
    pub size_hint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Ingress {
    #[serde(rename = "match")]
    pub route_match: IngressMatch,
    pub target: IngressTarget,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IngressMatch {
    pub prefix: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IngressTarget {
    pub component: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HookSpec {
    pub component: String,
    pub when: String,
    pub run_command: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Mode {
    #[serde(default)]
    pub envs: EnvMap,
    #[serde(default)]
    pub components: BTreeMap<String, ComponentOverride>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComponentOverride {
    #[serde(default)]
    pub disabled: Option<bool>,
    #[serde(default)]
    pub envs: Vec<String>,
}
