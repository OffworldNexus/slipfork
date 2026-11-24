//! Who is answering on `daemon.sock`: a client refuses a daemon that is not this very build.
//!
//! `make install`, or a package upgrade that leaves the old unit running, can keep
//! `slipfork __daemon` serving old code on the same socket. The `ping` reply therefore carries
//! the daemon's compiled version, canonical binary path, and a cheap build stamp (mtime + size
//! read from one `metadata` call), so a rebuild at the same path invalidates the running daemon
//! instead of silently serving the old behaviour.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

use super::client::DaemonError;

/// One binary build, identified by its mtime and byte length from a single `metadata` call.
///
/// No hashing: a rebuild rewrites the file (or bumps its mtime), which is enough to tell the
/// running daemon apart from the binary now installed at the very same path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BuildStamp {
    mtime_ns: u64,
    len: u64,
}

impl BuildStamp {
    /// Stats `path` once; `None` when it cannot be read, so identity construction never fails.
    fn read(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        let modified = metadata.modified().ok()?;
        let elapsed = modified.duration_since(UNIX_EPOCH).ok()?;
        Some(Self {
            mtime_ns: u64::try_from(elapsed.as_nanos()).ok()?,
            len: metadata.len(),
        })
    }

    /// The stamp a `ping` reply reports, when it carries one.
    fn from_reply(reply: &Value) -> Option<Self> {
        let build = reply.get("build")?;
        Some(Self {
            mtime_ns: build.get("mtime_ns")?.as_u64()?,
            len: build.get("len")?.as_u64()?,
        })
    }

    fn to_json(self) -> Value {
        json!({ "mtime_ns": self.mtime_ns, "len": self.len })
    }
}

/// One `slipfork` binary: its canonical path, compiled version, and the build at that path.
#[derive(Debug, Clone)]
pub(crate) struct DaemonIdentity {
    exe: PathBuf,
    version: &'static str,
    /// Stat'ed once per identity — the daemon at start-up, the client once per handshake — so a
    /// rebuild between spawn and handshake can never masquerade as a matching identity.
    stamp: Option<BuildStamp>,
}

impl DaemonIdentity {
    /// This process's own identity, from the binary its composition root resolved.
    ///
    /// A binary that cannot be stat'ed still yields a usable identity; the stamp is simply absent.
    #[must_use]
    pub(crate) fn new(exe: &Path) -> Self {
        let exe = canonical_exe(exe);
        Self {
            stamp: BuildStamp::read(&exe),
            exe,
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

    /// The build stamp as the daemon's `ping` reply carries it; `null` when this exe is unreadable.
    #[must_use]
    pub(crate) fn build(&self) -> Value {
        self.stamp.map_or(Value::Null, BuildStamp::to_json)
    }

    /// The identity fields of the daemon's `ping` reply: path, version, and build stamp.
    ///
    /// One owner for the reply's identity shape, so the daemon arm never restates it.
    #[must_use]
    pub(crate) fn ping_fields(&self) -> Value {
        json!({
            "version": self.version(),
            "exe": self.exe().display().to_string(),
            "build": self.build(),
        })
    }

    /// Checks the `ping` reply of the daemon just connected to: same path, version, and stamp.
    ///
    /// # Errors
    ///
    /// Returns the typed `daemon-skew` refusal when the daemon is another binary, another
    /// version, an older build at this same path, or reports no build stamp at all; and the
    /// `predates` variant when the reply carries no identity.
    pub(crate) fn verify(&self, reply: &Value, socket: &Path) -> Result<(), DaemonError> {
        let pid = reply_pid(reply);
        let (Some(exe), Some(version)) = (
            reply.get("exe").and_then(Value::as_str),
            reply.get("version").and_then(Value::as_str),
        ) else {
            return Err(self.predates(pid, socket));
        };
        let daemon = canonical_exe(Path::new(exe));
        if daemon != self.exe || version != self.version {
            return Err(self.skew(&daemon, version, pid, Skew::Other));
        }
        match (self.stamp, BuildStamp::from_reply(reply)) {
            (Some(mine), Some(theirs)) if mine == theirs => Ok(()),
            (Some(_), Some(_)) => Err(self.skew(&daemon, version, pid, Skew::Rebuilt)),
            (Some(_), None) => Err(self.skew(&daemon, version, pid, Skew::NoStamp)),
            // Fail closed: an unverifiable daemon is refused like any other stale build.
            (None, _) => Err(self.skew(&daemon, version, pid, Skew::Unverifiable)),
        }
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

    /// One typed refusal narrowing why a daemon of this path and version was still refused.
    fn skew(&self, daemon: &Path, version: &str, pid: Option<u32>, kind: Skew) -> DaemonError {
        let detail = match kind {
            Skew::Other => "",
            Skew::Rebuilt => {
                " — a stale build: the binary at this path was rebuilt after that daemon started"
            }
            Skew::NoStamp => " — a stale build: its `ping` carries no build stamp",
            Skew::Unverifiable => {
                " — this CLI's own binary could not be stat'ed, so the running daemon cannot be verified"
            }
        };
        DaemonError::new(
            "daemon-skew",
            format!(
                "the running daemon is `{}` ({version}){detail}; this CLI is `{}` ({})\n\
                 next: {}",
                daemon.display(),
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

/// Why a `ping` reply from the same path and version was still refused.
#[derive(Debug, Clone, Copy)]
enum Skew {
    /// Another binary, or another compiled version.
    Other,
    /// Same path and version, but the file there was rewritten after the daemon started.
    Rebuilt,
    /// Same path and version, but the daemon's `ping` predates the build stamp.
    NoStamp,
    /// This CLI could not stat its own binary, so no build can be proven to match.
    Unverifiable,
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

    /// A real file under `/var/tmp` (disk, never RAM `/tmp`), so `metadata` drives the stamp.
    fn scratch(name: &str, bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::Builder::new()
            .prefix("slipfork-identity-")
            .tempdir_in("/var/tmp")
            .expect("scratch directory under /var/tmp");
        let path = directory.path().join(name);
        std::fs::write(&path, bytes).expect("write the scratch binary");
        (directory, path)
    }

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
        let (_directory, own) = scratch("slipfork", b"own");
        let client = DaemonIdentity::new(&own);
        let reply = serde_json::json!({
            "pong": true,
            "pid": 7,
            "version": "0.0.1",
            "exe": "/opt/slipfork/slipfork",
            "build": { "mtime_ns": 1, "len": 2 },
        });
        let error = client
            .verify(&reply, Path::new("/run/user/1000/slipfork/daemon.sock"))
            .expect_err("a different binary must be refused");
        assert!(error.message.contains("`/opt/slipfork/slipfork` (0.0.1)"));
        assert!(
            error
                .message
                .contains(&format!("`{}`", client.exe().display()))
        );
        assert!(error.message.contains(env!("CARGO_PKG_VERSION")));
        let next = single_next(&error);
        assert!(next.contains("kill 7 (the stale daemon)"), "{next}");
        assert!(next.contains("`slipfork up`"), "{next}");
        assert!(!next.contains("systemctl"), "{next}");
    }

    /// An in-place upgrade keeps the path and version but swaps the bytes: the old build is
    /// refused as a stale daemon. With no `pid`, the remedy falls back to the packaged unit.
    #[test]
    fn a_rebuilt_binary_at_the_same_path_is_refused() {
        let (_directory, own) = scratch("slipfork", b"old build");
        let client = DaemonIdentity::new(&own);
        let reply = serde_json::json!({
            "pong": true,
            "version": env!("CARGO_PKG_VERSION"),
            "exe": client.exe().display().to_string(),
            "build": { "mtime_ns": 1, "len": 1 },
        });
        let error = client
            .verify(&reply, Path::new("/run/user/1000/slipfork/daemon.sock"))
            .expect_err("stale code at the same path must be refused");
        assert!(error.message.contains("stale build"), "{}", error.message);
        assert!(error.message.contains("rebuilt"), "{}", error.message);
        assert!(
            error
                .message
                .contains(&format!("`{}`", client.exe().display()))
        );
        let next = single_next(&error);
        assert!(next.contains("systemctl --user stop slipfork"), "{next}");
        assert!(next.contains("pkill -f 'slipfork __daemon'"), "{next}");
        assert!(!next.contains("the stale daemon"), "{next}");
    }

    /// A daemon that reports path and version but no stamp cannot prove it is this build.
    #[test]
    fn a_reply_without_a_build_stamp_is_refused() {
        let (_directory, own) = scratch("slipfork", b"own");
        let client = DaemonIdentity::new(&own);
        let reply = serde_json::json!({
            "pong": true,
            "version": env!("CARGO_PKG_VERSION"),
            "exe": client.exe().display().to_string(),
        });
        let error = client
            .verify(&reply, Path::new("/run/user/1000/slipfork/daemon.sock"))
            .expect_err("a reply without a stamp must be refused");
        assert!(
            error.message.contains("no build stamp"),
            "{}",
            error.message
        );
        let next = single_next(&error);
        assert!(next.contains("systemctl --user stop slipfork"), "{next}");
    }

    /// A reply naming a pid that cannot be signalled must never produce a `kill 0` line.
    #[test]
    fn an_unusable_pid_falls_back_to_the_packaged_unit_remedy() {
        let (_directory, own) = scratch("slipfork", b"own");
        let client = DaemonIdentity::new(&own);
        let reply = serde_json::json!({
            "pong": true,
            "pid": 0,
            "version": "0.0.1",
            "exe": "/opt/slipfork/slipfork",
            "build": { "mtime_ns": 1, "len": 2 },
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
        let (_directory, own) = scratch("slipfork", b"own");
        let client = DaemonIdentity::new(&own);
        let reply = serde_json::json!({ "pong": true, "pid": 7, "worlds": 0 });
        let error = client
            .verify(&reply, Path::new("/run/user/1000/daemon.sock"))
            .expect_err("a daemon without identity must be refused");
        assert!(error.message.contains("predates this CLI"));
        assert!(error.message.contains("/run/user/1000/daemon.sock"));
        assert!(
            error
                .message
                .contains(&format!("`{}`", client.exe().display()))
        );
        assert!(error.message.contains(env!("CARGO_PKG_VERSION")));
        let next = single_next(&error);
        assert!(next.contains("kill 7 (the stale daemon)"), "{next}");
        assert!(next.contains("`slipfork up`"), "{next}");
        assert!(!next.contains("systemctl"), "{next}");
    }

    #[test]
    fn this_binarys_own_reply_passes() {
        let (_directory, own) = scratch("slipfork", b"own");
        let client = DaemonIdentity::new(&own);
        let reply = serde_json::json!({
            "pong": true,
            "version": env!("CARGO_PKG_VERSION"),
            "exe": client.exe().display().to_string(),
            "build": client.build(),
        });
        assert!(
            client
                .verify(&reply, Path::new("/run/user/1000/slipfork/daemon.sock"))
                .is_ok()
        );
    }

    /// An exe that cannot be stat'ed fails closed: no build can be proven, so the daemon is
    /// refused rather than trusted against unknowable bytes.
    #[test]
    fn an_exe_that_cannot_be_statted_refuses_the_daemon() {
        let client = DaemonIdentity::new(Path::new("/nonexistent-slipfork/self"));
        assert!(client.build().is_null());
        let reply = serde_json::json!({
            "pong": true,
            "version": env!("CARGO_PKG_VERSION"),
            "exe": "/nonexistent-slipfork/self",
        });
        let error = client
            .verify(&reply, Path::new("/run/user/1000/slipfork/daemon.sock"))
            .expect_err("an unverifiable daemon must be refused");
        assert!(
            error.message.contains("could not be stat'ed"),
            "{}",
            error.message
        );
        let next = single_next(&error);
        assert!(next.contains("systemctl --user stop slipfork"), "{next}");
    }
}
