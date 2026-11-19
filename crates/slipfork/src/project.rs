//! The single injected [`ProjectSlugOverride`] adapter.
//!
//! `slipfork` and `slipforkd` both implement `<vcs_root>/slipfork.toml` `project = "<slug>"` here,
//! over `slipfork_dag::SlipforkConfig`; `crates/slipfork-vcs` therefore stays TOML-free.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;

use slipfork_dag::SlipforkConfig;
use slipfork_vcs::{GitRepoRootResolver, ProjectSlugOverride, RepoRootResolver, VcsError};
use slipfork_world::ProjectSlug;

/// The one repository-resolver constructor: `ProjectSlugOverride` + `GitRepoRootResolver`.
pub fn repository_resolver() -> Arc<dyn RepoRootResolver> {
    Arc::new(GitRepoRootResolver::new(Arc::new(ConfigProjectOverride)))
}

/// The one `slipfork.toml` loader. A missing or unreadable file is an error; callers that model
/// an optional manifest must use [`load_optional_slipfork_config`] instead.
pub fn load_slipfork_config(path: &Path) -> Result<SlipforkConfig, String> {
    let raw =
        fs::read_to_string(path).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    SlipforkConfig::parse_toml(&raw).map_err(|err| format!("{}: {err}", path.display()))
}

/// Load an **optional** `<vcs_root>/slipfork.toml`: absent means [`SlipforkConfig::default`],
/// while a malformed or unreadable file still fails closed.
pub fn load_optional_slipfork_config(path: &Path) -> Result<SlipforkConfig, String> {
    match fs::metadata(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(SlipforkConfig::default()),
        Err(err) => Err(format!("cannot read {}: {err}", path.display())),
        Ok(_) => load_slipfork_config(path),
    }
}

/// Reads `<vcs_root>/slipfork.toml` through the one `SlipforkConfig` parser.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConfigProjectOverride;

impl ProjectSlugOverride for ConfigProjectOverride {
    fn project_override(&self, vcs_root: &Path) -> Result<Option<ProjectSlug>, VcsError> {
        let path = vcs_root.join("slipfork.toml");
        match fs::metadata(&path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(VcsError::Io {
                    path,
                    message: err.to_string(),
                });
            }
            Ok(_) => {}
        }
        load_slipfork_config(&path)
            .map(|config| config.project)
            .map_err(|message| VcsError::Io { path, message })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_means_no_override() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            ConfigProjectOverride
                .project_override(dir.path())
                .expect("missing config is not an error"),
            None
        );
    }

    #[test]
    fn project_key_is_read_through_slipfork_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("slipfork.toml"), "project = \"acme\"\n").expect("write config");
        assert_eq!(
            ConfigProjectOverride
                .project_override(dir.path())
                .expect("valid config"),
            Some(ProjectSlug::parse("acme").expect("valid slug"))
        );
    }

    #[test]
    fn invalid_config_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("slipfork.toml"), "project = \"Bad_Slug\"\n")
            .expect("write config");
        let err = ConfigProjectOverride
            .project_override(dir.path())
            .expect_err("invalid slug must fail closed");
        assert!(err.to_string().contains("slipfork.toml"), "got: {err}");
    }
}
