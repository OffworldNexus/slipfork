//! The connect-path notice shown when this invocation has to start an ephemeral daemon.

use std::fmt;

/// The one notice the connect path shows when it has to start an ephemeral daemon.
///
/// It owns its own colour decision, so a boot renderer never re-renders the hint and the bytes
/// stay identical to the single line the CLI printed before this type existed.
#[derive(Debug, Clone, Copy)]
pub enum ConnectNotice {
    /// No packaged daemon answered, so this invocation starts a detached ephemeral one.
    Ephemeral {
        /// Whether ANSI styling is allowed for this invocation (`host.color`).
        color: bool,
    },
}

impl ConnectNotice {
    /// The exact bytes the ephemeral line has always printed.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Ephemeral { color } => {
                crate::ui::report::Palette::new(*color).meta(EPHEMERAL_HINT)
            }
        }
    }
}

impl fmt::Display for ConnectNotice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.render())
    }
}

/// The one ephemeral hint; the package enables the user service and the `:80` socket.
const EPHEMERAL_HINT: &str = "• daemon not installed; running ephemeral — the package enables it; on a tarball run `docs/install.sh`";
