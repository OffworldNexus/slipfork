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

/// Kinds bare `slipfork logs` merges by default (application services only).
const SERVICES: &[PsKind] = &[PsKind::Service];
/// Kinds `slipfork logs -a` merges (services plus pod-backed resources).
const ALL_LOGS: &[PsKind] = &[
    PsKind::Service,
    PsKind::Image,
    PsKind::Postgres,
    PsKind::Redis,
];
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
        let all = params.get("all").and_then(Value::as_bool).unwrap_or(false);
        let requested = params
            .get("service")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let (pods_dir, roster_ids, service, filter_pods) = match requested {
            Some(target) => {
                let (pods_dir, resolved) = self.resolve_log_target(&project, &target)?;
                (pods_dir, vec![resolved.clone()], Some(resolved), None)
            }
            None => {
                let kinds = if all { ALL_LOGS } else { SERVICES };
                let (pods_dir, ids) = self.live_pods(&project, kinds)?;
                if ids.is_empty() {
                    return Err(Failure::new(
                        "no-target",
                        "no pods are running in this world",
                    ));
                }
                let filter = Some(ids.clone());
                (pods_dir, ids, None, filter)
            }
        };
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
                pods: filter_pods,
                follow,
            })
            .map_err(|error| Failure::new("world-unavailable", error.to_string()))?;
        // The roster arrives once so the renderer fixes its gutter width before any line.
        let roster = json!({
            "kind": "roster",
            "pod": "",
            "stream": "combined",
            "line": "",
            "pods": roster_ids,
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

    /// Maps an explicit `logs <target>` onto its pod id; a native S3 bucket has no pod stream, so
    /// it fails with the one-line remedy instead of an opaque init error.
    fn resolve_log_target(
        &mut self,
        project: &Path,
        target: &str,
    ) -> Result<(PathBuf, String), Failure> {
        let pods_dir = self.live_pods_dir(project)?;
        let reply = self.ps()?;
        for world in world_rows(&reply, project) {
            if world
                .services
                .iter()
                .any(|svc| svc.id == target && svc.kind == PsKind::S3)
            {
                return Err(Failure::new(
                    "no-pod",
                    "native S3 has no pod logs; use `slipfork status`",
                ));
            }
        }
        Ok((pods_dir, target.to_owned()))
    }

    /// The live world's pod root and the running pod ids of the requested kinds.
    fn live_pods(
        &mut self,
        project: &Path,
        kinds: &[PsKind],
    ) -> Result<(PathBuf, Vec<String>), Failure> {
        let pods_dir = self.live_pods_dir(project)?;
        let reply = self.ps()?;
        let mut ids = Vec::new();
        for world in world_rows(&reply, project) {
            for service in &world.services {
                if !kinds.contains(&service.kind) || !service.running {
                    continue;
                }
                if !ids.contains(&service.id) {
                    ids.push(service.id.clone());
                }
            }
        }
        Ok((pods_dir, ids))
    }

    fn live_pods_dir(&self, project: &Path) -> Result<PathBuf, Failure> {
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
