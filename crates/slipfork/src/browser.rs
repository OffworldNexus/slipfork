//! The one concrete desktop launcher (`docs/design/dx-0.1.0-ux.md` §6).
//!
//! [`Browser`] holds only what a Composition Root resolved: the absolute path of the host's opener
//! program (`xdg-open`, or `gio` with its `open` subcommand) and whether a desktop session exists.
//! It never reads `BROWSER`, `DISPLAY`, `WAYLAND_DISPLAY`, or `PATH`; `slipfork` and `slipforkd`
//! resolve those once and inject the value, so no domain code resolves a host fact.
//!
//! Error lines never interpolate the URL: a pairing URL carries its single-use `#handle=` fragment
//! and must not reach a log or diagnostic.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The one actionable refusal when this host has no usable desktop launcher.
pub const NO_BROWSER_REMEDY: &str = "no desktop browser available on this host; run `slipfork open --remote` for the SSH port-forward recipe";

/// The headless approval line: the URL is browsed on another machine, never spawned here.
pub const APPROVAL_HINT_PREFIX: &str = "open this URL in a browser on any machine to approve: ";

/// The host's desktop launcher, resolved to one absolute executable path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Browser {
    /// Absolute path of the resolved opener program (`None` when the host ships none).
    program: Option<PathBuf>,
    /// Subcommand words the program needs before the URL (`gio open`), empty for `xdg-open`.
    leading: &'static [&'static str],
    /// Whether the Composition Root resolved a desktop session (`DISPLAY`/`WAYLAND_DISPLAY`).
    available: bool,
}

impl Browser {
    /// Construct the launcher over the resolved absolute opener program path (`xdg-open`).
    pub fn new(program: impl Into<PathBuf>, available: bool) -> Self {
        Self {
            program: Some(program.into()),
            leading: &[],
            available,
        }
    }

    /// Construct the `gio open <url>` fallback when the host ships no `xdg-open`.
    pub fn gio(program: impl Into<PathBuf>, available: bool) -> Self {
        Self {
            program: Some(program.into()),
            leading: &["open"],
            available,
        }
    }

    /// No opener program resolved on this host: every open fails closed with [`NO_BROWSER_REMEDY`].
    pub fn none() -> Self {
        Self {
            program: None,
            leading: &[],
            available: false,
        }
    }

    /// Resolve `xdg-open`, then `gio`, through the `PATH` value the Composition Root read.
    ///
    /// Only an executable regular file is selected, so a `chmod -x` launcher is never spawned.
    pub fn resolve(path: Option<&OsStr>, available: bool) -> Self {
        let Some(path) = path else {
            return Self::none();
        };
        let resolve = |name: &str| {
            std::env::split_paths(path)
                .map(|directory| directory.join(name))
                .find(|candidate| is_executable(candidate))
                .and_then(|candidate| fs::canonicalize(candidate).ok())
        };
        if let Some(program) = resolve("xdg-open") {
            return Self::new(program, available);
        }
        if let Some(program) = resolve("gio") {
            return Self::gio(program, available);
        }
        Self::none()
    }

    /// Spawn the real opener with the URL and wait for it to exit.
    ///
    /// Returns `Ok(false)` when this host has no desktop session (the caller prints
    /// [`APPROVAL_HINT_PREFIX`] and never spawns), `Ok(true)` once the opener exited zero. A
    /// missing program is the headless-host case and fails closed on one line naming
    /// [`NO_BROWSER_REMEDY`]; no error ever interpolates `url`.
    pub fn open(&self, url: &str) -> Result<bool, String> {
        let Some(program) = self.program.as_ref() else {
            return Err(NO_BROWSER_REMEDY.to_owned());
        };
        if !self.available {
            return Ok(false);
        }
        let status = Command::new(program)
            .args(self.leading)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|err| {
                format!(
                    "cannot run {}: {err}; {NO_BROWSER_REMEDY}",
                    program.display()
                )
            })?;
        if !status.success() {
            return Err(format!(
                "{} exited with {status}; {NO_BROWSER_REMEDY}",
                program.display()
            ));
        }
        Ok(true)
    }
}

/// `true` when `path` is a regular file this account may execute.
fn is_executable(path: &Path) -> bool {
    path.is_file()
        && fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}
