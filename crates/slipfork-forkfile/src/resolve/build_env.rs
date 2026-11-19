//! The one canonical expression evaluator, plus the local-ownership (`claims`) predicates.
//!
//! Runtime `env`, Dockerfile `build_env`, and argv slots all funnel through [`resolve_assignments`]
//! and [`eval_expr`], so a value can never disagree between the three entry points.

use std::collections::{BTreeMap, BTreeSet};

use crate::diag::{Diagnostic, Span};
use crate::ir::{EntityKind, EvaluatedFork, EvaluatedService, Resolvers, TaintedString};
use crate::projection::WorldFact;
use crate::syntax::{EnvAssignment, Expr, GeneratorKind};
use crate::vault::{EntropySource, Namespace, ProviderError};

impl EvaluatedFork {
    /// The kind of the entity declared under `id`, if this fork declares one.
    pub fn entity_kind(&self, id: &str) -> Option<EntityKind> {
        if self.postgres.iter().any(|cluster| cluster.id == id) {
            return Some(EntityKind::PgCluster);
        }
        if self
            .postgres
            .iter()
            .any(|cluster| cluster.databases.iter().any(|database| database.id == id))
        {
            return Some(EntityKind::PgDatabase);
        }
        if self.redis.iter().any(|cluster| cluster.id == id) {
            return Some(EntityKind::Redis);
        }
        if self.s3.iter().any(|bucket| bucket == id) {
            return Some(EntityKind::S3);
        }
        if self.domains.iter().any(|domain| domain == id) {
            return Some(EntityKind::Domain);
        }
        if self.services.iter().any(|service| service.id == id) {
            return Some(EntityKind::Service);
        }
        None
    }

    /// The `claims` namespace owning `id`, when a client can claim that entity.
    pub fn claim_namespace(&self, id: &str) -> Option<&'static str> {
        self.entity_kind(id).and_then(EntityKind::claim_namespace)
    }

    /// Whether `expr` reads any entry the local world owns through `owns`.
    pub fn expr_is_claimed(&self, expr: &Expr, owns: &dyn Fn(&str, &[String]) -> bool) -> bool {
        match expr {
            Expr::Var(key) => owns("vars", std::slice::from_ref(&key.value)),
            Expr::Secret(key) => owns("secrets", std::slice::from_ref(&key.value)),
            Expr::Ref(path) => self.ref_is_claimed(&path.value, owns),
            Expr::Fallback { lhs, rhs, .. } => {
                self.expr_is_claimed(lhs, owns) || self.expr_is_claimed(rhs, owns)
            }
            Expr::Concat { parts, .. } => parts.iter().any(|part| self.expr_is_claimed(part, owns)),
            Expr::Str(_) | Expr::Int(_) | Expr::Bool(_) | Expr::Generator { .. } => false,
        }
    }

    fn ref_is_claimed(&self, path: &[String], owns: &dyn Fn(&str, &[String]) -> bool) -> bool {
        let Some((head, rest)) = path.split_first() else {
            return false;
        };
        match head.as_str() {
            "deployment" => false,
            "app" => rest.len() == 1 && owns("resources", &["app".to_owned(), rest[0].clone()]),
            _ => match self.claim_namespace(head) {
                Some(namespace) => {
                    owns(namespace, std::slice::from_ref(head))
                        || owns("resources", &[namespace.to_owned(), head.clone()])
                }
                None => false,
            },
        }
    }

    /// Every env key of `service_id` whose expression reads a claimed entry.
    pub fn claimed_env_keys(
        &self,
        service_id: &str,
        owns: &dyn Fn(&str, &[String]) -> bool,
    ) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        for service in self
            .services
            .iter()
            .filter(|service| service.id == service_id)
        {
            collect_claimed_keys(service, self, owns, &mut keys);
        }
        keys
    }

    /// Every env key, across every service, whose expression reads a claimed entry.
    pub fn all_claimed_env_keys(&self, owns: &dyn Fn(&str, &[String]) -> bool) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        for service in &self.services {
            collect_claimed_keys(service, self, owns, &mut keys);
        }
        keys
    }
}

pub(super) fn collect_claimed_keys(
    service: &EvaluatedService,
    fork: &EvaluatedFork,
    owns: &dyn Fn(&str, &[String]) -> bool,
    keys: &mut BTreeSet<String>,
) {
    for assignment in &service.env {
        if fork.expr_is_claimed(&assignment.expr, owns) {
            keys.insert(assignment.key.value.clone());
        }
    }
}

pub(super) fn resolve_assignments(
    assignments: &[EnvAssignment],
    fork: &EvaluatedFork,
    resolvers: &Resolvers<'_>,
    is_claimed: &dyn Fn(&Expr) -> bool,
) -> Result<BTreeMap<String, TaintedString>, Vec<Diagnostic>> {
    // One projection per resolution: every reference in the batch reads the same fact table, so
    // resolution never rebuilds (or re-reads passwords into) `O(refs x facts)` copies.
    let facts = fork
        .world_projection(resolvers)
        .map_err(|diagnostic| vec![diagnostic])?;
    let mut env = BTreeMap::new();
    let mut diags = Vec::new();
    for assignment in assignments {
        if is_claimed(&assignment.expr) {
            env.insert(
                assignment.key.value.clone(),
                TaintedString::plain(assignment.expr.render()),
            );
            continue;
        }
        match eval_expr(&assignment.expr, fork, resolvers, &facts) {
            Ok(Some(value)) => {
                env.insert(assignment.key.value.clone(), value);
            }
            Ok(None) => diags.push(
                Diagnostic::new(
                    assignment.expr.span(),
                    format!("`{}` has no value", assignment.key.value),
                )
                .with_hint("provide it through the local vault, OSPP, or a `??` fallback"),
            ),
            Err(diagnostic) => diags.push(diagnostic),
        }
    }
    if diags.is_empty() {
        Ok(env)
    } else {
        Err(diags)
    }
}

fn eval_expr(
    expr: &Expr,
    fork: &EvaluatedFork,
    resolvers: &Resolvers<'_>,
    facts: &[WorldFact],
) -> Result<Option<TaintedString>, Diagnostic> {
    match expr {
        Expr::Str(value) => Ok(Some(TaintedString::plain(value.value.clone()))),
        Expr::Int(value) => Ok(Some(TaintedString::plain(value.value.to_string()))),
        Expr::Bool(value) => Ok(Some(TaintedString::plain(if value.value {
            "true"
        } else {
            "false"
        }))),
        Expr::Ref(reference) => fork
            .resolve_property(&reference.value, reference.span, facts)
            .map(Some),
        Expr::Var(key) => match resolvers.provider.get(Namespace::Vars, &key.value) {
            Ok(Some(value)) => Ok(Some(TaintedString::plain(value))),
            Ok(None) => Ok(None),
            Err(error) => Err(provider_diagnostic(&error, key.span)),
        },
        Expr::Secret(key) => match resolvers.provider.get(Namespace::Secrets, &key.value) {
            Ok(Some(value)) => Ok(Some(TaintedString::tainted(value))),
            Ok(None) => Ok(None),
            Err(error) => Err(provider_diagnostic(&error, key.span)),
        },
        Expr::Fallback { lhs, rhs, span } => {
            if let Some(value) = eval_expr(lhs, fork, resolvers, facts)? {
                return Ok(Some(value));
            }
            if let (Expr::Secret(key), Expr::Generator { kind, arg, .. }) = (&**lhs, &**rhs) {
                let generated = generate(*kind, arg.value, resolvers.entropy)
                    .map_err(|message| Diagnostic::new(*span, message))?;
                let stored = resolvers
                    .provider
                    .ensure_secret(&key.value, &generated)
                    .map_err(|error| provider_diagnostic(&error, key.span))?;
                return Ok(Some(TaintedString::tainted(stored)));
            }
            eval_expr(rhs, fork, resolvers, facts)
        }
        Expr::Concat { parts, .. } => {
            let mut text = String::new();
            let mut tainted = false;
            for part in parts {
                let Some(value) = eval_expr(part, fork, resolvers, facts)? else {
                    return Err(Diagnostic::new(
                        part.span(),
                        "an unresolved value cannot be concatenated",
                    )
                    .with_hint("give every `+` part a value or a `??` fallback"));
                };
                text.push_str(value.expose());
                tainted |= value.is_tainted();
            }
            Ok(Some(if tainted {
                TaintedString::tainted(text)
            } else {
                TaintedString::plain(text)
            }))
        }
        Expr::Generator { kind, arg, span } => {
            let value = generate(*kind, arg.value, resolvers.entropy)
                .map_err(|message| Diagnostic::new(*span, message))?;
            Ok(Some(TaintedString::plain(value)))
        }
    }
}

fn provider_diagnostic(error: &ProviderError, span: Span) -> Diagnostic {
    Diagnostic::new(span, error.to_string())
}

fn generate(kind: GeneratorKind, arg: u32, entropy: &dyn EntropySource) -> Result<String, String> {
    const LIMIT: u32 = 1024;
    if arg > LIMIT {
        return Err(format!("`{}` is limited to {LIMIT}", kind.as_str()));
    }
    match kind {
        GeneratorKind::Uuid4 => {
            let mut bytes = [0u8; 16];
            entropy
                .fill_bytes(&mut bytes)
                .map_err(|error| error.to_string())?;
            bytes[6] = (bytes[6] & 0x0f) | 0x40;
            bytes[8] = (bytes[8] & 0x3f) | 0x80;
            let hex = hex::encode(bytes);
            Ok(format!(
                "{}-{}-{}-{}-{}",
                &hex[0..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..32]
            ))
        }
        GeneratorKind::RandomHex => {
            let mut bytes = vec![0u8; arg as usize];
            entropy
                .fill_bytes(&mut bytes)
                .map_err(|error| error.to_string())?;
            Ok(hex::encode(bytes))
        }
        GeneratorKind::RandomBase64 => {
            use base64::Engine as _;
            let mut bytes = vec![0u8; arg as usize];
            entropy
                .fill_bytes(&mut bytes)
                .map_err(|error| error.to_string())?;
            Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
        }
        GeneratorKind::RandomString => {
            const ALPHABET: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut bytes = vec![0u8; arg as usize];
            entropy
                .fill_bytes(&mut bytes)
                .map_err(|error| error.to_string())?;
            Ok(bytes
                .into_iter()
                .map(|byte| char::from(ALPHABET[usize::from(byte & 0x3f)]))
                .collect())
        }
    }
}
