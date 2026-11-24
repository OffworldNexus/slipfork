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
        let (Some(exe), Some(version)) = (
            reply.get("exe").and_then(Value::as_str),
            reply.get("version").and_then(Value::as_str),
        ) else {
            return Err(self.predates(socket));
        };
        let daemon = canonical_exe(Path::new(exe));
        if daemon == self.exe && version == self.version {
            return Ok(());
        }
        Err(DaemonError::new(
            "daemon-skew",
            format!(
                "the running daemon is `{}` ({version}), this CLI is `{}` ({})\n\
                 next: run `systemctl --user stop slipfork` to let this binary start its own daemon, \
                 or run `{}`",
                daemon.display(),
                self.exe.display(),
                self.version,
                daemon.display(),
            ),
        ))
    }

    /// The refusal for a daemon whose `ping` carries no identity at all.
    #[must_use]
    pub(crate) fn predates(&self, socket: &Path) -> DaemonError {
        DaemonError::new(
            "daemon-skew",
            format!(
                "the running daemon at `{}` predates this CLI (its `ping` carries no version), \
                 this CLI is `{}` ({})\n\
                 next: run `systemctl --user stop slipfork` so this binary starts its own daemon, \
                 or reinstall the package",
                socket.display(),
                self.exe.display(),
                self.version,
            ),
        )
    }
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

    /// Every refusal carries exactly one `next:` and names both sides, whichever it is.
    fn assert_one_remedy(error: &DaemonError, daemon: &str, client: &str) {
        assert_eq!(error.code, "daemon-skew");
        assert_eq!(
            error
                .message
                .lines()
                .filter(|line| line.starts_with("next: "))
                .count(),
            1
        );
        assert!(error.message.contains(daemon), "{}", error.message);
        assert!(error.message.contains(client), "{}", error.message);
        assert!(
            error.message.contains("systemctl --user stop slipfork"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_skewed_daemon_names_both_binaries_and_their_versions() {
        let client = DaemonIdentity::new(Path::new("/usr/bin/slipfork"));
        let reply = serde_json::json!({
            "pong": true,
            "version": "0.0.1",
            "exe": "/opt/slipfork/slipfork",
        });
        let error = client
            .verify(&reply, Path::new("/run/user/1000/slipfork/daemon.sock"))
            .expect_err("a different binary must be refused");
        assert_one_remedy(
            &error,
            "`/opt/slipfork/slipfork` (0.0.1)",
            "`/usr/bin/slipfork`",
        );
        assert!(error.message.contains(env!("CARGO_PKG_VERSION")));
    }

    /// An in-place upgrade keeps the path but swaps the code: the old inode must be refused.
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
        assert_one_remedy(&error, "`/usr/bin/slipfork` (0.0.1)", "`/usr/bin/slipfork`");
    }

    #[test]
    fn the_legacy_daemon_refusal_says_it_predates_this_cli() {
        let client = DaemonIdentity::new(Path::new("/usr/bin/slipfork"));
        let reply = serde_json::json!({ "pong": true, "pid": 7, "worlds": 0 });
        let error = client
            .verify(&reply, Path::new("/run/user/1000/daemon.sock"))
            .expect_err("a daemon without identity must be refused");
        assert!(error.message.contains("predates this CLI"));
        assert_one_remedy(&error, "/run/user/1000/daemon.sock", "`/usr/bin/slipfork`");
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
