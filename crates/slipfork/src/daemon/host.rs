//! `env` and `secret` control-plane methods: the `Forkfile` visitor plus the `0600` vault.
//!
//! The daemon owns one `SecretVault` adapter at `<state_dir>/secrets.json`; the CLI never touches
//! the file, and a plaintext value crosses the socket only when the caller passed `--reveal` on a
//! real terminal. Components and keys come from [`Forkfile::env_bindings`], the canonical
//! `on`-aware visitor — never from a re-scan of source text.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use slipfork_forkfile::EnvBinding;
use slipfork_ospp::{FileSecretVault, Secret, SecretVault};

use super::Failure;
use crate::world::features::WorldFeatures;

/// `<state_dir>/secrets.json`.
fn vault_path(state_dir: &Path) -> PathBuf {
    state_dir.join("secrets.json")
}

/// Resolves every declared runtime variable for one project, or one component.
///
/// Values stay `null` unless `reveal` is true; the CLI only asks for them on `--reveal` at a TTY,
/// so plaintext is never sent to a piped or `--json` caller.
pub(super) fn env(
    state_dir: &Path,
    process_env: &BTreeMap<String, String>,
    params: &Value,
) -> Result<Option<Value>, Failure> {
    let project = project_of(params)?;
    let wanted = params.get("component").and_then(Value::as_str);
    let reveal = params.get("reveal").and_then(Value::as_bool) == Some(true);
    let document = document(&project)?;
    let features = features_of(&project, params, &document)?;
    let components = document.env_bindings_with(&features);
    let vault = FileSecretVault::new(vault_path(state_dir));
    let vars = FileSecretVault::new(state_dir.join("vars.json"));
    let resolver = crate::secrets::EnvironmentResolver::new(&vault, &vars, process_env);
    let scheme = "http";
    let identities = crate::world::WorldIdentities::open(state_dir.to_path_buf())
        .map_err(|message| Failure::new("ledger", message))?;
    let world = inferred_world(&project, &identities)?;
    let evaluated = {
        let failure = std::cell::RefCell::new(None);
        let secret =
            |component: &str, key: &str, generator: Option<&slipfork_forkfile::GeneratorSpec>| {
                resolver
                    .secret(component, key, generator)
                    .unwrap_or_else(|error| {
                        failure.borrow_mut().replace(error);
                        None
                    })
            };
        let var = |key: &str, generator: Option<&slipfork_forkfile::GeneratorSpec>| {
            resolver.var(key, generator).unwrap_or_else(|error| {
                failure.borrow_mut().replace(error);
                None
            })
        };
        let evaluated = document.eval_all_env_with_features(
            &world,
            scheme,
            &slipfork_forkfile::Resolvers {
                secret: &secret,
                var: &var,
            },
            &features,
        );
        if let Some(error) = failure.into_inner() {
            return Err(Failure::new("vault", error.message));
        }
        evaluated
    };
    let mut reply = serde_json::Map::new();
    for (component, declared) in components {
        if wanted.is_some_and(|id| id != component) {
            continue;
        }
        let eval_comp = evaluated.get(&component);
        let mut values = serde_json::Map::new();
        for (key, binding) in declared {
            let stored = load(&vault, &component, &key)?;
            let eval_entry = eval_comp.and_then(|map| map.get(&key));
            let tainted = binding.is_secret() || eval_entry.is_some_and(|(_, t)| *t);
            let set = stored.is_some() || eval_entry.is_some();
            let value = if tainted && !reveal {
                None
            } else {
                stored
                    .as_ref()
                    .map(|s| s.expose().to_owned())
                    .or_else(|| eval_entry.map(|(v, _)| v.clone()))
                    .or_else(|| resolve(&binding, None))
            };
            values.insert(
                key,
                json!({
                    "secret": tainted,
                    "set": set,
                    "value": value,
                }),
            );
        }
        reply.insert(component, Value::Object(values));
    }
    if let Some(id) = wanted
        && reply.is_empty()
    {
        return Err(Failure::new(
            "unknown-component",
            format!("`{id}` declares no runtime variables in this Forkfile"),
        ));
    }
    Ok(Some(json!({ "components": reply })))
}

/// Lists every declared variable with no value bytes on the wire.
pub(super) fn secret_list(state_dir: &Path, params: &Value) -> Result<Option<Value>, Failure> {
    let project = project_of(params)?;
    let wanted = params.get("component").and_then(Value::as_str);
    let document = document(&project)?;
    let features = features_of(&project, params, &document)?;
    let components = document.env_bindings_with(&features);
    let vault = FileSecretVault::new(vault_path(state_dir));
    let mut reply = serde_json::Map::new();
    for (component, declared) in components {
        if wanted.is_some_and(|id| id != component) {
            continue;
        }
        let mut keys = Vec::new();
        for (key, binding) in declared {
            if !binding.is_secret() {
                continue;
            }
            let declared_key = binding.secret.unwrap_or(&key);
            let stored = load(&vault, &component, declared_key)?.is_some();
            keys.push(json!({
                "key": key,
                "set": stored,
                "minted": binding.secret_generator.is_some() && !stored,
            }));
        }
        if !keys.is_empty() {
            reply.insert(component, Value::Array(keys));
        }
    }
    Ok(Some(json!({ "components": reply })))
}

/// Stores one declared secret; the value never appears in a reply or a log line.
pub(super) fn secret_set(state_dir: &Path, params: &Value) -> Result<Option<Value>, Failure> {
    let (component, key) = path_of(params)?;
    let value = params
        .get("value")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::new("bad-params", "`value` is required"))?;
    let project = project_of(params)?;
    let document = document(&project)?;
    let features = features_of(&project, params, &document)?;
    let components = document.env_bindings_with(&features);
    let declared = components.get(&component).ok_or_else(|| {
        Failure::new(
            "unknown-component",
            format!("`{component}` declares no runtime variables in this Forkfile"),
        )
    })?;
    match declared.get(&key) {
        Some(binding) if binding.is_secret() => {}
        Some(_) => {
            return Err(Failure::new(
                "unknown-secret",
                format!("`{component}.{key}` is not declared as secret(...)"),
            ));
        }
        None => {
            return Err(Failure::new(
                "unknown-secret",
                format!("`{component}.{key}` is not declared in this Forkfile"),
            ));
        }
    }
    let vault = FileSecretVault::new(vault_path(state_dir));
    vault
        .store(&component, &key, &Secret::new(value))
        .map_err(|error| Failure::new("vault", error.to_string()))?;
    Ok(Some(json!({ "component": component, "key": key })))
}

/// Removes exactly one stored key through the vault port; a missing key is a typed `no-secret`.
pub(super) fn secret_unset(state_dir: &Path, params: &Value) -> Result<Option<Value>, Failure> {
    let (component, key) = path_of(params)?;
    let project = project_of(params)?;
    let document = document(&project)?;
    let features = features_of(&project, params, &document)?;
    let components = document.env_bindings_with(&features);
    let Some(declared) = components.get(&component) else {
        return Err(Failure::new(
            "unknown-component",
            format!("`{component}` declares no runtime variables in this Forkfile"),
        ));
    };
    if !declared.get(&key).is_some_and(EnvBinding::is_secret) {
        return Err(Failure::new(
            "unknown-secret",
            format!("`{component}.{key}` is not declared as secret(...)"),
        ));
    }
    let vault = FileSecretVault::new(vault_path(state_dir));
    let removed = vault
        .unset(&component, &key)
        .map_err(|error| Failure::new("vault", error.to_string()))?;
    if !removed {
        return Err(Failure::new(
            "no-secret",
            format!("`{component}.{key}` is not set"),
        ));
    }
    Ok(Some(json!({ "component": component, "key": key })))
}

fn project_of(params: &Value) -> Result<PathBuf, Failure> {
    params
        .get("project")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| Failure::new("bad-params", "`project` is required"))
}

fn path_of(params: &Value) -> Result<(String, String), Failure> {
    let component = params
        .get("component")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Failure::new("bad-params", "`component` is required"))?;
    let key = params
        .get("key")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Failure::new("bad-params", "`key` is required"))?;
    Ok((component.to_owned(), key.to_owned()))
}

fn load(vault: &FileSecretVault, component: &str, key: &str) -> Result<Option<Secret>, Failure> {
    vault
        .load(component, key)
        .map_err(|error| Failure::new("vault", error.to_string()))
}

fn resolve(binding: &EnvBinding<'_>, stored: Option<&Secret>) -> Option<String> {
    match stored {
        Some(secret) => Some(secret.expose().to_owned()),
        None if binding.is_secret() => None,
        None => binding.literal.clone(),
    }
}

/// Resolves the world the ledger row owns for `<project>/Forkfile`; never a stale record.
fn inferred_world(
    project: &Path,
    identities: &crate::world::WorldIdentities<'_>,
) -> Result<String, Failure> {
    identities
        .name(project, None)
        .map(|name| name.as_str().to_owned())
        .map_err(|message| Failure::new("world", message))
}

/// The world's features: the request merged over the persisted file (evaluate-only for `env`).
fn features_of(
    project: &Path,
    params: &Value,
    document: &slipfork_forkfile::Forkfile,
) -> Result<slipfork_forkfile::FeatureSet, Failure> {
    let request = WorldFeatures::from_params(params)
        .map_err(|error| Failure::new("feature", format!("{error}\nnext: pass `-F <feature>`")))?
        .unwrap_or_default();
    let config = WorldFeatures::merged(project, &request).map_err(|error| {
        Failure::new(
            "feature",
            format!("{error}\nnext: {}", WorldFeatures::REMEDY),
        )
    })?;
    config.resolve(document).map_err(|diagnostic| {
        Failure::new(
            "feature",
            format!(
                "Forkfile:{}: {}\nnext: {}",
                diagnostic.span.line(),
                diagnostic.message,
                diagnostic.next
            ),
        )
    })
}

fn document(project: &Path) -> Result<slipfork_forkfile::Forkfile, Failure> {
    let path = project.join("Forkfile");
    let source = std::fs::read_to_string(&path).map_err(|error| {
        Failure::new(
            "no-forkfile",
            format!("cannot read `{}`: {error}", path.display()),
        )
    })?;
    slipfork_forkfile::compile(&source)
        .map(|checked| *checked.document)
        .map_err(|diagnostics| {
            let detail = diagnostics.first().map_or_else(
                || "invalid Forkfile".to_owned(),
                |item| format!("Forkfile:{}: {}", item.span.line(), item.message),
            );
            Failure::new("invalid-forkfile", detail)
        })
}
