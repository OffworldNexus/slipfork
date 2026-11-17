//! Ubuntu 24.04 LTS AppArmor `userns` profile rendering (`ADR-0010` §7).
//!
//! On `noble` (Linux 6.8+, AppArmor 4.0) with
//! `kernel.apparmor_restrict_unprivileged_userns=1`, an unprivileged `CLONE_NEWUSER` from an
//! unconfined process is denied with `EPERM`; the failure looks like a Slipfork bug. The rendered
//! profile grants `userns,` to every executable that performs `CLONE_NEWUSER` for a world — the
//! `slipfork`/`slipforkd`/`slipfork-world-init` binaries in `bin_dir`, host `/usr/bin/unshare`
//! (outer world `CLONE_NEWUSER` and `doctor`), and in-world `/.slipfork/bin/crun` (inner OCI
//! `CLONE_NEWUSER`) — under one leading-slash brace expression, because AppArmor 4.0 accepts a
//! single attachment expression per profile header. It deliberately contains no file or mount
//! rules so AppArmor keeps file/mount mediation inactive.
//!
//! `slipfork daemon unit --only apparmor` prints this as the `/etc/apparmor.d/slipfork` body.

#![forbid(unsafe_code)]

use std::path::Path;

/// The four-line `/etc/apparmor.d/slipfork` `userns` profile for the binaries in `bin_dir`.
pub fn render_userns_profile(bin_dir: &Path) -> String {
    let dir = bin_dir.display().to_string();
    let rel_bin_dir = dir.trim_matches('/');
    let bin_clause = if rel_bin_dir.is_empty() {
        "slipfork{,d,-world-init}".to_owned()
    } else {
        format!("{rel_bin_dir}/slipfork{{,d,-world-init}}")
    };
    format!(
        "abi <abi/4.0>,\nprofile slipfork /{{{bin_clause},usr/bin/unshare,.slipfork/bin/crun}} \
         flags=(unconfined) {{\n  userns,\n}}\n"
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
            "abi <abi/4.0>,\nprofile slipfork \
             /{home/dev/.local/bin/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun} \
             flags=(unconfined) {\n  userns,\n}\n"
        );
        assert_eq!(profile.lines().count(), 4);
    }

    #[test]
    fn the_root_bin_dir_renders_a_bare_bin_clause_without_a_double_slash() {
        let profile = render_userns_profile(Path::new("/"));
        assert_eq!(
            profile,
            "abi <abi/4.0>,\nprofile slipfork \
             /{slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun} \
             flags=(unconfined) {\n  userns,\n}\n"
        );
        assert!(!profile.contains("//"), "{profile}");
    }

    #[test]
    fn every_rendered_attachment_has_no_double_slash() {
        for bin_dir in [
            "/",
            "/usr/bin",
            "/home/dev/.local/bin",
            "/home/dev/.local/bin/",
        ] {
            let profile = render_userns_profile(Path::new(bin_dir));
            assert!(
                !profile.contains("//"),
                "bin_dir {bin_dir} rendered a `//` sequence: {profile}"
            );
        }
    }

    #[test]
    fn one_brace_expression_names_every_clone_newuser_executable() {
        let profile = render_userns_profile(Path::new("/usr/local/bin"));
        let header = profile.lines().nth(1).expect("profile header line");
        let attachment = header
            .strip_prefix("profile slipfork ")
            .and_then(|rest| rest.strip_suffix(" flags=(unconfined) {"))
            .expect("profile header carries one attachment expression");
        assert!(attachment.starts_with("/{"), "{attachment}");
        assert!(attachment.ends_with('}'), "{attachment}");
        assert_eq!(attachment.matches("/{").count(), 1, "{attachment}");

        // `{,d,-world-init}` expands to the three `bin_dir` executables.
        let bin_clause = "usr/local/bin/slipfork{,d,-world-init}";
        assert!(
            attachment.contains(&format!("/{{{bin_clause},")),
            "needle {bin_clause:?} not in {attachment}"
        );
        let (stem, group) = bin_clause.split_once('{').expect("brace group");
        let group = group.strip_suffix('}').expect("closed brace group");
        let names: Vec<String> = group
            .split(',')
            .map(|suffix| format!("/{stem}{suffix}"))
            .collect();
        assert_eq!(
            names,
            [
                "/usr/local/bin/slipfork",
                "/usr/local/bin/slipforkd",
                "/usr/local/bin/slipfork-world-init",
            ]
        );

        for executable in ["usr/bin/unshare,", ".slipfork/bin/crun}"] {
            assert!(
                attachment.contains(executable),
                "attachment {attachment} does not name {executable}"
            );
        }
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
