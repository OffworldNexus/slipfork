//! Environment resolution: expressions to tainted strings through injected ports.

use std::collections::BTreeMap;

use crate::diag::{Diagnostic, Span};
use crate::ir::{EntityKind, EvaluatedFork, Resolvers, TaintedString};
use crate::syntax::{Expr, GeneratorKind};
use crate::vault::{EntropySource, ProviderError};

impl EvaluatedFork {
    /// Resolve one service's environment through the injected ports and facts.
    pub fn resolve_env(
        &self,
        service_id: &str,
        resolvers: &Resolvers<'_>,
    ) -> Result<BTreeMap<String, TaintedString>, Vec<Diagnostic>> {
        let Some(service) = self
            .services
            .iter()
            .find(|service| service.id == service_id)
        else {
            return Err(vec![Diagnostic::new(
                Span::zero(),
                format!("unknown service `{service_id}`"),
            )]);
        };
        let mut env = BTreeMap::new();
        let mut diags = Vec::new();
        for assignment in &service.env {
            match eval_expr(&assignment.expr, self, resolvers) {
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

    fn entity_kind(&self, id: &str) -> Option<EntityKind> {
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
}
fn eval_expr(
    expr: &Expr,
    fork: &EvaluatedFork,
    resolvers: &Resolvers<'_>,
) -> Result<Option<TaintedString>, Diagnostic> {
    match expr {
        Expr::Str(value) => Ok(Some(TaintedString::plain(value.value.clone()))),
        Expr::Int(value) => Ok(Some(TaintedString::plain(value.value.to_string()))),
        Expr::Bool(value) => Ok(Some(TaintedString::plain(if value.value {
            "true"
        } else {
            "false"
        }))),
        Expr::Ref(reference) => {
            let segments = &reference.value;
            let Some(head) = segments.first() else {
                return Ok(None);
            };
            let (facts, property) = match head.as_str() {
                "app" => (resolvers.facts.app.get(&segments[1..].join(".")), "app"),
                "deployment" => (
                    resolvers.facts.deployment.get(&segments[1..].join(".")),
                    "deployment",
                ),
                _ => {
                    let property = segments[1..].join(".");
                    let found = fork
                        .entity_kind(head)
                        .and_then(|kind| resolvers.facts.lookup(kind, head, &property));
                    let Some(found) = found else {
                        return Err(Diagnostic::new(
                            reference.span,
                            format!("`{}` is not available in this world", segments.join(".")),
                        )
                        .with_hint(
                            "the daemon resolves resource properties before the world starts",
                        ));
                    };
                    return Ok(Some(if found.secret {
                        TaintedString::tainted(found.value.clone())
                    } else {
                        TaintedString::plain(found.value.clone())
                    }));
                }
            };
            let Some(found) = facts else {
                return Err(Diagnostic::new(
                    reference.span,
                    format!("`{}` is not available in this world", segments.join(".")),
                )
                .with_hint(format!(
                    "`{property}` facts are resolved before the world starts"
                )));
            };
            Ok(Some(if found.secret {
                TaintedString::tainted(found.value.clone())
            } else {
                TaintedString::plain(found.value.clone())
            }))
        }
        Expr::Var(key) => match resolvers
            .provider
            .get(crate::vault::Namespace::Vars, &key.value)
        {
            Ok(Some(value)) => Ok(Some(TaintedString::plain(value))),
            Ok(None) => Ok(None),
            Err(error) => Err(provider_diagnostic(&error, key.span)),
        },
        Expr::Secret(key) => match resolvers
            .provider
            .get(crate::vault::Namespace::Secrets, &key.value)
        {
            Ok(Some(value)) => Ok(Some(TaintedString::tainted(value))),
            Ok(None) => Ok(None),
            Err(error) => Err(provider_diagnostic(&error, key.span)),
        },
        Expr::Fallback { lhs, rhs, span } => {
            if let Some(value) = eval_expr(lhs, fork, resolvers)? {
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
            eval_expr(rhs, fork, resolvers)
        }
        Expr::Concat { parts, .. } => {
            let mut text = String::new();
            let mut tainted = false;
            for part in parts {
                let Some(value) = eval_expr(part, fork, resolvers)? else {
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
