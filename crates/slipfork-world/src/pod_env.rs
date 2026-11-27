//! Effective pod identity, its home directory, and the environment baseline.
//!
//! Two layers, one owner each: [`Identity`] (in `slipfork_world::identity`) is *who runs inside
//! the container* — uid/gid, capabilities, and the image account; [`PodIdentityMapping`] is *how
//! that identity maps into the world user namespace and this pod spec* — the single `1:1` mapping
//! onto the invoking host uid/gid plus the synthetic account files it implies.
//!
//! A pod runs as the image's resolved `USER` (default container root) mapped onto the invoking
//! host uid. The image's own `/etc/passwd`/`/etc/group` stay authoritative; Slipfork only writes a
//! synthetic pair when an identity has no entry there (the historical invoking-host identity, or a
//! numeric `USER`).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::error::{Error, Result};
use crate::identity::{Account, Identity};
use crate::pod::{PodHost, PodSpec};

const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// In-pod home directory of the unprivileged legacy pod user.
pub(crate) const POD_HOME: &str = "/home/slipfork";
/// Home directory of uid 0.
pub(crate) const ROOT_HOME: &str = "/root";

/// Colour hints for the programs we run.
///
/// A pod's stdout is a pipe, never a terminal, so a program that keys off `isatty` stays plain
/// (accepted: we never fake a TTY). Tools honouring the de-facto `CLICOLOR_FORCE`/`CLICOLOR`
/// convention still colourise, and an explicit image/`Forkfile` value always wins.
///
/// `FORCE_COLOR` is deliberately absent: it is a Node/`chalk` convention that also makes JS
/// libraries override `NO_COLOR`, so it would change library behaviour for no non-Node win.
pub const COLOUR_HINTS: [(&str, &str); 4] = [
    ("TERM", "xterm-256color"),
    ("CLICOLOR", "1"),
    ("CLICOLOR_FORCE", "1"),
    ("COLORTERM", "truecolor"),
];

/// The effective identity of one pod plus the account and synthetic-file decision it implies.
///
/// This is the *mapping* layer: the container identity (`uid`/`gid`) is rendered as one `1:1`
/// user-namespace mapping onto the invoking host uid/gid, and its home/login come from the image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PodIdentityMapping {
    /// Container uid/gid mapped onto the invoking host uid/gid.
    pub(crate) identity: Identity,
    /// Login account rendered into `HOME`/`USER`/`LOGNAME` and the synthetic files.
    pub(crate) account: Account,
    /// `true` when the bundle must shadow `/etc/passwd`/`/etc/group` with synthetic entries.
    pub(crate) synthetic: bool,
}

impl PodIdentityMapping {
    /// Resolves a spec: an explicit image identity uses the image's account; `None` keeps the
    /// historical invoking-host identity with its synthetic `slipfork` account.
    pub(crate) fn resolve(spec: &PodSpec, host: &PodHost) -> Self {
        let identity = spec.identity.unwrap_or(Identity {
            uid: host.uid,
            gid: host.gid,
        });
        let account = if spec.identity.is_none() {
            None
        } else {
            identity.account(&spec.rootfs)
        };
        let (account, synthetic) = match account {
            Some(account) => (account, false),
            None => (fallback(identity), true),
        };
        Self {
            identity,
            account,
            synthetic,
        }
    }
}

/// The account a synthetic entry describes: root keeps `/root`, everything else `slipfork`.
fn fallback(identity: Identity) -> Account {
    if identity.uid == 0 {
        Account {
            user: "root".to_owned(),
            group: "root".to_owned(),
            home: ROOT_HOME.to_owned(),
            shell: "/sbin/nologin".to_owned(),
        }
    } else {
        Account {
            user: "slipfork".to_owned(),
            group: "slipfork".to_owned(),
            home: POD_HOME.to_owned(),
            shell: "/bin/sh".to_owned(),
        }
    }
}

/// `true` when the legacy invoking-host identity still needs Slipfork's writable home injected.
///
/// A user-declared `HOME` other than `/root` always wins; uid 0 keeps the image's `HOME`.
pub(crate) fn needs_default_home(uid: u32, current: Option<&str>) -> bool {
    uid != 0 && current.is_none_or(|value| value.is_empty() || value == ROOT_HOME)
}

/// Renders the synthetic `/etc/passwd`: one entry for the effective identity plus root.
pub(crate) fn passwd_contents(account: &Account, uid: u32, gid: u32) -> String {
    let mut contents = String::new();
    if uid != 0 {
        contents.push_str("root:x:0:0:root:/root:/sbin/nologin\n");
    }
    contents.push_str(&format!(
        "{}:x:{}:{}:{}:{}:{}\n",
        account.user, uid, gid, account.user, account.home, account.shell
    ));
    contents
}

/// Renders the synthetic `/etc/group`: the account's primary group plus root.
pub(crate) fn group_contents(account: &Account, gid: u32) -> String {
    let mut contents = String::new();
    if gid != 0 {
        contents.push_str("root:x:0:\n");
    }
    contents.push_str(&format!("{}:x:{}\n", account.group, gid));
    contents
}

/// Creates a writable, world-traversable home inside the pod rootfs; idempotent.
///
/// The bundle writer runs as the invoking identity and creates this `0755` directory with it, so
/// the pod (identity-mapped to the caller) already owns a writable home. A foreign-owned home
/// means the caller or composition root supplied a mismatched identity; it is a bug surfaced
/// upstream, never repaired here.
pub(crate) fn materialise_home(home: &Path) -> Result<()> {
    fs::create_dir_all(home).map_err(|source| Error::io("create", home, source))?;
    fs::set_permissions(home, fs::Permissions::from_mode(0o755))
        .map_err(|source| Error::io("chmod", home, source))?;
    Ok(())
}

/// The image `ENV HOME` value inherited verbatim from the image config, if any.
fn inherited_home(inherited_env: &[String]) -> Option<&str> {
    inherited_env
        .iter()
        .find_map(|pair| pair.strip_prefix("HOME="))
}

/// `true` when the bundle must materialise the account's home directory.
///
/// The legacy invoking-host identity follows the historical rule on the *image* `HOME`; an image
/// identity only creates a home the image does not already ship, so an image-owned `0700 /root`
/// is never re-created or re-chmodded.
pub(crate) fn needs_home(spec: &PodSpec, pod: &PodIdentityMapping, host_uid: u32) -> bool {
    if spec.identity.is_none() {
        needs_default_home(host_uid, inherited_home(&spec.inherited_env))
    } else {
        !spec
            .rootfs
            .join(pod.account.home.trim_start_matches('/'))
            .exists()
    }
}

/// Image env first, then the pod's own env, then the resolved identity's `HOME`/`USER`/`LOGNAME`,
/// then the colour hints the image did not declare.
///
/// The legacy invoking-host identity (a resource pod) keeps its historical env exactly: a custom
/// non-empty `HOME` wins, an absent/empty/`/root` one becomes the writable in-pod home, and no
/// login variables are invented. An image identity additionally names `USER`/`LOGNAME`; a
/// non-empty declared or inherited `HOME` still wins.
pub(crate) fn merged_env(spec: &PodSpec, pod: &PodIdentityMapping, host_uid: u32) -> Vec<String> {
    let mut ordered: Vec<(String, String)> = Vec::new();
    // Whether the image or `Forkfile` already declared `key`; taken before defaults are inserted.
    let declares = |key: &str| {
        spec.inherited_env
            .iter()
            .any(|pair| pair.split_once('=').is_some_and(|(name, _)| name == key))
            || spec.env.iter().any(|entry| entry.key == key)
    };
    let mut upsert = |key: &str, value: &str| {
        if let Some(slot) = ordered.iter_mut().find(|(existing, _)| existing == key) {
            slot.1 = value.to_owned();
        } else {
            ordered.push((key.to_owned(), value.to_owned()));
        }
    };
    upsert("PATH", DEFAULT_PATH);
    for pair in &spec.inherited_env {
        if let Some((key, value)) = pair.split_once('=') {
            upsert(key, value);
        }
    }
    for entry in &spec.env {
        upsert(&entry.key, &entry.value);
    }
    let declared_home = spec
        .env
        .iter()
        .find(|entry| entry.key == "HOME")
        .map(|entry| entry.value.as_str());
    let current_home = declared_home.or_else(|| inherited_home(&spec.inherited_env));
    let mut wanted = vec![("HOME", pod.account.home.as_str())];
    if spec.identity.is_some() {
        wanted.push(("USER", pod.account.user.as_str()));
        wanted.push(("LOGNAME", pod.account.user.as_str()));
    }
    for (key, value) in wanted {
        if key == "HOME" {
            let custom = current_home.is_some_and(|home| !home.is_empty() && home != ROOT_HOME);
            if custom {
                continue;
            }
            // The legacy invoking-host identity keeps its historical uid-0 rule; an image
            // identity always names its own account home.
            if spec.identity.is_none() && !needs_default_home(host_uid, current_home) {
                continue;
            }
        } else if spec.env.iter().any(|entry| entry.key == key) {
            continue;
        }
        upsert(key, value);
    }
    // Encourage colour without a TTY: only keys the image/Forkfile did not declare, and a
    // declared `NO_COLOR` opts the pod out of every hint.
    if !declares("NO_COLOR") {
        for (key, value) in COLOUR_HINTS {
            if !declares(key) {
                upsert(key, value);
            }
        }
    }
    ordered
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect()
}

#[cfg(test)]
mod tests;
