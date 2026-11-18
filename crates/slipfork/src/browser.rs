//! The single production [`BrowserOpener`] (`docs/design/dx-0.1.0-ux.md` §6).
//!
//! [`BrowserOpener`] lives in `slipfork-ospp` so that crate stays free of OS and environment
//! access, and the zeroizing [`LivePairingUrl`] never leaves a validated in-process value. This
//! module is its one production implementation: both Composition Roots (`slipfork` and
//! `slipforkd`) resolve the desktop inputs (`BROWSER`, `DISPLAY`, `WAYLAND_DISPLAY`) once and hand
//! them to [`SystemBrowserOpener::from_host_inputs`], so no domain code ever reads the environment.
//!
//! A host with none of those three inputs is headless: the opener refuses with
//! [`NO_BROWSER_REMEDY`] instead of spawning a desktop launcher that cannot work, and
//! `slipfork open` names the SSH recipe that does.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use slipfork_ospp::{BrowserOpener, LivePairingUrl, OsppError};
use slipfork_world::WorldOrigin;

/// The one actionable refusal on a headless host (`docs/design/dx-0.1.0-ux.md` §6).
pub const NO_BROWSER_REMEDY: &str = "no desktop browser available on this host; run `slipfork open --remote` for the SSH port-forward recipe";

/// The desktop launcher `xdg-open` when the developer names no `BROWSER`.
const DEFAULT_OPENER: &str = "xdg-open";

/// [`BrowserOpener`] over this host's desktop launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemBrowserOpener {
    /// Launcher executable: the developer's `BROWSER`, else `xdg-open`.
    program: PathBuf,
    /// Whether any desktop input was resolved (`BROWSER`, `DISPLAY`, or `WAYLAND_DISPLAY`).
    available: bool,
}

impl SystemBrowserOpener {
    /// Construct the opener from inputs the Composition Root resolved.
    ///
    /// `browser` is the developer's explicit `BROWSER` program; `has_display` / `has_wayland`
    /// report whether `DISPLAY` / `WAYLAND_DISPLAY` name a desktop session. Any one of the three
    /// is a real launch capability; with none of them the host is headless and every open fails
    /// closed with [`NO_BROWSER_REMEDY`].
    pub fn from_host_inputs(
        browser: Option<PathBuf>,
        has_display: bool,
        has_wayland: bool,
    ) -> Self {
        let available = browser.is_some() || has_display || has_wayland;
        Self {
            program: browser.unwrap_or_else(|| PathBuf::from(DEFAULT_OPENER)),
            available,
        }
    }

    /// Whether this host can launch a browser at all.
    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Hand one already-validated URL to the desktop launcher.
    fn open(&self, url: &str) -> Result<(), OsppError> {
        if !self.available {
            return Err(OsppError::Transport(NO_BROWSER_REMEDY.to_owned()));
        }
        let status = Command::new(&self.program)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|err| {
                OsppError::Transport(format!("cannot run {}: {err}", self.program.display()))
            })?;
        if !status.success() {
            return Err(OsppError::Transport(format!(
                "{} exited with {status}",
                self.program.display()
            )));
        }
        Ok(())
    }
}

impl BrowserOpener for SystemBrowserOpener {
    fn open_pairing_url(&self, url: LivePairingUrl) -> Result<(), OsppError> {
        self.open(url.as_str())
    }

    fn open_origin(&self, origin: &WorldOrigin) -> Result<(), OsppError> {
        self.open(origin.url())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_browser_program_is_a_launch_capability_even_without_a_display() {
        let opener = SystemBrowserOpener::from_host_inputs(
            Some(PathBuf::from("/usr/bin/w3m")),
            false,
            false,
        );
        assert!(opener.is_available());
        assert_eq!(opener.program, PathBuf::from("/usr/bin/w3m"));
    }

    #[test]
    fn a_desktop_session_without_browser_uses_xdg_open() {
        for opener in [
            SystemBrowserOpener::from_host_inputs(None, true, false),
            SystemBrowserOpener::from_host_inputs(None, false, true),
        ] {
            assert!(opener.is_available());
            assert_eq!(opener.program, PathBuf::from(DEFAULT_OPENER));
        }
    }

    #[test]
    fn a_headless_host_refuses_to_launch_and_names_the_remote_recipe() {
        let opener = SystemBrowserOpener::from_host_inputs(None, false, false);
        assert!(!opener.is_available());
        let error = opener
            .open("http://demo.localhost")
            .expect_err("a headless host never spawns a launcher");
        assert_eq!(
            error.to_string(),
            format!("ospp-v1 transport failed: {NO_BROWSER_REMEDY}")
        );
        assert!(NO_BROWSER_REMEDY.contains("slipfork open --remote"));
    }
}
