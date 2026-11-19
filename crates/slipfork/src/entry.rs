//! Native terminal hand-over into a world: the one exec shim behind `exec`/`shell`/`hook`/`db`.
//!
//! The child's working directory, environment, and argv all come from the daemon's typed
//! [`WorldExecTarget`]; this module binds no ambient host state of its own.

use std::process::{Command, Stdio};

use slipfork_wire::control::WorldExecTarget;

/// Enter a world (or a backing-service container) with native terminal stdio.
pub(crate) fn spawn_entry(target: &WorldExecTarget) -> Result<std::process::ExitStatus, String> {
    // `crun exec` takes the command directly after the container id and does not strip a
    // trailing `--` separator, so drop it before appending the interactive argv.
    let mut args_prefix = target.entry.args_prefix.clone();
    if args_prefix.last().map(String::as_str) == Some("--") {
        args_prefix.pop();
    }
    let mut command = Command::new(&target.entry.program);
    command
        .args(&args_prefix)
        .args(&target.target_argv)
        .current_dir(&target.entry.cwd)
        .env_clear()
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    for (name, value) in target.env.iter() {
        command.env(name, value.as_ref());
    }
    command
        .status()
        .map_err(|err| format!("cannot enter world '{}': {err}", target.world))
}
