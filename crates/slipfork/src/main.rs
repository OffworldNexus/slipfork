//! The `slipfork` binary: the single Composition Root for the whole system.
//!
//! Only this entry point reads ambient process state (`std::env`, `/proc`, bound
//! sockets) and assembles the constructor-injected tree. Library crates stay pure:
//! they receive their dependencies as arguments and never inspect the environment.

mod cli;

use std::ffi::OsString;
use std::io::IsTerminal;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let term_is_dumb = std::env::var_os("TERM").is_some_and(|term| term == "dumb");
    let color =
        std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() && !term_is_dumb;

    match cli::CommandLine::parse(&args) {
        Ok(line) => line.run(color),
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("next: run `slipfork --help` to see the available surface.");
            ExitCode::from(2)
        }
    }
}
