//! The standalone `slipfork-lsp` binary: the composition root for the language
//! server.
//!
//! Only this entry point touches argv and the process stdio. The library never
//! reads the environment or opens a socket, so an editor launch is the whole
//! ambient surface.

use std::io;
use std::process::ExitCode;

use slipfork_lsp::Server;

const USAGE: &str = "\
slipfork-lsp — Forkfile language server over stdio

Usage: slipfork-lsp [--help | --version]

Editors speak JSON-RPC on stdin/stdout: point any LSP client at `slipfork-lsp`
with no arguments. In Zed, `lsp.slipfork-lsp.binary.path` selects a binary you
built; otherwise `slipfork-lsp` on PATH is used.

Options:
  -h, --help      Print this overview
  -V, --version   Print the version
";

fn main() -> ExitCode {
    let args: Vec<String> = match std::env::args_os()
        .skip(1)
        .map(|argument| argument.into_string())
        .collect()
    {
        Ok(args) => args,
        Err(_) => return usage_error("arguments must be valid UTF-8"),
    };
    let Some((flag, rest)) = args.split_first() else {
        return serve();
    };
    match flag.as_str() {
        "-h" | "--help" => {
            if let Some(other) = rest.first() {
                return usage_error(&format!("unknown argument `{other}`"));
            }
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        "-V" | "--version" => {
            if let Some(other) = rest.first() {
                return usage_error(&format!("unknown argument `{other}`"));
            }
            println!("slipfork-lsp {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        other => usage_error(&format!("unknown argument `{other}`")),
    }
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    eprintln!("next: run `slipfork-lsp --help`; editors only need the bare command.");
    ExitCode::from(2)
}

fn serve() -> ExitCode {
    let stdin = io::stdin();
    let stdout = io::stdout();
    match Server::new(stdin.lock(), stdout.lock()).serve() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: the language server stopped: {error}");
            eprintln!("next: restart the editor session or rerun `slipfork-lsp`.");
            ExitCode::FAILURE
        }
    }
}
