//! Rootless Linux `FICLONE` + unprivileged namespace (`CLONE_NEWUSER | CLONE_NEWNET | CLONE_NEWPID`)
//! implementation of [`WorldEngine`] (`R10`, `R11`, `ADR-0002`).

#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::linux_cow::reflink_tree;
use crate::{
    EngineCapabilities, EngineError, ExecOutput, ExecSpec, HostBindMount, SnapshotId, WorldEngine,
    WorldHandle,
};

/// Unprivileged Linux CoW world engine backed by `ioctl(FICLONE)` + isolated `netns`/`pidns`.
#[derive(Debug, Clone)]
pub struct ReflinkNetnsEngine {
    state_root: PathBuf,
    host_uid: u32,
    host_gid: u32,
}

impl ReflinkNetnsEngine {
    /// Construct a `ReflinkNetnsEngine` storing snapshots and cloned state under `state_root`
    /// (which MUST reside on a reflink-capable filesystem such as XFS `reflink=1` or Btrfs).
    pub fn new(state_root: impl AsRef<Path>, host_uid: u32, host_gid: u32) -> Self {
        Self {
            state_root: state_root.as_ref().to_path_buf(),
            host_uid,
            host_gid,
        }
    }

    fn snapshot_dir(&self, label: &str) -> PathBuf {
        self.state_root.join("snapshots").join(label)
    }

    fn world_dir(&self, world_name: &str) -> PathBuf {
        self.state_root.join("worlds").join(world_name)
    }
}

impl WorldEngine for ReflinkNetnsEngine {
    fn name(&self) -> &'static str {
        "reflink-netns"
    }

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            disk_cow: true,
            stateful_ram_snapshot: false,
            isolated_netns: true,
            uid_passthrough: true,
        }
    }

    fn create_snapshot(
        &self,
        source_state_dir: &Path,
        label: &str,
        stateful_ram: bool,
    ) -> Result<SnapshotId, EngineError> {
        if stateful_ram {
            return Err(EngineError::StatefulRamUnsupported {
                engine: self.name(),
            });
        }
        let dst = self.snapshot_dir(label);
        if dst.exists() {
            fs::remove_dir_all(&dst).map_err(|e| EngineError::Io {
                path: dst.display().to_string(),
                message: e.to_string(),
            })?;
        }
        reflink_tree(source_state_dir, &dst)?;
        Ok(SnapshotId(label.to_owned()))
    }

    fn clone_world(
        &self,
        snapshot: &SnapshotId,
        world_name: &str,
        bind: &HostBindMount,
    ) -> Result<WorldHandle, EngineError> {
        let src = self.snapshot_dir(&snapshot.0);
        if !src.exists() {
            return Err(EngineError::SnapshotNotFound(snapshot.0.clone()));
        }
        let world_root = self.world_dir(world_name);
        if world_root.exists() {
            fs::remove_dir_all(&world_root).map_err(|e| EngineError::Io {
                path: world_root.display().to_string(),
                message: e.to_string(),
            })?;
        }
        let state_dir = world_root.join("state");
        reflink_tree(&src, &state_dir)?;
        Ok(WorldHandle {
            id: world_name.to_owned(),
            state_dir,
            workspace_dir: bind.host_path.clone(),
        })
    }

    fn exec_in_world(
        &self,
        world: &WorldHandle,
        spec: &ExecSpec,
    ) -> Result<ExecOutput, EngineError> {
        if spec.argv.is_empty() {
            return Err(EngineError::Execution(
                "ExecSpec.argv must not be empty".to_owned(),
            ));
        }
        // Identity UID/GID mapping (`$UID -> $UID`) + `--keep-caps` preserves exact `$UID:$GID`
        // ownership on every file written to the shared workspace AND allows `ip link set lo up`
        // inside the private network namespace before invoking the target command via `"$@"`.
        let mut cmd = Command::new("unshare");
        cmd.arg(format!("--map-user={}", self.host_uid))
            .arg(format!("--map-group={}", self.host_gid))
            .arg("--keep-caps")
            .arg("--net")
            .arg("--pid")
            .arg("--fork")
            .arg("sh")
            .arg("-c")
            .arg("ip link set lo up && exec \"$@\"")
            .arg("slipfork-world-init")
            .args(&spec.argv)
            .current_dir(&spec.cwd)
            .env("SLIPFORK_WORLD_ID", &world.id)
            .env("SLIPFORK_WORLD_STATE", &world.state_dir)
            .envs(&spec.env);

        let out = cmd
            .output()
            .map_err(|e| EngineError::Execution(e.to_string()))?;
        Ok(ExecOutput {
            exit_code: out.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError> {
        let world_root = self.world_dir(&world.id);
        if world_root.exists() {
            fs::remove_dir_all(&world_root).map_err(|e| EngineError::Io {
                path: world_root.display().to_string(),
                message: e.to_string(),
            })?;
        }
        Ok(())
    }
}
