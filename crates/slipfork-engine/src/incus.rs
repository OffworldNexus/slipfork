//! Incus `/1.0` REST API implementation of [`WorldEngine`] over the Incus Unix domain socket
//! (`R12`, `ADR-0002`).
//!
//! Supports Incus system containers (LXC) and QEMU VMs with ZFS/Btrfs/LVM-thin/XFS CoW, stateful
//! CRIU/QEMU RAM snapshots, and `shift=true` (`MOUNT_ATTR_IDMAP`) shared-UID host mounts. Live
//! daemon coverage is gated on open `H-004` (`M2.21`); this module is exercised by the shared
//! conformance suite against an in-tree Unix-socket protocol harness.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::child_env;
use crate::{
    EngineError, ExecEnv, ExecOutput, MountMode, ServiceHandle, ServiceLogs, ServiceSpec,
    SnapshotId, SnapshotMode, StreamChunk, StreamSource, WorldEngine, WorldEntryCommand,
    WorldHandle, WorldMount, WorldSpec, WorldStatus, target_slug,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Incus hypervisor driver talking directly to the Incus Unix socket (`HTTP/1.1` `/1.0`).
#[derive(Debug, Clone)]
pub struct IncusEngine {
    socket_path: PathBuf,
    project: String,
    source_instance: String,
    host_uid: u32,
    host_gid: u32,
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    worlds: Mutex<BTreeMap<u64, WorldRecord>>,
    next_seq: AtomicU64,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            worlds: Mutex::new(BTreeMap::new()),
            next_seq: AtomicU64::new(0),
        }
    }
}

#[derive(Debug, Clone)]
struct WorldRecord {
    instance: String,
    base_env: ExecEnv,
    status: WorldStatus,
}

/// JSON payload sent to `POST /1.0/instances` for an instant CoW clone with shared-UID mounts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IncusClonePayload {
    pub name: String,
    pub ephemeral: bool,
    pub stateful: bool,
    pub source: IncusCloneSource,
    pub devices: BTreeMap<String, BTreeMap<String, String>>,
}

/// `source` descriptor in `POST /1.0/instances`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IncusCloneSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub instance_only: bool,
    pub source: String,
}

impl IncusEngine {
    /// Construct an `IncusEngine` bound to `socket_path` (for example `/var/lib/incus/unix.socket`).
    pub fn new(
        socket_path: impl AsRef<Path>,
        project: impl Into<String>,
        source_instance: impl Into<String>,
        host_uid: u32,
        host_gid: u32,
    ) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_path_buf(),
            project: project.into(),
            source_instance: source_instance.into(),
            host_uid,
            host_gid,
            shared: Arc::new(Shared::default()),
        }
    }

    /// Build the exact `POST /1.0/instances` CoW copy payload for `spec`'s mounts.
    pub fn build_clone_payload(&self, spec: &WorldSpec, world_name: &str) -> IncusClonePayload {
        let mut devices = BTreeMap::new();
        for (index, mount) in spec.mounts.iter().enumerate() {
            devices.insert(format!("mount-{index}"), incus_disk_device(mount));
        }
        IncusClonePayload {
            name: world_name.to_owned(),
            ephemeral: false,
            stateful: false,
            source: IncusCloneSource {
                source_type: "copy".to_owned(),
                instance_only: true,
                source: format!("{}/{}", self.source_instance, spec.snapshot.0),
            },
            devices,
        }
    }

    /// Perform one synchronous JSON request against the Incus Unix socket REST API.
    pub fn request_json(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), EngineError> {
        let mut stream =
            UnixStream::connect(&self.socket_path).map_err(|e| EngineError::DaemonUnavailable {
                socket: self.socket_path.display().to_string(),
                reason: e.to_string(),
            })?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

        let body_bytes = match body {
            Some(v) => serde_json::to_vec(v).map_err(|e| EngineError::Execution(e.to_string()))?,
            None => Vec::new(),
        };
        let sep = if path.contains('?') { '&' } else { '?' };
        let full_path = format!("{path}{sep}project={}", self.project);
        let req_head = format!(
            "{method} {full_path} HTTP/1.1\r\nHost: incus.unix\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
            body_bytes.len()
        );
        let mut out_frame = req_head.into_bytes();
        out_frame.extend_from_slice(&body_bytes);
        stream
            .write_all(&out_frame)
            .map_err(|e| EngineError::DaemonUnavailable {
                socket: self.socket_path.display().to_string(),
                reason: e.to_string(),
            })?;

        let mut raw_resp = String::new();
        stream
            .read_to_string(&mut raw_resp)
            .map_err(|e| EngineError::Execution(e.to_string()))?;
        let (head, payload) = raw_resp.split_once("\r\n\r\n").ok_or_else(|| {
            EngineError::Execution("malformed HTTP response from incusd".to_owned())
        })?;
        let status_code = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| EngineError::Execution(format!("invalid HTTP status line: {head}")))?;
        let parsed: Value = serde_json::from_str(payload.trim())
            .map_err(|e| EngineError::Execution(format!("invalid Incus JSON: {e}")))?;
        Ok((status_code, parsed))
    }

    fn lock(&self) -> Result<MutexGuard<'_, BTreeMap<u64, WorldRecord>>, EngineError> {
        self.shared
            .worlds
            .lock()
            .map_err(|_| EngineError::Execution("incus world registry lock is poisoned".to_owned()))
    }

    fn owned<'a>(
        &self,
        worlds: &'a mut BTreeMap<u64, WorldRecord>,
        world: &WorldHandle,
    ) -> Result<&'a mut WorldRecord, EngineError> {
        if !world.belongs_to(self.name()) {
            return Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            });
        }
        worlds
            .get_mut(&world.seq())
            .ok_or_else(|| EngineError::WorldGone {
                world: format!("{world:?}"),
            })
    }

    fn instance_snapshot_request(
        &self,
        instance: &str,
        id: &SnapshotId,
        mode: SnapshotMode,
    ) -> Result<(u16, Value), EngineError> {
        let body = json!({
            "name": id.validate()?,
            "stateful": mode == SnapshotMode::StatefulRam,
        });
        self.request_json(
            "POST",
            &format!("/1.0/instances/{instance}/snapshots"),
            Some(&body),
        )
    }
}

impl WorldEngine for IncusEngine {
    fn name(&self) -> &'static str {
        "incus"
    }

    fn create_snapshot(
        &self,
        _source_dir: &Path,
        id: &SnapshotId,
        mode: SnapshotMode,
    ) -> Result<(), EngineError> {
        let (status, resp) = self.instance_snapshot_request(&self.source_instance, id, mode)?;
        if !(200..300).contains(&status) {
            return Err(EngineError::Execution(format!(
                "Incus snapshot creation failed ({status}): {resp}"
            )));
        }
        Ok(())
    }

    fn freeze_world(
        &self,
        world: &WorldHandle,
        id: &SnapshotId,
        mode: SnapshotMode,
    ) -> Result<(), EngineError> {
        // No service supervision is implemented for Incus yet (live adapter gated on `H-004`,
        // `M2.21`), so there is nothing to stop before sealing.
        let instance = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            match record.status {
                WorldStatus::Live => {
                    record.status = WorldStatus::Freezing;
                    record.instance.clone()
                }
                WorldStatus::Freezing | WorldStatus::Frozen | WorldStatus::Destroyed => {
                    return Err(EngineError::WorldNotFreezable {
                        world: format!("{world:?}"),
                    });
                }
            }
        };

        let sealed = self
            .instance_snapshot_request(&instance, id, mode)
            .and_then(|(status, resp)| {
                if (200..300).contains(&status) {
                    Ok(())
                } else {
                    Err(EngineError::Execution(format!(
                        "Incus world freeze failed ({status}): {resp}"
                    )))
                }
            });
        let mut worlds = self.lock()?;
        let record = self.owned(&mut worlds, world)?;
        match sealed {
            Ok(()) if record.status == WorldStatus::Freezing => {
                record.status = WorldStatus::Frozen;
                Ok(())
            }
            Ok(()) => Err(EngineError::WorldNotFreezable {
                world: format!("{world:?}"),
            }),
            Err(err) => {
                if record.status == WorldStatus::Freezing {
                    record.status = WorldStatus::Live;
                }
                Err(err)
            }
        }
    }

    fn clone_world(&self, spec: &WorldSpec) -> Result<WorldHandle, EngineError> {
        spec.validate()?;
        spec.snapshot.validate()?;
        let seq = self.shared.next_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let world = WorldHandle::mint(self.name(), seq);
        let instance = format!("{}-{seq}", target_slug(&spec.target));
        let payload = self.build_clone_payload(spec, &instance);
        let body =
            serde_json::to_value(&payload).map_err(|e| EngineError::Execution(e.to_string()))?;
        let (status, resp) = self.request_json("POST", "/1.0/instances", Some(&body))?;
        if status == 404 {
            return Err(EngineError::SnapshotNotFound(spec.snapshot.0.clone()));
        }
        if !(200..300).contains(&status) {
            return Err(EngineError::Execution(format!(
                "Incus CoW clone failed ({status}): {resp}"
            )));
        }
        self.lock()?.insert(
            seq,
            WorldRecord {
                instance,
                base_env: spec.base_env.clone(),
                status: WorldStatus::Live,
            },
        );
        Ok(world)
    }

    fn exec_in_world(
        &self,
        world: &WorldHandle,
        argv: &[String],
        cwd: &Path,
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        if argv.is_empty() {
            return Err(EngineError::Execution(
                "exec argv must not be empty".to_owned(),
            ));
        }
        let (instance, env) = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            if record.status == WorldStatus::Destroyed {
                return Err(EngineError::WorldGone {
                    world: format!("{world:?}"),
                });
            }
            (
                record.instance.clone(),
                child_env::materialize(&record.base_env, overlay_env, |var| {
                    std::env::var(var.name()).ok()
                }),
            )
        };
        let body = json!({
            "command": argv,
            "cwd": cwd.display().to_string(),
            "environment": env,
            "user": self.host_uid,
            "group": self.host_gid,
            "wait-for-websocket": false,
            "record-output": true,
        });
        let (status, resp) = self.request_json(
            "POST",
            &format!("/1.0/instances/{instance}/exec"),
            Some(&body),
        )?;
        if !(200..300).contains(&status) {
            return Err(EngineError::Execution(format!(
                "Incus exec failed ({status}): {resp}"
            )));
        }
        let exit_code = resp
            .pointer("/metadata/metadata/return")
            .and_then(Value::as_i64)
            .unwrap_or(0) as i32;
        let stdout = resp
            .pointer("/metadata/metadata/stdout")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let stderr = resp
            .pointer("/metadata/metadata/stderr")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if !stdout.is_empty() {
            on_output(StreamChunk {
                source: StreamSource::Stdout,
                bytes: stdout.clone().into_bytes(),
            });
        }
        if !stderr.is_empty() {
            on_output(StreamChunk {
                source: StreamSource::Stderr,
                bytes: stderr.clone().into_bytes(),
            });
        }
        Ok(ExecOutput {
            exit_code,
            stdout,
            stderr,
        })
    }

    fn spawn_service(
        &self,
        _world: &WorldHandle,
        _spec: &ServiceSpec,
    ) -> Result<ServiceHandle, EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "world service supervision (live Incus integration lands in M2.21)",
        })
    }

    fn stop_service(&self, _service: &ServiceHandle) -> Result<(), EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "world service supervision (live Incus integration lands in M2.21)",
        })
    }

    fn service_logs(&self, _world: &WorldHandle, _id: &str) -> Result<ServiceLogs, EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "world service log capture (live Incus integration lands in M2.21)",
        })
    }

    fn exec_in_service(
        &self,
        _service: &ServiceHandle,
        _argv: &[String],
        _overlay_env: &ExecEnv,
        _on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        // Incus service supervision (including `crun exec`) stays `Unsupported` until the live
        // adapter lands in `M2.21`; there is no app-rootfs fallback client (`ADR-0009` §2–§3).
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "exec inside a backing-service container (live Incus integration lands in M2.21)",
        })
    }

    fn service_entry_command(
        &self,
        _service: &ServiceHandle,
    ) -> Result<WorldEntryCommand, EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "service container entry (live Incus integration lands in M2.21)",
        })
    }

    fn entry_command(
        &self,
        world: &WorldHandle,
        cwd: &Path,
    ) -> Result<WorldEntryCommand, EngineError> {
        let instance = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            if record.status == WorldStatus::Destroyed {
                return Err(EngineError::WorldGone {
                    world: format!("{world:?}"),
                });
            }
            record.instance.clone()
        };
        Ok(WorldEntryCommand {
            program: PathBuf::from("incus"),
            args_prefix: vec![
                "exec".to_owned(),
                instance,
                "--cwd".to_owned(),
                cwd.display().to_string(),
                "--".to_owned(),
            ],
            cwd: cwd.to_path_buf(),
        })
    }

    fn dial_world_tcp(&self, _world: &WorldHandle, _port: u16) -> Result<TcpStream, EngineError> {
        Err(EngineError::Unsupported {
            engine: self.name(),
            capability: "dial_world_tcp over an Incus instance (live Incus integration lands in M2.21)",
        })
    }

    fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError> {
        let instance = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, &world)?;
            if record.status == WorldStatus::Destroyed {
                return Err(EngineError::WorldGone {
                    world: format!("{world:?}"),
                });
            }
            record.status = WorldStatus::Destroyed;
            record.instance.clone()
        };
        let (status, resp) =
            self.request_json("DELETE", &format!("/1.0/instances/{instance}"), None)?;
        if !(200..300).contains(&status) && status != 404 {
            return Err(EngineError::Execution(format!(
                "Incus delete failed ({status}): {resp}"
            )));
        }
        // The instance is gone, so the record goes too: a long-lived engine must not accumulate one
        // entry per destroyed world, and the old handle now resolves to `WorldGone`.
        self.lock()?.remove(&world.seq());
        Ok(())
    }
}

fn incus_disk_device(mount: &WorldMount) -> BTreeMap<String, String> {
    let mut device = BTreeMap::from([
        ("type".to_owned(), "disk".to_owned()),
        ("source".to_owned(), mount.host_path.display().to_string()),
        ("path".to_owned(), mount.guest_path.display().to_string()),
        ("shift".to_owned(), "true".to_owned()),
    ]);
    if mount.mode == MountMode::ReadOnly {
        device.insert("readonly".to_owned(), "true".to_owned());
    }
    device
}
