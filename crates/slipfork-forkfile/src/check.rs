//! Whole-file static checker: every statement and every `on` branch, active or inactive.

use std::collections::{BTreeMap, BTreeSet};

use crate::diag::{Diagnostic, Span};
use crate::lookup::each_stmt;
use crate::props::{FeatureDecl, gather_env_keys};
use crate::syntax::{
    ArgPart, ArgRefKind, Argv, Condition, EnvAssignment, Expr, ForkFile, ImageSource, ServiceDef,
    ServiceDirective, Stmt,
};

/// An external catalog (CI variables, cloud resources) consulted by [`ForkFile::validate`].
pub trait ExternalCheck {
    /// Return `Err(message)` when the catalog does not know `key`.
    fn check(&self, key: &str) -> Result<(), String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdKind {
    PgCluster,
    PgDatabase,
    Redis,
    S3,
    Domain,
    Service,
    EnvGroup,
    Task,
    Feature,
}

impl IdKind {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::PgCluster => "postgres cluster",
            Self::PgDatabase => "postgres database",
            Self::Redis => "redis cluster",
            Self::S3 => "s3 bucket",
            Self::Domain => "domain",
            Self::Service => "service",
            Self::EnvGroup => "env group",
            Self::Task => "task",
            Self::Feature => "feature",
        }
    }

    pub(crate) fn article(self) -> &'static str {
        match self {
            Self::S3 | Self::EnvGroup => "an",
            _ => "a",
        }
    }
}

pub(crate) struct Checker<'a> {
    pub(crate) file: &'a ForkFile,
    pub(crate) ids: BTreeMap<String, (IdKind, Span)>,
    pub(crate) pg_databases: BTreeMap<String, Vec<String>>,
    pub(crate) env_groups: BTreeMap<String, Vec<EnvAssignment>>,
    pub(crate) service_env_keys: BTreeMap<String, BTreeSet<String>>,
    pub(crate) features: BTreeSet<String>,
    pub(crate) feature_decls: Vec<FeatureDecl>,
    pub(crate) project: Option<Span>,
    pub(crate) diags: Vec<Diagnostic>,
}

/// Run every static check over `file`, collecting all diagnostics in one pass.
pub(crate) fn check(file: &ForkFile) -> Vec<Diagnostic> {
    let mut checker = Checker {
        file,
        ids: BTreeMap::new(),
        pg_databases: BTreeMap::new(),
        env_groups: BTreeMap::new(),
        service_env_keys: BTreeMap::new(),
        features: BTreeSet::new(),
        feature_decls: Vec::new(),
        project: None,
        diags: Vec::new(),
    };
    checker.collect();
    checker.validate();
    checker.diags
}

impl Checker<'_> {
    fn collect(&mut self) {
        let file = self.file;
        each_stmt(&file.statements, &mut |statement| {
            self.collect_names(statement)
        });
        each_stmt(&file.statements, &mut |statement| {
            self.collect_service_env(statement)
        });
    }

    fn collect_names(&mut self, statement: &Stmt) {
        match statement {
            Stmt::Project(value) => {
                if let Some(first) = self.project {
                    self.diags.push(
                        Diagnostic::new(value.span, "duplicate `project` statement")
                            .with_hint(format!("first declared on line {}", first.line)),
                    );
                } else {
                    self.project = Some(value.span);
                }
            }
            Stmt::Features { definitions, .. } => {
                for definition in definitions {
                    self.features.insert(definition.name.value.clone());
                    self.insert_id(
                        IdKind::Feature,
                        &definition.name.value,
                        definition.name.span,
                    );
                    self.feature_decls.push(FeatureDecl {
                        name: definition.name.value.clone(),
                        enables: definition.enables.clone(),
                        span: definition.span,
                    });
                }
            }
            Stmt::Postgres(cluster) => {
                self.insert_id(IdKind::PgCluster, &cluster.id.value, cluster.id.span);
                let mut databases = Vec::new();
                for database in &cluster.databases {
                    databases.push(database.id.value.clone());
                    self.insert_id(IdKind::PgDatabase, &database.id.value, database.id.span);
                }
                for block in &cluster.on {
                    for database in &block.databases {
                        databases.push(database.id.value.clone());
                        self.insert_id(IdKind::PgDatabase, &database.id.value, database.id.span);
                    }
                }
                self.pg_databases
                    .insert(cluster.id.value.clone(), databases);
            }
            Stmt::Redis(cluster) => {
                self.insert_id(IdKind::Redis, &cluster.id.value, cluster.id.span);
            }
            Stmt::S3(bucket) => self.insert_id(IdKind::S3, &bucket.id.value, bucket.id.span),
            Stmt::Domain(domain) => {
                self.insert_id(IdKind::Domain, &domain.id.value, domain.id.span);
            }
            Stmt::EnvGroup(group) => {
                self.insert_id(IdKind::EnvGroup, &group.name.value, group.name.span);
                self.env_groups
                    .insert(group.name.value.clone(), group.vars.clone());
            }
            Stmt::Service(service) => {
                self.insert_id(IdKind::Service, &service.id.value, service.id.span);
            }
            Stmt::Task(task) => self.insert_id(IdKind::Task, &task.name.value, task.name.span),
            Stmt::On(_) => {}
        }
    }

    fn collect_service_env(&mut self, statement: &Stmt) {
        let Stmt::Service(service) = statement else {
            return;
        };
        let mut keys = BTreeSet::new();
        let mut build = Vec::new();
        for directive in &service.directives {
            gather_env_keys(directive, &self.env_groups, &mut keys, &mut build);
        }
        self.service_env_keys.insert(service.id.value.clone(), keys);
    }

    fn insert_id(&mut self, kind: IdKind, id: &str, span: Span) {
        match self.ids.get(id).copied() {
            Some((first_kind, first_span)) => {
                let message = format!(
                    "duplicate id `{id}` ({} and {})",
                    first_kind.describe(),
                    kind.describe()
                );
                self.diags.push(
                    Diagnostic::new(span, message)
                        .with_hint(format!("first declared on line {}", first_span.line)),
                );
            }
            None => {
                self.ids.insert(id.to_owned(), (kind, span));
            }
        }
    }

    fn validate(&mut self) {
        self.validate_features();
        let file = self.file;
        each_stmt(&file.statements, &mut |statement| {
            self.validate_stmt(statement)
        });
    }

    fn validate_stmt(&mut self, statement: &Stmt) {
        match statement {
            Stmt::Project(_) | Stmt::Features { .. } | Stmt::S3(_) | Stmt::Domain(_) => {}
            Stmt::Postgres(cluster) => {
                for block in &cluster.on {
                    self.check_condition(&block.cond);
                }
            }
            Stmt::Redis(cluster) => {
                for block in &cluster.on {
                    self.check_condition(&block.cond);
                }
            }
            Stmt::EnvGroup(group) => {
                for variable in &group.vars {
                    self.check_expr(&variable.expr);
                }
            }
            Stmt::Service(service) => self.check_service(service),
            Stmt::Task(task) => {
                let owns_service = self
                    .ids
                    .get(&task.service.value)
                    .is_some_and(|(kind, _)| *kind == IdKind::Service);
                if !owns_service {
                    self.diags.push(
                        Diagnostic::new(
                            task.service.span,
                            format!("unknown service `{}`", task.service.value),
                        )
                        .with_hint("declare `service <id> ...` first"),
                    );
                }
                let keys = self.service_env_keys.get(&task.service.value).cloned();
                for command in &task.commands {
                    self.check_argv(command, keys.as_ref());
                }
            }
            Stmt::On(block) => self.check_condition(&block.cond),
        }
    }

    fn check_service(&mut self, service: &ServiceDef) {
        let mut env_keys = BTreeSet::new();
        let mut build_keys = Vec::new();
        for directive in &service.directives {
            gather_env_keys(directive, &self.env_groups, &mut env_keys, &mut build_keys);
        }
        for key in build_keys {
            if env_keys.contains(&key.value) {
                self.diags.push(
                    Diagnostic::new(
                        key.span,
                        format!("`{}` is declared in both `env` and `build_env`", key.value),
                    )
                    .with_hint("rename one of the two variables"),
                );
            }
        }
        self.check_directives(service, &env_keys, &service.directives);
    }

    fn check_directives(
        &mut self,
        service: &ServiceDef,
        env_keys: &BTreeSet<String>,
        directives: &[ServiceDirective],
    ) {
        for directive in directives {
            match directive {
                ServiceDirective::Port(port) => self.check_port(port),
                ServiceDirective::InternalPorts { ports, .. } => {
                    for port in ports {
                        self.check_port(port);
                    }
                }
                ServiceDirective::Route(route) => self.check_route(route),
                ServiceDirective::Volume(volume) => {
                    if volume.value.trim().is_empty() {
                        self.diags
                            .push(Diagnostic::new(volume.span, "expected a volume path"));
                    }
                }
                ServiceDirective::UseEnv(group) => {
                    if !self.env_groups.contains_key(&group.value) {
                        self.diags.push(
                            Diagnostic::new(
                                group.span,
                                format!("unknown env group `{}`", group.value),
                            )
                            .with_hint("declare `env <group> { ... }` at the top level"),
                        );
                    }
                }
                ServiceDirective::EnvBlock { vars, .. } => {
                    for variable in vars {
                        self.check_expr(&variable.expr);
                    }
                }
                ServiceDirective::EnvAssignment(variable) => self.check_expr(&variable.expr),
                ServiceDirective::BuildEnv { vars, span } => {
                    if matches!(service.source, ImageSource::OciImage { .. }) {
                        self.diags.push(
                            Diagnostic::new(
                                *span,
                                "`from <image>` services cannot use `build_env`",
                            )
                            .with_hint("build the service from `./dir` or remove `build_env`"),
                        );
                    }
                    for variable in vars {
                        self.check_expr(&variable.expr);
                    }
                }
                ServiceDirective::Stage(stage) | ServiceDirective::Dockerfile(stage) => {
                    if matches!(service.source, ImageSource::OciImage { .. }) {
                        self.diags.push(
                            Diagnostic::new(
                                stage.span,
                                "`from <image>` services cannot use `stage`/`dockerfile`",
                            )
                            .with_hint("build the service from `./dir` or remove the directive"),
                        );
                    }
                }
                ServiceDirective::Run(argv) => self.check_argv(argv, Some(env_keys)),
                ServiceDirective::Hook { commands, .. } => {
                    for argv in commands {
                        self.check_argv(argv, Some(env_keys));
                    }
                }
                ServiceDirective::On { cond, body, .. } => {
                    self.check_condition(cond);
                    self.check_directives(service, env_keys, body);
                }
                ServiceDirective::Size(_) => {}
            }
        }
    }

    fn check_port(&mut self, port: &crate::syntax::Spanned<u32>) {
        if !(1..=65535).contains(&port.value) {
            self.diags.push(
                Diagnostic::new(port.span, format!("port `{}` is out of range", port.value))
                    .with_hint("ports must be between 1 and 65535"),
            );
        }
    }

    fn check_route(&mut self, route: &crate::syntax::Spanned<String>) {
        let value = route.value.trim();
        let subdomain = value
            .strip_suffix(".*")
            .is_some_and(|prefix| !prefix.is_empty() && !prefix.contains(char::is_whitespace));
        if value.starts_with('/') || subdomain {
            return;
        }
        self.diags.push(
            Diagnostic::new(route.span, format!("invalid route `{value}`"))
                .with_hint("use `/`, `/prefix`, or `subdomain.*`"),
        );
    }

    fn check_condition(&mut self, condition: &Condition) {
        let mut features = Vec::new();
        condition.collect_features(&mut features);
        for feature in features {
            if !self.features.contains(&feature.value) {
                self.diags.push(
                    Diagnostic::new(feature.span, format!("unknown feature `{}`", feature.value))
                        .with_hint("declare it in `features { ... }`"),
                );
            }
        }
    }

    fn check_expr(&mut self, expr: &Expr) {
        self.check_expr_inner(expr, false);
    }

    fn check_expr_inner(&mut self, expr: &Expr, generator_allowed: bool) {
        match expr {
            Expr::Ref(reference) => self.check_ref(&reference.value, reference.span),
            Expr::Fallback { lhs, rhs, .. } => {
                self.check_expr_inner(lhs, false);
                let allowed = matches!(**lhs, Expr::Secret(_));
                self.check_expr_inner(rhs, allowed);
            }
            Expr::Concat { parts, .. } => {
                for part in parts {
                    self.check_expr_inner(part, false);
                }
            }
            Expr::Generator { kind, span, .. } => {
                if !generator_allowed {
                    self.diags.push(
                        Diagnostic::new(
                            *span,
                            format!(
                                "`{}` is only valid as the fallback of `secret(<KEY>) ?? <generator>`",
                                kind.as_str()
                            ),
                        )
                        .with_hint("write `secret(KEY) ?? random_hex(32)`"),
                    );
                }
            }
            Expr::Str(_) | Expr::Int(_) | Expr::Bool(_) | Expr::Var(_) | Expr::Secret(_) => {}
        }
    }

    fn check_argv(&mut self, argv: &Argv, env_keys: Option<&BTreeSet<String>>) {
        for slot in &argv.slots {
            for part in &slot.parts {
                let ArgPart::Ref(reference) = part else {
                    continue;
                };
                match &reference.kind {
                    ArgRefKind::Port => {}
                    ArgRefKind::Env(key) => {
                        let known = env_keys.is_some_and(|keys| keys.contains(key));
                        if !known {
                            self.diags.push(
                                Diagnostic::new(
                                    reference.span,
                                    format!("`$env.{key}` is not declared in any `env` block"),
                                )
                                .with_hint(
                                    "declare it in `env { ... }` or a reusable `env <group>`",
                                ),
                            );
                        }
                    }
                    ArgRefKind::Property(segments) => {
                        if segments.len() < 2 {
                            self.diags.push(
                                Diagnostic::new(
                                    reference.span,
                                    format!("unknown `$` reference `${}`", segments.join(".")),
                                )
                                .with_hint("use `$port`, `$env.KEY`, or `$<entity>.<prop>`"),
                            );
                        } else {
                            self.check_ref(segments, reference.span);
                        }
                    }
                }
            }
        }
    }
}
