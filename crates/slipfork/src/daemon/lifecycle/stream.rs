//! Streaming verbs: `exec` and the real-PTY `shell` relayed over the world's `control.sock`.
//!
//! The daemon never re-renders output: it forwards typed `line`/`divider`/`eof` events and lets
//! the CLI own gutters and colour. Service ids resolve against the typed `ps` projection, and the
//! `shell` kind dispatch (bash/sh, `psql`, `redis-cli`, or a refusal) happens here, never by
//! re-compiling the `Forkfile` in the CLI.

use std::fs;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use slipfork_forkfile::{Forkfile, ResourceDeclKind};
use slipfork_world::control::{
    PROTOCOL_VERSION, Request, StreamClient, control_socket_path, encode,
};

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

/// A connected world PTY: the control socket that carries resize/final frames and the master fd.
pub(crate) struct PtyLink {
    /// The world's `control.sock`, used for `ExecResize` frames and the final reply.
    pub(crate) socket: UnixStream,
    /// The `/dev/pts/N` master handed back by the world init.
    pub(crate) master: OwnedFd,
}

/// The resolved command one shell target maps onto.
struct ShellPlan {
    pod: String,
    argv: Vec<String>,
}

impl Core {
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
        let pods_dir = self.live_pods_dir(&project)?;
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

    /// Opens one real PTY inside the resolved pod and returns its master plus control socket.
    pub(crate) fn open_pty(&mut self, params: &Value) -> Result<PtyLink, Failure> {
        let project = project_param(params)?;
        let target = target_param(params)?;
        let cols = params
            .get("cols")
            .and_then(Value::as_u64)
            .and_then(|cols| u16::try_from(cols).ok())
            .unwrap_or(80);
        let rows = params
            .get("rows")
            .and_then(Value::as_u64)
            .and_then(|rows| u16::try_from(rows).ok())
            .unwrap_or(24);
        let plan = self.shell_plan(&project, target, true)?;
        let pods_dir = self.live_pods_dir(&project)?;
        let path = control_socket_path(&pods_dir).map_err(|error| {
            Failure::new("world-unavailable", format!("PTY socket path: {error}"))
        })?;
        let mut socket = UnixStream::connect(&path).map_err(|error| {
            Failure::new(
                "world-unavailable",
                format!("connect `{}`: {error}", path.display()),
            )
        })?;
        let request = Request::ExecPty {
            v: PROTOCOL_VERSION,
            id: "daemon-pty".to_owned(),
            pod: plan.pod,
            argv: plan.argv,
            cols,
            rows,
        };
        socket
            .write_all(
                &encode(&request)
                    .map_err(|error| Failure::new("world-unavailable", error.to_string()))?,
            )
            .and_then(|()| socket.flush())
            .map_err(|error| {
                Failure::new("world-unavailable", format!("send exec-pty: {error}"))
            })?;
        let (payload, master) = slipfork_world::fdpass::recv_fd(&socket).map_err(|error| {
            Failure::new(
                "world-unavailable",
                format!("receive the PTY master: {error}"),
            )
        })?;
        let Some(master) = master else {
            return Err(Failure::new(
                "world-unavailable",
                format!(
                    "the world answered `{}` without a PTY descriptor",
                    String::from_utf8_lossy(&payload)
                ),
            ));
        };
        Ok(PtyLink { socket, master })
    }

    /// Resolves a `shell` target against the live roster and builds the canonical argv.
    fn shell_plan(
        &mut self,
        project: &Path,
        target: &str,
        tty: bool,
    ) -> Result<ShellPlan, Failure> {
        let reply = self.ps()?;
        let worlds: Vec<&PsWorld> = world_rows(&reply, project).collect();
        if let Some(world) = worlds.iter().find(|world| world.booting) {
            return Err(Failure::new(
                "world-booting",
                format!("world `{}` is still starting", world.world),
            ));
        }
        let mut shellable: Vec<String> = Vec::new();
        for world in &worlds {
            for service in &world.services {
                if SHELLABLE.contains(&service.kind)
                    && service.running
                    && !shellable.contains(&service.id)
                {
                    shellable.push(service.id.clone());
                }
            }
        }
        let found = worlds.iter().find_map(|world| {
            world
                .services
                .iter()
                .find(|service| service.id == target)
                .map(|service| (world, service))
        });
        if let Some((world, service)) = found {
            let name = world.world.clone();
            return match service.kind {
                PsKind::S3 => Err(refusal_s3(target, &shellable)),
                PsKind::Cdn => Err(refusal_unknown(target, &shellable)),
                PsKind::Postgres => {
                    ensure_running(service.running, target)?;
                    Ok(ShellPlan {
                        pod: service.id.clone(),
                        argv: psql_argv(tty, None),
                    })
                }
                PsKind::Redis => {
                    ensure_running(service.running, target)?;
                    Ok(ShellPlan {
                        pod: service.id.clone(),
                        argv: vec!["redis-cli".to_owned()],
                    })
                }
                PsKind::Service | PsKind::Image => {
                    ensure_running(service.running, target)?;
                    Ok(ShellPlan {
                        pod: service.id.clone(),
                        argv: shell_argv(&name, target, tty),
                    })
                }
            };
        }
        let document = load_document(project);
        // A declared `database <db>` resolves to its owning postgres resource pod.
        if let Some(owner) = document
            .as_ref()
            .and_then(|document| database_owner(document, target))
        {
            let running = worlds.iter().any(|world| {
                world
                    .services
                    .iter()
                    .any(|service| service.id == owner && service.running)
            });
            ensure_running(running, &owner)?;
            return Ok(ShellPlan {
                pod: owner,
                argv: psql_argv(tty, Some(target)),
            });
        }
        // An image id shells into this project's running service built from that image.
        if let Some((name, pod)) = worlds.iter().find_map(|world| {
            world
                .services
                .iter()
                .find(|service| service.running && service.image.as_deref() == Some(target))
                .map(|service| (world.world.clone(), service.id.clone()))
        }) {
            return Ok(ShellPlan {
                pod,
                argv: shell_argv(&name, target, tty),
            });
        }
        Err(refusal_unknown(target, &shellable))
    }

    /// The live world's pod root.
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

fn ensure_running(running: bool, target: &str) -> Result<(), Failure> {
    if running {
        Ok(())
    } else {
        Err(Failure::new(
            "no-target",
            format!("`{target}` is not running in this world"),
        ))
    }
}

fn shell_argv(world: &str, target: &str, tty: bool) -> Vec<String> {
    let script = if tty {
        format!(
            "export PS1='({world}:{target}) \\w$ '; \
             if [ -x /bin/bash ]; then exec /bin/bash --noprofile --norc -i; fi; \
             exec /bin/sh -i"
        )
    } else {
        "if [ -x /bin/bash ]; then exec /bin/bash; fi; exec /bin/sh".to_owned()
    };
    vec!["sh".to_owned(), "-c".to_owned(), script]
}

fn psql_argv(tty: bool, database: Option<&str>) -> Vec<String> {
    let mut argv: Vec<String> = if tty {
        vec!["psql".to_owned(), "-U".to_owned(), "postgres".to_owned()]
    } else {
        vec![
            "psql".to_owned(),
            "-X".to_owned(),
            "-q".to_owned(),
            "-v".to_owned(),
            "ON_ERROR_STOP=1".to_owned(),
        ]
    };
    if let Some(database) = database {
        argv.push("-d".to_owned());
        argv.push(database.to_owned());
    }
    argv
}

fn refusal_s3(target: &str, shellable: &[String]) -> Failure {
    let next = if shellable.is_empty() {
        "slipfork up".to_owned()
    } else {
        format!("slipfork shell {}", shellable.join(", "))
    };
    Failure::new(
        "shell-refused",
        format!("✗ '{target}' is an s3 bucket, not a shell\n  next: {next}"),
    )
}

fn refusal_unknown(target: &str, shellable: &[String]) -> Failure {
    let next = shellable.first().map_or_else(
        || "slipfork up".to_owned(),
        |id| format!("slipfork shell {id}"),
    );
    Failure::new(
        "shell-refused",
        format!("✗ No shellable target '{target}'\n  next: {next}"),
    )
}

/// The `Forkfile` loaded through the one compile path, when it is present and valid.
pub(super) fn load_document(project: &Path) -> Option<Forkfile> {
    let source = fs::read_to_string(project.join("Forkfile")).ok()?;
    slipfork_forkfile::compile(&source)
        .ok()
        .map(|checked| *checked.document)
}

/// The postgres resource whose `database <id>` child is `target`, when any.
fn database_owner(document: &Forkfile, target: &str) -> Option<String> {
    document
        .resources()
        .into_iter()
        .find(|resource| {
            resource.kind == ResourceDeclKind::Postgres && resource.databases.contains(&target)
        })
        .map(|resource| resource.id.to_owned())
}

pub(super) fn project_param(params: &Value) -> Result<PathBuf, Failure> {
    params
        .get("project")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| Failure::new("bad-params", "`project` is required"))
}

fn target_param(params: &Value) -> Result<&str, Failure> {
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
