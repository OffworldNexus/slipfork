//! Builds the intermediate `Forkfile` model from typed `snow.yml` input.

use std::collections::BTreeSet;

use super::declarations::{hook_keyword, push_unique};
use super::snow::{Component, EnvMap, ImageSpec, Snow, Version};
use super::spans;
use super::template;
use crate::diag::{Diagnostic, Diagnostics};
use crate::expr::Expr;
use crate::span::Span;

pub(crate) type EnvEntries = Vec<(String, Expr)>;

pub(crate) struct Model {
    pub features: Vec<String>,
    pub postgres: Vec<PgOut>,
    pub redis: Vec<RedisOut>,
    pub s3: Vec<String>,
    pub domains: Vec<String>,
    pub envs: Vec<(String, EnvEntries)>,
    pub mode_envs: Vec<ModeEnv>,
    pub images: Vec<ImageOut>,
}

pub(crate) struct PgOut {
    pub id: String,
    pub version: Option<String>,
    pub databases: Vec<String>,
}

pub(crate) struct RedisOut {
    pub id: String,
    pub version: Option<String>,
}

pub(crate) enum SourceOut {
    Dir(String),
    From(String),
}

pub(crate) struct ModeEnv {
    pub mode: String,
    pub group: String,
    pub entries: EnvEntries,
}

pub(crate) struct ModeUses {
    pub mode: String,
    pub groups: Vec<String>,
}

pub(crate) struct ImageOut {
    pub id: String,
    pub source: SourceOut,
    pub dockerfile: Option<String>,
    pub uses: Vec<String>,
    pub mode_uses: Vec<ModeUses>,
    pub services: Vec<ServiceOut>,
}

pub(crate) struct ServiceOut {
    pub id: String,
    pub port: Option<u16>,
    pub internal_ports: Vec<u16>,
    pub routes: Vec<String>,
    pub hooks: Vec<(String, String)>,
    pub run: String,
}

pub(crate) struct Builder<'a> {
    pub(crate) source: &'a str,
    pub(crate) diags: Diagnostics,
    pub(crate) pg_databases: BTreeSet<String>,
    pub(crate) redis: BTreeSet<String>,
    pub(crate) buckets: BTreeSet<String>,
    pub(crate) domains: BTreeSet<String>,
    pub(crate) components: BTreeSet<String>,
    pub(crate) images: BTreeSet<String>,
    pub(crate) env_groups: BTreeSet<String>,
}

impl<'a> Builder<'a> {
    pub(crate) fn new(source: &'a str) -> Self {
        Self {
            source,
            diags: Diagnostics::new(),
            pg_databases: BTreeSet::new(),
            redis: BTreeSet::new(),
            buckets: BTreeSet::new(),
            domains: BTreeSet::new(),
            components: BTreeSet::new(),
            images: BTreeSet::new(),
            env_groups: BTreeSet::new(),
        }
    }

    fn template(&self) -> template::Template<'_> {
        template::Template {
            pg_databases: &self.pg_databases,
            redis: &self.redis,
            buckets: &self.buckets,
            domains: &self.domains,
            components: &self.components,
        }
    }

    fn error(&mut self, path: &[&str], message: impl Into<String>, next: impl Into<String>) {
        let at = path.join(".");
        self.diags.push(Diagnostic::new(
            Span::at(spans::locate(self.source, path)),
            format!("{at}: {}", message.into()),
            next,
        ));
    }

    fn validate(&mut self, snow: &Snow) {
        for database in &snow.resources.pg_databases {
            if !snow
                .resources
                .pg_clusters
                .iter()
                .any(|cluster| cluster.id == database.cluster)
            {
                self.error(
                    &["resources", "pg_databases", database.id.as_str()],
                    format!(
                        "database `{}` references unknown cluster `{}`",
                        database.id, database.cluster
                    ),
                    "declare the cluster under `resources.pg_clusters`",
                );
            }
        }
        for image in &snow.images {
            for group in &image.envs {
                self.require_env(group, &["images", image.id.as_str(), "envs"]);
            }
        }
        for component in &snow.components {
            if !self.images.contains(&component.image) {
                self.error(
                    &["components", component.id.as_str()],
                    format!(
                        "component `{}` references undeclared image `{}`",
                        component.id, component.image
                    ),
                    "declare `image <id> <dir>` for it",
                );
            }
            for group in &component.envs {
                self.require_env(group, &["components", component.id.as_str(), "envs"]);
            }
        }
        for ingress in &snow.ingresses {
            if !self.components.contains(&ingress.target.component) {
                self.error(
                    &["ingresses", ingress.target.component.as_str()],
                    format!(
                        "ingress targets unknown component `{}`",
                        ingress.target.component
                    ),
                    "declare the component before routing to it",
                );
            }
        }
        for hook in &snow.hooks {
            if !self.components.contains(&hook.component) {
                self.error(
                    &["hooks", hook.component.as_str()],
                    format!("hook targets unknown component `{}`", hook.component),
                    "declare the component before attaching a hook",
                );
            }
            if hook_keyword(&hook.when).is_none() {
                self.error(
                    &["hooks", hook.component.as_str(), "when"],
                    format!("unknown hook `when` value `{}`", hook.when),
                    "use `before_start`, `pre_deploy`, `post_deploy`, or `failed_deploy`",
                );
            }
        }
        for (mode, spec) in &snow.modes {
            for (id, over) in &spec.components {
                if !self.components.contains(id) {
                    self.error(
                        &["modes", mode.as_str(), "components", id.as_str()],
                        format!("mode `{mode}` overrides unknown component `{id}`"),
                        "declare the component at the top level",
                    );
                }
                for group in &over.envs {
                    self.require_env(
                        group,
                        &["modes", mode.as_str(), "components", id.as_str(), "envs"],
                    );
                }
            }
        }
    }

    fn require_env(&mut self, group: &str, path: &[&str]) {
        if !self.env_groups.contains(group) {
            self.error(
                path,
                format!("unknown env group `{group}`"),
                "declare it under `envs` before referencing it",
            );
        }
    }

    pub(crate) fn model(&mut self, snow: &Snow) -> Model {
        self.collect(snow);
        self.validate(snow);
        let features = snow
            .modes
            .keys()
            .filter(|mode| mode.as_str() != "all")
            .cloned()
            .collect();
        let postgres = snow
            .resources
            .pg_clusters
            .iter()
            .map(|cluster| PgOut {
                id: cluster.id.clone(),
                version: cluster.version.as_ref().map(Version::text),
                databases: snow
                    .resources
                    .pg_databases
                    .iter()
                    .filter(|database| database.cluster == cluster.id)
                    .map(|database| database.id.clone())
                    .collect(),
            })
            .collect();
        let redis = snow
            .resources
            .redis_clusters
            .iter()
            .map(|cluster| RedisOut {
                id: cluster.id.clone(),
                version: cluster.version.as_ref().map(Version::text),
            })
            .collect();
        let s3 = snow
            .resources
            .s3_buckets
            .iter()
            .map(|bucket| bucket.id().to_string())
            .collect();
        let domains = snow
            .domains
            .iter()
            .map(|domain| domain.id().to_string())
            .collect();
        let mut envs = self.env_map(&snow.envs, &["envs"]);
        if let Some(all) = snow.modes.get("all")
            && !all.envs.is_empty()
        {
            envs.extend(self.env_map(&all.envs, &["modes", "all", "envs"]));
        }
        let mut mode_envs = Vec::new();
        for (mode, spec) in &snow.modes {
            if mode == "all" {
                continue;
            }
            for (group, entries) in self.env_map(&spec.envs, &["modes", mode, "envs"]) {
                mode_envs.push(ModeEnv {
                    mode: mode.clone(),
                    group,
                    entries,
                });
            }
        }
        let mut images = Vec::new();
        for spec in &snow.images {
            let source = self.image_source(spec);
            let mut uses = Vec::new();
            for group in &spec.envs {
                push_unique(&mut uses, group);
            }
            let components: Vec<&Component> = snow
                .components
                .iter()
                .filter(|component| component.image == spec.id)
                .collect();
            for component in &components {
                for group in &component.envs {
                    push_unique(&mut uses, group);
                }
            }
            let mut mode_uses = Vec::new();
            for (mode, mode_spec) in &snow.modes {
                if mode == "all" {
                    continue;
                }
                let mut groups = Vec::new();
                for component in &components {
                    if let Some(over) = mode_spec.components.get(&component.id) {
                        for group in &over.envs {
                            push_unique(&mut groups, group);
                        }
                    }
                }
                if !groups.is_empty() {
                    mode_uses.push(ModeUses {
                        mode: mode.clone(),
                        groups,
                    });
                }
            }
            let services = components
                .iter()
                .map(|component| self.service(component, snow))
                .collect();
            images.push(ImageOut {
                id: spec.id.clone(),
                source,
                dockerfile: spec.dockerfile.clone(),
                uses,
                mode_uses,
                services,
            });
        }
        Model {
            features,
            postgres,
            redis,
            s3,
            domains,
            envs,
            mode_envs,
            images,
        }
    }

    fn image_source(&mut self, spec: &ImageSpec) -> SourceOut {
        if let Some(context) = &spec.context {
            return SourceOut::Dir(context.clone());
        }
        let Some(repository) = &spec.repository else {
            self.error(
                &["images", spec.id.as_str()],
                format!(
                    "image `{}` needs a `context` or `registry` + `repository`",
                    spec.id
                ),
                "set `context: <dir>` for a build directory",
            );
            return SourceOut::From(spec.id.clone());
        };
        let mut reference = repository.clone();
        if let Some(registry) = &spec.registry {
            reference = format!("{registry}/{reference}");
        }
        let tag = spec.tag.as_deref().unwrap_or("latest");
        SourceOut::From(format!("{reference}:{tag}"))
    }

    fn service(&mut self, component: &Component, snow: &Snow) -> ServiceOut {
        let mut ports = BTreeSet::new();
        let mut routes = Vec::new();
        for ingress in &snow.ingresses {
            if ingress.target.component != component.id {
                continue;
            }
            ports.insert(ingress.target.port);
            let prefix = ingress.route_match.prefix.clone();
            if !prefix.starts_with('/') {
                self.error(
                    &["ingresses", component.id.as_str(), "match", "prefix"],
                    format!("route prefix `{prefix}` must start with `/`"),
                    "prefix every `match.prefix` with `/`",
                );
            }
            routes.push(prefix);
        }
        if ports.len() > 1 {
            self.error(
                &["components", component.id.as_str()],
                format!("component `{}` is routed on two ports", component.id),
                "route each component on exactly one port",
            );
        }
        let mut internal_ports = Vec::new();
        let mut seen_ports = BTreeSet::new();
        for port in &component.internal_ports {
            if !seen_ports.insert(*port) {
                self.error(
                    &["components", component.id.as_str(), "internal_ports"],
                    format!("duplicate internal port {port}"),
                    "list each internal port once",
                );
            }
            internal_ports.push(*port);
        }
        let mut hooks = Vec::new();
        for hook in &snow.hooks {
            if hook.component != component.id {
                continue;
            }
            let Some(keyword) = hook_keyword(&hook.when) else {
                continue;
            };
            match split_command(&hook.run_command) {
                Ok(argv) => hooks.push((keyword.to_string(), argv.join(" "))),
                Err(next) => self.error(
                    &["hooks", component.id.as_str(), "run_command"],
                    "hook command is not a direct argv",
                    next,
                ),
            }
        }
        let run = match split_command(&component.run_command) {
            Ok(argv) => argv.join(" "),
            Err(next) => {
                self.error(
                    &["components", component.id.as_str(), "run_command"],
                    "shell metacharacters cannot be migrated",
                    next,
                );
                String::new()
            }
        };
        ServiceOut {
            id: component.id.clone(),
            port: ports.iter().next().copied(),
            internal_ports,
            routes,
            hooks,
            run,
        }
    }

    fn env_map(&mut self, envs: &EnvMap, prefix: &[&str]) -> Vec<(String, EnvEntries)> {
        let mut errors = Vec::new();
        let mut out = Vec::new();
        {
            let context = self.template();
            for (group, entries) in envs {
                let mut rendered = Vec::new();
                for (key, raw) in entries {
                    let mut path: Vec<&str> = prefix.to_vec();
                    path.push(group.as_str());
                    path.push(key.as_str());
                    let span = Span::at(spans::locate(self.source, &path));
                    match template::render(raw, &context, &path.join("."), span) {
                        Ok(expression) => rendered.push((key.clone(), expression)),
                        Err(error) => errors.push(error),
                    }
                }
                out.push((group.clone(), rendered));
            }
        }
        self.diags.items.extend(errors);
        out
    }
}

fn split_command(raw: &str) -> Result<Vec<String>, String> {
    const SHELL_META: &[char] = &[
        '|', '&', ';', '<', '>', '$', '`', '*', '?', '(', ')', '{', '}', '[', ']', '~', '!', '"',
        '\'', '\n',
    ];
    if raw.chars().any(|character| SHELL_META.contains(&character)) {
        return Err("move the pipeline into `scripts/<name>.sh`".to_string());
    }
    let argv: Vec<String> = raw.split_whitespace().map(str::to_string).collect();
    if argv.is_empty() {
        return Err("add a command to `run_command`".to_string());
    }
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_split_into_argv_and_pipelines_are_refused() {
        let argv = split_command("gunicorn app.main:app --bind 0.0.0.0:8000").expect("splits");
        assert_eq!(
            argv,
            vec!["gunicorn", "app.main:app", "--bind", "0.0.0.0:8000"]
        );
        let error = split_command("cat a | grep b").expect_err("refused");
        assert!(error.contains("scripts/<name>.sh"));
    }
}
