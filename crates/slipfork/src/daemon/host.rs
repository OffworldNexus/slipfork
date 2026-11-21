//! `env` and `secret` control-plane methods: the evaluated `Forkfile` plus the `0600` vault.
//!
//! The daemon owns one `SecretVault` adapter at `<state_dir>/secrets.json`; the CLI never touches
//! the file, and a value only reaches it when the caller passed `--reveal` on a real terminal.
//! Components and keys come from the parsed `Forkfile` (top-level `env` groups, image inline
//! `env` blocks, and `use env` groups), never from a re-scan of source text.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use slipfork_forkfile::ast::{EnvItem, ImageItem, OnBody, TopItem};
use slipfork_forkfile::{Expr, ExprKind};
use slipfork_ospp::{FileSecretVault, Secret, SecretVault};

use super::Failure;

/// One declared runtime variable as the `Forkfile` sees it.
#[derive(Debug, Clone)]
struct Binding {
    /// Whether the value is a `secret(KEY)` lookup served from the vault.
    secret: bool,
    /// The static literal, when the declaration has one.
    literal: Option<String>,
}

/// `<state_dir>/secrets.json`.
fn vault_path(state_dir: &Path) -> PathBuf {
    state_dir.join("secrets.json")
}

/// Resolves every declared runtime variable for one project, or one component.
pub(super) fn env(state_dir: &Path, params: &Value) -> Result<Option<Value>, Failure> {
    let project = project_of(params)?;
    let wanted = params.get("component").and_then(Value::as_str);
    let components = collect(&project)?;
    let vault = FileSecretVault::new(vault_path(state_dir));
    let mut reply = serde_json::Map::new();
    for (component, bindings) in components {
        if wanted.is_some_and(|id| id != component) {
            continue;
        }
        let mut values = serde_json::Map::new();
        for (key, binding) in bindings {
            let stored = load(&vault, &component, &key)?;
            values.insert(
                key,
                json!({
                    "secret": binding.secret,
                    "set": stored.is_some(),
                    "value": resolve(binding, stored),
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
    let components = collect(&project)?;
    let vault = FileSecretVault::new(vault_path(state_dir));
    let mut reply = serde_json::Map::new();
    for (component, bindings) in components {
        if wanted.is_some_and(|id| id != component) {
            continue;
        }
        let mut keys = Vec::new();
        for (key, binding) in bindings {
            if !binding.secret {
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
    let components = collect(&project)?;
    let bindings = components.get(&component).ok_or_else(|| {
        Failure::new(
            "unknown-component",
            format!("`{component}` declares no runtime variables in this Forkfile"),
        )
    })?;
    match bindings.get(&key) {
        Some(binding) if binding.secret => {}
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

/// Removes exactly one stored key; a missing key is a typed `no-secret`.
pub(super) fn secret_unset(state_dir: &Path, params: &Value) -> Result<Option<Value>, Failure> {
    let (component, key) = path_of(params)?;
    let project = project_of(params)?;
    let components = collect(&project)?;
    let Some(bindings) = components.get(&component) else {
        return Err(Failure::new(
            "unknown-component",
            format!("`{component}` declares no runtime variables in this Forkfile"),
        ));
    };
    if !bindings.get(&key).is_some_and(|binding| binding.secret) {
        return Err(Failure::new(
            "unknown-secret",
            format!("`{component}.{key}` is not declared as secret(...)"),
        ));
    }
    let vault = FileSecretVault::new(vault_path(state_dir));
    if load(&vault, &component, &key)?.is_none() {
        return Err(Failure::new(
            "no-secret",
            format!("`{component}.{key}` is not set"),
        ));
    }
    remove(vault.path(), &component, &key)?;
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

fn resolve(binding: Binding, stored: Option<Secret>) -> Option<String> {
    match stored {
        Some(secret) => Some(secret.expose().to_owned()),
        None if binding.secret => None,
        None => binding.literal,
    }
}

/// Reads every component's declared variables from `<project>/Forkfile`.
fn collect(project: &Path) -> Result<BTreeMap<String, BTreeMap<String, Binding>>, Failure> {
    let path = project.join("Forkfile");
    let source = fs::read_to_string(&path).map_err(|error| {
        Failure::new(
            "no-forkfile",
            format!("cannot read `{}`: {error}", path.display()),
        )
    })?;
    let document = slipfork_forkfile::parse(&source).map_err(|diagnostics| {
        let detail = diagnostics.first().map_or_else(
            || "invalid Forkfile".to_owned(),
            |item| format!("Forkfile:{}: {}", item.span.line(), item.message),
        );
        Failure::new("invalid-forkfile", detail)
    })?;
    let mut groups: BTreeMap<String, BTreeMap<String, Binding>> = BTreeMap::new();
    for item in &document.items.items {
        collect_groups(item, &mut groups);
    }
    let mut components = groups.clone();
    for item in &document.items.items {
        collect_images(item, &groups, &mut components);
    }
    Ok(components)
}

fn collect_groups(item: &TopItem, groups: &mut BTreeMap<String, BTreeMap<String, Binding>>) {
    match item {
        TopItem::Env(env) => {
            let entry = groups.entry(env.group.clone()).or_default();
            fill(entry, &env.entries.items);
        }
        TopItem::On(on) => match &on.body {
            OnBody::Inline(inner) => collect_groups(inner, groups),
            OnBody::Block(block) => {
                for item in &block.items {
                    collect_groups(item, groups);
                }
            }
        },
        _ => {}
    }
}

fn collect_images(
    item: &TopItem,
    groups: &BTreeMap<String, BTreeMap<String, Binding>>,
    components: &mut BTreeMap<String, BTreeMap<String, Binding>>,
) {
    match item {
        TopItem::Image(image) => {
            let mut bindings = BTreeMap::new();
            image_env(&image.body.items, groups, &mut bindings);
            components.insert(image.id.clone(), bindings.clone());
            services(&image.body.items, &bindings, components);
        }
        TopItem::On(on) => match &on.body {
            OnBody::Inline(inner) => collect_images(inner, groups, components),
            OnBody::Block(block) => {
                for item in &block.items {
                    collect_images(item, groups, components);
                }
            }
        },
        _ => {}
    }
}

fn image_env(
    items: &[ImageItem],
    groups: &BTreeMap<String, BTreeMap<String, Binding>>,
    bindings: &mut BTreeMap<String, Binding>,
) {
    for item in items {
        match item {
            ImageItem::Env(block) => fill(bindings, &block.entries.items),
            ImageItem::UseEnv(group) => {
                if let Some(inherited) = groups.get(group) {
                    bindings.extend(inherited.clone());
                }
            }
            ImageItem::On(on) => match &on.body {
                OnBody::Inline(inner) => {
                    image_env(std::slice::from_ref(inner.as_ref()), groups, bindings);
                }
                OnBody::Block(block) => image_env(&block.items, groups, bindings),
            },
            _ => {}
        }
    }
}

fn services(
    items: &[ImageItem],
    bindings: &BTreeMap<String, Binding>,
    components: &mut BTreeMap<String, BTreeMap<String, Binding>>,
) {
    for item in items {
        match item {
            ImageItem::Service(service) => {
                components.insert(service.id.clone(), bindings.clone());
            }
            ImageItem::On(on) => match &on.body {
                OnBody::Inline(inner) => {
                    services(std::slice::from_ref(inner.as_ref()), bindings, components)
                }
                OnBody::Block(block) => services(&block.items, bindings, components),
            },
            _ => {}
        }
    }
}

fn fill(bindings: &mut BTreeMap<String, Binding>, items: &[EnvItem]) {
    for item in items {
        match item {
            EnvItem::Entry(entry) => {
                bindings.insert(entry.key.clone(), binding(&entry.value));
            }
            EnvItem::On(on) => match &on.body {
                OnBody::Inline(inner) => fill(bindings, std::slice::from_ref(inner.as_ref())),
                OnBody::Block(block) => fill(bindings, &block.items),
            },
        }
    }
}

fn binding(expr: &Expr) -> Binding {
    Binding {
        secret: secret_key(expr).is_some(),
        literal: literal(expr),
    }
}

/// The first `secret(...)` reachable through fallbacks or concatenation.
fn secret_key(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Secret(key) => Some(key),
        ExprKind::Fallback(left, right) => secret_key(left).or_else(|| secret_key(right)),
        ExprKind::Concat(left, right) => secret_key(left).or_else(|| secret_key(right)),
        _ => None,
    }
}

/// The static literal a declaration would resolve to without the vault.
fn literal(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::String(text) => Some(text.clone()),
        ExprKind::Bool(value) => Some(value.to_string()),
        ExprKind::Int(value) => Some(value.to_string()),
        ExprKind::Fallback(left, right) => literal(left).or_else(|| literal(right)),
        _ => None,
    }
}

/// Deletes one key while preserving the vault's atomic `0600` write.
///
/// `SecretVault` intentionally exposes no `remove`; this is the daemon's own persisted
/// projection, so one narrow rewrite here is smaller than a second vault abstraction.
fn remove(path: &Path, component: &str, key: &str) -> Result<(), Failure> {
    let mut entries: BTreeMap<String, BTreeMap<String, String>> = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            Failure::new(
                "vault",
                format!("`{}` is not valid JSON: {error}", path.display()),
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(Failure::new(
                "vault",
                format!("read `{}`: {error}", path.display()),
            ));
        }
    };
    if let Some(values) = entries.get_mut(component) {
        values.remove(key);
        if values.is_empty() {
            entries.remove(component);
        }
    }
    let bytes = serde_json::to_vec_pretty(&entries)
        .map_err(|error| Failure::new("vault", format!("encode `{}`: {error}", path.display())))?;
    write_private(path, &bytes)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    let parent = path
        .parent()
        .ok_or_else(|| Failure::new("vault", "the vault path has no parent directory"))?;
    fs::create_dir_all(parent).map_err(|error| {
        Failure::new("vault", format!("create `{}`: {error}", parent.display()))
    })?;
    let temporary = parent.join(format!(
        ".secrets-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos())
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| {
                Failure::new(
                    "vault",
                    format!("create `{}`: {error}", temporary.display()),
                )
            })?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                Failure::new("vault", format!("write `{}`: {error}", temporary.display()))
            })?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).map_err(|error| {
            Failure::new("vault", format!("chmod `{}`: {error}", temporary.display()))
        })?;
        fs::rename(&temporary, path).map_err(|error| {
            Failure::new(
                "vault",
                format!("rename into `{}`: {error}", path.display()),
            )
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
