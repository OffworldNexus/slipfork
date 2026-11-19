//! `slipfork-engine`: the [`BtrfsEngine`] implementation of `slipfork_world::WorldEngine` for copy-on-write worlds, and
//! its single shipped implementation, [`BtrfsEngine`], over the concrete
//! [`btrfs::KernelBtrfsDriver`] (one unified Btrfs subvolume per world, `ADR-0010`).
//!
//! [`BtrfsEngine`] satisfies the shared conformance contract in `slipfork-world`: a world is
//! materialized from an immutable `SnapshotId` plus a multi-mount `WorldSpec`, its services are
//! supervised and stopped before `WorldEngine::freeze_world` seals it, and it is referenced only
//! through the opaque in-process `WorldHandle` (owned by `slipforkd`, never serialized,
//! `H-010` #1).
//!
//! Read-only snapshot creation (`WorldEngine::create_snapshot`, `WorldEngine::freeze_world`) and
//! writable world cloning (`WorldEngine::clone_world`) are distinct signatures: there is no
//! snapshot-mode flag and no capability boolean to branch on (`ADR-0010` §3).

pub mod btrfs;
pub mod btrfs_engine;
pub mod child_env;
mod child_io;
pub mod clock;
pub mod linux_cow;
pub mod oci;
pub mod rootfs;
pub mod stream;
mod supervise;
mod wait;
pub mod world_init;

use std::path::Path;

pub use btrfs_engine::{BtrfsEngine, CrunSurface, pod_bundle_guest};
pub use child_env::InheritedEnv;
pub use clock::elapsed_millis;
pub use linux_cow::current_uid_gid;

use slipfork_world::WorldTarget;

/// Single AppArmor/user-namespace remediation formatter (`ADR-0010` §4, §7).
///
/// Shared by [`BtrfsEngine`]'s world boot and the CLI's host probe: it never calls `current_exe()`,
/// so the composition root decides which binary directory the profile must name. The profile text
/// is the one `slipfork daemon unit --only apparmor` prints, so this hint never drifts from it: one
/// leading-slash brace expression naming every executable that performs `CLONE_NEWUSER` for a
/// world — the `bin_dir` binaries, host `/usr/bin/unshare`, and in-world `/.slipfork/bin/crun`.
pub fn userns_remediation_hint(bin_dir: &Path) -> String {
    let dir = bin_dir.display().to_string();
    let rel_bin_dir = dir.trim_matches('/');
    let bin_clause = if rel_bin_dir.is_empty() {
        "slipfork{,d,-world-init}".to_owned()
    } else {
        format!("{rel_bin_dir}/slipfork{{,d,-world-init}}")
    };
    format!(
        "unprivileged user namespaces are denied by AppArmor \
         (`kernel.apparmor_restrict_unprivileged_userns=1`); install the profile printed by \
         `slipfork daemon unit --only apparmor` at /etc/apparmor.d/slipfork \
         (`abi <abi/4.0>,` plus `profile slipfork \
         /{{{bin_clause},usr/bin/unshare,.slipfork/bin/crun}} flags=(unconfined) {{ userns, }}`), \
         then reload it with `sudo apparmor_parser -r /etc/apparmor.d/slipfork`"
    )
}

/// Stable filesystem label for a world target (fork/slot names are already validated DNS labels).
pub(crate) fn target_slug(target: &WorldTarget) -> String {
    match target {
        WorldTarget::Canonical => "canonical".to_owned(),
        WorldTarget::UserFork(fork) => format!("fork-{fork}"),
        WorldTarget::TestSlot(slot) => format!("slot-{slot}"),
    }
}

/// Lifecycle state of a world inside one engine instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorldStatus {
    Live,
    Freezing,
    Frozen,
    /// The last reload tore the old world down but could not boot its replacement: no PID 1
    /// survives, so live operations fail closed while a retried [`WorldEngine::reload_world`] or
    /// [`WorldEngine::destroy_world`] still owns the record.
    Failed,
    Destroyed,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::userns_remediation_hint;

    #[test]
    fn the_userns_hint_names_every_clone_newuser_executable_and_the_reload() {
        let hint = userns_remediation_hint(Path::new("/usr/local/bin"));
        for needle in [
            "/{usr/local/bin/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun}",
            "slipfork daemon unit --only apparmor",
            "apparmor_parser -r /etc/apparmor.d/slipfork",
        ] {
            assert!(hint.contains(needle), "hint missing {needle}: {hint}");
        }
        assert!(!hint.contains("//"), "{hint}");
    }

    #[test]
    fn the_userns_hint_renders_a_bare_clause_for_the_root_bin_dir() {
        let hint = userns_remediation_hint(Path::new("/"));
        assert!(
            hint.contains("/{slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun}"),
            "{hint}"
        );
        assert!(!hint.contains("//"), "{hint}");
    }
}
