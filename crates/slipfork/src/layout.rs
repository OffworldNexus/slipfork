//! Pure filesystem layout derived from explicit composition-root inputs.
//!
//! No function here reads the environment or the process working directory: `slipfork` and
//! `slipforkd` resolve `HOME`, `$XDG_CACHE_HOME`, `$XDG_DATA_HOME`, and `SUDO_USER` once, then pass
//! those values in. There is exactly one runtime directory — `<state_dir>/run` (`ADR-0011` §2):
//! the thin client has no `--runtime-dir` override, so it can never dial a different
//! `daemon.sock` than the installed `slipforkd.service`.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::user::ResolvedAccount;
use slipfork_world::ProjectSlug;

/// Accept an environment value only when it names an absolute path.
pub fn absolute_path(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|path| path.is_absolute())
}

/// Directory holding the developer's private `0600` vaults (`<data_home>/slipfork/vaults`).
const VAULTS_DIRECTORY: &str = "vaults";

/// Directory holding the daemon's control socket, always directly under the state root.
const RUNTIME_DIRECTORY: &str = "run";

/// The explicit state/runtime/data roots every `slipfork`/`slipforkd` invocation shares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    state_dir: PathBuf,
    runtime_dir: PathBuf,
    data_home: PathBuf,
}

impl Layout {
    /// Resolve the roots for one already-resolved OS account (`ADR-0011` §2).
    ///
    /// `state_dir` falls back to `$XDG_CACHE_HOME/slipfork` (or `<home>/.cache/slipfork`);
    /// `runtime_dir` is unconditionally `<state_dir>/run` (the daemon composition root has
    /// its own `explicit` constructor); `data_home` falls back to `$XDG_DATA_HOME` (or
    /// `~/.local/share`) and holds the `0600` vaults.
    pub fn for_account(
        account: &ResolvedAccount,
        state_dir: Option<PathBuf>,
        xdg_cache_home: Option<&Path>,
        xdg_data_home: Option<&Path>,
    ) -> Self {
        Self::with_roots(
            state_dir,
            None,
            &account.home,
            xdg_cache_home,
            xdg_data_home,
        )
    }

    /// Resolve the client/default roots from the developer's home and XDG cache/data homes.
    ///
    /// The runtime directory is **always** `<state_dir>/run`; there is no `$XDG_RUNTIME_DIR`
    /// branch, because a system unit and a login shell must derive the same `daemon.sock`
    /// (`ADR-0011` §2).
    pub fn resolve(
        state_dir: Option<PathBuf>,
        home: &Path,
        xdg_cache_home: Option<&Path>,
        xdg_data_home: Option<&Path>,
    ) -> Self {
        Self::with_roots(state_dir, None, home, xdg_cache_home, xdg_data_home)
    }

    /// Both roots explicit: the `slipforkd` composition root, whose `--runtime-dir` is required.
    pub fn explicit(
        state_dir: PathBuf,
        runtime_dir: PathBuf,
        home: &Path,
        xdg_data_home: Option<&Path>,
    ) -> Self {
        Self::with_roots(
            Some(state_dir),
            Some(runtime_dir),
            home,
            None,
            xdg_data_home,
        )
    }

    /// The single layout formula every constructor above delegates to.
    fn with_roots(
        state_dir: Option<PathBuf>,
        runtime_dir: Option<PathBuf>,
        home: &Path,
        xdg_cache_home: Option<&Path>,
        xdg_data_home: Option<&Path>,
    ) -> Self {
        let state_dir = state_dir.unwrap_or_else(|| match xdg_cache_home {
            Some(xdg) if xdg.is_absolute() => xdg.join("slipfork"),
            _ => home.join(".cache").join("slipfork"),
        });
        let runtime_dir = runtime_dir.unwrap_or_else(|| state_dir.join(RUNTIME_DIRECTORY));
        let data_home = match xdg_data_home {
            Some(xdg) if xdg.is_absolute() => xdg.to_path_buf(),
            _ => home.join(".local").join("share"),
        };
        Self {
            state_dir,
            runtime_dir,
            data_home,
        }
    }

    /// Explicit CoW state root (snapshots, world clones, generated world homes).
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Explicit runtime root holding the `0600` `<runtime_dir>/daemon.sock`.
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    /// Developer data home holding the `0600` vaults (`$XDG_DATA_HOME` or `~/.local/share`).
    pub fn data_home(&self) -> &Path {
        &self.data_home
    }

    /// Path of one project's local `0600` vault (`<data_home>/slipfork/vaults/<project>.json`).
    pub fn vault_path(&self, project: &ProjectSlug) -> PathBuf {
        self.data_home
            .join("slipfork")
            .join(VAULTS_DIRECTORY)
            .join(format!("{project}.json"))
    }

    /// `0600` Unix control socket of the daemon (`<runtime_dir>/daemon.sock`).
    pub fn daemon_socket(&self) -> PathBuf {
        slipfork_wire::control::control_socket_path(&self.runtime_dir)
    }

    /// `0600` `AF_UNIX` HTTP ingress socket (`<state_dir>/http-ingress.sock`).
    pub fn http_ingress_socket(&self) -> PathBuf {
        self.state_dir.join("http-ingress.sock")
    }

    /// World-scoped `HOME` directory for one world (created by the engine on clone).
    pub fn world_home(&self, project: &ProjectSlug, label: &str) -> PathBuf {
        self.state_dir
            .join("homes")
            .join(project.as_str())
            .join(label)
    }

    /// Empty seed directory snapshotted once per project to mint its base world state.
    pub fn base_seed_dir(&self, project: &ProjectSlug) -> PathBuf {
        self.state_dir.join("base").join(project.as_str())
    }

    /// Directory holding one world's retained hook logs (`ADR-0008` §4 rescue path).
    pub fn world_logs_dir(&self, project: &ProjectSlug, world: &str) -> PathBuf {
        self.state_dir
            .join(project.as_str())
            .join("worlds")
            .join(world)
            .join("logs")
    }

    /// `0600` machine-authentication P-256 scalar used for OSPP `p256` auth and pairing.
    pub fn device_key_path(&self) -> PathBuf {
        self.data_home
            .join("slipfork")
            .join("device-key-software.json")
    }
}

/// Create `path` (and parents) as a user-private `0700` directory when missing.
pub fn ensure_private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|err| format!("cannot create {}: {err}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|err| format!("cannot restrict {} to 0700: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        PathBuf::from("/home/developer")
    }

    fn account() -> ResolvedAccount {
        ResolvedAccount {
            name: "developer".to_owned(),
            uid: 1000,
            gid: 1000,
            home: home(),
        }
    }

    #[test]
    fn explicit_state_wins_and_runtime_is_always_its_run_directory() {
        let layout = Layout::resolve(
            Some(PathBuf::from("/state")),
            &home(),
            Some(Path::new("/cache")),
            Some(Path::new("/xdg-data")),
        );
        assert_eq!(layout.state_dir(), Path::new("/state"));
        assert_eq!(layout.runtime_dir(), Path::new("/state/run"));
        assert_eq!(layout.data_home(), Path::new("/xdg-data"));
    }

    #[test]
    fn defaults_follow_xdg_then_home_and_keep_runtime_on_disk() {
        let xdg = Layout::resolve(
            None,
            &home(),
            Some(Path::new("/cache")),
            Some(Path::new("/data")),
        );
        assert_eq!(xdg.state_dir(), Path::new("/cache/slipfork"));
        assert_eq!(xdg.runtime_dir(), Path::new("/cache/slipfork/run"));
        assert_eq!(xdg.data_home(), Path::new("/data"));

        let bare = Layout::resolve(None, &home(), None, None);
        assert_eq!(
            bare.state_dir(),
            Path::new("/home/developer/.cache/slipfork")
        );
        assert_eq!(
            bare.runtime_dir(),
            Path::new("/home/developer/.cache/slipfork/run")
        );
        assert_eq!(bare.data_home(), Path::new("/home/developer/.local/share"));
    }

    #[test]
    fn for_account_derives_both_roots_from_the_target_accounts_home() {
        let layout = Layout::for_account(&account(), None, None, None);
        assert_eq!(
            layout.state_dir(),
            Path::new("/home/developer/.cache/slipfork")
        );
        assert_eq!(
            layout.runtime_dir(),
            Path::new("/home/developer/.cache/slipfork/run")
        );
    }

    #[test]
    fn only_the_daemon_explicit_constructor_overrides_the_runtime_directory() {
        let layout = Layout::explicit(
            PathBuf::from("/state"),
            PathBuf::from("/elsewhere/run"),
            &home(),
            None,
        );
        assert_eq!(layout.state_dir(), Path::new("/state"));
        assert_eq!(layout.runtime_dir(), Path::new("/elsewhere/run"));

        let account_layout =
            Layout::for_account(&account(), Some(PathBuf::from("/state")), None, None);
        assert_eq!(account_layout.runtime_dir(), Path::new("/state/run"));
    }

    #[test]
    fn relative_xdg_values_are_not_trusted() {
        let layout = Layout::resolve(
            None,
            &home(),
            Some(Path::new("cache")),
            Some(Path::new("data")),
        );
        assert_eq!(
            layout.state_dir(),
            Path::new("/home/developer/.cache/slipfork")
        );
        assert_eq!(
            layout.data_home(),
            Path::new("/home/developer/.local/share")
        );
    }

    #[test]
    fn vaults_are_keyed_by_canonical_project_slug() {
        let project = ProjectSlug::parse("acme").expect("valid slug");
        let layout = Layout::resolve(
            Some(PathBuf::from("/state")),
            &home(),
            None,
            Some(Path::new("/data")),
        );
        assert_eq!(
            layout.vault_path(&project),
            PathBuf::from("/data/slipfork/vaults/acme.json")
        );
    }
}
