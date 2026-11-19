//! Upstream `snow.yml` model: reading, template/argv materialization, and cross-reference
//! validation. Everything here runs before [`super::render`] emits a byte.

use std::collections::BTreeMap;

use super::access::{Access, parent_ids, parent_sequence};
use super::template::{render_template, split_command};
use super::{MigrateError, schema};
use crate::yaml::{Entry, Node};

/// The validated upstream document.
pub struct Snow {
    pub(super) images: Vec<Image>,
    pub(super) pg_clusters: Vec<(String, String)>,
    pub(super) pg_databases: Vec<PgDatabase>,
    pub(super) redis: Vec<(String, String)>,
    pub(super) s3: Vec<String>,
    pub(super) domains: Vec<String>,
    /// Rendered `Forkfile` expressions, keyed by env group then variable.
    pub(super) env_groups: BTreeMap<String, BTreeMap<String, String>>,
    /// Mode-scoped env groups: `(mode, group, vars)`, rendered as `on <mode> { env … }`.
    pub(super) mode_envs: Vec<(String, String, BTreeMap<String, String>)>,
    pub(super) components: Vec<Component>,
    pub(super) ingresses: Vec<Ingress>,
    pub(super) hooks: Vec<Hook>,
    /// Feature names referenced by `on <mode>` overrides, in first-use order.
    pub(super) features: Vec<String>,
}

pub(super) struct Image {
    pub(super) id: String,
    pub(super) path: String,
    /// `None` means "pull this reference"; `Some(context)` means "build from `./<context>`".
    pub(super) context: Option<String>,
    pub(super) dockerfile: String,
    pub(super) reference: String,
    pub(super) build_envs: Vec<String>,
}

pub(super) struct PgDatabase {
    pub(super) id: String,
    pub(super) cluster: String,
    pub(super) pool: bool,
}

pub(super) struct Component {
    pub(super) id: String,
    pub(super) path: String,
    pub(super) image: String,
    pub(super) run: Vec<String>,
    pub(super) size_hint: String,
    pub(super) envs: Vec<String>,
    pub(super) internal_ports: Vec<u16>,
    /// `(mode, argv)` overrides, translated to `on <mode>: run …`.
    pub(super) mode_runs: Vec<(String, Vec<String>)>,
}

pub(super) struct Ingress {
    pub(super) path: String,
    pub(super) prefix: String,
    pub(super) component: String,
    pub(super) port: u16,
}

pub(super) struct Hook {
    pub(super) path: String,
    pub(super) component: String,
    pub(super) phase: String,
    pub(super) argv: Vec<String>,
}

impl Snow {
    pub(super) fn read(root: &Node) -> Result<Self, MigrateError> {
        let images = root
            .sequence("images")?
            .iter()
            .enumerate()
            .map(|(index, node)| Image::read(node, index))
            .collect::<Result<Vec<_>, _>>()?;
        let resources = root.get("resources");
        let pg_clusters = parent_sequence(resources, "pg_clusters")?
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let path = format!("resources.pg_clusters[{index}]");
                Ok((
                    node.scalar("id", &path)?,
                    node.text("version").unwrap_or_else(|| "18".to_owned()),
                ))
            })
            .collect::<Result<Vec<_>, MigrateError>>()?;
        let pg_databases = parent_sequence(resources, "pg_databases")?
            .iter()
            .enumerate()
            .map(|(index, node)| PgDatabase::read(node, index))
            .collect::<Result<Vec<_>, _>>()?;
        let redis = parent_sequence(resources, "redis_clusters")?
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let path = format!("resources.redis_clusters[{index}]");
                Ok((
                    node.scalar("id", &path)?,
                    node.text("version").unwrap_or_else(|| "7".to_owned()),
                ))
            })
            .collect::<Result<Vec<_>, MigrateError>>()?;
        let s3 = parent_ids(resources, "s3_buckets", "resources.s3_buckets")?;
        let domains = root.ids("domains", "domains")?;
        let env_groups = read_env_groups(root)?;
        let mut components = root
            .sequence("components")?
            .iter()
            .enumerate()
            .map(|(index, node)| Component::read(node, index))
            .collect::<Result<Vec<_>, _>>()?;
        let ingresses = root
            .sequence("ingresses")?
            .iter()
            .enumerate()
            .map(|(index, node)| Ingress::read(node, index))
            .collect::<Result<Vec<_>, _>>()?;
        let hooks = root
            .sequence("hooks")?
            .iter()
            .enumerate()
            .map(|(index, node)| Hook::read(node, index))
            .collect::<Result<Vec<_>, _>>()?;
        let mut features: Vec<String> = Vec::new();
        let mut mode_envs = Vec::new();
        read_modes(root, &mut components, &mut features, &mut mode_envs)?;
        Ok(Self {
            images,
            pg_clusters,
            pg_databases,
            redis,
            s3,
            domains,
            env_groups,
            mode_envs,
            components,
            ingresses,
            hooks,
            features,
        })
    }
}

impl Image {
    fn read(node: &Node, index: usize) -> Result<Self, MigrateError> {
        let path = format!("images[{index}]");
        let id = node.scalar("id", &path)?;
        let (context, dockerfile, reference) = if node.get("context").is_some() {
            let context = node.scalar("context", &path)?;
            let context = context.trim_start_matches("./").to_owned();
            let dockerfile = node
                .text("dockerfile")
                .unwrap_or_else(|| "Dockerfile".to_owned());
            (Some(context), dockerfile, String::new())
        } else {
            let registry = node.scalar("registry", &path)?;
            let repository = node.scalar("repository", &path)?;
            let tag = node.text("tag").unwrap_or_else(|| "latest".to_owned());
            (
                None,
                "Dockerfile".to_owned(),
                format!("{registry}/{repository}:{tag}"),
            )
        };
        let build_envs = node.strings("envs").map_err(|error| match error {
            MigrateError::Schema { message, .. } => schema(
                format!("{path}.envs"),
                message,
                "list declared env-group names only",
            ),
            other => other,
        })?;
        Ok(Self {
            id,
            path: path.clone(),
            context,
            dockerfile,
            reference,
            build_envs,
        })
    }
}

impl PgDatabase {
    fn read(node: &Node, index: usize) -> Result<Self, MigrateError> {
        let path = format!("resources.pg_databases[{index}]");
        Ok(Self {
            id: node.scalar("id", &path)?,
            cluster: node.scalar("cluster", &path)?,
            pool: node.boolean("pool", &path)?.unwrap_or(true),
        })
    }
}

impl Component {
    fn read(node: &Node, index: usize) -> Result<Self, MigrateError> {
        let path = format!("components[{index}]");
        let run_command = node.scalar("run_command", &path)?;
        let run = split_command(&run_command, &format!("{path}.run_command"))?;
        if run.is_empty() {
            return Err(schema(
                format!("{path}.run_command"),
                "`run_command` must not be empty: a service needs a `run` line",
                "declare the component's run command",
            ));
        }
        Ok(Self {
            id: node.scalar("id", &path)?,
            path: path.clone(),
            image: node.scalar("image", &path)?,
            run,
            size_hint: node.text("size_hint").unwrap_or_else(|| "*".to_owned()),
            envs: node.strings("envs")?,
            internal_ports: node
                .items("internal_ports")
                .iter()
                .map(|entry| {
                    entry
                        .as_scalar()
                        .and_then(|value| value.parse::<u16>().ok())
                        .ok_or_else(|| {
                            schema(
                                format!("{path}.internal_ports"),
                                "expected a u16 port",
                                "write ports as integers",
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?,
            mode_runs: Vec::new(),
        })
    }
}

impl Ingress {
    fn read(node: &Node, index: usize) -> Result<Self, MigrateError> {
        let path = format!("ingresses[{index}]");
        let match_rule = node.get("match").ok_or_else(|| {
            schema(
                format!("{path}.match"),
                "missing `match`",
                "add `match: { prefix: /… }`",
            )
        })?;
        let target = node.get("target").ok_or_else(|| {
            schema(
                format!("{path}.target"),
                "missing `target`",
                "add `target: { component: <id>, port: <u16> }`",
            )
        })?;
        let port = target
            .get("port")
            .and_then(Node::as_scalar)
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or_else(|| {
                schema(
                    format!("{path}.target.port"),
                    "expected a u16 port",
                    "write the target port as an integer",
                )
            })?;
        Ok(Self {
            prefix: match_rule.scalar("prefix", &format!("{path}.match"))?,
            component: target.scalar("component", &format!("{path}.target"))?,
            port,
            path: path.clone(),
        })
    }
}

impl Hook {
    fn read(node: &Node, index: usize) -> Result<Self, MigrateError> {
        let path = format!("hooks[{index}]");
        let run_command = node.scalar("run_command", &path)?;
        Ok(Self {
            component: node.scalar("component", &path)?,
            phase: node.scalar("when", &path)?,
            argv: split_command(&run_command, &format!("{path}.run_command"))?,
            path: path.clone(),
        })
    }
}

fn read_modes(
    root: &Node,
    components: &mut [Component],
    features: &mut Vec<String>,
    mode_envs: &mut Vec<(String, String, BTreeMap<String, String>)>,
) -> Result<(), MigrateError> {
    let Some(modes) = root.get("modes").and_then(Node::as_map) else {
        return Ok(());
    };
    for mode in modes {
        let name = mode.key.as_str();
        let spec = &mode.value;
        if name == "all" {
            if spec
                .get("components")
                .and_then(Node::as_map)
                .is_some_and(|entries| !entries.is_empty())
            {
                return Err(schema(
                    "modes.all.components",
                    "universal component defaults belong on top-level `components`, not `modes.all`",
                    "move the entries to `components`",
                ));
            }
            continue;
        }
        // `modes.<mode>.envs` becomes `on <mode> { env <group> { … } }`: the forkfile keeps one
        // group per name, and the active world mode selects it exactly like any other feature.
        if let Some(envs) = spec.get("envs").and_then(Node::as_map) {
            let mut groups = BTreeMap::new();
            merge_group_entries(envs, &format!("modes.{name}.envs"), &mut groups)?;
            if !groups.is_empty() {
                note_feature(features, name);
            }
            for (group, vars) in groups {
                mode_envs.push((name.to_owned(), group, vars));
            }
        }
        let Some(overrides) = spec.get("components").and_then(Node::as_map) else {
            continue;
        };
        for override_entry in overrides {
            let id = override_entry.key.as_str();
            let override_node = &override_entry.value;
            let path = format!("modes.{name}.components.{id}");
            if override_node.boolean("disabled", &path)?.unwrap_or(false) {
                return Err(schema(
                    format!("{path}.disabled"),
                    "a Forkfile cannot remove a declared service for one mode",
                    "delete the component from `components` and keep the mode override out",
                ));
            }
            let Some(run_command) = override_node.get("run_command").and_then(Node::as_scalar)
            else {
                continue;
            };
            let component = components
                .iter_mut()
                .find(|component| component.id == id)
                .ok_or_else(|| {
                    schema(
                        format!("{path}.run_command"),
                        format!("mode-only component `{id}` is not declared in `components`"),
                        "declare the component at the top level before overriding it",
                    )
                })?;
            let argv = split_command(run_command, &format!("{path}.run_command"))?;
            component.mode_runs.push((name.to_owned(), argv));
            note_feature(features, name);
        }
    }
    Ok(())
}

/// Record a feature name once, keeping first-use order.
fn note_feature(features: &mut Vec<String>, name: &str) {
    if !features.iter().any(|existing| existing == name) {
        features.push(name.to_owned());
    }
}

fn read_env_groups(
    root: &Node,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, MigrateError> {
    let mut groups: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    if let Some(envs) = root.get("envs") {
        merge_groups(envs, "envs", &mut groups)?;
    }
    if let Some(all) = root
        .get("modes")
        .and_then(|modes| modes.get("all"))
        .and_then(|all| all.get("envs"))
    {
        merge_groups(all, "modes.all.envs", &mut groups)?;
    }
    Ok(groups)
}

fn merge_groups(
    node: &Node,
    path: &str,
    groups: &mut BTreeMap<String, BTreeMap<String, String>>,
) -> Result<(), MigrateError> {
    let Some(entries) = node.as_map() else {
        return Err(schema(
            path,
            "expected an env-group map",
            "write `envs: { <group>: { KEY: value } }`",
        ));
    };
    merge_group_entries(entries, path, groups)
}

fn merge_group_entries(
    entries: &[Entry],
    path: &str,
    groups: &mut BTreeMap<String, BTreeMap<String, String>>,
) -> Result<(), MigrateError> {
    for group in entries {
        let Some(vars) = group.value.as_map() else {
            return Err(schema(
                format!("{path}.{}", group.key),
                "expected `KEY: value` entries",
                "write each variable as `KEY: \"template\"`",
            ));
        };
        let target = groups.entry(group.key.clone()).or_default();
        for var in vars {
            let Some(template) = var.value.as_scalar() else {
                return Err(schema(
                    format!("{path}.{}.{}", group.key, var.key),
                    "expected a scalar template",
                    "write the value as a quoted string",
                ));
            };
            target.insert(
                var.key.clone(),
                render_template(template, &format!("envs.{}.{}", group.key, var.key))?,
            );
        }
    }
    Ok(())
}
