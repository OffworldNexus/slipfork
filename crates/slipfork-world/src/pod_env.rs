//! Home directory and environment baseline for unprivileged pods.
//!
//! Every pod runs as the invoking uid/gid inside the world user namespace. Images such as
//! `python:3.13-slim` set no `HOME`, so `uv`, `git`, `bash`, and `pnpm` fall back to `/root`
//! (mode `0700`, owned by uid 0) and fail. The pod baseline gives that identity a writable
//! `/home/slipfork` that both the synthetic `passwd` entry and `config.json` name; no image needs
//! an `ENV HOME` change.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::error::{Error, Result};
use crate::pod::{PodHost, PodSpec};

const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// In-pod home directory of the unprivileged pod user.
pub(crate) const POD_HOME: &str = "/home/slipfork";
/// Home directory of uid 0.
pub(crate) const ROOT_HOME: &str = "/root";

/// `true` when an unprivileged pod needs Slipfork's writable home injected as `HOME`.
///
/// A user-declared `HOME` other than `/root` always wins; uid 0 keeps the image's `HOME`.
pub(crate) fn needs_default_home(uid: u32, current: Option<&str>) -> bool {
    uid != 0 && current.is_none_or(|value| value.is_empty() || value == ROOT_HOME)
}

/// Renders the two-line synthetic `/etc/passwd`; unprivileged pods name [`POD_HOME`].
pub(crate) fn passwd_contents(uid: u32, gid: u32) -> String {
    let home = if uid == 0 { ROOT_HOME } else { POD_HOME };
    format!("root:x:0:0:root:/root:/sbin/nologin\nslipfork:x:{uid}:{gid}:slipfork:{home}:/bin/sh\n")
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

/// Image env first, the pod's own env last, so an own entry overrides the same key.
pub(crate) fn merged_env(spec: &PodSpec, host: &PodHost) -> Vec<String> {
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
    let needs_home = {
        let current = ordered
            .iter()
            .find(|(key, _)| key == "HOME")
            .map(|(_, value)| value.as_str());
        needs_default_home(host.uid, current)
    };
    if needs_home {
        if let Some(slot) = ordered.iter_mut().find(|(key, _)| key == "HOME") {
            slot.1 = POD_HOME.to_owned();
        } else {
            ordered.push(("HOME".to_owned(), POD_HOME.to_owned()));
        }
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
        }
    }

    fn env_value(env: &[String], key: &str) -> Option<String> {
        env.iter()
            .find_map(|pair| pair.strip_prefix(&format!("{key}=")))
            .map(str::to_owned)
    }

    #[test]
    fn passwd_names_the_writable_home_for_unprivileged_pods() {
        let passwd = passwd_contents(1000, 1001);
        assert!(passwd.contains("slipfork:x:1000:1001:slipfork:/home/slipfork:/bin/sh\n"));
        assert!(passwd.contains("root:x:0:0:root:/root:/sbin/nologin\n"));
        assert!(passwd_contents(0, 0).contains("slipfork:x:0:0:slipfork:/root:/bin/sh\n"));
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
    fn merged_env_injects_home_unless_a_custom_one_is_declared() {
        let probe = TempDir::new_in("/var/tmp").expect("real disk for the rootfs probe");
        let owner = fs::metadata(probe.path()).expect("stat the probe dir");
        let host = host(owner.uid(), owner.gid());

        let unset = merged_env(&spec(probe.path().into(), Vec::new(), Vec::new()), &host);
        assert_eq!(env_value(&unset, "HOME").as_deref(), Some(POD_HOME));

        let root = merged_env(
            &spec(
                probe.path().into(),
                vec!["HOME=/root".to_owned()],
                Vec::new(),
            ),
            &host,
        );
        assert_eq!(env_value(&root, "HOME").as_deref(), Some(POD_HOME));

        let empty = merged_env(
            &spec(
                probe.path().into(),
                Vec::new(),
                vec![PodEnv::new("HOME", "")],
            ),
            &host,
        );
        assert_eq!(env_value(&empty, "HOME").as_deref(), Some(POD_HOME));

        let custom = merged_env(
            &spec(
                probe.path().into(),
                vec!["HOME=/root".to_owned()],
                vec![PodEnv::new("HOME", "/opt/app")],
            ),
            &host,
        );
        assert_eq!(env_value(&custom, "HOME").as_deref(), Some("/opt/app"));
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
        assert!(
            fs::read_to_string(bundle.dir.join("passwd"))
                .expect("read passwd")
                .contains("/home/slipfork:/bin/sh")
        );

        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(&bundle.config).expect("read config"))
                .expect("parse config");
        let env: Vec<String> = json["process"]["env"]
            .as_array()
            .expect("env")
            .iter()
            .map(|value| value.as_str().expect("env string").to_owned())
            .collect();
        assert_eq!(env_value(&env, "HOME").as_deref(), Some(POD_HOME));

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
        assert!(
            !json["process"]["env"]
                .as_array()
                .expect("env")
                .iter()
                .any(|value| value.as_str().is_some_and(|pair| pair.starts_with("HOME=")))
        );
    }
}
