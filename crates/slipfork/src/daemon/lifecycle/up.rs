//! Spawning `__supervise` and reserving the booting placeholder.

use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

use super::core::{Core, World};
use crate::daemon::Failure;
use crate::daemon::boot::BootSession;
use crate::daemon::rpc::{key_of, pid_alive, world_answers};

impl Core {
    /// Spawns `__supervise` and leaves a `Booting` placeholder; the returned session owns the boot.
    ///
    /// The caller holds one `Core` guard for the reservation, so a fork snapshot and its boot can
    /// never observe two different world maps.
    pub(crate) fn begin_up(&mut self, params: &Value) -> Result<BootSession, Failure> {
        let project = PathBuf::from(
            params
                .get("project")
                .and_then(Value::as_str)
                .ok_or_else(|| Failure::new("bad-params", "`project` is required"))?,
        );
        let key = key_of(&project);
        match self.worlds.get(&key) {
            Some(World::Booting { world, .. }) => {
                return Err(Failure::new(
                    "world-booting",
                    format!("world `{world}` is still starting"),
                ));
            }
            Some(World::Live { entry, .. })
                if pid_alive(entry.supervisor_pid) && world_answers(&entry.pods_dir) =>
            {
                return Err(Failure::new(
                    "world-running",
                    format!("world `{}` is already running", entry.world),
                ));
            }
            _ => {}
        }
        self.worlds.remove(&key);
        self.spawn(project, key, params)
    }

    fn spawn(
        &mut self,
        project: PathBuf,
        key: String,
        params: &Value,
    ) -> Result<BootSession, Failure> {
        let requested = params.get("world").and_then(Value::as_str);
        let world = crate::world::world_name(&project, requested)
            .map_err(|message| Failure::new("invalid-world", message))?;
        let log_dir = project.join(".slipfork");
        fs::create_dir_all(&log_dir).map_err(|error| {
            Failure::new(
                "log-dir",
                format!("create `{}`: {error}", log_dir.display()),
            )
        })?;
        let log_path = log_dir.join("supervisor.log");
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|error| {
                Failure::new(
                    "log-open",
                    format!("open `{}`: {error}", log_path.display()),
                )
            })?;
        let child = Command::new(&self.host.exe)
            .arg("__supervise")
            .arg("--project")
            .arg(&project)
            .arg("--world")
            .arg(world.as_str())
            .arg("--json")
            .current_dir(&project)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|error| {
                Failure::new(
                    "supervisor-spawn",
                    format!("cannot start `__supervise`: {error}"),
                )
            })?;
        self.worlds.insert(
            key.clone(),
            World::Booting {
                project: project.clone(),
                world: world.as_str().to_owned(),
                supervisor_pid: child.id(),
            },
        );
        Ok(BootSession::new(key, log_path, child, project))
    }
}
