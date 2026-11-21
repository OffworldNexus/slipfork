//! The `Forkfile` language server driven over stdio.

/// Entry marker for the language server owned by this crate.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LanguageServer;

impl LanguageServer {
    /// Builds the language server marker.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}
