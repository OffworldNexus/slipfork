//! Property tables, reference validation, feature implications, and external checks.

use std::collections::{BTreeMap, BTreeSet};

use crate::check::{Checker, ExternalCheck, IdKind};
use crate::diag::{Diagnostic, Span};
use crate::lookup::each_stmt;
use crate::syntax::{EnvAssignment, Expr, ForkFile, ServiceDirective, Stmt};

const PG_DB_PROPERTIES: [&str; 8] = [
    "host", "port", "user", "password", "db_name", "url", "jdbc", "ca_cert",
];
const REDIS_PROPERTIES: [&str; 6] = ["host", "port", "user", "password", "url", "ca_cert"];
const S3_PROPERTIES: [&str; 6] = [
    "endpoint",
    "bucket",
    "region",
    "access_key",
    "secret_key",
    "url",
];
const DOMAIN_PROPERTIES: [&str; 3] = ["name", "leader_name", "url"];
const SERVICE_PROPERTIES: [&str; 6] = [
    "private_domain",
    "private_url",
    "private_port",
    "commit_hash",
    "public_route_path",
    "public_url",
];
const APP_PROPERTIES: [&str; 3] = ["domain", "url", "id"];

pub(crate) struct FeatureDecl {
    pub(crate) name: String,
    pub(crate) enables: Vec<crate::syntax::Spanned<String>>,
    pub(crate) span: Span,
}

impl Checker<'_> {
    pub(crate) fn check_ref(&mut self, segments: &[String], span: Span) {
        let Some(head) = segments.first() else {
            return;
        };
        if head == "app" || head == "deployment" {
            self.check_scope_path(head, &segments[1..], span);
            return;
        }
        let Some((kind, _)) = self.ids.get(head.as_str()).copied() else {
            self.diags.push(
                Diagnostic::new(span, format!("unknown identifier `{head}`"))
                    .with_hint("declare it before referencing it"),
            );
            return;
        };
        let property = segments[1..].join(".");
        match kind {
            IdKind::Domain => {
                if segments.len() == 1 {
                    self.diags.push(
                        Diagnostic::new(span, format!("`{head}` is a domain, not a value"))
                            .with_hint(format!("use `{head}.url`")),
                    );
                } else {
                    self.check_properties(head, &DOMAIN_PROPERTIES, &property, span);
                }
            }
            IdKind::PgCluster => {
                let first_database = self
                    .pg_databases
                    .get(head.as_str())
                    .and_then(|databases| databases.first())
                    .cloned();
                let mut diagnostic = Diagnostic::new(
                    span,
                    format!("`{head}` is a postgres cluster, not a database"),
                );
                diagnostic = match first_database {
                    Some(database) if PG_DB_PROPERTIES.contains(&property.as_str()) => diagnostic
                        .with_hint(format!("did you mean database `{database}.{property}`?")),
                    Some(database) => diagnostic.with_hint(format!(
                        "use `<cluster>.<database>.<property>` (e.g. `{database}.url`)"
                    )),
                    None => {
                        diagnostic.with_hint("declare a `database <name>` inside the cluster first")
                    }
                };
                self.diags.push(diagnostic);
            }
            IdKind::PgDatabase => {
                self.check_properties(head, &PG_DB_PROPERTIES, &property, span);
            }
            IdKind::Redis => self.check_properties(head, &REDIS_PROPERTIES, &property, span),
            IdKind::S3 => self.check_properties(head, &S3_PROPERTIES, &property, span),
            IdKind::Service => self.check_properties(head, &SERVICE_PROPERTIES, &property, span),
            IdKind::EnvGroup | IdKind::Task | IdKind::Feature => {
                self.diags.push(
                    Diagnostic::new(
                        span,
                        format!(
                            "`{head}` is {} {}, not a value",
                            kind.article(),
                            kind.describe()
                        ),
                    )
                    .with_hint("reference a resource or service property instead"),
                );
            }
        }
    }
    pub(crate) fn check_properties(
        &mut self,
        entity: &str,
        allowed: &[&str],
        property: &str,
        span: Span,
    ) {
        if property.is_empty() {
            self.diags.push(
                Diagnostic::new(span, format!("`{entity}` has no property"))
                    .with_hint(format!("expected one of: {}", allowed.join(", "))),
            );
            return;
        }
        if allowed.contains(&property) {
            return;
        }
        self.diags.push(
            Diagnostic::new(span, format!("`{entity}` has no property `{property}`"))
                .with_hint(format!("expected one of: {}", allowed.join(", "))),
        );
    }
    pub(crate) fn check_scope_path(&mut self, scope: &str, segments: &[String], span: Span) {
        if segments.is_empty() {
            self.diags.push(
                Diagnostic::new(span, format!("`{scope}` is a scope, not a value"))
                    .with_hint(format!("use `{scope}.<property>`")),
            );
            return;
        }
        let property = segments.join(".");
        let valid = if scope == "app" {
            APP_PROPERTIES.contains(&property.as_str())
        } else {
            deployment_property(&property)
        };
        if valid {
            return;
        }
        let hint = if scope == "app" {
            format!("expected one of: {}", APP_PROPERTIES.join(", "))
        } else {
            "expected a property such as `env.name` or `env_group.KEY`".to_owned()
        };
        self.diags.push(
            Diagnostic::new(span, format!("`{scope}` has no property `{property}`"))
                .with_hint(hint),
        );
    }
    pub(crate) fn validate_features(&mut self) {
        let declarations: Vec<(String, Vec<crate::syntax::Spanned<String>>)> = self
            .feature_decls
            .iter()
            .map(|declaration| (declaration.name.clone(), declaration.enables.clone()))
            .collect();
        for (_, enables) in &declarations {
            for enable in enables {
                if !self.features.contains(&enable.value) {
                    self.diags.push(
                        Diagnostic::new(enable.span, format!("unknown feature `{}`", enable.value))
                            .with_hint("declare it in `features { ... }`"),
                    );
                }
            }
        }
        let mut adjacency: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (name, enables) in &declarations {
            adjacency
                .entry(name.as_str())
                .or_default()
                .extend(enables.iter().map(|enable| enable.value.as_str()));
        }
        if let Some(cycle) = find_cycle(&adjacency) {
            let message = format!("feature cycle: {}", cycle.join(" -> "));
            let span = self
                .feature_decls
                .first()
                .map_or(Span::zero(), |declaration| declaration.span);
            self.diags.push(Diagnostic::new(span, message));
        }
    }
}

fn deployment_property(property: &str) -> bool {
    matches!(
        property,
        "id" | "date_create"
            | "date_event"
            | "cause"
            | "commit_sha"
            | "is_production"
            | "is_leader"
            | "env.name"
            | "env.branch.name"
            | "env.branch.repo"
            | "product.name"
            | "product.slug"
    ) || property
        .strip_prefix("env_group.")
        .is_some_and(|key| !key.is_empty() && !key.contains('.'))
}
pub(crate) fn gather_env_keys(
    directive: &ServiceDirective,
    groups: &BTreeMap<String, Vec<EnvAssignment>>,
    env_keys: &mut BTreeSet<String>,
    build_keys: &mut Vec<crate::syntax::Spanned<String>>,
) {
    match directive {
        ServiceDirective::UseEnv(group) => {
            if let Some(vars) = groups.get(&group.value) {
                for variable in vars {
                    env_keys.insert(variable.key.value.clone());
                }
            }
        }
        ServiceDirective::EnvBlock { vars, .. } => {
            for variable in vars {
                env_keys.insert(variable.key.value.clone());
            }
        }
        ServiceDirective::EnvAssignment(variable) => {
            env_keys.insert(variable.key.value.clone());
        }
        ServiceDirective::BuildEnv { vars, .. } => {
            for variable in vars {
                build_keys.push(variable.key.clone());
            }
        }
        ServiceDirective::On { body, .. } => {
            for inner in body {
                gather_env_keys(inner, groups, env_keys, build_keys);
            }
        }
        _ => {}
    }
}
fn find_cycle<'a>(adjacency: &BTreeMap<&'a str, Vec<&'a str>>) -> Option<Vec<&'a str>> {
    fn visit<'a>(
        node: &'a str,
        adjacency: &BTreeMap<&'a str, Vec<&'a str>>,
        state: &mut BTreeMap<&'a str, u8>,
        stack: &mut Vec<&'a str>,
    ) -> Option<Vec<&'a str>> {
        match state.get(node).copied().unwrap_or(0) {
            1 => {
                let start = stack.iter().position(|entry| *entry == node).unwrap_or(0);
                let mut cycle = stack[start..].to_vec();
                cycle.push(node);
                return Some(cycle);
            }
            2 => return None,
            _ => {}
        }
        state.insert(node, 1);
        stack.push(node);
        if let Some(children) = adjacency.get(node) {
            for child in children {
                if let Some(cycle) = visit(child, adjacency, state, stack) {
                    return Some(cycle);
                }
            }
        }
        stack.pop();
        state.insert(node, 2);
        None
    }

    let mut state = BTreeMap::new();
    let mut stack = Vec::new();
    for node in adjacency.keys() {
        if let Some(cycle) = visit(node, adjacency, &mut state, &mut stack) {
            return Some(cycle);
        }
    }
    None
}
impl ForkFile {
    /// Ask the external catalog about every `var(...)` / `secret(...)` key in the manifest.
    pub fn validate(&self, external: &dyn ExternalCheck) -> Vec<Diagnostic> {
        let mut expressions: Vec<&Expr> = Vec::new();
        each_stmt(&self.statements, &mut |statement| {
            collect_exprs(statement, &mut expressions);
        });
        let mut diags = Vec::new();
        for expression in expressions {
            check_external(expression, external, &mut diags);
        }
        diags
    }
}
fn collect_exprs<'a>(statement: &'a Stmt, out: &mut Vec<&'a Expr>) {
    match statement {
        Stmt::EnvGroup(group) => {
            for variable in &group.vars {
                out.push(&variable.expr);
            }
        }
        Stmt::Service(service) => collect_directive_exprs(&service.directives, out),
        _ => {}
    }
}
fn collect_directive_exprs<'a>(directives: &'a [ServiceDirective], out: &mut Vec<&'a Expr>) {
    for directive in directives {
        match directive {
            ServiceDirective::EnvBlock { vars, .. } | ServiceDirective::BuildEnv { vars, .. } => {
                for variable in vars {
                    out.push(&variable.expr);
                }
            }
            ServiceDirective::EnvAssignment(variable) => out.push(&variable.expr),
            ServiceDirective::On { body, .. } => collect_directive_exprs(body, out),
            _ => {}
        }
    }
}
fn check_external(expr: &Expr, external: &dyn ExternalCheck, diags: &mut Vec<Diagnostic>) {
    match expr {
        Expr::Var(key) | Expr::Secret(key) => {
            if let Err(message) = external.check(&key.value) {
                diags.push(Diagnostic::new(key.span, message));
            }
        }
        Expr::Fallback { lhs, rhs, .. } => {
            check_external(lhs, external, diags);
            check_external(rhs, external, diags);
        }
        Expr::Concat { parts, .. } => {
            for part in parts {
                check_external(part, external, diags);
            }
        }
        Expr::Str(_) | Expr::Int(_) | Expr::Bool(_) | Expr::Ref(_) | Expr::Generator { .. } => {}
    }
}
