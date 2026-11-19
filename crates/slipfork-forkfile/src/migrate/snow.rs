//! Upstream `snow.yml` model: reading, template/argv materialization, and cross-reference
//! validation. Everything here runs before [`super::render`] emits a byte.

use std::collections::BTreeMap;

use super::template::{render_template, split_command};
use super::{MigrateError, schema};
use crate::yaml::Node;

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
        let images = sequence(root, "images")?
            .iter()
            .enumerate()
            .map(|(index, node)| Image::read(node, index))
            .collect::<Result<Vec<_>, _>>()?;
        let resources = root.get("resources");
        let pg_clusters = sequence_opt(resources, "pg_clusters")?
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let path = format!("resources.pg_clusters[{index}]");
                Ok((
                    scalar(node, "id", &path)?,
                    optional_scalar(node, "version").unwrap_or_else(|| "18".to_owned()),
                ))
            })
            .collect::<Result<Vec<_>, MigrateError>>()?;
        let pg_databases = sequence_opt(resources, "pg_databases")?
            .iter()
            .enumerate()
            .map(|(index, node)| PgDatabase::read(node, index))
            .collect::<Result<Vec<_>, _>>()?;
        let redis = sequence_opt(resources, "redis_clusters")?
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let path = format!("resources.redis_clusters[{index}]");
                Ok((
                    scalar(node, "id", &path)?,
                    optional_scalar(node, "version").unwrap_or_else(|| "7".to_owned()),
                ))
            })
            .collect::<Result<Vec<_>, MigrateError>>()?;
        let s3 = id_list(
            sequence_opt(resources, "s3_buckets")?,
            "resources.s3_buckets",
        )?;
        let domains = id_list(sequence(root, "domains")?, "domains")?;
        let env_groups = read_env_groups(root)?;
        let mut components = sequence(root, "components")?
            .iter()
            .enumerate()
            .map(|(index, node)| Component::read(node, index))
            .collect::<Result<Vec<_>, _>>()?;
        let ingresses = sequence(root, "ingresses")?
            .iter()
            .enumerate()
            .map(|(index, node)| Ingress::read(node, index))
            .collect::<Result<Vec<_>, _>>()?;
        let hooks = sequence(root, "hooks")?
            .iter()
            .enumerate()
            .map(|(index, node)| Hook::read(node, index))
            .collect::<Result<Vec<_>, _>>()?;
        let mut features: Vec<String> = Vec::new();
        read_modes(root, &mut components, &mut features)?;
        Ok(Self {
            images,
            pg_clusters,
            pg_databases,
            redis,
            s3,
            domains,
            env_groups,
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
        let id = scalar(node, "id", &path)?;
        let (context, dockerfile, reference) = if node.get("context").is_some() {
            let context = scalar(node, "context", &path)?;
            let context = context.trim_start_matches("./").to_owned();
            let dockerfile =
                optional_scalar(node, "dockerfile").unwrap_or_else(|| "Dockerfile".to_owned());
            (Some(context), dockerfile, String::new())
        } else {
            let registry = scalar(node, "registry", &path)?;
            let repository = scalar(node, "repository", &path)?;
            let tag = optional_scalar(node, "tag").unwrap_or_else(|| "latest".to_owned());
            (
                None,
                "Dockerfile".to_owned(),
                format!("{registry}/{repository}:{tag}"),
            )
        };
        let build_envs = string_list(node, "envs").map_err(|error| match error {
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
            id: scalar(node, "id", &path)?,
            cluster: scalar(node, "cluster", &path)?,
            pool: optional_bool(node, "pool", &path)?.unwrap_or(true),
        })
    }
}

impl Component {
    fn read(node: &Node, index: usize) -> Result<Self, MigrateError> {
        let path = format!("components[{index}]");
        let run_command = scalar(node, "run_command", &path)?;
        let run = split_command(&run_command, &format!("{path}.run_command"))?;
        if run.is_empty() {
            return Err(schema(
                format!("{path}.run_command"),
                "`run_command` must not be empty: a service needs a `run` line",
                "declare the component's run command",
            ));
        }
        Ok(Self {
            id: scalar(node, "id", &path)?,
            path: path.clone(),
            image: scalar(node, "image", &path)?,
            run,
            size_hint: optional_scalar(node, "size_hint").unwrap_or_else(|| "*".to_owned()),
            envs: string_list(node, "envs")?,
            internal_ports: optional_sequence(node, "internal_ports")
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
            prefix: scalar(match_rule, "prefix", &format!("{path}.match"))?,
            component: scalar(target, "component", &format!("{path}.target"))?,
            port,
            path: path.clone(),
        })
    }
}

impl Hook {
    fn read(node: &Node, index: usize) -> Result<Self, MigrateError> {
        let path = format!("hooks[{index}]");
        let run_command = scalar(node, "run_command", &path)?;
        Ok(Self {
            component: scalar(node, "component", &path)?,
            phase: scalar(node, "when", &path)?,
            argv: split_command(&run_command, &format!("{path}.run_command"))?,
            path: path.clone(),
        })
    }
}

fn read_modes(
    root: &Node,
    components: &mut [Component],
    features: &mut Vec<String>,
) -> Result<(), MigrateError> {
    let Some(modes) = root.get("modes").and_then(Node::as_map) else {
        return Ok(());
    };
    for (mode, spec) in modes {
        if mode == "all" {
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
        if spec
            .get("envs")
            .and_then(Node::as_map)
            .is_some_and(|entries| !entries.is_empty())
        {
            return Err(schema(
                format!("modes.{mode}.envs"),
                "mode-scoped env groups cannot be represented in one Forkfile",
                "merge them into `envs`/`modes.all.envs` or select the group with a feature",
            ));
        }
        let Some(overrides) = spec.get("components").and_then(Node::as_map) else {
            continue;
        };
        for (id, override_node) in overrides {
            let path = format!("modes.{mode}.components.{id}");
            if optional_bool(override_node, "disabled", &path)?.unwrap_or(false) {
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
                .find(|component| component.id == *id)
                .ok_or_else(|| {
                    schema(
                        format!("{path}.run_command"),
                        format!("mode-only component `{id}` is not declared in `components`"),
                        "declare the component at the top level before overriding it",
                    )
                })?;
            let argv = split_command(run_command, &format!("{path}.run_command"))?;
            component.mode_runs.push((mode.clone(), argv));
            if !features.contains(mode) {
                features.push(mode.clone());
            }
        }
    }
    Ok(())
}

fn read_env_groups(
    root: &Node,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, MigrateError> {
    let mut groups: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut merge = |node: &Node, path: &str| -> Result<(), MigrateError> {
        let Some(entries) = node.as_map() else {
            return Err(schema(
                path,
                "expected an env-group map",
                "write `envs: { <group>: { KEY: value } }`",
            ));
        };
        for (group, vars) in entries {
            let Some(vars) = vars.as_map() else {
                return Err(schema(
                    format!("{path}.{group}"),
                    "expected `KEY: value` entries",
                    "write each variable as `KEY: \"template\"`",
                ));
            };
            let target = groups.entry(group.clone()).or_default();
            for (key, value) in vars {
                let Some(template) = value.as_scalar() else {
                    return Err(schema(
                        format!("{path}.{group}.{key}"),
                        "expected a scalar template",
                        "write the value as a quoted string",
                    ));
                };
                target.insert(
                    key.clone(),
                    render_template(template, &format!("envs.{group}.{key}"))?,
                );
            }
        }
        Ok(())
    };
    if let Some(envs) = root.get("envs") {
        merge(envs, "envs")?;
    }
    if let Some(all) = root
        .get("modes")
        .and_then(|modes| modes.get("all"))
        .and_then(|all| all.get("envs"))
    {
        merge(all, "modes.all.envs")?;
    }
    Ok(groups)
}

fn sequence<'a>(node: &'a Node, key: &str) -> Result<&'a [Node], MigrateError> {
    match node.get(key) {
        Some(Node::Seq(nodes)) => Ok(nodes),
        Some(Node::Scalar(value)) if value.is_empty() => Ok(&[]),
        Some(_) => Err(schema(
            key,
            "expected a sequence",
            "write the entries as `- key: value` items",
        )),
        None => Ok(&[]),
    }
}

fn sequence_opt<'a>(node: Option<&'a Node>, key: &str) -> Result<&'a [Node], MigrateError> {
    match node {
        Some(node) => sequence(node, key),
        None => Ok(&[]),
    }
}

fn optional_sequence<'a>(node: &'a Node, key: &str) -> &'a [Node] {
    node.get(key).and_then(Node::as_seq).unwrap_or(&[])
}

fn id_list(nodes: &[Node], path: &str) -> Result<Vec<String>, MigrateError> {
    nodes
        .iter()
        .enumerate()
        .map(|(index, node)| match node {
            Node::Scalar(id) => Ok(id.clone()),
            Node::Map(_) => scalar(node, "id", &format!("{path}[{index}]")),
            Node::Seq(_) => Err(schema(
                format!("{path}[{index}]"),
                "expected a bucket/domain map or id",
                "write `- id: <name>`",
            )),
        })
        .collect()
}

fn string_list(node: &Node, key: &str) -> Result<Vec<String>, MigrateError> {
    optional_sequence(node, key)
        .iter()
        .map(|entry| {
            entry
                .as_scalar()
                .map(str::to_owned)
                .ok_or_else(|| schema(key, "expected a string list", "write the values as scalars"))
        })
        .collect()
}

fn scalar(node: &Node, key: &str, path: &str) -> Result<String, MigrateError> {
    node.get(key)
        .and_then(Node::as_scalar)
        .map(str::to_owned)
        .ok_or_else(|| {
            schema(
                format!("{path}.{key}"),
                format!("missing `{key}`"),
                format!("declare `{key}`"),
            )
        })
}

fn optional_scalar(node: &Node, key: &str) -> Option<String> {
    node.get(key).and_then(Node::as_scalar).map(str::to_owned)
}

fn optional_bool(node: &Node, key: &str, path: &str) -> Result<Option<bool>, MigrateError> {
    let Some(value) = node.get(key) else {
        return Ok(None);
    };
    let Some(value) = value.as_scalar().map(str::trim) else {
        return Err(schema(
            format!("{path}.{key}"),
            "expected `true` or `false`",
            "write a boolean literal",
        ));
    };
    match value {
        "true" => Ok(Some(true)),
        "false" => Ok(Some(false)),
        other => Err(schema(
            format!("{path}.{key}"),
            format!("expected `true` or `false`, got `{other}`"),
            "write `pool: false` (or `true`)",
        )),
    }
}
