//! Zed extension glue: launches the shipped `slipfork lsp` binary.

use zed_extension_api::{self as zed, Command, LanguageServerId, Result, Worktree};

struct ForkfileExtension;

impl zed::Extension for ForkfileExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let command = worktree.which("slipfork").ok_or_else(|| {
            "`slipfork` is not on PATH. Install Slipfork, then reopen the project.".to_string()
        })?;
        Ok(Command {
            command,
            args: vec!["lsp".to_string()],
            env: Vec::new(),
        })
    }
}

zed::register_extension!(ForkfileExtension);
