//! Standalone `slipfork-lsp` binary. `slipfork lsp` embeds the same [`slipfork_lsp::run_on`] loop.

#![forbid(unsafe_code)]

fn main() -> std::process::ExitCode {
    let code = u8::try_from(slipfork_lsp::run_on(
        std::io::BufReader::new(std::io::stdin().lock()),
        std::io::BufWriter::new(std::io::stdout().lock()),
    ))
    .unwrap_or(1);
    std::process::ExitCode::from(code)
}
