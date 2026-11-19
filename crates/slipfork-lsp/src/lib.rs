//! `slipfork-lsp`: the `Forkfile` language server.
//!
//! A synchronous, single-threaded JSON-RPC loop over `Content-Length`-framed stdio (zero `tokio`).
//! It never parses `Forkfile` syntax itself: diagnostics, formatting, and hover text all come from
//! the canonical `slipfork-forkfile` crate, and positions are translated to UTF-16 here because
//! Zed ignores the negotiated `positionEncoding`.

#![forbid(unsafe_code)]

mod framing;
mod hover;
mod server;
mod utf16;

pub use server::run_on;
