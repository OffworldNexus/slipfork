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
//! - [`doctor`]: the pre-daemon `slipfork doctor` seam (`ADR-0010` §4): `HostProbe` + report
//!   rendering over an injected `SubvolumeDriver` (the CLI owns no `WorldEngine`).
//! - [`apparmor`]: the Ubuntu 24.04 `/etc/apparmor.d/slipfork` `userns` profile body.
//! - [`layout`]: pure paths derived from already-resolved roots (`state`, `runtime`, data/vaults).
//! - [`project`]: the injected [`slipfork_vcs::ProjectSlugOverride`] and the one
//!   `SlipforkConfig` loader/resolver constructor (no second TOML parser, no silent defaults).
//! - [`user`]: the [`user::UserIdentity`] port and its `id -un` OS-account adapter.
//! - [`vaults`]: the injected project-vault factory over the `0600` local vault.
//! - [`ospp`]: the injected paired-`OSPP v1` session coordinate store (`0600`, zero secrets).
//! - [`vcs`]: the composition-root Git/Jujutsu [`slipfork_vcs::VcsWorkspaceFactory`] router.
//! - [`oci_store`]: the injected [`slipfork_engine::rootfs::OciImageStore`] over the Docker CLI.
//! - [`runtime`]: pinned `crun` + `slipfork-world-init` payload provisioning for every `L0` rootfs.
//! - [`daemon`]: the in-process [`slipfork_edge::DaemonControl`] implementation `slipforkd` injects
//!   into [`slipfork_edge::DaemonControlServer`].
//! - [`daemon_unit_install`]: the single `slipfork daemon install` orchestration over a narrow
//!   [`daemon_unit_install::UnitInstaller`] port (`ADR-0011` §1).

#![forbid(unsafe_code)]

pub mod apparmor;
pub mod cli;
pub mod daemon;
pub mod daemon_unit_install;
pub mod doctor;
pub mod layout;
pub mod oci_store;
pub mod ospp;
pub mod project;
pub mod runtime;
pub mod user;
pub mod vaults;
pub mod vcs;
