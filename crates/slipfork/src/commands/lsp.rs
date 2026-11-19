//! `slipfork lsp`: embed the `Forkfile` language server on this process's stdio.

use std::io::{BufReader, BufWriter, IsTerminal};
use std::process::ExitCode;

use crate::CliApp;

/// Run the synchronous framed JSON-RPC loop until the editor sends `shutdown` + `exit`.
///
/// A terminal on stdin means a human typed `slipfork lsp`; JSON-RPC would silently sit on the TTY,
/// so print the editor wiring hint and fail with exit code `2` instead.
pub(crate) fn run(_app: &CliApp) -> Result<ExitCode, String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprintln!(
            "slipfork lsp speaks JSON-RPC on stdin — add \"slipfork lsp\" to your editor's \
             language-server config."
        );
        return Ok(ExitCode::from(2));
    }
    let code = u8::try_from(slipfork_lsp::run_on(
        BufReader::new(stdin.lock()),
        BufWriter::new(std::io::stdout().lock()),
    ))
    .unwrap_or(1);
    Ok(ExitCode::from(code))
}
