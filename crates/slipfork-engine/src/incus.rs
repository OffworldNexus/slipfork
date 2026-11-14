//! Incus `/1.0` REST API implementation of [`WorldEngine`] over Unix domain socket (`R12`, `ADR-0002`).
//!
//! Supports both Incus System Containers (LXC) and Incus QEMU VMs with ZFS/Btrfs/LVM-thin/XFS CoW,
//! stateful CRIU/QEMU RAM snapshots, and `shift=true` (`MOUNT_ATTR_IDMAP`) shared-UID host mounts.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    EngineCapabilities, EngineError, ExecOutput, ExecSpec, HostBindMount, SnapshotId, WorldEngine,
    WorldHandle,
};

/// Incus hypervisor driver talking directly to `/var/lib/incus/unix.socket` (`HTTP/1.1` `/1.0`).
#[derive(Debug, Clone)]
pub struct IncusEngine {
    socket_path: PathBuf,
    project: String,
    base_instance: String,
    host_uid: u32,
    host_gid: u32,
}

/// JSON payload sent to `POST /1.0/instances` for an instant CoW clone + shared-UID workspace mount.
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
        base_instance: impl Into<String>,
        host_uid: u32,
        host_gid: u32,
    ) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_path_buf(),
            project: project.into(),
            base_instance: base_instance.into(),
            host_uid,
            host_gid,
        }
    }

    /// Build the exact `POST /1.0/instances` CoW copy payload with `shift=true` host UID mount.
    pub fn build_clone_payload(
        &self,
        snapshot: &SnapshotId,
        world_name: &str,
        bind: &HostBindMount,
        stateful: bool,
    ) -> IncusClonePayload {
        let host_dir = bind.host_path.display().to_string();
        let workspace_dev = BTreeMap::from([
            ("type".to_owned(), "disk".to_owned()),
            ("source".to_owned(), host_dir.clone()),
            ("path".to_owned(), host_dir),
            ("shift".to_owned(), "true".to_owned()),
        ]);
        IncusClonePayload {
            name: world_name.to_owned(),
            ephemeral: false,
            stateful,
            source: IncusCloneSource {
                source_type: "copy".to_owned(),
                instance_only: true,
                source: format!("{}/{}", self.base_instance, snapshot.0),
            },
            devices: BTreeMap::from([("workspace".to_owned(), workspace_dev)]),
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
}

impl WorldEngine for IncusEngine {
    fn name(&self) -> &'static str {
        "incus"
    }

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            disk_cow: true,
            stateful_ram_snapshot: true,
            isolated_netns: true,
            uid_passthrough: true,
        }
    }

    fn create_snapshot(
        &self,
        _source_state_dir: &Path,
        label: &str,
        stateful_ram: bool,
    ) -> Result<SnapshotId, EngineError> {
        let body = json!({
            "name": label,
            "stateful": stateful_ram,
        });
        let (status, resp) = self.request_json(
            "POST",
            &format!("/1.0/instances/{}/snapshots", self.base_instance),
            Some(&body),
        )?;
        if !(200..300).contains(&status) {
            return Err(EngineError::Execution(format!(
                "Incus snapshot creation failed ({status}): {resp}"
            )));
        }
        Ok(SnapshotId(label.to_owned()))
    }

    fn clone_world(
        &self,
        snapshot: &SnapshotId,
        world_name: &str,
        bind: &HostBindMount,
    ) -> Result<WorldHandle, EngineError> {
        let payload = self.build_clone_payload(snapshot, world_name, bind, false);
        let val =
            serde_json::to_value(&payload).map_err(|e| EngineError::Execution(e.to_string()))?;
        let (status, resp) = self.request_json("POST", "/1.0/instances", Some(&val))?;
        if status == 404 {
            return Err(EngineError::SnapshotNotFound(snapshot.0.clone()));
        }
        if !(200..300).contains(&status) {
            return Err(EngineError::Execution(format!(
                "Incus CoW clone failed ({status}): {resp}"
            )));
        }
        Ok(WorldHandle {
            id: world_name.to_owned(),
            state_dir: PathBuf::from(format!("incus:{}/{}", self.project, world_name)),
            workspace_dir: bind.host_path.clone(),
        })
    }

    fn exec_in_world(
        &self,
        world: &WorldHandle,
        spec: &ExecSpec,
    ) -> Result<ExecOutput, EngineError> {
        let body = json!({
            "command": spec.argv,
            "cwd": spec.cwd.display().to_string(),
            "environment": spec.env,
            "user": self.host_uid,
            "group": self.host_gid,
            "wait-for-websocket": false,
            "record-output": true,
        });
        let (status, resp) = self.request_json(
            "POST",
            &format!("/1.0/instances/{}/exec", world.id),
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
        Ok(ExecOutput {
            exit_code,
            stdout,
            stderr,
        })
    }

    fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError> {
        let (status, resp) =
            self.request_json("DELETE", &format!("/1.0/instances/{}", world.id), None)?;
        if !(200..300).contains(&status) && status != 404 {
            return Err(EngineError::Execution(format!(
                "Incus delete failed ({status}): {resp}"
            )));
        }
        Ok(())
    }
}
