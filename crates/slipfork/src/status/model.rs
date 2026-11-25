//! Render options plus the small pure formatters shared by the tree and its tests.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::ps::{PsReply, PsWorld};
use crate::ui::report::Palette;

/// How one status render should look.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Emit ANSI escapes.
    pub color: bool,
    /// Add supervisor pid to live status headers.
    pub verbose: bool,
    /// `$HOME`, used to shorten project paths to `~/…`.
    pub home: Option<PathBuf>,
}

impl Options {
    /// Builds plain options with the caller's colour decision.
    #[must_use]
    pub fn new(color: bool) -> Self {
        Self {
            color,
            verbose: false,
            home: None,
        }
    }

    /// Sets the home directory used for path shortening.
    #[must_use]
    pub fn with_home(mut self, home: Option<PathBuf>) -> Self {
        self.home = home;
        self
    }

    /// Enables the supervisor pid column.
    #[must_use]
    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }
}

/// Decodes the daemon's `ps` payload; a malformed payload is an error, never an empty tree.
///
/// # Errors
///
/// Returns the serde diagnostic so the caller can name the exact remedy.
pub fn parse(value: &Value) -> Result<PsReply, String> {
    serde_json::from_value(value.clone()).map_err(|error| error.to_string())
}

/// The fork's own name, or the world id for a top-level world.
#[must_use]
pub fn display_name(world: &PsWorld) -> &str {
    world.fork.as_deref().unwrap_or(&world.world)
}

/// Shortens `path` under `home` to `~/…`.
#[must_use]
pub fn display_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home
        && let Ok(rest) = path.strip_prefix(home)
    {
        if rest.as_os_str().is_empty() {
            return "~".to_owned();
        }
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

/// The dim `○ No world for ~/dir` line for a scoped `ps` in an unrelated directory.
#[must_use]
pub fn no_world_line(cwd: &Path, home: Option<&Path>, color: bool) -> String {
    format!(
        "{}\n",
        Palette::new(color).meta(&format!("○ No world for {}", display_path(cwd, home)))
    )
}
