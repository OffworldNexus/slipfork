//! Feature evaluation, per-service environment resolution, and the canonical JSON IR.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::diag::{Diagnostic, Span};
use crate::ir::{
    EvaluatedEnvGroup, EvaluatedFork, EvaluatedImage, EvaluatedPg, EvaluatedPgDatabase,
    EvaluatedRedis, EvaluatedService, EvaluatedTask, HookRun,
};
use crate::syntax::{
    EnvAssignment, EnvGroup, ForkFile, ImageSource, PgClusterDef, PgDatabaseDef, RedisClusterDef,
    ServiceDef, ServiceDirective, Stmt, TaskDef,
};

/// The Cargo-style feature selection (`-F a,b` / `--no-default-features`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureSet {
    /// Explicitly requested feature names.
    pub features: BTreeSet<String>,
    /// When `true`, `default` is not enabled implicitly.
    pub no_default_features: bool,
}

impl FeatureSet {
    /// An empty selection (defaults still apply).
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the selection from CLI arguments.
    pub fn from_cli(features: &[String], no_default_features: bool) -> Self {
        Self {
            features: features.iter().cloned().collect(),
            no_default_features,
        }
    }

    /// Build the selection from any iterator of feature names.
    pub fn with_features<I, S>(features: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            features: features.into_iter().map(Into::into).collect(),
            no_default_features: false,
        }
    }

    /// Disable the implicit `default` feature.
    pub fn without_default_features(mut self) -> Self {
        self.no_default_features = true;
        self
    }
}

#[derive(Default)]
struct Builder {
    project: Option<String>,
    groups: Vec<EvaluatedEnvGroup>,
    postgres: Vec<EvaluatedPg>,
    redis: Vec<EvaluatedRedis>,
    s3: Vec<String>,
    domains: Vec<String>,
    services: Vec<EvaluatedService>,
    tasks: Vec<EvaluatedTask>,
}

impl Builder {
    /// The authoritative declaration of an env group: the **last** declaration wins.
    fn group_vars(&self, name: &str) -> Option<&Vec<EnvAssignment>> {
        self.groups
            .iter()
            .rev()
            .find(|group| group.name == name)
            .map(|group| &group.vars)
    }
    fn walk(&mut self, statements: &[Stmt], active: &BTreeSet<String>) {
        for statement in statements {
            match statement {
                Stmt::Project(value) => self.project = Some(value.value.clone()),
                Stmt::Features { .. } => {}
                Stmt::Postgres(cluster) => self.apply_postgres(cluster, active),
                Stmt::Redis(cluster) => self.apply_redis(cluster, active),
                Stmt::S3(bucket) => push_unique(&mut self.s3, &bucket.id.value),
                Stmt::Domain(domain) => push_unique(&mut self.domains, &domain.id.value),
                Stmt::EnvGroup(group) => self.apply_env_group(group),
                Stmt::Service(service) => self.apply_service(service, active),
                Stmt::Task(task) => self.apply_task(task),
                Stmt::On(block) => {
                    if block.cond.eval(active) {
                        self.walk(&block.body, active);
                    }
                }
            }
        }
    }

    fn apply_postgres(&mut self, cluster: &PgClusterDef, active: &BTreeSet<String>) {
        let index = match self
            .postgres
            .iter()
            .position(|existing| existing.id == cluster.id.value)
        {
            Some(index) => index,
            None => {
                self.postgres.push(EvaluatedPg {
                    id: cluster.id.value.clone(),
                    version: "18".to_owned(),
                    databases: Vec::new(),
                });
                self.postgres.len() - 1
            }
        };
        let evaluated = &mut self.postgres[index];
        if let Some(version) = &cluster.version {
            evaluated.version = version.value.clone();
        }
        for database in &cluster.databases {
            upsert_database(&mut evaluated.databases, database);
        }
        for block in &cluster.on {
            if !block.cond.eval(active) {
                continue;
            }
            if let Some(version) = &block.version {
                evaluated.version = version.value.clone();
            }
            for database in &block.databases {
                upsert_database(&mut evaluated.databases, database);
            }
        }
    }

    fn apply_redis(&mut self, cluster: &RedisClusterDef, active: &BTreeSet<String>) {
        let index = match self
            .redis
            .iter()
            .position(|existing| existing.id == cluster.id.value)
        {
            Some(index) => index,
            None => {
                self.redis.push(EvaluatedRedis {
                    id: cluster.id.value.clone(),
                    version: "7".to_owned(),
                });
                self.redis.len() - 1
            }
        };
        let evaluated = &mut self.redis[index];
        if let Some(version) = &cluster.version {
            evaluated.version = version.value.clone();
        }
        for block in &cluster.on {
            if !block.cond.eval(active) {
                continue;
            }
            if let Some(version) = &block.version {
                evaluated.version = version.value.clone();
            }
        }
    }

    fn apply_env_group(&mut self, group: &EnvGroup) {
        self.groups.push(EvaluatedEnvGroup {
            name: group.name.value.clone(),
            vars: group.vars.clone(),
        });
    }

    fn apply_service(&mut self, definition: &ServiceDef, active: &BTreeSet<String>) {
        let mut service = EvaluatedService {
            id: definition.id.value.clone(),
            image: match &definition.source {
                ImageSource::Dockerfile { context_dir, .. } => EvaluatedImage::Dockerfile {
                    context_dir: normalize_context(context_dir),
                    dockerfile: "Dockerfile".to_owned(),
                    stage: None,
                    build_env: Vec::new(),
                },
                ImageSource::OciImage { image_ref } => EvaluatedImage::OciImage {
                    image_ref: image_ref.value.clone(),
                },
            },
            port: None,
            internal_ports: Vec::new(),
            routes: Vec::new(),
            volumes: Vec::new(),
            size: "*".to_owned(),
            env: Vec::new(),
            run: None,
            hooks: Vec::new(),
        };
        self.apply_directives(&mut service, &definition.directives, active);
        match self
            .services
            .iter()
            .position(|existing| existing.id == service.id)
        {
            Some(index) => self.services[index] = service,
            None => self.services.push(service),
        }
    }

    fn apply_directives(
        &self,
        service: &mut EvaluatedService,
        directives: &[ServiceDirective],
        active: &BTreeSet<String>,
    ) {
        for directive in directives {
            match directive {
                ServiceDirective::Stage(stage) => {
                    if let EvaluatedImage::Dockerfile { stage: slot, .. } = &mut service.image {
                        *slot = Some(stage.value.clone());
                    }
                }
                ServiceDirective::Dockerfile(path) => {
                    if let EvaluatedImage::Dockerfile { dockerfile, .. } = &mut service.image {
                        *dockerfile = path.value.clone();
                    }
                }
                ServiceDirective::Size(size) => service.size = size.value.clone(),
                ServiceDirective::Port(port) => service.port = Some(port.value),
                ServiceDirective::InternalPorts { ports, .. } => {
                    service.internal_ports = ports.iter().map(|port| port.value).collect();
                }
                ServiceDirective::Route(route) => service.routes.push(route.value.clone()),
                ServiceDirective::Volume(volume) => service.volumes.push(volume.value.clone()),
                ServiceDirective::UseEnv(group) => {
                    if let Some(vars) = self.group_vars(&group.value) {
                        service.env.extend(vars.iter().cloned());
                    }
                }
                ServiceDirective::EnvBlock { vars, .. } => {
                    service.env.extend(vars.iter().cloned());
                }
                ServiceDirective::EnvAssignment(assignment) => {
                    service.env.push(assignment.clone());
                }
                ServiceDirective::BuildEnv { vars, .. } => {
                    if let EvaluatedImage::Dockerfile { build_env, .. } = &mut service.image {
                        build_env.extend(vars.iter().cloned());
                    }
                }
                ServiceDirective::Run(argv) => service.run = Some(argv.clone()),
                ServiceDirective::Hook {
                    phase, commands, ..
                } => {
                    for argv in commands {
                        service.hooks.push(HookRun {
                            phase: *phase,
                            argv: argv.clone(),
                        });
                    }
                }
                ServiceDirective::On { cond, body, .. } => {
                    if cond.eval(active) {
                        self.apply_directives(service, body, active);
                    }
                }
            }
        }
    }

    fn apply_task(&mut self, definition: &TaskDef) {
        let task = EvaluatedTask {
            name: definition.name.value.clone(),
            service: definition.service.value.clone(),
            commands: definition.commands.clone(),
            multi: definition.multi,
        };
        match self
            .tasks
            .iter()
            .position(|existing| existing.name == task.name)
        {
            Some(index) => self.tasks[index] = task,
            None => self.tasks.push(task),
        }
    }
}

/// Evaluate `self` against `features` into the canonical [`EvaluatedFork`].
impl ForkFile {
    /// Whether this manifest declares feature `name` in any `features` block.
    ///
    /// The walk descends an `on` block because a `features` statement is legal there; it never
    /// interprets the manifest's expressions, so the answer is exactly the declared feature set.
    #[must_use]
    pub fn declares_feature(&self, name: &str) -> bool {
        fn walk(statements: &[Stmt], name: &str) -> bool {
            statements.iter().any(|statement| match statement {
                Stmt::Features { definitions, .. } => definitions
                    .iter()
                    .any(|definition| definition.name.value == name),
                Stmt::On(block) => walk(&block.body, name),
                _ => false,
            })
        }
        walk(&self.statements, name)
    }

    /// Resolve the active feature closure and apply every active statement top to bottom.
    pub fn evaluate(&self, features: &FeatureSet) -> Result<EvaluatedFork, Vec<Diagnostic>> {
        let active = self.resolve_features(features)?;
        let mut builder = Builder::default();
        builder.walk(&self.statements, &active);
        Ok(EvaluatedFork {
            project: builder.project,
            features: active.into_iter().collect(),
            postgres: builder.postgres,
            redis: builder.redis,
            s3: builder.s3,
            domains: builder.domains,
            env_groups: builder.groups,
            services: builder.services,
            tasks: builder.tasks,
        })
    }

    fn resolve_features(&self, features: &FeatureSet) -> Result<BTreeSet<String>, Vec<Diagnostic>> {
        let mut declared = BTreeSet::new();
        let mut enables: BTreeMap<String, Vec<String>> = BTreeMap::new();
        gather_features(&self.statements, &mut declared, &mut enables);

        let mut diags = Vec::new();
        let mut active = BTreeSet::new();
        for feature in &features.features {
            if declared.contains(feature) {
                active.insert(feature.clone());
            } else {
                diags.push(Diagnostic::new(
                    Span::zero(),
                    format!("feature `{feature}` is not declared in `features {{ ... }}`"),
                ));
            }
        }
        if !features.no_default_features && declared.contains("default") {
            active.insert("default".to_owned());
        }
        if !diags.is_empty() {
            return Err(diags);
        }

        let mut queue: Vec<String> = active.iter().cloned().collect();
        while let Some(feature) = queue.pop() {
            if let Some(targets) = enables.get(&feature) {
                for target in targets {
                    if active.insert(target.clone()) {
                        queue.push(target.clone());
                    }
                }
            }
        }
        Ok(active)
    }
}

fn gather_features(
    statements: &[Stmt],
    declared: &mut BTreeSet<String>,
    enables: &mut BTreeMap<String, Vec<String>>,
) {
    for statement in statements {
        match statement {
            Stmt::Features { definitions, .. } => {
                for definition in definitions {
                    declared.insert(definition.name.value.clone());
                    enables.insert(
                        definition.name.value.clone(),
                        definition
                            .enables
                            .iter()
                            .map(|enable| enable.value.clone())
                            .collect(),
                    );
                }
            }
            Stmt::On(block) => gather_features(&block.body, declared, enables),
            _ => {}
        }
    }
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_owned());
    }
}

fn upsert_database(databases: &mut Vec<EvaluatedPgDatabase>, definition: &PgDatabaseDef) {
    let pool = definition.pool.as_ref().is_none_or(|pool| pool.value);
    match databases
        .iter()
        .position(|existing| existing.id == definition.id.value)
    {
        Some(index) => databases[index].pool = pool,
        None => databases.push(EvaluatedPgDatabase {
            id: definition.id.value.clone(),
            pool,
        }),
    }
}

fn normalize_context(context: &str) -> String {
    let trimmed = context
        .trim()
        .trim_start_matches("./")
        .trim_end_matches('/');
    if trimmed.is_empty() {
        ".".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_closure_is_transitive() {
        let file = crate::parse_forkfile(
            "features {\n    default = [dev]\n    dev = [mail]\n    mail\n}\n",
        )
        .expect("manifest");
        let evaluated = file.evaluate(&FeatureSet::new()).expect("evaluate");
        assert_eq!(evaluated.features, vec!["default", "dev", "mail"]);
        let evaluated = file
            .evaluate(&FeatureSet::new().without_default_features())
            .expect("evaluate");
        assert!(evaluated.features.is_empty());
    }

    #[test]
    fn unknown_cli_feature_is_reported() {
        let file = crate::parse_forkfile("features {\n    dev\n}\n").expect("manifest");
        let error = file
            .evaluate(&FeatureSet::from_cli(&["ghost".to_owned()], false))
            .expect_err("unknown feature");
        assert!(error[0].message.contains("feature `ghost` is not declared"));
    }
}
