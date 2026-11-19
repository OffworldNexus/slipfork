//! Environment resolution: expressions to tainted strings through injected ports.

use std::collections::BTreeMap;

use crate::diag::{Diagnostic, Span};
use crate::ir::{EvaluatedFork, EvaluatedImage, Resolvers, TaintedString};
use crate::projection::{WorldFact, lookup, password_key};
use crate::syntax::{ArgPart, ArgRefKind, Argv, EnvAssignment, Expr};

mod build_env;

use build_env::resolve_assignments;

impl EvaluatedFork {
    /// Resolve one service's environment through the injected ports.
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
            return Err(vec![unknown_service(service_id)]);
        };
        resolve_assignments(&service.env, self, resolvers, is_claimed)
    }

    /// Resolve one Dockerfile service's `build_env` through the same canonical evaluator.
    ///
    /// Build-time references read the identical world projection as runtime ones
    /// ([`Self::world_facts`]), so a `$app_db.url` in `build_env` can never disagree with the value
    /// the container receives at runtime.
    pub fn resolve_build_env(
        &self,
        service_id: &str,
        resolvers: &Resolvers<'_>,
    ) -> Result<BTreeMap<String, TaintedString>, Vec<Diagnostic>> {
        let Some(service) = self
            .services
            .iter()
            .find(|service| service.id == service_id)
        else {
            return Err(vec![unknown_service(service_id)]);
        };
        let EvaluatedImage::Dockerfile { build_env, .. } = &service.image else {
            return Err(vec![
                Diagnostic::new(
                    Span::zero(),
                    format!("service `{service_id}` is pulled from an OCI image"),
                )
                .with_hint("build it from `./dir` to declare a build environment"),
            ]);
        };
        resolve_assignments(build_env, self, resolvers, &|_| false)
    }

    /// The world-level base environment: the union of **every** declared `env` group.
    ///
    /// Groups are folded in declaration order and a later declaration of the same key wins, which
    /// is the precedence the `Forkfile` specification defines; the CLI and the daemon therefore
    /// agree without either one re-deriving it. Values go through the same canonical evaluator as
    /// [`Self::resolve_env`] and [`Self::resolve_build_env`], so this base env is real resolved
    /// text (`hello`, not `"hello"`) and `secret(...)` values stay tainted.
    pub fn world_env(
        &self,
        resolvers: &Resolvers<'_>,
    ) -> Result<BTreeMap<String, TaintedString>, Vec<Diagnostic>> {
        let assignments: Vec<EnvAssignment> = self
            .env_groups
            .iter()
            .flat_map(|group| group.vars.iter().cloned())
            .collect();
        resolve_assignments(&assignments, self, resolvers, &|_| false)
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
        let facts = self
            .world_projection(resolvers)
            .map_err(|diagnostic| vec![diagnostic])?;
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
                                self.resolve_property(segments, reference.span, &facts)
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

    /// Resolve one `$<entity>.<prop>` reference through an already-built world projection.
    fn resolve_property(
        &self,
        segments: &[String],
        span: Span,
        facts: &[WorldFact],
    ) -> Result<TaintedString, Diagnostic> {
        // The parser never emits an empty reference: `segments[0]` is the declared head.
        let head = &segments[0];
        let property = segments[1..].join(".");
        let Some(value) = lookup(facts, head, &property) else {
            return Err(Diagnostic::new(
                span,
                format!("`{}` is not available in this world", segments.join(".")),
            )
            .with_hint(self.remedy(head, &property)));
        };
        Ok(value.clone())
    }

    /// The next action for an unavailable reference, never claiming a declared entity is missing.
    fn remedy(&self, entity: &str, property: &str) -> String {
        if matches!(property, "password" | "secret_key" | "access_key") {
            return format!(
                "read it as `secret(<KEY>)` (e.g. `secret({})`)",
                password_key(entity)
            );
        }
        if entity == "app" && self.project.is_none() {
            return "add `project <slug>` to the Forkfile".to_owned();
        }
        if entity == "deployment" {
            return "pass the deployment metadata through `Resolvers::with_deployment`".to_owned();
        }
        if self.entity_kind(entity).is_some() {
            return format!("`{entity}` is declared but publishes no `{property}`");
        }
        "declare the resource in the `Forkfile`, or supply a `??` fallback".to_owned()
    }
}

fn unknown_service(service_id: &str) -> Diagnostic {
    Diagnostic::new(Span::zero(), format!("unknown service `{service_id}`"))
}

#[cfg(test)]
mod argv_tests {
    use super::*;
    use crate::eval::FeatureSet;
    use crate::ir::Resolvers;
    use crate::parser::parse_forkfile;
    use crate::syntax::{ArgPart, ArgRef, ArgvSlot};
    use crate::vault::{FileVaultProvider, OsCsprngEntropy};

    fn evaluated(source: &str) -> EvaluatedFork {
        parse_forkfile(source)
            .expect("parse")
            .evaluate(&FeatureSet::new())
            .expect("evaluate")
    }

    fn vault(dir: &tempfile::TempDir) -> FileVaultProvider {
        FileVaultProvider::open(dir.path().join("vault.json")).expect("vault")
    }

    fn disk_tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("forkfile-resolve")
            .tempdir_in("/var/tmp")
            .expect("disk scratch directory")
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
        let dir = disk_tempdir();
        let vault = vault(&dir);
        let entropy = OsCsprngEntropy;
        let resolvers = Resolvers::new(&vault, &entropy);
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
                "postgres://slipfork@127.0.0.1:5432/app_db",
                "one slot",
            ],
            "assignments fold in front; `$port`/`$env.KEY`/`$app_db.url` resolve; quotes stay one slot"
        );
        assert!(!slots[3].is_tainted(), "a literal env value is not tainted");
    }

    #[test]
    fn a_secret_reference_taints_only_its_slot() {
        let fork = evaluated(
            r#"
features {}
service api ./api {
    env { TOKEN = secret(TOKEN) }
    setup: echo $env.TOKEN "plain"
}
"#,
        );
        let dir = disk_tempdir();
        let vault = vault(&dir);
        vault
            .ensure_secret("TOKEN", "hunter2")
            .expect("store secret");
        let entropy = OsCsprngEntropy;
        let resolvers = Resolvers::new(&vault, &entropy);
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
        let dir = disk_tempdir();
        let vault = vault(&dir);
        let entropy = OsCsprngEntropy;
        let resolvers = Resolvers::new(&vault, &entropy);
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
        let dir = disk_tempdir();
        let vault = vault(&dir);
        let entropy = OsCsprngEntropy;
        let resolvers = Resolvers::new(&vault, &entropy);

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
        let dir = disk_tempdir();
        let vault = vault(&dir);
        let entropy = OsCsprngEntropy;
        let resolvers = Resolvers::new(&vault, &entropy);
        let errors = fork
            .resolve_argv(
                "api",
                &ref_argv(
                    vec![
                        ArgRefKind::Property(vec!["app_db".to_owned(), "password".to_owned()]),
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
        let password = errors[0].hint.as_deref().expect("password remedy");
        assert!(
            password.contains("secret(<KEY>)") && password.contains("APP_DB_PASSWORD"),
            "a credential property names the secret form: {password}"
        );
        assert_eq!(
            errors[1].hint.as_deref(),
            Some("declare the resource in the `Forkfile`, or supply a `??` fallback"),
            "an undeclared resource keeps the declare-it remedy: {errors:?}"
        );
    }
}
