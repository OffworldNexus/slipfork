//! Shared composition-layer types for the thin `slipfork` client and the `slipforkd` daemon.
//!
//! Only the two binaries built from this crate (`slipfork` = `src/main.rs`, `slipforkd` =
//! `src/bin/slipforkd.rs`) are Composition Roots: they resolve `HOME`, `$XDG_*`, the developer's OS
//! account, and the explicit `--state-dir`/`--runtime-dir` once, then wire the concrete adapters
//! from `slipfork-edge`, `slipfork-engine`, `slipfork-vcs`, `slipfork-snow`, and `slipfork-dag`
//! into the domain ports. Everything in this library takes those resolved values as arguments and
//! contains no ambient environment access.
//!
//! - [`cli`]: the single `clap` surface shared by both binaries.
//! - [`layout`]: pure paths derived from already-resolved roots (`state`, `runtime`, data/vaults).
//! - [`project`]: the injected [`slipfork_vcs::ProjectSlugOverride`] and the one
//!   `SlipforkConfig` loader/resolver constructor (no second TOML parser, no silent defaults).
//! - [`user`]: the [`user::UserIdentity`] port and its `id -un` OS-account adapter.
//! - [`vaults`]: the injected project-vault factory over the `0600` local vault.
//! - [`daemon`]: the in-process [`slipfork_edge::DaemonControl`] implementation `slipforkd` injects
//!   into [`slipfork_edge::DaemonControlServer`].

#![forbid(unsafe_code)]

pub mod cli;
pub mod daemon;
pub mod layout;
pub mod project;
pub mod user;
pub mod vaults;
