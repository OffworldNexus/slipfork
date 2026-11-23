//! Real-PTY world links: `shell` argv plans and the `ExecPty` descriptor handshake.
//!
//! Both `shell` and `exec` end at the same [`Core::connect_pty`]: one `ExecPty` request on the
//! world's `control.sock`, one `SCM_RIGHTS` master back. `exec` reuses [`Core::resolve_exec`], so
//! target validation is identical on the PTY and pipe paths.

use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;

use serde_json::Value;
use slipfork_forkfile::{Forkfile, ResourceDeclKind};
use slipfork_world::control::{PROTOCOL_VERSION, Request, control_socket_path, encode};

use super::super::Failure;
use super::core::Core;
use super::stream::{ExecTarget, SHELLABLE, project_param, target_param, world_rows};
use crate::ps::PsKind;

/// A connected world PTY: the control socket that carries resize/final frames and the master fd.
pub(crate) struct PtyLink {
    /// The world's `control.sock`, used for `ExecResize` frames and the final reply.
    pub(crate) socket: UnixStream,
    /// The `/dev/pts/N` master handed back by the world init.
    pub(crate) master: OwnedFd,
}

/// The resolved command one shell target maps onto.
pub(super) struct ShellPlan {
    pub(super) pod: String,
    pub(super) argv: Vec<String>,
}

impl Core {
    /// Opens one real PTY inside a resolved `shell` target.
    pub(crate) fn open_pty(&mut self, params: &Value) -> Result<PtyLink, Failure> {
        let project = project_param(params)?;
        let target = target_param(params)?;
        let (cols, rows) = geometry(params);
        let plan = self.shell_plan(&project, target, true)?;
        self.connect_pty(&project, plan.pod, plan.argv, cols, rows)
    }

    /// Opens one real PTY for a validated `exec` target; never builds a shell argv.
    pub(crate) fn open_exec_pty(&mut self, params: &Value) -> Result<PtyLink, Failure> {
        let ExecTarget { project, pod, argv } = self.resolve_exec(params)?;
        let (cols, rows) = geometry(params);
        self.connect_pty(&project, pod, argv, cols, rows)
    }

    /// The one descriptor handshake: `ExecPty` on `control.sock`, then `recv_fd` for the master.
    fn connect_pty(
        &mut self,
        project: &Path,
        pod: String,
        argv: Vec<String>,
        cols: u16,
        rows: u16,
    ) -> Result<PtyLink, Failure> {
        let pods_dir = self.live_pods_dir(project)?;
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
            pod,
            argv,
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
    pub(super) fn shell_plan(
        &mut self,
        project: &Path,
        target: &str,
        tty: bool,
    ) -> Result<ShellPlan, Failure> {
        let reply = self.ps()?;
        let worlds: Vec<_> = world_rows(&reply, project).collect();
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
        let document = super::stream::load_document(project);
        let features = match document.as_ref() {
            Some(document) => Some(
                crate::world::features::WorldFeatures::load_resolved(project, document)
                    .map_err(|error| Failure::new("feature", error))?,
            ),
            None => None,
        };
        // A declared `database <db>` resolves to its owning postgres resource pod.
        if let Some((document, features)) = document.as_ref().zip(features.as_ref())
            && let Some(owner) = database_owner(document, features, target)
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
}

fn geometry(params: &Value) -> (u16, u16) {
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
    (cols, rows)
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

/// The postgres resource whose `database <id>` child is `target`, when any.
fn database_owner(
    document: &Forkfile,
    features: &slipfork_forkfile::FeatureSet,
    target: &str,
) -> Option<String> {
    document
        .resources_with(features)
        .into_iter()
        .find(|resource| {
            resource.kind == ResourceDeclKind::Postgres && resource.databases.contains(&target)
        })
        .map(|resource| resource.id.to_owned())
}
