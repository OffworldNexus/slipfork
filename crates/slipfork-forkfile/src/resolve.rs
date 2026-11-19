//! Environment resolution: expressions to tainted strings through injected ports.

use std::collections::{BTreeMap, BTreeSet};

use crate::diag::{Diagnostic, Span};
use crate::ir::{EntityKind, EvaluatedFork, Resolvers, TaintedString};
use crate::syntax::{ArgPart, ArgRefKind, Argv, Expr, GeneratorKind};
use crate::vault::{EntropySource, ProviderError};

impl EvaluatedFork {
    /// Resolve one service's environment through the injected ports and facts.
    pub fn resolve_env(
        &self,
        service_id: &str,
        resolvers: &Resolvers<'_>,
    ) -> Result<BTreeMap<String, TaintedString>, Vec<Diagnostic>> {
        self.resolve_env_with_claims(service_id, resolvers, &|_| false)
    }

    /// Resolve one service's environment, leaving claimed expressions byte-verbatim.
    ///
    /// `is_claimed` is the caller's `claims` predicate: an assignment whose whole expression reads
    /// a locally owned entry is returned as canonical `Forkfile` source (via [`Expr::render`])
    /// instead of being evaluated, exactly as the uniform OSPP claim rule requires. Everything
    /// else goes through the one canonical evaluator below.
    pub fn resolve_env_with_claims(
        &self,
        service_id: &str,
        resolvers: &Resolvers<'_>,
        is_claimed: &dyn Fn(&Expr) -> bool,
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
            if is_claimed(&assignment.expr) {
                env.insert(
                    assignment.key.value.clone(),
                    TaintedString::plain(assignment.expr.render()),
                );
                continue;
            }
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

    /// Resolve one command line into its argv, word-split-safe.
    ///
    /// Each [`ArgvSlot`](crate::syntax::ArgvSlot) is exactly one argv element. Leading
    /// `FOO=bar` `EnvPrefix` assignments are folded in front as plain `FOO=bar` elements (their
    /// inline form is what `execve` passes through, and the language only allows literal values
    /// there). A slot built from any secret-tainted reference is returned as
    /// [`TaintedString::tainted`], so a caller can print it as `[REDACTED]`.
    pub fn resolve_argv(
        &self,
        service_id: &str,
        argv: &Argv,
        resolvers: &Resolvers<'_>,
    ) -> Result<Vec<TaintedString>, Vec<Diagnostic>> {
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
        let mut resolved: Vec<TaintedString> = argv
            .assignments
            .iter()
            .map(|prefix| TaintedString::plain(format!("{}={}", prefix.key.value, prefix.value)))
            .collect();
        // `$env.KEY` reads the service's resolved environment; resolve it once, and only when a
        // slot actually asks for it, so a literal-only hook never touches the providers.
        let env = if argv.slots.iter().any(|slot| {
            slot.parts.iter().any(|part| {
                matches!(
                    part,
                    ArgPart::Ref(crate::syntax::ArgRef {
                        kind: ArgRefKind::Env(_),
                        ..
                    })
                )
            })
        }) {
            Some(self.resolve_env(service_id, resolvers)?)
        } else {
            None
        };
        let mut diagnostics = Vec::new();
        for slot in &argv.slots {
            let mut value = String::new();
            let mut tainted = false;
            for part in &slot.parts {
                match part {
                    ArgPart::Text(text) => value.push_str(text),
                    ArgPart::Ref(reference) => {
                        let resolved_ref = match &reference.kind {
                            ArgRefKind::Port => match service.port {
                                Some(port) => Ok(TaintedString::plain(port.to_string())),
                                None => Err(Diagnostic::new(
                                    reference.span,
                                    format!(
                                        "service `{service_id}` declares no `port` to interpolate"
                                    ),
                                )),
                            },
                            ArgRefKind::Env(key) => env
                                .as_ref()
                                .and_then(|env| env.get(key))
                                .cloned()
                                .ok_or_else(|| {
                                    Diagnostic::new(
                                        reference.span,
                                        format!("`env.{key}` is not available in this world"),
                                    )
                                }),
                            ArgRefKind::Property(segments) => {
                                self.resolve_property(segments, reference.span, resolvers)
                            }
                        };
                        match resolved_ref {
                            Ok(value_part) => {
                                tainted |= value_part.is_tainted();
                                value.push_str(value_part.expose());
                            }
                            Err(diagnostic) => diagnostics.push(diagnostic),
                        }
                    }
                }
            }
            resolved.push(if tainted {
                TaintedString::tainted(value)
            } else {
                TaintedString::plain(value)
            });
        }
        if diagnostics.is_empty() {
            Ok(resolved)
        } else {
            Err(diagnostics)
        }
    }

    /// Resolve one `$<entity>.<prop>` reference through the canonical facts lookup.
    fn resolve_property(
        &self,
        segments: &[String],
        span: Span,
        resolvers: &Resolvers<'_>,
    ) -> Result<TaintedString, Diagnostic> {
        // The parser never emits an empty reference: `segments[0]` is the declared head.
        let head = &segments[0];
        let property = segments[1..].join(".");
        let found = match head.as_str() {
            "app" => resolvers.facts.app.get(&property),
            "deployment" => resolvers.facts.deployment.get(&property),
            _ => self
                .entity_kind(head)
                .and_then(|kind| resolvers.facts.lookup(kind, head, &property)),
        };
        let Some(found) = found else {
            return Err(Diagnostic::new(
                span,
                format!("`{}` is not available in this world", segments.join(".")),
            )
            .with_hint("the daemon resolves resource properties before the world starts"));
        };
        Ok(if found.secret {
            TaintedString::tainted(found.value.clone())
        } else {
            TaintedString::plain(found.value.clone())
        })
    }

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

fn collect_claimed_keys(
    service: &crate::ir::EvaluatedService,
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
        Expr::Ref(reference) => fork
            .resolve_property(&reference.value, reference.span, resolvers)
            .map(Some),
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

#[cfg(test)]
mod argv_tests {
    use super::*;
    use crate::eval::FeatureSet;
    use crate::ir::{Facts, FactsValue, Resolvers};
    use crate::parser::parse_forkfile;
    use crate::syntax::{ArgPart, ArgRef, ArgvSlot};
    use crate::vault::{FileVaultProvider, OsCsprngEntropy};

    fn evaluated(source: &str) -> EvaluatedFork {
        parse_forkfile(source)
            .expect("parse")
            .evaluate(&FeatureSet::new())
            .expect("evaluate")
    }

    fn secret_facts() -> Facts {
        let mut facts = Facts::default();
        facts.postgres.insert(
            "app_db".to_owned(),
            [(
                "url".to_owned(),
                FactsValue {
                    value: "postgres://user:hunter2@db/app".to_owned(),
                    secret: true,
                },
            )]
            .into(),
        );
        facts
    }

    fn vault(dir: &tempfile::TempDir) -> FileVaultProvider {
        FileVaultProvider::open(dir.path().join("vault.json")).expect("vault")
    }

    fn setup_hook(fork: &EvaluatedFork) -> &Argv {
        &fork.services[0].hooks[0].argv
    }

    #[test]
    fn slots_resolve_word_split_safe_and_canonically() {
        let fork = evaluated(
            r#"
features {}
postgres pg { database app_db }
service api ./api {
    port 8000
    env { GREETING = "hello world" }
    setup: FOO=bar echo $port $env.GREETING $app_db.url "one slot"
}
"#,
        );
        let dir = tempfile::tempdir().expect("disk vault dir");
        let vault = vault(&dir);
        let facts = secret_facts();
        let entropy = OsCsprngEntropy;
        let resolvers = Resolvers::new(&vault, &entropy, &facts);
        let slots = fork
            .resolve_argv("api", setup_hook(&fork), &resolvers)
            .expect("resolve argv");
        let exposed: Vec<&str> = slots.iter().map(TaintedString::expose).collect();
        assert_eq!(
            exposed,
            [
                "FOO=bar",
                "echo",
                "8000",
                "hello world",
                "postgres://user:hunter2@db/app",
                "one slot",
            ],
            "assignments fold in front; `$port`/`$env.KEY` resolve; quotes stay one slot"
        );
        assert!(!slots[3].is_tainted(), "a literal env value is not tainted");
    }

    #[test]
    fn a_secret_reference_taints_only_its_slot() {
        let fork = evaluated(
            r#"
features {}
postgres pg { database app_db }
service api ./api {
    setup: echo $app_db.url "plain"
}
"#,
        );
        let dir = tempfile::tempdir().expect("disk vault dir");
        let vault = vault(&dir);
        let facts = secret_facts();
        let entropy = OsCsprngEntropy;
        let resolvers = Resolvers::new(&vault, &entropy, &facts);
        let slots = fork
            .resolve_argv("api", setup_hook(&fork), &resolvers)
            .expect("resolve argv");
        assert!(slots[1].is_tainted(), "secret-derived slot must be tainted");
        assert_eq!(slots[1].redacted(), "[REDACTED]");
        assert!(
            !slots[2].is_tainted(),
            "a plain literal slot stays printable"
        );
    }

    fn ref_argv(kinds: Vec<ArgRefKind>, line: u32) -> Argv {
        let span = Span::new(0, 1, line, 1);
        Argv {
            assignments: Vec::new(),
            slots: kinds
                .into_iter()
                .map(|kind| ArgvSlot {
                    parts: vec![ArgPart::Ref(ArgRef { kind, span })],
                    span,
                })
                .collect(),
            span,
        }
    }

    #[test]
    fn an_unknown_service_is_reported_by_resolve_argv() {
        let fork = evaluated(
            r#"
features {}
service api ./api { setup: echo hi }
"#,
        );
        let dir = tempfile::tempdir().expect("disk vault dir");
        let vault = vault(&dir);
        let facts = Facts::default();
        let entropy = OsCsprngEntropy;
        let resolvers = Resolvers::new(&vault, &entropy, &facts);
        let error = fork
            .resolve_argv("nope", setup_hook(&fork), &resolvers)
            .expect_err("`nope` is not a declared service");
        assert_eq!(error.len(), 1);
        assert!(error[0].message.contains("unknown service `nope`"));
    }

    #[test]
    fn a_missing_port_or_env_key_names_the_reference() {
        let fork = evaluated(
            r#"
features {}
service api ./api {
    env { GREETING = "hi" }
    setup: echo hi
}
"#,
        );
        let dir = tempfile::tempdir().expect("disk vault dir");
        let vault = vault(&dir);
        let facts = Facts::default();
        let entropy = OsCsprngEntropy;
        let resolvers = Resolvers::new(&vault, &entropy, &facts);

        let no_port = fork
            .resolve_argv("api", &ref_argv(vec![ArgRefKind::Port], 5), &resolvers)
            .expect_err("a service without `port` cannot interpolate `$port`");
        assert!(no_port[0].message.contains("declares no `port`"));

        let no_env = fork
            .resolve_argv(
                "api",
                &ref_argv(vec![ArgRefKind::Env("MISSING".to_owned())], 5),
                &resolvers,
            )
            .expect_err("an undeclared env key cannot interpolate");
        assert!(no_env[0].message.contains("`env.MISSING` is not available"));
    }

    #[test]
    fn an_unavailable_property_carries_its_remedy_and_errors_accumulate() {
        let fork = evaluated(
            r#"
features {}
postgres pg { database app_db }
service api ./api { setup: echo hi }
"#,
        );
        let dir = tempfile::tempdir().expect("disk vault dir");
        let vault = vault(&dir);
        let facts = Facts::default();
        let entropy = OsCsprngEntropy;
        let resolvers = Resolvers::new(&vault, &entropy, &facts);
        let errors = fork
            .resolve_argv(
                "api",
                &ref_argv(
                    vec![
                        ArgRefKind::Property(vec!["app_db".to_owned(), "url".to_owned()]),
                        ArgRefKind::Property(vec!["ghost".to_owned(), "one".to_owned()]),
                    ],
                    7,
                ),
                &resolvers,
            )
            .expect_err("both property slots are unresolved");
        assert_eq!(
            errors.len(),
            2,
            "every unresolved slot is reported: {errors:?}"
        );
        assert!(
            errors.iter().all(|diagnostic| diagnostic.hint.as_deref()
                == Some("the daemon resolves resource properties before the world starts")),
            "the missing-property remedy survives: {errors:?}"
        );
    }
}
