//! The one daemon dialing policy shared by every verb.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use slipfork::daemon_unit_install::{DAEMON_GRACE, DAEMON_POLL};
use slipfork::layout::{Layout, ensure_private_dir};
use slipfork_edge::DaemonControlClient;

use crate::CliApp;

/// Dial the daemon, failing closed with the one line that fixes an absent `slipforkd`.
///
/// Read/diagnostic verbs never spawn a daemon: auto-spawn is reserved for `up`/`fork`, so a
/// `slipfork list` can never leave a mystery `--listen 127.0.0.1:0` process behind.
pub(crate) fn connect(layout: &Layout) -> Result<DaemonControlClient, String> {
    served_client(layout).ok_or_else(|| no_daemon_line(layout))
}

/// The bound control client when a daemon answers, else `None`.
fn served_client(layout: &Layout) -> Option<DaemonControlClient> {
    let client = DaemonControlClient::bind(layout.runtime_dir());
    client.is_served().then_some(client)
}

/// The single one-line refusal read/diagnostic verbs print when nothing answers.
fn no_daemon_line(layout: &Layout) -> String {
    format!(
        "no slipforkd on {} — run `slipfork up`",
        DaemonControlClient::bind(layout.runtime_dir())
            .socket_path()
            .display()
    )
}

/// Dial the daemon, auto-spawning it once when `<state_dir>/run/daemon.sock` does not answer.
///
/// Only `up` and `fork` call this (`ADR-0011` §3); every other verb uses [`connect`].
pub(crate) fn connect_or_spawn(app: &CliApp) -> Result<DaemonControlClient, String> {
    let layout = &app.layout;
    if let Some(client) = served_client(layout) {
        return Ok(client);
    }
    let client = DaemonControlClient::bind(layout.runtime_dir());
    ensure_private_dir(layout.runtime_dir())?;
    let log_path = layout.runtime_dir().join("daemon.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)
        .map_err(|err| format!("cannot open {}: {err}", log_path.display()))?;
    fs::set_permissions(&log_path, std::fs::Permissions::from_mode(0o600))
        .map_err(|err| format!("cannot restrict {} to 0600: {err}", log_path.display()))?;
    let stderr = log
        .try_clone()
        .map_err(|err| format!("cannot duplicate {}: {err}", log_path.display()))?;
    let daemon = app.ui.daemon_executable()?;
    let mut child = Command::new(&daemon)
        .arg("--listen")
        .arg("127.0.0.1:0")
        .arg("--state-dir")
        .arg(layout.state_dir())
        .arg("--runtime-dir")
        .arg(layout.runtime_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .map_err(|err| {
            format!(
                "cannot start {} zero-setup: {err}; start it manually with `slipforkd --listen 127.0.0.1:0 --state-dir {} --runtime-dir {}`",
                daemon.display(),
                layout.state_dir().display(),
                layout.runtime_dir().display()
            )
        })?;

    let deadline = Instant::now() + DAEMON_GRACE;
    loop {
        if client.is_served() {
            return Ok(client);
        }
        if let Ok(Some(status)) = child.try_wait() {
            // A concurrent CLI may have won the socket race while this child exited cleanly.
            if client.is_served() {
                return Ok(client);
            }
            return Err(format!(
                "slipforkd exited with {status} before serving {}; see {}",
                client.socket_path().display(),
                log_path.display()
            ));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "slipforkd did not serve {} within {}s; see {}",
                client.socket_path().display(),
                DAEMON_GRACE.as_secs(),
                log_path.display()
            ));
        }
        std::thread::sleep(DAEMON_POLL);
    }
}

/// Resolve `start` to its repository root through the one Git resolver.
pub(crate) fn repo_root(start: &Path) -> Result<slipfork_vcs::RepoRoot, String> {
    slipfork::project::repository_resolver()
        .resolve(start)
        .map_err(|err| err.to_string())
}
