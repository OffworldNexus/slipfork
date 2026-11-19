//! Zed extension for `Forkfile`: the tree-sitter grammar plus the shipped `slipfork lsp`.

use zed_extension_api::{self as zed, LanguageServerId, Result, Worktree};

struct ForkfileExtension;

impl zed::Extension for ForkfileExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<zed::Command> {
        let command = worktree.which("slipfork").ok_or_else(|| {
            "`slipfork` is not on PATH. Install it, then reload Zed \
             (next: install slipfork from https://github.com/OffworldNexus/slipfork)."
                .to_string()
        })?;
        Ok(zed::Command {
            command,
            args: vec!["lsp".to_string()],
            env: Vec::new(),
        })
    }
}

zed::register_extension!(ForkfileExtension);
