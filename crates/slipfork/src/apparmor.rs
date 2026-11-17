//! Ubuntu 24.04 LTS AppArmor `userns` profile rendering (`ADR-0010` §7).
//!
//! On `noble` (Linux 6.8+, AppArmor 4.0) with
//! `kernel.apparmor_restrict_unprivileged_userns=1`, an unprivileged `CLONE_NEWUSER` from an
//! unconfined process is denied with `EPERM`; the failure looks like a Slipfork bug. The rendered
//! profile grants `userns,` to the two Slipfork executables (they inherit the profile across the
//! `unshare` / `slipfork-world-init` `execve`), and deliberately contains no file or mount rules so
//! AppArmor keeps file/mount mediation inactive.
//!
//! `slipfork daemon unit --only apparmor` prints this as the `/etc/apparmor.d/slipfork` body.

#![forbid(unsafe_code)]

use std::path::Path;

/// The four-line `/etc/apparmor.d/slipfork` `userns` profile for the binaries in `bin_dir`.
pub fn render_userns_profile(bin_dir: &Path) -> String {
    let dir = bin_dir.display();
    format!(
        "abi <abi/4.0>,\nprofile slipfork {dir}/slipfork{{,d}} flags=(unconfined) {{\n  userns,\n}}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_profile_is_exactly_four_lines_with_the_userns_rule() {
        let profile = render_userns_profile(Path::new("/home/dev/.local/bin"));
        assert_eq!(
            profile,
            "abi <abi/4.0>,\nprofile slipfork /home/dev/.local/bin/slipfork{,d} flags=(unconfined) {\n  userns,\n}\n"
        );
        assert_eq!(profile.lines().count(), 4);
    }

    #[test]
    fn no_file_or_mount_rules_are_rendered() {
        let profile = render_userns_profile(Path::new("/usr/bin"));
        for rule in ["/**", "mount", "file,", "network,"] {
            assert!(
                !profile.contains(rule),
                "the profile must keep file/mount mediation inactive: {profile}"
            );
        }
    }
}
