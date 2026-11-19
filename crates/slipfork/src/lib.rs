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
//!   rendering over the concrete [`slipfork_engine::btrfs::KernelBtrfsDriver`] probe seam (the CLI
//!   owns no `WorldEngine`).
//! - [`apparmor`]: the Ubuntu 24.04 `/etc/apparmor.d/slipfork` `userns` profile body.
//! - [`layout`]: pure paths derived from already-resolved roots (`state`, `runtime`, data/vaults).
//! - [`logs`]: the one attributed `slipfork logs` renderer (`<source>: ` prefixes for fan-out,
//!   byte-exact raw output for one named target).
//! - [`project`]: the injected [`slipfork_vcs::ProjectSlugOverride`] and the one
//!   `SlipforkConfig` loader/resolver constructor (no second TOML parser, no silent defaults).
//! - [`user`]: the concrete [`user::UserIdentity`] value (resolved once at a composition root
//!   from `id -un` / `getent passwd`) and the `kernel_hostname()` (`uname -n`) recipe helper.
//! - [`vaults`]: the injected project-vault factory over the `0600` local vault.
//! - [`vcs`]: the composition-root Git/Jujutsu [`slipfork_vcs::VcsWorkspaceFactory`] router.
//! - [`runtime`]: pinned `crun` + `slipfork-world-init` payload provisioning for every `L0` rootfs.
//! - [`daemon`]: the in-process [`slipfork_edge::DaemonControl`] implementation `slipforkd` injects
//!   into [`slipfork_edge::DaemonControlServer`].
//! - [`daemon_unit_install`]: the single `slipfork daemon install` orchestration over the concrete
//!   [`daemon_unit_install::UnitInstaller`] the composition root builds over
//!   `/etc/systemd/system` + `/etc/apparmor.d` (`ADR-0011` §1).
//! - [`status`]: the bare `slipfork` / `slipfork status` dashboard and its single human lineage
//!   line, rendered purely from the daemon's [`slipfork_wire::control::DaemonStatus`].
//! - [`restart`]: the one actionable `up` / `fork` / `restart` block and its `--json` document,
//!   rendered purely from the daemon's [`slipfork_wire::control::WorldUpSummary`].
//! - [`ps`]: the per-world process tree, rendered purely from the daemon's
//!   [`slipfork_wire::control::WorldProcessReport`].
//! - [`open`]: world selection, the typed-origin hand-off to [`browser::Browser`], and the verified
//!   `open --remote` SSH recipe.
//! - [`browser`]: the one concrete desktop launcher over the Composition Root's resolved program.

#![forbid(unsafe_code)]

pub mod apparmor;
pub mod browser;
pub mod cli;
pub mod daemon;
pub mod daemon_handover;
pub mod daemon_install_report;
pub mod daemon_unit_install;
pub mod doctor;
pub mod layout;
pub mod logs;
pub mod open;
pub mod project;
pub mod ps;
pub mod restart;
pub mod runtime;
pub mod status;
pub mod ui;
pub mod user;
pub mod vaults;
pub mod vcs;
