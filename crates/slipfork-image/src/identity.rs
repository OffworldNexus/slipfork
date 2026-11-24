//! Effective build identity: `USER`/`COPY --chown` resolved against the step's own rootfs.
//!
//! A build step is its own container with a single uid/gid mapping, so only one identity is
//! representable at a time. The account names themselves live in the image, never in a synthetic
//! host file: `useradd` in an earlier `RUN` must be visible to a later `USER app` step.

use std::fs;
use std::path::Path;

use crate::error::{Error, Result};
use crate::runtime::rooted;

/// Container ids a step runs as (`0:0` until `USER` names another account).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Identity {
    /// Container uid mapped to the invoking host uid.
    pub(crate) uid: u32,
    /// Container gid mapped to the invoking host gid.
    pub(crate) gid: u32,
}

impl Identity {
    /// The root identity every stage starts with.
    pub(crate) const ROOT: Self = Self { uid: 0, gid: 0 };

    /// Resolves `spec` (`name`, `name:group`, `uid`, `uid:gid`) against `rootfs`.
    ///
    /// Numeric ids are accepted as written; a name needs a matching entry in the rootfs's own
    /// `/etc/passwd` (and `/etc/group` when a group name follows the colon), so an unknown account
    /// fails closed exactly where Docker does.
    pub(crate) fn resolve(rootfs: &Path, spec: Option<&str>) -> Result<Self> {
        let Some(spec) = spec else {
            return Ok(Self::ROOT);
        };
        let (user, group) = match spec.split_once(':') {
            Some((user, group))
                if !user.is_empty() && !group.is_empty() && !group.contains(':') =>
            {
                (user, Some(group))
            }
            Some(_) => return Err(invalid(spec)),
            None if spec.is_empty() => return Err(invalid(spec)),
            None => (spec, None),
        };
        let (uid, primary_gid) = match user.parse::<u32>() {
            Ok(uid) => (uid, 0),
            Err(_) => {
                let fields = account(rootfs, "passwd", user)?;
                (
                    number(&fields, 2, "passwd", user)?,
                    number(&fields, 3, "passwd", user)?,
                )
            }
        };
        let gid = match group {
            None => primary_gid,
            Some(group) => match group.parse::<u32>() {
                Ok(gid) => gid,
                Err(_) => number(&account(rootfs, "group", group)?, 2, "group", group)?,
            },
        };
        Ok(Self { uid, gid })
    }
}

/// The colon-separated fields of `name`'s entry in `<rootfs>/etc/<file>`.
fn account(rootfs: &Path, file: &'static str, name: &str) -> Result<Vec<String>> {
    let path = rooted(rootfs, &format!("/etc/{file}"));
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::UnknownAccount {
                account: name.to_owned(),
                file,
            });
        }
        Err(source) => return Err(Error::io("read", &path, source)),
    };
    contents
        .lines()
        .map(|line| line.split(':').collect::<Vec<_>>())
        .find(|fields| fields.first() == Some(&name))
        .map(|fields| fields.iter().map(|field| (*field).to_owned()).collect())
        .ok_or_else(|| Error::UnknownAccount {
            account: name.to_owned(),
            file,
        })
}

/// Field `index` of a passwd/group entry, parsed as a numeric id.
fn number(fields: &[String], index: usize, file: &'static str, name: &str) -> Result<u32> {
    fields
        .get(index)
        .and_then(|field| field.parse().ok())
        .ok_or_else(|| Error::UnknownAccount {
            account: name.to_owned(),
            file,
        })
}

/// A `USER`/`--chown` spec with more than one colon or an empty part fails closed.
fn invalid(spec: &str) -> Error {
    Error::StepFailed {
        line: 0,
        phase: "user",
        instruction: String::new().into(),
        message: format!("`{spec}` is not a valid user spec"),
        exit_code: None,
        runtime: None,
        next: "write `<name>[:<group>]` or a numeric `<uid>[:<gid>]`".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::Identity;
    use crate::error::Error;

    fn lab(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let root = PathBuf::from("/var/tmp").join(format!(
            "slipfork-identity-{prefix}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("etc")).expect("the lab needs real disk under /var/tmp");
        root
    }

    fn image(prefix: &str, passwd: &str, group: &str) -> PathBuf {
        let root = lab(prefix);
        fs::write(root.join("etc/passwd"), passwd).expect("write the image passwd");
        fs::write(root.join("etc/group"), group).expect("write the image group");
        root
    }

    /// No `USER` is root without reading `/etc/passwd` at all; numeric specs are accepted as
    /// written, and names resolve their primary or named group from the image itself.
    #[test]
    fn names_and_numbers_resolve_against_the_step_rootfs() {
        let empty = lab("empty");
        assert_eq!(
            Identity::resolve(&empty, None).expect("no USER is root"),
            Identity { uid: 0, gid: 0 },
            "a missing `/etc/passwd` never blocks the default root identity"
        );

        let root = image(
            "accounts",
            "root:x:0:0:root:/root:/bin/sh\n\
             app:x:1001:1002:App:/home/app:/bin/sh\n\
             postgres:x:105:109::/var/lib/postgresql:/bin/bash\n",
            "root:x:0:\napp:x:1002:\nstaff:x:50:\npostgres:x:109:\n",
        );

        assert_eq!(
            Identity::resolve(&root, Some("app")).expect("a passwd name"),
            Identity {
                uid: 1001,
                gid: 1002
            }
        );
        assert_eq!(
            Identity::resolve(&root, Some("app:staff")).expect("a group name"),
            Identity { uid: 1001, gid: 50 }
        );
        assert_eq!(
            Identity::resolve(&root, Some("postgres:postgres")).expect("matching names"),
            Identity { uid: 105, gid: 109 }
        );
        assert_eq!(
            Identity::resolve(&root, Some("1001")).expect("a bare uid keeps gid 0"),
            Identity { uid: 1001, gid: 0 }
        );
        assert_eq!(
            Identity::resolve(&root, Some("1001:1001")).expect("numeric pairs"),
            Identity {
                uid: 1001,
                gid: 1001
            }
        );
        assert_eq!(
            Identity::resolve(&root, Some("1001:staff")).expect("numeric uid, named group"),
            Identity { uid: 1001, gid: 50 }
        );

        let _ = fs::remove_dir_all(&empty);
        let _ = fs::remove_dir_all(&root);
    }

    /// An account the image never created fails closed with the typed remedy, and a malformed spec
    /// is a one-line step failure rather than a silent mis-ownership.
    #[test]
    fn unknown_accounts_and_malformed_specs_fail_closed() {
        let root = image("missing", "root:x:0:0:root:/root:/bin/sh\n", "root:x:0:\n");

        let error = Identity::resolve(&root, Some("app")).expect_err("no such account");
        assert!(
            matches!(&error, Error::UnknownAccount { file: "passwd", .. }),
            "the passwd file is named: {error}"
        );
        assert!(
            error.cause().contains("`app`") && error.cause().contains("/etc/passwd"),
            "the cause names the account and its file: {}",
            error.cause()
        );
        assert!(
            error.remedy().is_some_and(|next| next.contains("useradd")),
            "the remedy points at useradd"
        );

        let error = Identity::resolve(&root, Some("root:nogroup")).expect_err("no such group");
        assert!(
            matches!(&error, Error::UnknownAccount { file: "group", .. }),
            "the group file is named: {error}"
        );

        for spec in ["app:staff:extra", "app:", ":staff", ""] {
            let error = Identity::resolve(&root, Some(spec))
                .expect_err("a malformed spec must fail closed");
            assert!(
                matches!(&error, Error::StepFailed { .. }),
                "`{spec}` is malformed: {error}"
            );
            assert!(
                error.to_string().contains("next:"),
                "`{spec}` carries a remedy: {error}"
            );
        }

        let _ = fs::remove_dir_all(&root);
    }
}
