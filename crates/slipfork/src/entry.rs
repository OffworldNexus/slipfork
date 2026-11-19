//! Native terminal hand-over into a running service container: the one exec shim behind
//! `exec`/`shell`/`hook`/`db`.
//!
//! The environment and argv all come from the daemon's typed [`WorldExecTarget`]; this module
//! binds no ambient host state of its own.

use std::process::{Command, Stdio};

use slipfork_wire::control::WorldExecTarget;

/// Enter a service container with native terminal stdio.
pub(crate) fn spawn_entry(target: &WorldExecTarget) -> Result<std::process::ExitStatus, String> {
    let mut command = Command::new(&target.entry.program);
    command
        .args(&target.entry.args_prefix)
        .current_dir("/")
        .env_clear()
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .args(&target.target_argv);
    for (name, value) in target.env.iter() {
        command.env(name, value.as_ref());
    }
    command
        .status()
        .map_err(|err| format!("cannot enter world '{}': {err}", target.world))
}
