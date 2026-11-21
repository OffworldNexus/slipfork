//! Streaming verbs: `logs` and `exec` relayed over the world's single `control.sock`.
//!
//! The daemon never re-renders output: it forwards typed `line`/`divider`/`eof` events and lets
//! the CLI own gutters and colour. Service ids resolve against the typed `ps` projection, so no
//! verb parses a pod name on its own.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use slipfork_world::control::{PROTOCOL_VERSION, Request, StreamClient, StreamFrame};

use super::super::Failure;
use super::super::rpc::key_of;
use super::core::{Core, World};
use crate::ps::{PsKind, PsReply, PsWorld};

/// Kinds a plain `shell`/`exec` may target.
const ANY: &[PsKind] = &[
    PsKind::Service,
    PsKind::Image,
    PsKind::Postgres,
    PsKind::Redis,
];
/// Kinds `slipfork db` may target.
const DATABASES: &[PsKind] = &[PsKind::Postgres];
/// Kinds `slipfork redis` may target.
const CACHES: &[PsKind] = &[PsKind::Redis];

impl Core {
    /// Streams captured pod output; `alive` reports whether the CLI connection still exists.
    pub(crate) fn logs(
        &mut self,
        params: &Value,
        emit: &mut dyn FnMut(Value) -> bool,
        alive: &mut dyn FnMut() -> bool,
    ) -> Result<Value, Failure> {
        let project = project_param(params)?;
        let (pods_dir, ids) = self.live_pods(&project, ANY)?;
        let service = params
            .get("service")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if ids.is_empty() && service.is_none() {
            return Err(Failure::new(
                "no-target",
                "no pods are running in this world",
            ));
        }
        let follow = params
            .get("follow")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut client = StreamClient::open(&pods_dir)
            .map_err(|error| Failure::new("world-unavailable", error.to_string()))?;
        client
            .send(&Request::Logs {
                v: PROTOCOL_VERSION,
                id: "daemon-logs".to_owned(),
                service,
                follow,
            })
            .map_err(|error| Failure::new("world-unavailable", error.to_string()))?;
        // The roster arrives once so the renderer fixes its gutter width before any line.
        let roster = json!({
            "kind": "roster",
            "pod": "",
            "stream": "combined",
            "line": "",
            "pods": ids,
        });
        if !emit(roster) {
            client.close();
            return Ok(json!({ "closed": true }));
        }
        loop {
            if !alive() {
                client.close();
                return Ok(json!({ "closed": true }));
            }
            match client.poll() {
                Ok(Some(StreamFrame::Output(event))) => {
                    let value = json!({
                        "kind": event.kind.as_str(),
                        "pod": event.pod,
                        "stream": event.stream.as_str(),
                        "line": event.line,
                    });
                    if !emit(value) {
                        client.close();
                        return Ok(json!({ "closed": true }));
                    }
                }
                Ok(Some(StreamFrame::Reply { ok, error })) => {
                    return finish(ok, error);
                }
                Ok(None) => thread::sleep(Duration::from_millis(40)),
                Err(error) => {
                    return Err(Failure::new("world-stream", error.to_string()));
                }
            }
        }
    }

    /// Resolves one `exec` target and opens its world stream; the caller pumps stdin.
    pub(crate) fn open_exec(&mut self, params: &Value) -> Result<(StreamClient, bool), Failure> {
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
        let (pods_dir, ids) = self.live_pods(&project, kinds)?;
        let pod = choose(target, &ids, select)?;
        let stdin = params
            .get("stdin")
            .and_then(Value::as_bool)
            .unwrap_or(false);
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

    /// The live world's pod root and the running pod ids of the requested kinds.
    fn live_pods(
        &mut self,
        project: &Path,
        kinds: &[PsKind],
    ) -> Result<(PathBuf, Vec<String>), Failure> {
        let entity = self
            .worlds
            .get(&key_of(project))
            .ok_or_else(|| Failure::new("no-world", "no world is running in this directory"))?;
        let entry = match entity {
            World::Booting { world, .. } => {
                return Err(Failure::new(
                    "world-booting",
                    format!("world `{world}` is still starting"),
                ));
            }
            World::Live { entry, .. } => entry.clone(),
        };
        let reply = self.ps()?;
        let ids = world_rows(&reply, project)
            .flat_map(|world| world.services.iter())
            .filter(|service| kinds.contains(&service.kind) && service.running)
            .map(|service| service.id.clone())
            .collect();
        Ok((entry.pods_dir, ids))
    }
}

fn project_param(params: &Value) -> Result<PathBuf, Failure> {
    params
        .get("project")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| Failure::new("bad-params", "`project` is required"))
}

/// The world matching `project`, plus its forks.
fn world_rows<'a>(reply: &'a PsReply, project: &Path) -> impl Iterator<Item = &'a PsWorld> {
    reply
        .worlds
        .iter()
        .flat_map(|world| std::iter::once(world).chain(world.forks.iter()))
        .filter(move |world| world.project == project)
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
