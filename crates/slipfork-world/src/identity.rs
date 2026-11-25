//! The single effective container identity: a `USER`/`--chown` spec resolved against one rootfs.
//!
//! Contract: `Identity` is *who runs inside the container* (uid/gid, Docker capabilities, home and
//! account); `pod_env::PodIdentityMapping` is *how that identity maps into the world user namespace
//! and the pod spec*. This module is the only owner of both halves of the resolution rule.
//!
//! Build steps and service pods both express exactly one container uid/gid mapped to the invoking
//! host identity, so this module owns the resolution rules, Docker's default capability set, and
//! the image's own account database. Nothing here guesses a home or a shell: [`Identity::account`]
//! reads `<rootfs>/etc/passwd`/`/etc/group`, the same files the image itself ships.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Container ids one process runs as (`0:0` until `USER` names another account).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// Container uid mapped to the invoking host uid.
    pub uid: u32,
    /// Container gid mapped to the invoking host gid.
    pub gid: u32,
}

/// One account as the image's own `/etc/passwd`/`/etc/group` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Login name (also `USER`/`LOGNAME`).
    pub user: String,
    /// Primary group name.
    pub group: String,
    /// Absolute home directory.
    pub home: String,
    /// Login shell.
    pub shell: String,
}

/// Docker's default capability set for a process that runs as container root.
const ROOT_CAPABILITIES: [&str; 14] = [
    "CAP_CHOWN",
    "CAP_DAC_OVERRIDE",
    "CAP_FOWNER",
    "CAP_FSETID",
    "CAP_KILL",
    "CAP_SETGID",
    "CAP_SETUID",
    "CAP_SETPCAP",
    "CAP_NET_BIND_SERVICE",
    "CAP_NET_RAW",
    "CAP_SYS_CHROOT",
    "CAP_MKNOD",
    "CAP_AUDIT_WRITE",
    "CAP_SETFCAP",
];

impl Identity {
    /// The root identity every image starts with.
    pub const ROOT: Self = Self { uid: 0, gid: 0 };

    /// Resolves `spec` (`name`, `name:group`, `uid`, `uid:gid`) against `rootfs`.
    ///
    /// Numeric ids are accepted as written; a name needs a matching entry in the rootfs's own
    /// `/etc/passwd` (and `/etc/group` when a group name follows the colon), so an unknown account
    /// fails closed exactly where Docker does.
    pub fn resolve(rootfs: &Path, spec: Option<&str>) -> Result<Self, IdentityError> {
        let Some(spec) = spec else {
            return Ok(Self::ROOT);
        };
        let (user, group) = match spec.split_once(':') {
            Some((user, group))
                if !user.is_empty() && !group.is_empty() && !group.contains(':') =>
            {
                (user, Some(group))
            }
            Some(_) => return Err(IdentityError::invalid(spec)),
            None if spec.is_empty() => return Err(IdentityError::invalid(spec)),
            None => (spec, None),
        };
        let (uid, primary_gid) = match user.parse::<u32>() {
            Ok(uid) => (uid, 0),
            Err(_) => {
                let fields = account_fields(rootfs, "passwd", user)?;
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
                Err(_) => number(&account_fields(rootfs, "group", group)?, 2, "group", group)?,
            },
        };
        Ok(Self { uid, gid })
    }

    /// Docker's default capabilities inside the user namespace: the full set only for root.
    #[must_use]
    pub fn capabilities(self) -> &'static [&'static str] {
        if self.uid == 0 {
            &ROOT_CAPABILITIES
        } else {
            &[]
        }
    }

    /// The account this identity names in the image's own files, or `None` when no entry exists.
    ///
    /// Root always resolves (a `root` fallback describes `/root`) so an image that omits
    /// `/etc/passwd` still runs its `CMD` as root; any other uid needs a real image entry.
    #[must_use]
    pub fn account(&self, rootfs: &Path) -> Option<Account> {
        if let Some(fields) = lookup(rootfs, "passwd", |fields| {
            field(fields, 2).and_then(|value| value.parse().ok()) == Some(self.uid)
        }) {
            let user = field(&fields, 0).unwrap_or("root").to_owned();
            let home = field(&fields, 5).unwrap_or("/root").to_owned();
            let shell = field(&fields, 6).unwrap_or("/bin/sh").to_owned();
            let group = lookup(rootfs, "group", |fields| {
                field(fields, 2).and_then(|value| value.parse().ok()) == Some(self.gid)
            })
            .and_then(|fields| field(&fields, 0).map(str::to_owned))
            .unwrap_or_else(|| self.gid.to_string());
            return Some(Account {
                user,
                group,
                home,
                shell,
            });
        }
        (self.uid == 0).then(|| Account {
            user: "root".to_owned(),
            group: "root".to_owned(),
            home: "/root".to_owned(),
            shell: "/bin/sh".to_owned(),
        })
    }
}

/// A resolution failure, rendered as one cause line plus a `next:` remedy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    /// The image has no account with that name.
    UnknownAccount {
        /// The account name exactly as the spec wrote it.
        account: String,
        /// The image file the lookup searched (`passwd` or `group`).
        file: &'static str,
    },
    /// A spec with more than one colon or an empty part.
    Invalid {
        /// The malformed spec exactly as written.
        spec: String,
    },
}

impl IdentityError {
    fn invalid(spec: &str) -> Self {
        Self::Invalid {
            spec: spec.to_owned(),
        }
    }

    /// The raw one-line cause, without a `next:` remedy.
    #[must_use]
    pub fn cause(&self) -> String {
        match self {
            Self::UnknownAccount { account, file } => {
                format!("the image has no account `{account}` in `/etc/{file}`")
            }
            Self::Invalid { spec } => format!("`{spec}` is not a valid user spec"),
        }
    }

    /// The one-line remedy that tells the caller what to write instead.
    #[must_use]
    pub fn remedy(&self) -> String {
        match self {
            Self::UnknownAccount { .. } => {
                "create it in an earlier `RUN` (e.g. `useradd`) or use a numeric uid:gid".to_owned()
            }
            Self::Invalid { .. } => {
                "write `<name>[:<group>]` or a numeric `<uid>[:<gid>]`".to_owned()
            }
        }
    }
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\nnext: {}", self.cause(), self.remedy())
    }
}

impl std::error::Error for IdentityError {}

/// Field `index` of a colon-separated entry, when present and non-empty.
fn field(fields: &[String], index: usize) -> Option<&str> {
    fields.get(index).map(String::as_str)
}

/// The entry in `<rootfs>/etc/<file>` whose fields satisfy `matches`.
fn lookup(
    rootfs: &Path,
    file: &'static str,
    matches: impl Fn(&[String]) -> bool,
) -> Option<Vec<String>> {
    let path = rooted(rootfs, &format!("/etc/{file}"));
    let contents = fs::read_to_string(path).ok()?;
    contents
        .lines()
        .map(|line| line.split(':').map(str::to_owned).collect::<Vec<String>>())
        .find(|fields| matches(fields))
}

/// The colon-separated fields of `name`'s entry in `<rootfs>/etc/<file>`.
fn account_fields(
    rootfs: &Path,
    file: &'static str,
    name: &str,
) -> Result<Vec<String>, IdentityError> {
    lookup(rootfs, file, |fields| {
        fields.first().map(String::as_str) == Some(name)
    })
    .ok_or_else(|| IdentityError::UnknownAccount {
        account: name.to_owned(),
        file,
    })
}

/// Field `index` of a passwd/group entry, parsed as a numeric id.
fn number(
    fields: &[String],
    index: usize,
    file: &'static str,
    name: &str,
) -> Result<u32, IdentityError> {
    fields
        .get(index)
        .and_then(|field| field.parse().ok())
        .ok_or_else(|| IdentityError::UnknownAccount {
            account: name.to_owned(),
            file,
        })
}

fn rooted(rootfs: &Path, destination: &str) -> PathBuf {
    rootfs.join(destination.trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{Account, Identity, IdentityError};

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
    fn names_and_numbers_resolve_against_the_image_rootfs() {
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
            matches!(&error, IdentityError::UnknownAccount { file: "passwd", .. }),
            "the passwd file is named: {error}"
        );
        assert!(
            error.cause().contains("`app`") && error.cause().contains("/etc/passwd"),
            "the cause names the account and its file: {}",
            error.cause()
        );
        assert!(
            error.remedy().contains("useradd"),
            "the remedy points at useradd"
        );
        assert!(
            error.to_string().contains("\nnext: "),
            "one cause line plus a remedy: {error}"
        );

        let error = Identity::resolve(&root, Some("root:nogroup")).expect_err("no such group");
        assert!(
            matches!(&error, IdentityError::UnknownAccount { file: "group", .. }),
            "the group file is named: {error}"
        );

        for spec in ["app:staff:extra", "app:", ":staff", ""] {
            let error = Identity::resolve(&root, Some(spec))
                .expect_err("a malformed spec must fail closed");
            assert!(
                matches!(&error, IdentityError::Invalid { .. }),
                "`{spec}` is malformed: {error}"
            );
            assert!(
                error.to_string().contains("next:"),
                "`{spec}` carries a remedy: {error}"
            );
        }

        let _ = fs::remove_dir_all(&root);
    }

    /// The account comes from the image's own files; root is always describable, an unknown uid is
    /// not, and the capability set is Docker's default for root only.
    #[test]
    fn accounts_and_capabilities_follow_the_image_and_uid() {
        let root = image(
            "account",
            "root:x:0:0:root:/root:/bin/bash\n\
             app:x:1001:1002:App:/srv/app:/bin/zsh\n",
            "root:x:0:\napp:x:1002:\n",
        );

        assert_eq!(
            Identity::ROOT.account(&root),
            Some(Account {
                user: "root".to_owned(),
                group: "root".to_owned(),
                home: "/root".to_owned(),
                shell: "/bin/bash".to_owned(),
            })
        );
        assert_eq!(
            Identity {
                uid: 1001,
                gid: 1002
            }
            .account(&root),
            Some(Account {
                user: "app".to_owned(),
                group: "app".to_owned(),
                home: "/srv/app".to_owned(),
                shell: "/bin/zsh".to_owned(),
            })
        );
        assert!(
            Identity {
                uid: 4242,
                gid: 4242
            }
            .account(&root)
            .is_none(),
            "an unknown uid has no account, so the caller writes a synthetic one"
        );

        let empty = lab("no-passwd");
        assert_eq!(
            Identity::ROOT.account(&empty).map(|account| account.home),
            Some("/root".to_owned()),
            "root is describable even when the image ships no passwd"
        );

        assert_eq!(Identity::ROOT.capabilities().len(), 14);
        assert!(Identity::ROOT.capabilities().contains(&"CAP_CHOWN"));
        assert!(
            Identity {
                uid: 1000,
                gid: 1000
            }
            .capabilities()
            .is_empty(),
            "a non-root USER drops Docker's default caps"
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&empty);
    }
}
