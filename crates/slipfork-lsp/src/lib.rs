//! The `Forkfile` language server: framed stdio JSON-RPC over the canonical
//! `slipfork-forkfile` parser, checker, and formatter.
//!
//! The library never reads the process environment or binds a socket. The
//! composition root (`crates/slipfork-lsp/src/main.rs`) passes its stdin/stdout
//! handles in.

mod document;
mod features;
mod protocol;
mod references;
mod server;
pub mod text;

pub use server::Server;
pub use text::{Position, Range, TextDocument};
