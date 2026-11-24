//! Who is answering on `daemon.sock`: a client refuses a daemon that is not this very binary.
//!
//! `make install`, or a package upgrade that leaves the old unit running, can keep
//! `slipfork __daemon` serving old code on the same socket. The `ping` reply therefore carries
//! the daemon's compiled version and canonical binary path, and the client refuses any mismatch
//! with exactly one remedy instead of silently running against stale behaviour.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::client::DaemonError;

/// One `slipfork` binary: its canonical path and the version it was compiled from.
#[derive(Debug, Clone)]
pub(crate) struct DaemonIdentity {
    exe: PathBuf,
    version: &'static str,
}

impl DaemonIdentity {
    /// This process's own identity, from the binary its composition root resolved.
    #[must_use]
    pub(crate) fn new(exe: &Path) -> Self {
        Self {
            exe: canonical_exe(exe),
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    /// The canonical path of this binary, echoed in the daemon's `ping` reply.
    #[must_use]
    pub(crate) fn exe(&self) -> &Path {
        &self.exe
    }

    /// The version this binary was compiled from, echoed in the daemon's `ping` reply.
    #[must_use]
    pub(crate) fn version(&self) -> &'static str {
        self.version
    }

    /// Checks the `ping` reply of the daemon just connected to: same binary, same code.
    ///
    /// # Errors
    ///
    /// Returns the typed `daemon-skew` refusal when the daemon is another binary (or another
    /// build of this one), and its `predates` variant when the reply carries no identity.
    pub(crate) fn verify(&self, reply: &Value, socket: &Path) -> Result<(), DaemonError> {
        let pid = reply_pid(reply);
        let (Some(exe), Some(version)) = (
            reply.get("exe").and_then(Value::as_str),
            reply.get("version").and_then(Value::as_str),
        ) else {
            return Err(self.predates(pid, socket));
        };
        let daemon = canonical_exe(Path::new(exe));
        if daemon == self.exe && version == self.version {
            return Ok(());
        }
        Err(DaemonError::new(
            "daemon-skew",
            format!(
                "the running daemon is `{}` ({version}), this CLI is `{}` ({})\n\
                 next: {}",
                daemon.display(),
                self.exe.display(),
                self.version,
                self.remedy(pid),
            ),
        ))
    }

    /// The refusal for a daemon whose `ping` carries no identity at all.
    #[must_use]
    pub(crate) fn predates(&self, pid: Option<u32>, socket: &Path) -> DaemonError {
        DaemonError::new(
            "daemon-skew",
            format!(
                "the running daemon at `{}` predates this CLI (its `ping` carries no version), \
                 this CLI is `{}` ({})\n\
                 next: {}",
                socket.display(),
                self.exe.display(),
                self.version,
                self.remedy(pid),
            ),
        )
    }

    /// The single remedy line for the daemon that answered: its own pid when `ping` reported
    /// one (the usual stale auto-spawned `__daemon`), else the packaged-unit paths.
    fn remedy(&self, pid: Option<u32>) -> String {
        match pid {
            Some(pid) => format!("kill {pid} (the stale daemon), then run `slipfork up`"),
            None => {
                "run `systemctl --user stop slipfork`, or `pkill -f 'slipfork __daemon'`".to_owned()
            }
        }
    }
}

/// The pid a `ping` reply reports, when it names one that can be signalled: a `0`, a negative,
/// or an out-of-range value must never reach a `kill` line.
fn reply_pid(reply: &Value) -> Option<u32> {
    reply
        .get("pid")
        .and_then(Value::as_u64)
        .filter(|pid| *pid > 0 && *pid <= i32::MAX as u64)
        .map(|pid| pid as u32)
}

/// The canonical path of a binary, so `~/.local/bin/slipfork` and `/usr/bin/slipfork` never
/// alias through a symlink or a relative `$0`.
#[must_use]
pub(crate) fn canonical_exe(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one and only `next: …` line of a refusal, proving no second remedy is embedded.
    fn single_next(error: &DaemonError) -> &str {
        assert_eq!(error.code, "daemon-skew");
        let mut remedies = error
            .message
            .lines()
            .filter(|line| line.starts_with("next: "));
        let next = remedies
            .next()
            .unwrap_or_else(|| panic!("no remedy line: {}", error.message));
        assert!(
            remedies.next().is_none(),
            "more than one remedy line: {}",
            error.message
        );
        next
    }

    #[test]
    fn a_skewed_daemon_names_both_binaries_and_its_pid() {
        let client = DaemonIdentity::new(Path::new("/usr/bin/slipfork"));
        let reply = serde_json::json!({
            "pong": true,
            "pid": 7,
            "version": "0.0.1",
            "exe": "/opt/slipfork/slipfork",
        });
        let error = client
            .verify(&reply, Path::new("/run/user/1000/slipfork/daemon.sock"))
            .expect_err("a different binary must be refused");
        assert!(error.message.contains("`/opt/slipfork/slipfork` (0.0.1)"));
        assert!(error.message.contains("`/usr/bin/slipfork`"));
        assert!(error.message.contains(env!("CARGO_PKG_VERSION")));
        let next = single_next(&error);
        assert!(next.contains("kill 7 (the stale daemon)"), "{next}");
        assert!(next.contains("`slipfork up`"), "{next}");
        assert!(!next.contains("systemctl"), "{next}");
    }

    /// An in-place upgrade keeps the path but swaps the code: the old inode must be refused.
    /// With no `pid` in the reply, the remedy falls back to the packaged-unit paths.
    #[test]
    fn a_rebuilt_binary_at_the_same_path_is_refused() {
        let client = DaemonIdentity::new(Path::new("/usr/bin/slipfork"));
        let reply = serde_json::json!({
            "pong": true,
            "version": "0.0.1",
            "exe": "/usr/bin/slipfork",
        });
        let error = client
            .verify(&reply, Path::new("/run/user/1000/slipfork/daemon.sock"))
            .expect_err("stale code at the same path must be refused");
        assert!(error.message.contains("`/usr/bin/slipfork` (0.0.1)"));
        assert!(error.message.contains("`/usr/bin/slipfork`"));
        let next = single_next(&error);
        assert!(next.contains("systemctl --user stop slipfork"), "{next}");
        assert!(next.contains("pkill -f 'slipfork __daemon'"), "{next}");
        assert!(!next.contains("the stale daemon"), "{next}");
    }

    /// A reply naming a pid that cannot be signalled must never produce a `kill 0` line.
    #[test]
    fn an_unusable_pid_falls_back_to_the_packaged_unit_remedy() {
        let client = DaemonIdentity::new(Path::new("/usr/bin/slipfork"));
        let reply = serde_json::json!({
            "pong": true,
            "pid": 0,
            "version": "0.0.1",
            "exe": "/opt/slipfork/slipfork",
        });
        let error = client
            .verify(&reply, Path::new("/run/user/1000/slipfork/daemon.sock"))
            .expect_err("a different binary must be refused");
        let next = single_next(&error);
        assert!(next.contains("systemctl --user stop slipfork"), "{next}");
        assert!(next.contains("pkill -f 'slipfork __daemon'"), "{next}");
        assert!(!next.contains("kill 0"), "{next}");
        assert!(!next.contains("the stale daemon"), "{next}");
    }

    #[test]
    fn the_legacy_daemon_refusal_says_it_predates_this_cli() {
        let client = DaemonIdentity::new(Path::new("/usr/bin/slipfork"));
        let reply = serde_json::json!({ "pong": true, "pid": 7, "worlds": 0 });
        let error = client
            .verify(&reply, Path::new("/run/user/1000/daemon.sock"))
            .expect_err("a daemon without identity must be refused");
        assert!(error.message.contains("predates this CLI"));
        assert!(error.message.contains("/run/user/1000/daemon.sock"));
        assert!(error.message.contains("`/usr/bin/slipfork`"));
        assert!(error.message.contains(env!("CARGO_PKG_VERSION")));
        let next = single_next(&error);
        assert!(next.contains("kill 7 (the stale daemon)"), "{next}");
        assert!(next.contains("`slipfork up`"), "{next}");
        assert!(!next.contains("systemctl"), "{next}");
    }

    #[test]
    fn this_binarys_own_reply_passes() {
        let client = DaemonIdentity::new(Path::new("/usr/bin/slipfork"));
        let reply = serde_json::json!({
            "pong": true,
            "version": env!("CARGO_PKG_VERSION"),
            "exe": client.exe(),
        });
        assert!(
            client
                .verify(&reply, Path::new("/run/user/1000/daemon.sock"))
                .is_ok()
        );
    }
}
