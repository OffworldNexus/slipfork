//! Detached daemon start-up: the one path that runs `slipfork __daemon` for a client.

use std::fs::{self, OpenOptions};
use std::process::{Command, Stdio};

use super::client::DaemonError;
use crate::world::HostEnv;

/// Starts this very binary's daemon detached, logging to `<state_dir>/daemon.log`.
///
/// The spawned daemon *is* `host.exe`, so its `ping` identity always matches the client's.
pub(crate) fn spawn_daemon(host: &HostEnv) -> Result<(), DaemonError> {
    fs::create_dir_all(&host.runtime_dir).map_err(|error| {
        DaemonError::new(
            "daemon-unavailable",
            format!("create `{}`: {error}", host.runtime_dir.display()),
        )
    })?;
    fs::create_dir_all(&host.state_dir).map_err(|error| {
        DaemonError::new(
            "daemon-unavailable",
            format!("create `{}`: {error}", host.state_dir.display()),
        )
    })?;
    let log_path = host.state_dir.join("daemon.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| {
            DaemonError::new(
                "daemon-unavailable",
                format!("open `{}`: {error}", log_path.display()),
            )
        })?;
    Command::new(&host.exe)
        .arg("__daemon")
        // A vanished cwd must never outlive the CLI that auto-spawned the daemon.
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .map_err(|error| {
            DaemonError::new(
                "daemon-unavailable",
                format!("start `slipfork __daemon`: {error}"),
            )
        })?;
    Ok(())
}
