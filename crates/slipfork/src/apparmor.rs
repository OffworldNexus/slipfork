//! Ubuntu 24.04 LTS AppArmor `userns` profile rendering (`ADR-0010` §7).
//!
//! On `noble` (Linux 6.8+, AppArmor 4.0) with
//! `kernel.apparmor_restrict_unprivileged_userns=1`, an unprivileged `CLONE_NEWUSER` from an
//! unconfined process is denied with `EPERM`; the failure looks like a Slipfork bug. The rendered
//! profile grants `userns,` to every executable that performs `CLONE_NEWUSER` for a world — the
//! `slipfork`/`slipforkd`/`slipfork-world-init` binaries in every installed `--bin-dir`, host
//! `/usr/bin/unshare` (outer world `CLONE_NEWUSER` and `doctor`), and in-world
//! `/.slipfork/bin/crun` (inner OCI `CLONE_NEWUSER`) — under one leading-slash brace expression,
//! because AppArmor 4.0 accepts a single attachment expression per profile header. It deliberately
//! contains no file or mount rules so AppArmor keeps file/mount mediation inactive.
//!
//! The path is interpolated into a profile that `apparmor_parser -r` loads **as root**, so
//! [`render_userns_profile`] validates every directory first: it must be absolute and may not
//! contain `%`, whitespace/control characters, `{`, `}`, `,`, `"`, or `\`. A newline or brace would
//! otherwise inject profile lines or widen the `userns` grant.
//!
//! `slipfork daemon unit --only apparmor` and `slipfork daemon install` render these exact bytes.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

/// The four-line `/etc/apparmor.d/slipfork` `userns` profile for the binaries in `bin_dirs`.
///
/// Every directory becomes one flat `{rel_dir}/slipfork{,d,-world-init}` alternative inside the
/// single brace expression; directories are never nested and duplicates keep their given order.
/// Fails closed with one line when any directory is unsafe (see [`validate_bin_dir`]).
pub fn render_userns_profile(bin_dirs: &[PathBuf]) -> Result<String, String> {
    let mut alternatives: Vec<String> = Vec::with_capacity(bin_dirs.len() + 2);
    for bin_dir in bin_dirs {
        alternatives.push(bin_clause(bin_dir)?);
    }
    alternatives.push("usr/bin/unshare".to_owned());
    alternatives.push(".slipfork/bin/crun".to_owned());
    Ok(format!(
        "abi <abi/4.0>,\nprofile slipfork /{{{}}} flags=(unconfined) {{\n  userns,\n}}\n",
        alternatives.join(",")
    ))
}

/// Validate one `--bin-dir` before it reaches `/etc/apparmor.d/slipfork`.
///
/// The profile is loaded as root, so an absolute path is required and every character that can
/// change the profile grammar (`%`, whitespace/control, `{`, `}`, `,`, `"`, `\`) is refused.
pub fn validate_bin_dir(bin_dir: &Path) -> Result<(), String> {
    let display = bin_dir.display().to_string().escape_default().to_string();
    if !bin_dir.is_absolute() {
        return Err(format!(
            "invalid --bin-dir '{display}': it must be an absolute path"
        ));
    }
    let Some(text) = bin_dir.to_str() else {
        return Err(format!(
            "invalid --bin-dir '{display}': the path must be valid UTF-8"
        ));
    };
    let display = text.escape_default().to_string();
    if text.is_empty() {
        return Err("invalid --bin-dir: the path is empty".to_owned());
    }
    for forbidden in ['%', '{', '}', ',', '"', '\\'] {
        if text.contains(forbidden) {
            return Err(format!(
                "invalid --bin-dir '{display}': the character '{forbidden}' is forbidden in an AppArmor profile path"
            ));
        }
    }
    if text
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(format!(
            "invalid --bin-dir '{display}': whitespace and control characters are forbidden in an AppArmor profile path"
        ));
    }
    Ok(())
}

/// One `bin_dir` alternative (`<rel_dir>/slipfork{,d,-world-init}`), never with a leading slash.
fn bin_clause(bin_dir: &Path) -> Result<String, String> {
    validate_bin_dir(bin_dir)?;
    let text = bin_dir
        .to_str()
        .expect("validate_bin_dir guarantees valid UTF-8");
    let rel_bin_dir = text.trim_matches('/');
    if rel_bin_dir.is_empty() {
        Ok("slipfork{,d,-world-init}".to_owned())
    } else {
        Ok(format!("{rel_bin_dir}/slipfork{{,d,-world-init}}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn render(paths: &[&str]) -> String {
        render_userns_profile(&dirs(paths)).expect("valid bin dirs")
    }

    #[test]
    fn the_profile_is_exactly_four_lines_with_the_userns_rule() {
        let profile = render(&["/home/dev/.local/bin"]);
        assert_eq!(
            profile,
            "abi <abi/4.0>,\nprofile slipfork \
             /{home/dev/.local/bin/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun} \
             flags=(unconfined) {\n  userns,\n}\n"
        );
        assert_eq!(profile.lines().count(), 4);
    }

    #[test]
    fn every_bin_dir_is_one_flat_alternative_in_the_single_brace_expression() {
        let profile = render(&["/usr/local/bin", "/tmp/ux/bin"]);
        assert_eq!(
            profile,
            "abi <abi/4.0>,\nprofile slipfork \
             /{usr/local/bin/slipfork{,d,-world-init},tmp/ux/bin/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun} \
             flags=(unconfined) {\n  userns,\n}\n"
        );
        assert_eq!(profile.lines().count(), 4);
        assert_eq!(profile.matches("/{").count(), 1, "one brace expression");
        assert!(!profile.contains("{{"), "never nested braces: {profile}");
    }

    #[test]
    fn the_root_bin_dir_renders_a_bare_bin_clause_without_a_double_slash() {
        let profile = render(&["/"]);
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
            let profile = render(&[bin_dir]);
            assert!(
                !profile.contains("//"),
                "bin_dir {bin_dir} rendered a `//` sequence: {profile}"
            );
        }
    }

    #[test]
    fn unsafe_bin_dirs_fail_closed_before_any_profile_is_rendered() {
        for refused in [
            "",
            "relative/bin",
            "/tmp/a\nprofile x,",
            "/tmp/{wide}",
            "/tmp/a,b",
            "/tmp/a b",
            "/tmp/a\\b",
            "/tmp/a\"b",
            "/tmp/%i",
        ] {
            let error = render_userns_profile(&dirs(&[refused]))
                .expect_err("unsafe --bin-dir must be refused");
            assert!(
                error.starts_with("invalid --bin-dir"),
                "one-line refusal expected, got: {error}"
            );
            assert!(!error.contains('\n'), "the refusal is one line: {error}");
        }
        assert!(render_userns_profile(&[]).is_ok(), "no dirs still renders");
    }

    #[test]
    fn one_brace_expression_names_every_clone_newuser_executable() {
        let profile = render(&["/usr/local/bin"]);
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
        let profile = render(&["/usr/bin"]);
        for rule in ["/**", "mount", "file,", "network,"] {
            assert!(
                !profile.contains(rule),
                "the profile must keep file/mount mediation inactive: {profile}"
            );
        }
    }
}
