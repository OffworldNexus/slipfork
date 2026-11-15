//! Pure filesystem layout derived from explicit composition-root inputs.
//!
//! No function here reads the environment or the process working directory: `slipfork` and
//! `slipforkd` resolve `HOME`, `$XDG_CACHE_HOME`, `$XDG_DATA_HOME`, `$XDG_RUNTIME_DIR`, and their
//! `--state-dir`/`--runtime-dir` arguments once, then pass those values in.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use slipfork_world::ProjectSlug;

/// Directory holding the developer's private `0600` vaults (`<data_home>/slipfork/vaults`).
const VAULTS_DIRECTORY: &str = "vaults";

/// The explicit state/runtime/data roots every `slipfork`/`slipforkd` invocation shares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    state_dir: PathBuf,
    runtime_dir: PathBuf,
    data_home: PathBuf,
}

impl Layout {
    /// Resolve the roots from explicit flags and already-read environment values.
    ///
    /// `state_dir` falls back to `$XDG_CACHE_HOME/slipfork` (or `~/.cache/slipfork`);
    /// `runtime_dir` falls back to `$XDG_RUNTIME_DIR/slipfork` (or `<state_dir>/run`, which keeps
    /// the socket on disk when no session runtime directory exists); `data_home` falls back to
    /// `$XDG_DATA_HOME` (or `~/.local/share`) and holds the `0600` vaults.
    pub fn resolve(
        state_dir: Option<PathBuf>,
        runtime_dir: Option<PathBuf>,
        home: &Path,
        xdg_cache_home: Option<&Path>,
        xdg_runtime_dir: Option<&Path>,
        xdg_data_home: Option<&Path>,
    ) -> Self {
        let state_dir = state_dir.unwrap_or_else(|| match xdg_cache_home {
            Some(xdg) if xdg.is_absolute() => xdg.join("slipfork"),
            _ => home.join(".cache").join("slipfork"),
        });
        let runtime_dir = runtime_dir.unwrap_or_else(|| match xdg_runtime_dir {
            Some(xdg) if xdg.is_absolute() => xdg.join("slipfork"),
            _ => state_dir.join("run"),
        });
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
        slipfork_edge::DaemonControlServer::socket_path_in(&self.runtime_dir)
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

    #[test]
    fn explicit_roots_always_win() {
        let layout = Layout::resolve(
            Some(PathBuf::from("/state")),
            Some(PathBuf::from("/runtime")),
            &home(),
            Some(Path::new("/cache")),
            Some(Path::new("/xdg-runtime")),
            Some(Path::new("/xdg-data")),
        );
        assert_eq!(layout.state_dir(), Path::new("/state"));
        assert_eq!(layout.runtime_dir(), Path::new("/runtime"));
        assert_eq!(layout.data_home(), Path::new("/xdg-data"));
    }

    #[test]
    fn defaults_follow_xdg_then_home_and_keep_runtime_on_disk() {
        let xdg = Layout::resolve(
            None,
            None,
            &home(),
            Some(Path::new("/cache")),
            Some(Path::new("/run/user/1000")),
            Some(Path::new("/data")),
        );
        assert_eq!(xdg.state_dir(), Path::new("/cache/slipfork"));
        assert_eq!(xdg.runtime_dir(), Path::new("/run/user/1000/slipfork"));
        assert_eq!(xdg.data_home(), Path::new("/data"));

        let bare = Layout::resolve(None, None, &home(), None, None, None);
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
    fn relative_xdg_values_are_not_trusted() {
        let layout = Layout::resolve(
            None,
            None,
            &home(),
            Some(Path::new("cache")),
            None,
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
            Some(PathBuf::from("/runtime")),
            &home(),
            None,
            None,
            Some(Path::new("/data")),
        );
        assert_eq!(
            layout.vault_path(&project),
            PathBuf::from("/data/slipfork/vaults/acme.json")
        );
    }
}
