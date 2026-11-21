//! `env` and `secret` control-plane methods: the `Forkfile` visitor plus the `0600` vault.
//!
//! The daemon owns one `SecretVault` adapter at `<state_dir>/secrets.json`; the CLI never touches
//! the file, and a plaintext value crosses the socket only when the caller passed `--reveal` on a
//! real terminal. Components and keys come from [`Forkfile::env_bindings`], the canonical
//! `on`-aware visitor — never from a re-scan of source text.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use slipfork_forkfile::EnvBinding;
use slipfork_ospp::{FileSecretVault, Secret, SecretVault};

use super::Failure;

/// `<state_dir>/secrets.json`.
fn vault_path(state_dir: &Path) -> PathBuf {
    state_dir.join("secrets.json")
}

/// Resolves every declared runtime variable for one project, or one component.
///
/// Values stay `null` unless `reveal` is true; the CLI only asks for them on `--reveal` at a TTY,
/// so plaintext is never sent to a piped or `--json` caller.
pub(super) fn env(state_dir: &Path, params: &Value) -> Result<Option<Value>, Failure> {
    let project = project_of(params)?;
    let wanted = params.get("component").and_then(Value::as_str);
    let reveal = params.get("reveal").and_then(Value::as_bool) == Some(true);
    let document = document(&project)?;
    let components = document.env_bindings();
    let vault = FileSecretVault::new(vault_path(state_dir));
    let mut reply = serde_json::Map::new();
    for (component, declared) in components {
        if wanted.is_some_and(|id| id != component) {
            continue;
        }
        let mut values = serde_json::Map::new();
        for (key, binding) in declared {
            let stored = load(&vault, &component, &key)?;
            // Only secret values need the reveal gate; a literal is not a credential.
            let value = if binding.is_secret() && !reveal {
                None
            } else {
                resolve(&binding, stored.as_ref())
            };
            values.insert(
                key,
                json!({
                    "secret": binding.is_secret(),
                    "set": stored.is_some(),
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
    let components = document.env_bindings();
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
            let set = load(&vault, &component, &key)?.is_some();
            keys.push(json!({ "key": key, "set": set }));
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
    let components = document.env_bindings();
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
    let components = document.env_bindings();
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

/// Reads `<project>/Forkfile`; the visitor classifies every declared variable.
fn document(project: &Path) -> Result<slipfork_forkfile::Forkfile, Failure> {
    let path = project.join("Forkfile");
    let source = std::fs::read_to_string(&path).map_err(|error| {
        Failure::new(
            "no-forkfile",
            format!("cannot read `{}`: {error}", path.display()),
        )
    })?;
    slipfork_forkfile::parse(&source).map_err(|diagnostics| {
        let detail = diagnostics.first().map_or_else(
            || "invalid Forkfile".to_owned(),
            |item| format!("Forkfile:{}: {}", item.span.line(), item.message),
        );
        Failure::new("invalid-forkfile", detail)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORKFILE: &str = "# forkfile: 0.1\n\nenv app {\n    LOG_LEVEL = \"info\"\n    SECRET_KEY = secret(SECRET_KEY)\n}\n";
    const SECRET: &str = "s3cr3t-value";

    fn lab() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::Builder::new()
            .prefix("slipfork-host-")
            .tempdir_in("/var/tmp")
            .expect("disk temp dir");
        let project = root.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        std::fs::write(project.join("Forkfile"), FORKFILE).expect("write Forkfile");
        (root, project)
    }

    fn params(project: &Path) -> Value {
        json!({ "project": project })
    }

    #[test]
    fn env_sends_null_values_until_reveal_is_asked() {
        let (root, project) = lab();
        let state = root.path().join("state");
        secret_set(
            &state,
            &json!({ "project": project, "component": "app", "key": "SECRET_KEY", "value": SECRET }),
        )
        .expect("set");

        let redacted = env(&state, &params(&project)).expect("env").expect("reply");
        assert_eq!(
            redacted["components"]["app"]["SECRET_KEY"]["value"],
            Value::Null
        );
        assert_eq!(redacted["components"]["app"]["SECRET_KEY"]["set"], true);
        assert_eq!(redacted["components"]["app"]["SECRET_KEY"]["secret"], true);
        assert_eq!(redacted["components"]["app"]["LOG_LEVEL"]["value"], "info");
        assert!(!redacted.to_string().contains(SECRET));

        let revealed = env(&state, &json!({ "project": project, "reveal": true }))
            .expect("env")
            .expect("reply");
        assert_eq!(revealed["components"]["app"]["SECRET_KEY"]["value"], SECRET);
        assert_eq!(revealed["components"]["app"]["LOG_LEVEL"]["value"], "info");
    }

    #[test]
    fn secret_unset_goes_through_the_vault_port_and_refuses_unknown_keys() {
        let (root, project) = lab();
        let state = root.path().join("state");
        let target = json!({ "project": project, "component": "app", "key": "SECRET_KEY" });
        secret_set(
            &state,
            &json!({ "project": project, "component": "app", "key": "SECRET_KEY", "value": SECRET }),
        )
        .expect("set");
        let listed = secret_list(&state, &params(&project))
            .expect("list")
            .expect("reply");
        assert_eq!(listed["components"]["app"][0]["set"], true);

        secret_unset(&state, &target).expect("unset");
        let listed = secret_list(&state, &params(&project))
            .expect("list")
            .expect("reply");
        assert_eq!(listed["components"]["app"][0]["set"], false);
        let error = secret_unset(&state, &target).expect_err("already gone");
        assert_eq!(error.code, "no-secret");

        let not_secret = json!({ "project": project, "component": "app", "key": "LOG_LEVEL" });
        let error = secret_unset(&state, &not_secret).expect_err("not a secret");
        assert_eq!(error.code, "unknown-secret");
    }
}
