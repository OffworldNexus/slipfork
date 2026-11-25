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

/// Image env first, then the pod's own env, then the resolved identity's `HOME`/`USER`/`LOGNAME`.
///
/// The legacy invoking-host identity (a resource pod) keeps its historical env exactly: a custom
/// non-empty `HOME` wins, an absent/empty/`/root` one becomes the writable in-pod home, and no
/// login variables are invented. An image identity additionally names `USER`/`LOGNAME`; a
/// non-empty declared or inherited `HOME` still wins.
pub(crate) fn merged_env(spec: &PodSpec, pod: &PodIdentityMapping, host_uid: u32) -> Vec<String> {
    let mut ordered: Vec<(String, String)> = Vec::new();
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
    ordered
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;
    use crate::pod::{PodEnv, write_bundle};

    fn host(uid: u32, gid: u32) -> PodHost {
        PodHost {
            crun: "/usr/bin/crun".into(),
            resolv_conf: None,
            uid,
            gid,
        }
    }

    fn spec(rootfs: PathBuf, inherited_env: Vec<String>, env: Vec<PodEnv>) -> PodSpec {
        PodSpec {
            id: "api".to_owned(),
            rootfs,
            workdir: "/project".to_owned(),
            argv: vec!["true".to_owned()],
            inherited_env,
            env,
            binds: Vec::new(),
            files: Vec::new(),
            identity: None,
        }
    }

    fn env_value(env: &[String], key: &str) -> Option<String> {
        env.iter()
            .find_map(|pair| pair.strip_prefix(&format!("{key}=")))
            .map(str::to_owned)
    }

    fn seeded_image(disk: &TempDir, passwd: &str, group: &str) -> PathBuf {
        let rootfs = disk.path().join("rootfs");
        fs::create_dir_all(rootfs.join("etc")).expect("seed the image etc");
        fs::write(rootfs.join("etc/passwd"), passwd).expect("write passwd");
        fs::write(rootfs.join("etc/group"), group).expect("write group");
        rootfs
    }

    #[test]
    fn passwd_and_group_describe_the_effective_identity_without_shadowing_the_image() {
        let account = Account {
            user: "slipfork".to_owned(),
            group: "slipfork".to_owned(),
            home: POD_HOME.to_owned(),
            shell: "/bin/sh".to_owned(),
        };
        let passwd = passwd_contents(&account, 1000, 1001);
        assert!(passwd.contains("slipfork:x:1000:1001:slipfork:/home/slipfork:/bin/sh\n"));
        assert!(passwd.contains("root:x:0:0:root:/root:/sbin/nologin\n"));
        assert_eq!(
            group_contents(&account, 1001),
            "root:x:0:\nslipfork:x:1001\n"
        );

        let root = fallback(Identity::ROOT);
        assert!(passwd_contents(&root, 0, 0).contains("root:x:0:0:root:/root:/sbin/nologin\n"));
        assert_eq!(group_contents(&root, 0), "root:x:0\n");
    }

    #[test]
    fn home_resolution_covers_unset_empty_root_custom_and_root_user() {
        assert!(needs_default_home(1000, None));
        assert!(needs_default_home(1000, Some("")));
        assert!(needs_default_home(1000, Some("/root")));
        assert!(!needs_default_home(1000, Some("/opt/app")));
        assert!(!needs_default_home(0, None));
        assert!(!needs_default_home(0, Some("/root")));
    }

    #[test]
    fn legacy_env_injects_the_writable_home_unless_a_custom_one_is_declared() {
        let probe = TempDir::new_in("/var/tmp").expect("real disk for the rootfs probe");
        let owner = fs::metadata(probe.path()).expect("stat the probe dir");
        let host = host(owner.uid(), owner.gid());
        let env_of = |unset: PodSpec| {
            let pod = PodIdentityMapping::resolve(&unset, &host);
            merged_env(&unset, &pod, host.uid)
        };

        let unset = env_of(spec(probe.path().into(), Vec::new(), Vec::new()));
        assert_eq!(env_value(&unset, "HOME").as_deref(), Some(POD_HOME));
        assert_eq!(
            env_value(&unset, "USER"),
            None,
            "a resource pod invents no login variables"
        );
        assert_eq!(env_value(&unset, "LOGNAME"), None);

        let root = env_of(spec(
            probe.path().into(),
            vec!["HOME=/root".to_owned()],
            Vec::new(),
        ));
        assert_eq!(env_value(&root, "HOME").as_deref(), Some(POD_HOME));

        // A custom image `ENV HOME` still wins for the legacy identity.
        let image = env_of(spec(
            probe.path().into(),
            vec!["HOME=/opt/app".to_owned()],
            Vec::new(),
        ));
        assert_eq!(env_value(&image, "HOME").as_deref(), Some("/opt/app"));

        let empty = env_of(spec(
            probe.path().into(),
            Vec::new(),
            vec![PodEnv::new("HOME", "")],
        ));
        assert_eq!(
            env_value(&empty, "HOME").as_deref(),
            Some(POD_HOME),
            "an empty declared HOME still needs the writable in-pod home"
        );

        let custom = env_of(spec(
            probe.path().into(),
            vec!["HOME=/root".to_owned()],
            vec![PodEnv::new("HOME", "/opt/app")],
        ));
        assert_eq!(env_value(&custom, "HOME").as_deref(), Some("/opt/app"));
    }

    #[test]
    fn an_image_identity_names_its_login_and_an_empty_home_falls_back_to_the_account() {
        let disk = TempDir::new_in("/var/tmp").expect("real disk for the image rootfs");
        let rootfs = seeded_image(
            &disk,
            "root:x:0:0:root:/root:/bin/sh\napp:x:1001:1002:App:/srv/app:/bin/sh\n",
            "root:x:0:\napp:x:1002:\n",
        );
        let owner = fs::metadata(disk.path()).expect("stat the probe dir");
        let pod_host = host(owner.uid(), owner.gid());
        let env_of = |home: Option<&str>| {
            let mut image = spec(rootfs.clone(), Vec::new(), Vec::new());
            image.identity = Some(Identity {
                uid: 1001,
                gid: 1002,
            });
            if let Some(home) = home {
                image.env = vec![PodEnv::new("HOME", home)];
            }
            let pod = PodIdentityMapping::resolve(&image, &pod_host);
            merged_env(&image, &pod, pod_host.uid)
        };

        let account = env_of(None);
        assert_eq!(env_value(&account, "HOME").as_deref(), Some("/srv/app"));
        assert_eq!(env_value(&account, "USER").as_deref(), Some("app"));
        assert_eq!(env_value(&account, "LOGNAME").as_deref(), Some("app"));

        let empty = env_of(Some(""));
        assert_eq!(
            env_value(&empty, "HOME").as_deref(),
            Some("/srv/app"),
            "an empty HOME falls back to the image account's own home"
        );

        let declared = env_of(Some("/opt/app"));
        assert_eq!(env_value(&declared, "HOME").as_deref(), Some("/opt/app"));
    }

    #[test]
    fn an_image_user_uses_its_own_passwd_and_caps_are_root_only() {
        let disk = TempDir::new_in("/var/tmp").expect("real disk for the image rootfs");
        let rootfs = seeded_image(
            &disk,
            "root:x:0:0:root:/root:/bin/sh\napp:x:1001:1002:App:/srv/app:/bin/zsh\n",
            "root:x:0:\napp:x:1002:\n",
        );
        let owner = fs::metadata(disk.path()).expect("stat the probe dir");
        let pod_host = host(owner.uid(), owner.gid());
        let mut app_spec = spec(rootfs.clone(), vec!["PATH=/usr/bin".to_owned()], Vec::new());
        app_spec.identity = Some(Identity {
            uid: 1001,
            gid: 1002,
        });

        let bundle = write_bundle(&app_spec, &pod_host, &disk.path().join("bundle-app"))
            .expect("write the app bundle");
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(&bundle.config).expect("read config"))
                .expect("parse config");
        assert_eq!(json["process"]["user"]["uid"], 1001);
        assert_eq!(json["process"]["user"]["gid"], 1002);
        assert_eq!(
            json["linux"]["uidMappings"][0]["containerID"], 1001,
            "the container uid maps onto the invoking host uid"
        );
        assert_eq!(json["linux"]["uidMappings"][0]["hostID"], pod_host.uid);
        assert_eq!(
            json["process"]["capabilities"]["bounding"]
                .as_array()
                .expect("bounding")
                .len(),
            0,
            "a non-root USER drops Docker's default caps"
        );
        assert!(
            !bundle.dir.join("passwd").exists(),
            "a named image account keeps the image's own /etc/passwd"
        );
        assert!(
            rootfs.join("srv/app").is_dir(),
            "the account home is materialised"
        );

        let env: Vec<String> = json["process"]["env"]
            .as_array()
            .expect("env")
            .iter()
            .map(|value| value.as_str().expect("env string").to_owned())
            .collect();
        assert_eq!(env_value(&env, "HOME").as_deref(), Some("/srv/app"));
        assert_eq!(env_value(&env, "USER").as_deref(), Some("app"));
        assert_eq!(env_value(&env, "LOGNAME").as_deref(), Some("app"));
        assert_eq!(env_value(&env, "PATH").as_deref(), Some("/usr/bin"));

        // Root keeps the image files too and gets the full Docker capability set.
        let mut root_spec = spec(rootfs.clone(), Vec::new(), Vec::new());
        root_spec.identity = Some(Identity::ROOT);
        let root_bundle = write_bundle(&root_spec, &pod_host, &disk.path().join("bundle-root"))
            .expect("write the root bundle");
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(&root_bundle.config).expect("read config"))
                .expect("parse config");
        assert_eq!(json["process"]["user"]["uid"], 0);
        assert_eq!(
            json["process"]["capabilities"]["bounding"]
                .as_array()
                .expect("bounding")
                .len(),
            14,
            "container root gets Docker's default caps inside the userns"
        );
        assert_eq!(json["linux"]["uidMappings"][0]["containerID"], 0);
        assert_eq!(json["linux"]["uidMappings"][0]["hostID"], pod_host.uid);
        assert!(!root_bundle.dir.join("passwd").exists());
        let env: Vec<String> = json["process"]["env"]
            .as_array()
            .expect("env")
            .iter()
            .map(|value| value.as_str().expect("env string").to_owned())
            .collect();
        assert_eq!(env_value(&env, "HOME").as_deref(), Some("/root"));
        assert_eq!(env_value(&env, "USER").as_deref(), Some("root"));
    }

    #[test]
    fn an_image_root_home_is_never_recreated_or_rechmodded() {
        let disk = TempDir::new_in("/var/tmp").expect("real disk for the image rootfs");
        let rootfs = disk.path().join("rootfs");
        fs::create_dir_all(rootfs.join("root")).expect("seed the image /root");
        fs::set_permissions(rootfs.join("root"), fs::Permissions::from_mode(0o700))
            .expect("the image ships a private /root");
        fs::create_dir_all(rootfs.join("etc")).expect("seed the image etc");
        fs::write(rootfs.join("etc/passwd"), "root:x:0:0:root:/root:/bin/sh\n")
            .expect("write passwd");
        let owner = fs::metadata(disk.path()).expect("stat the probe dir");
        let pod_host = host(owner.uid(), owner.gid());

        let mut root_spec = spec(rootfs.clone(), Vec::new(), Vec::new());
        root_spec.identity = Some(Identity::ROOT);
        write_bundle(&root_spec, &pod_host, &disk.path().join("bundle-root"))
            .expect("write the root bundle");
        let meta = fs::metadata(rootfs.join("root")).expect("stat the image /root");
        assert_eq!(
            meta.mode() & 0o777,
            0o700,
            "an image-owned /root keeps its mode"
        );
    }

    #[test]
    fn bundle_materialises_a_0755_home_owned_by_the_pod_user() {
        let disk = TempDir::new_in("/var/tmp").expect("real disk for the pod rootfs");
        let owner = fs::metadata(disk.path()).expect("stat the probe dir");
        let pod_host = host(owner.uid(), owner.gid());
        let rootfs = disk.path().join("rootfs");
        fs::create_dir_all(&rootfs).expect("seed the rootfs");

        let bundle = write_bundle(
            &spec(rootfs.clone(), Vec::new(), Vec::new()),
            &pod_host,
            &disk.path().join("bundle"),
        )
        .expect("write the OCI bundle");

        let home = rootfs.join("home/slipfork");
        let meta = fs::metadata(&home).expect("the baseline home exists on disk");
        assert!(meta.is_dir());
        assert_eq!(meta.mode() & 0o777, 0o755);
        assert_eq!((meta.uid(), meta.gid()), (pod_host.uid, pod_host.gid));
        let rendered_passwd =
            String::from_utf8(fs::read(bundle.dir.join("passwd")).expect("read passwd"))
                .expect("the synthetic passwd is UTF-8");
        assert!(rendered_passwd.contains("/home/slipfork:/bin/sh"));
        let rendered_group =
            String::from_utf8(fs::read(bundle.dir.join("group")).expect("read group"))
                .expect("the synthetic group is UTF-8");
        assert!(rendered_group.contains("slipfork"));

        // A declared custom `HOME` wins and still gets a real directory on disk.
        let custom_rootfs = disk.path().join("rootfs-custom");
        fs::create_dir_all(&custom_rootfs).expect("seed the second rootfs");
        let custom = write_bundle(
            &spec(
                custom_rootfs.clone(),
                vec!["HOME=/root".to_owned()],
                vec![PodEnv::new("HOME", "/opt/app")],
            ),
            &pod_host,
            &disk.path().join("bundle-custom"),
        )
        .expect("write the custom bundle");
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(&custom.config).expect("read config"))
                .expect("parse config");
        assert!(
            json["process"]["env"]
                .as_array()
                .expect("env")
                .iter()
                .any(|value| value == "HOME=/opt/app")
        );
        assert!(custom_rootfs.join("home/slipfork").is_dir());
    }

    #[test]
    fn root_pods_keep_root_home_and_skip_the_baseline_directory() {
        let disk = TempDir::new_in("/var/tmp").expect("real disk for the root pod rootfs");
        let rootfs = disk.path().join("rootfs");
        fs::create_dir_all(&rootfs).expect("seed the rootfs");
        let bundle = write_bundle(
            &spec(rootfs.clone(), Vec::new(), Vec::new()),
            &host(0, 0),
            &disk.path().join("bundle"),
        )
        .expect("write the root bundle");
        assert!(!rootfs.join("home/slipfork").exists());
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(&bundle.config).expect("read config"))
                .expect("parse config");
        let env: Vec<String> = json["process"]["env"]
            .as_array()
            .expect("env")
            .iter()
            .map(|value| value.as_str().expect("env string").to_owned())
            .collect();
        assert_eq!(
            env_value(&env, "HOME"),
            None,
            "a uid-0 legacy pod keeps the image's own HOME"
        );
        assert_eq!(env_value(&env, "USER"), None);
    }
}
