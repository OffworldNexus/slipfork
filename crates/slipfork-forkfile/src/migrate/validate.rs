//! Cross-reference, id, and port validation for the upstream model.

use std::collections::BTreeMap;

use super::snow::Snow;
use super::{MigrateError, schema};

impl Snow {
    /// Prove every cross-reference, id, and port is representable before emitting anything.
    pub(super) fn validate(&self) -> Result<(), MigrateError> {
        let mut owners: BTreeMap<String, &'static str> = BTreeMap::new();
        let mut claim = |kind: &'static str, id: &str, path: &str| -> Result<(), MigrateError> {
            if let Some(existing) = owners.insert(id.to_owned(), kind) {
                return Err(schema(
                    format!("{kind}.{id}"),
                    format!(
                        "id `{id}` is declared as both {existing} and {kind}; Forkfile ids share \
                         one namespace"
                    ),
                    "rename one of the two declarations",
                ));
            }
            let _ = path;
            Ok(())
        };
        for (id, _version) in &self.pg_clusters {
            claim("postgres cluster", id, id)?;
        }
        for database in &self.pg_databases {
            claim("postgres database", &database.id, &database.id)?;
        }
        for (id, _version) in &self.redis {
            claim("redis cluster", id, id)?;
        }
        for id in &self.s3 {
            claim("s3 bucket", id, id)?;
        }
        for id in &self.domains {
            claim("domain", id, id)?;
        }
        for group in self.env_groups.keys() {
            claim("env group", group, group)?;
        }
        for component in &self.components {
            claim("service", &component.id, &component.path)?;
        }
        for database in &self.pg_databases {
            if !self
                .pg_clusters
                .iter()
                .any(|(cluster, _version)| cluster == &database.cluster)
            {
                return Err(schema(
                    format!("resources.pg_databases.{}", database.id),
                    format!("unknown cluster `{}`", database.cluster),
                    "declare the cluster under `resources.pg_clusters` first",
                ));
            }
        }
        for image in &self.images {
            for group in &image.build_envs {
                if !self.env_groups.contains_key(group) {
                    return Err(schema(
                        format!("{}.envs", image.path),
                        format!("unknown build env group `{group}`"),
                        "declare the group under `envs` or `modes.all.envs`",
                    ));
                }
            }
        }
        for component in &self.components {
            if !self.images.iter().any(|image| image.id == component.image) {
                return Err(schema(
                    format!("{}.image", component.path),
                    format!("unknown image `{}`", component.image),
                    "declare the image under `images` first",
                ));
            }
            for group in &component.envs {
                if !self.env_groups.contains_key(group) {
                    return Err(schema(
                        format!("{}.envs", component.path),
                        format!("unknown env group `{group}`"),
                        "declare the group under `envs` or `modes.all.envs`",
                    ));
                }
            }
        }
        let mut ports: BTreeMap<&str, u16> = BTreeMap::new();
        for ingress in &self.ingresses {
            if !self
                .components
                .iter()
                .any(|component| component.id == ingress.component)
            {
                return Err(schema(
                    format!("{}.target.component", ingress.path),
                    format!("unknown component `{}`", ingress.component),
                    "declare the component under `components` first",
                ));
            }
            match ports.insert(&ingress.component, ingress.port) {
                Some(existing) if existing != ingress.port => {
                    return Err(schema(
                        format!("{}.target.port", ingress.path),
                        format!(
                            "component `{}` is routed on two ports ({existing} and {}); a \
                             Forkfile service exposes one `port`",
                            ingress.component, ingress.port
                        ),
                        "route the component through one port",
                    ));
                }
                Some(_existing) => {}
                None => {}
            }
        }
        for hook in &self.hooks {
            if !self
                .components
                .iter()
                .any(|component| component.id == hook.component)
            {
                return Err(schema(
                    format!("{}.component", hook.path),
                    format!("unknown component `{}`", hook.component),
                    "declare the component under `components` first",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::MigrateError;
    use crate::migrate_snow_yaml;

    fn error(source: &str) -> MigrateError {
        migrate_snow_yaml(source).expect_err("migration must fail closed")
    }

    #[test]
    fn a_cross_kind_id_collision_names_both_kinds() {
        let error =
            error("resources:\n  redis_clusters:\n    - id: cache\ndomains:\n  - id: cache\n");
        let MigrateError::Schema { path, message, .. } = error else {
            panic!("expected a schema error, got {error:?}");
        };
        assert_eq!(path, "domain.cache");
        assert!(message.contains("redis cluster"), "{message}");
        assert!(message.contains("domain"), "{message}");
    }

    #[test]
    fn a_non_boolean_pool_is_refused() {
        let error = error(
            "resources:\n  pg_clusters:\n    - id: pg\n  pg_databases:\n    - id: app_db\n      cluster: pg\n      pool: no\n",
        );
        let MigrateError::Schema { path, message, .. } = error else {
            panic!("expected a schema error, got {error:?}");
        };
        assert_eq!(path, "resources.pg_databases[0].pool");
        assert!(message.contains("expected `true` or `false`"), "{message}");
    }

    #[test]
    fn an_unknown_ingress_component_is_reported_at_its_path() {
        let error = error(
            "components:\n  - id: api\n    image: api\n    run_command: api\nimages:\n  - id: api\n    context: ./api\ningresses:\n  - match:\n      prefix: /x\n    target:\n      component: ghost\n      port: 80\n",
        );
        let MigrateError::Schema { path, message, .. } = error else {
            panic!("expected a schema error, got {error:?}");
        };
        assert_eq!(path, "ingresses[0].target.component");
        assert!(message.contains("unknown component `ghost`"), "{message}");
    }

    const MODES: &str = "images:\n  - id: api\n    context: ./api\ncomponents:\n  - id: api\n    image: api\n    run_command: uv run api\nmodes:\n  dev:\n    components:\n      api:\n        run_command: uv run api --reload\n";

    #[test]
    fn a_mode_run_override_becomes_on_feature_run() {
        let output = migrate_snow_yaml(MODES).expect("migrate");
        assert!(output.contains("features {\n    dev\n}"), "{output}");
        let run_position = output
            .find("    run \"uv\" \"run\" \"api\"\n")
            .expect("base run");
        let override_position = output
            .find("    on dev: run \"uv\" \"run\" \"api\" \"--reload\"")
            .expect("mode override");
        assert!(
            run_position < override_position,
            "the unconditional run must precede the mode override: {output}"
        );

        let parsed = crate::parse_forkfile(&output).expect("migrated output must parse");
        // With `dev` active, the top-to-bottom `on dev: run …` is the resolved argv.
        let dev = parsed
            .evaluate(&crate::FeatureSet::with_features(["dev"]))
            .expect("evaluate with dev");
        let run = dev.services[0].run.as_ref().expect("run argv");
        let words: Vec<&str> = run
            .slots
            .iter()
            .map(|slot| match &slot.parts[0] {
                crate::ArgPart::Text(word) => word.as_str(),
                crate::ArgPart::Ref(_) => panic!("literal argv"),
            })
            .collect();
        assert_eq!(words, ["uv", "run", "api", "--reload"]);

        // Without the feature the base command stands.
        let plain = parsed
            .evaluate(&crate::FeatureSet::new())
            .expect("evaluate plain");
        let run = plain.services[0].run.as_ref().expect("run argv");
        let words: Vec<&str> = run
            .slots
            .iter()
            .map(|slot| match &slot.parts[0] {
                crate::ArgPart::Text(word) => word.as_str(),
                crate::ArgPart::Ref(_) => panic!("literal argv"),
            })
            .collect();
        assert_eq!(words, ["uv", "run", "api"]);
    }

    #[test]
    fn disabled_and_mode_scoped_envs_are_refused() {
        let disabled = MODES.replace(
            "run_command: uv run api --reload",
            "run_command: uv run api --reload\n        disabled: true",
        );
        let disabled_error = error(&disabled);
        assert!(
            matches!(&disabled_error, MigrateError::Schema { path, .. } if path == "modes.dev.components.api.disabled"),
            "{disabled_error:?}"
        );

        let mode_envs = "images: []\nmodes:\n  dev:\n    envs:\n      common:\n        A: b\n";
        let error = error(mode_envs);
        assert!(
            matches!(&error, MigrateError::Schema { path, .. } if path == "modes.dev.envs"),
            "{disabled_error:?}"
        );
    }
}
