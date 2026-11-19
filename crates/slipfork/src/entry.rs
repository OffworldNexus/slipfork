//! Native terminal hand-over into a world: the one exec shim behind `exec`/`shell`/`hook`/`db`.
//!
//! The child's working directory, environment, and argv all come from the daemon's typed
//! [`WorldExecTarget`]; this module binds no ambient host state of its own.

use std::process::{Command, Stdio};

use slipfork_wire::control::WorldExecTarget;

/// Enter a world (or a backing-service container) with native terminal stdio.
pub(crate) fn spawn_entry(target: &WorldExecTarget) -> Result<std::process::ExitStatus, String> {
    // `crun exec` does not strip a trailing `--`, so drop it only when the caller argv follows.
    let mut args_prefix = target.entry.args_prefix.clone();
    if !target.entry.consumes_target_argv && args_prefix.last().map(String::as_str) == Some("--") {
        args_prefix.pop();
    }
    let mut command = Command::new(&target.entry.program);
    command
        .args(&args_prefix)
        .current_dir(&target.entry.cwd)
        .env_clear()
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // The engine owns whether its prefix consumes the caller argv: an interactive relay bakes
    // its single program in and reports `consumes_target_argv`; a plain bridge takes the argv.
    if !target.entry.consumes_target_argv {
        command.args(&target.target_argv);
    }
    for (name, value) in target.env.iter() {
        command.env(name, value.as_ref());
    }
    command
        .status()
        .map_err(|err| format!("cannot enter world '{}': {err}", target.world))
}
