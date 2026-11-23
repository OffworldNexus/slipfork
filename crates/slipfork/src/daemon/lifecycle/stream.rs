//! Streaming verbs: `exec` target resolution and the `shell` → `exec` pipe hand-off.
//!
//! The daemon never re-renders output: it forwards typed `line`/`divider`/`eof` events and lets
//! the CLI own gutters and colour. Targets resolve against the typed `ps` projection, never by
//! re-compiling the `Forkfile` in the CLI; the real-PTY links live in [`super::pty`].

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use slipfork_forkfile::Forkfile;
use slipfork_world::control::{PROTOCOL_VERSION, Request, StreamClient};

use super::super::Failure;
use super::super::rpc::key_of;
use super::core::{Core, World};
use crate::ps::{PsKind, PsReply, PsWorld};

/// Kinds a plain `exec` may target.
const ANY: &[PsKind] = &[
    PsKind::Service,
    PsKind::Image,
    PsKind::Postgres,
    PsKind::Redis,
];
/// Kinds a `shell` may attach to interactively.
pub(super) const SHELLABLE: &[PsKind] = &[
    PsKind::Service,
    PsKind::Image,
    PsKind::Postgres,
    PsKind::Redis,
];
/// Kinds `slipfork exec --select postgres` may target.
const DATABASES: &[PsKind] = &[PsKind::Postgres];
/// Kinds `slipfork exec --select redis` may target.
const CACHES: &[PsKind] = &[PsKind::Redis];

/// One validated `exec` destination: the owning project, pod id, and argv.
pub(super) struct ExecTarget {
    pub(super) project: PathBuf,
    pub(super) pod: String,
    pub(super) argv: Vec<String>,
}

impl Core {
    /// Resolves one `exec` target and opens its world stream; the caller pumps stdin.
    pub(crate) fn open_exec(&mut self, params: &Value) -> Result<(StreamClient, bool), Failure> {
        let ExecTarget { project, pod, argv } = self.resolve_exec(params)?;
        let stdin = params
            .get("stdin")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let pods_dir = self.live_pods_dir(&project)?;
        let mut client = StreamClient::open(&pods_dir)
            .map_err(|error| Failure::new("world-unavailable", error.to_string()))?;
        client
            .send(&Request::Exec {
                v: PROTOCOL_VERSION,
                id: "daemon-exec".to_owned(),
                pod,
                argv,
                stdin,
            })
            .map_err(|error| Failure::new("world-unavailable", error.to_string()))?;
        Ok((client, stdin))
    }

    /// Resolves one `exec` `target`/`argv` pair against the live roster; the one validation path.
    pub(super) fn resolve_exec(&mut self, params: &Value) -> Result<ExecTarget, Failure> {
        let project = project_param(params)?;
        let select = params
            .get("select")
            .and_then(Value::as_str)
            .unwrap_or("any");
        let kinds = match select {
            "postgres" => DATABASES,
            "redis" => CACHES,
            _ => ANY,
        };
        let target = params.get("target").and_then(Value::as_str);
        let argv: Vec<String> = params
            .get("argv")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if argv.is_empty() {
            return Err(Failure::new(
                "bad-params",
                "`argv` must be a non-empty array",
            ));
        }
        let reply = self.ps()?;
        let mut ids = Vec::new();
        for world in world_rows(&reply, &project) {
            for service in &world.services {
                if kinds.contains(&service.kind) && service.running && !ids.contains(&service.id) {
                    ids.push(service.id.clone());
                }
            }
        }
        let pod = choose(target, &ids, select)?;
        Ok(ExecTarget { project, pod, argv })
    }

    /// Resolves one `shell` target into the `exec` payload the daemon relays for a piped stdio.
    pub(crate) fn shell_exec_params(&mut self, params: &Value) -> Result<Value, Failure> {
        let project = project_param(params)?;
        let target = target_param(params)?;
        let tty = params.get("tty").and_then(Value::as_bool).unwrap_or(false);
        let plan = self.shell_plan(&project, target, tty)?;
        Ok(json!({
            "project": project,
            "target": plan.pod,
            "argv": plan.argv,
            "stdin": true,
            "select": "any",
        }))
    }

    /// The live world's pod root.
    pub(super) fn live_pods_dir(&self, project: &Path) -> Result<PathBuf, Failure> {
        let entity = self
            .worlds
            .get(&key_of(project))
            .ok_or_else(|| Failure::new("no-world", "no world is running in this directory"))?;
        match entity {
            World::Booting { world, .. } => Err(Failure::new(
                "world-booting",
                format!("world `{world}` is still starting"),
            )),
            World::Live { entry, .. } => Ok(entry.pods_dir.clone()),
        }
    }
}

fn choose(target: Option<&str>, ids: &[String], select: &str) -> Result<String, Failure> {
    if let Some(target) = target {
        if ids.iter().any(|id| id == target) {
            return Ok(target.to_owned());
        }
        return Err(Failure::new(
            "no-target",
            format!("`{target}` is not a running target in this world"),
        ));
    }
    match ids {
        [] => Err(Failure::new(
            "no-target",
            format!("this world declares no running {} target", noun(select)),
        )),
        [only] => Ok(only.clone()),
        many => Err(Failure::new(
            "ambiguous-target",
            format!("{} candidates: {}", many.len(), many.join(", ")),
        )),
    }
}

fn noun(select: &str) -> &'static str {
    match select {
        "postgres" => "postgres",
        "redis" => "redis",
        _ => "service",
    }
}

/// The `Forkfile` loaded through the one compile path, when it is present and valid.
pub(super) fn load_document(project: &Path) -> Option<Forkfile> {
    let source = fs::read_to_string(project.join("Forkfile")).ok()?;
    slipfork_forkfile::compile(&source)
        .ok()
        .map(|checked| *checked.document)
}

pub(super) fn project_param(params: &Value) -> Result<PathBuf, Failure> {
    params
        .get("project")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| Failure::new("bad-params", "`project` is required"))
}

pub(super) fn target_param(params: &Value) -> Result<&str, Failure> {
    params
        .get("target")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::new("bad-params", "`target` is required"))
}

/// The world matching `project`, plus its forks.
pub(super) fn world_rows<'a>(
    reply: &'a PsReply,
    project: &Path,
) -> impl Iterator<Item = &'a PsWorld> {
    reply
        .worlds
        .iter()
        .flat_map(|world| std::iter::once(world).chain(world.forks.iter()))
        .filter(move |world| world.project == project)
}

/// Turns the world's final reply into the daemon's `ok` payload.
pub(crate) fn finish(
    ok: Option<Value>,
    error: Option<slipfork_world::ControlError>,
) -> Result<Value, Failure> {
    match (error, ok) {
        (Some(error), _) => Err(Failure::new("world-refused", error.message)),
        (None, Some(value)) => Ok(value),
        (None, None) => Err(Failure::new("world-refused", "the world sent no reply")),
    }
}
