//! Relays one boot's typed events and publishes the resulting live world.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Child;

use serde_json::{Value, json};

use super::registry::RegistryEntry;
use super::{Daemon, Failure};

fn kill_and_wait(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        slipfork_world::kill_group(pid);
    }
    let _ = child.wait();
}

/// Relays one boot's typed events, then publishes the live entry or forgets the placeholder.
pub(super) fn finish_boot(
    daemon: &Daemon,
    key: String,
    log: PathBuf,
    mut child: Child,
    project: PathBuf,
    emit: &mut dyn FnMut(Value),
) -> Result<Value, Failure> {
    let stdout = child.stdout.take().ok_or_else(|| {
        Failure::new(
            "supervisor-spawn",
            "the world supervisor gave no event stream",
        )
    })?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).map_err(|error| {
            Failure::new(
                "supervisor-read",
                format!("cannot read boot events: {error}"),
            )
        })?;
        if read == 0 {
            kill_and_wait(&mut child);
            daemon.core()?.forget(&key);
            return Err(Failure::new(
                "supervisor-exited",
                format!(
                    "the world supervisor exited before the world was ready (see `{}`)",
                    log.display()
                ),
            ));
        }
        let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        emit(event.clone());
        match event["event"].as_str().unwrap_or_default() {
            "ready" => {
                let pods_dir = event["pods_dir"]
                    .as_str()
                    .map(PathBuf::from)
                    .ok_or_else(|| {
                        Failure::new("world-state", "the world published no pod directory")
                    })?;
                let world = event["world"].as_str().unwrap_or_default().to_owned();
                let entry = RegistryEntry {
                    project,
                    world,
                    fork: None,
                    pods_dir,
                    supervisor_pid: child.id(),
                };
                daemon.core()?.publish(&key, entry, child)?;
                return Ok(json!({
                    "world": event["world"],
                    "url": event["url"],
                }));
            }
            "failed" => {
                kill_and_wait(&mut child);
                daemon.core()?.forget(&key);
                let message = event["message"]
                    .as_str()
                    .unwrap_or("the world failed to boot")
                    .to_owned();
                return Err(Failure::new("boot-failed", message));
            }
            _ => {}
        }
    }
}
