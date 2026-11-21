//! Spawning `__supervise` and leaving the booting placeholder.

use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

use super::core::{Core, World};
use crate::daemon::Failure;
use crate::daemon::rpc::{key_of, pid_alive, world_answers};

impl Core {
    /// Spawns `__supervise` and leaves a `Booting` placeholder; the guard is dropped by the caller.
    pub(crate) fn begin_up(&mut self, params: &Value) -> Result<(String, Child, PathBuf), Failure> {
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
    ) -> Result<(String, Child, PathBuf), Failure> {
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
        let mut child = Command::new(&self.host.exe)
            .arg("__supervise")
            .arg("--project")
            .arg(&project)
            .arg("--world")
            .arg(world.as_str())
            .arg("--json")
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
        child.stdout.take().ok_or_else(|| {
            Failure::new(
                "supervisor-spawn",
                "the world supervisor gave no event stream",
            )
        })?;
        self.worlds.insert(
            key.clone(),
            World::Booting {
                project,
                world: world.as_str().to_owned(),
                supervisor_pid: child.id(),
            },
        );
        Ok((key, child, log_path))
    }
}
