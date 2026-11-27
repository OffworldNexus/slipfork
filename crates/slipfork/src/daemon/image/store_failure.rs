//! Typed store/cache refusals for the image lab: one cause, one named path, one `next:`.
//!
//! Both the world engine and the OCI cache append their own remedies in `Display`; this module is
//! the single place that maps a real failure onto the exact path the user must fix, so the daemon
//! never leaks a second `next:` line or a vague "check the store" hint.

use std::io;
use std::path::Path;

use super::{Failure, failure};

/// The typed refusal for a store root that cannot be prepared, always naming that root.
///
/// The world engine's own `Display` appends a `next:` line, so this mapping owns the whole
/// remedy: a non-Btrfs root points at the workspace-image recipe, an unwritable child at that
/// child's permissions, and anything else keeps the first-line cause with one plain retry hint.
pub(super) fn store_failure(root: &Path, error: &slipfork_world::Error) -> Failure {
    let root_text = root.display();
    let (message, next) = match error {
        slipfork_world::Error::NotBtrfs { .. } => (
            format!("cannot prepare the image store: `{root_text}` is not on Btrfs"),
            "mount a Btrfs workspace image in your home folder, then retry \u{2014} see docs/install.md#on-ext4-or-xfs-mount-a-btrfs-workspace-image-in-your-home-folder".to_owned(),
        ),
        // `<root>` itself may exist while a root-owned child (`images`, `worlds`) does not, so
        // the io error's own path wins over the parent root here.
        slipfork_world::Error::Io {
            path,
            source,
            ..
        } if source.kind() == io::ErrorKind::PermissionDenied => (
            format!(
                "cannot prepare the image store: `{}` is not writable",
                path.display()
            ),
            format!(
                "make `{}` writable by your user and retry",
                path.display()
            ),
        ),
        // Never echo the engine's own embedded `next:` line: this maps one cause to one remedy.
        slipfork_world::Error::Io {
            action,
            path,
            source,
        } => (
            format!(
                "cannot prepare the image store: could not {action} `{}`: {source}",
                path.display()
            ),
            format!("check that `{root_text}` is writable and retry"),
        ),
        other => {
            let rendered = other.to_string();
            let first = rendered.split('\n').next().unwrap_or(rendered.as_str());
            (
                format!("cannot prepare the image store: {first}"),
                format!("check that `{root_text}` is writable and retry"),
            )
        }
    };
    failure("image-store", message, &next)
}

/// The typed refusal for an OCI cache root that cannot be prepared, naming the failed path.
///
/// `images.ensure()` creates `<store>/images` and its own children, so a permission failure names
/// that child; every other cause keeps a single cause-specific `next:` line.
pub(super) fn cache_failure(root: &Path, error: &slipfork_image::Error) -> Failure {
    let (message, next) = match error {
        slipfork_image::Error::Io { path, source, .. }
            if source.kind() == io::ErrorKind::PermissionDenied =>
        {
            (
                format!(
                    "cannot prepare the image cache: `{}` is not writable",
                    path.display()
                ),
                format!("make `{}` writable by your user and retry", path.display()),
            )
        }
        slipfork_image::Error::Io {
            action,
            path,
            source,
        } => (
            format!(
                "cannot prepare the image cache: could not {action} `{}`: {source}",
                path.display()
            ),
            format!("check that `{}` is writable and retry", path.display()),
        ),
        other => {
            let rendered = other.to_string();
            let first = rendered.split('\n').next().unwrap_or(rendered.as_str());
            (
                format!("cannot prepare the image cache: {first}"),
                format!("check that `{}` is writable and retry", root.display()),
            )
        }
    };
    failure("image-store", message, &next)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use slipfork_image::{Error as ImageError, RegistryCredentials};
    use slipfork_world::Error as WorldError;

    use super::{cache_failure, store_failure};
    use crate::world::HostEnv;

    /// A representative data-home store root; nothing here touches the real store.
    fn lab_root() -> PathBuf {
        PathBuf::from("/var/tmp/slipfork-store-lab/data/slipfork/store")
    }

    /// A non-Btrfs data home names the resolved root and hands over the workspace-image recipe.
    #[test]
    fn a_non_btrfs_store_names_the_root_and_the_workspace_image_remedy() {
        let root = lab_root();
        let error = WorldError::NotBtrfs { path: root.clone() };

        let refusal = store_failure(&root, &error);

        assert_eq!(refusal.code, "image-store");
        assert!(
            refusal.message.contains(&root.display().to_string()),
            "{}",
            refusal.message
        );
        assert!(
            refusal.message.contains("not on Btrfs"),
            "{}",
            refusal.message
        );
        assert_eq!(refusal.message.matches("\nnext: ").count(), 1);
        assert!(
            refusal.message.contains(
                "docs/install.md#on-ext4-or-xfs-mount-a-btrfs-workspace-image-in-your-home-folder"
            ),
            "{}",
            refusal.message
        );
    }

    /// A permission failure names the child `ensure_store` was creating, not only the root.
    #[test]
    fn an_unwritable_store_child_is_named_in_the_message_and_the_remedy() {
        let root = lab_root();
        let child = root.join("images");
        let error = WorldError::Io {
            action: "create",
            path: child.clone(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };

        let refusal = store_failure(&root, &error);

        assert!(
            refusal.message.contains("not writable"),
            "{}",
            refusal.message
        );
        assert!(
            refusal.message.contains(&child.display().to_string()),
            "{}",
            refusal.message
        );
        assert!(
            refusal
                .message
                .contains(&format!("make `{}` writable", child.display())),
            "{}",
            refusal.message
        );
        assert_eq!(refusal.message.matches("\nnext: ").count(), 1);
    }

    /// The catch-all arm keeps the first-line cause, names the root, and adds exactly one remedy.
    #[test]
    fn another_store_failure_keeps_one_remedy() {
        let root = lab_root();
        let error = WorldError::NotADirectory { path: root.clone() };

        let refusal = store_failure(&root, &error);

        assert_eq!(refusal.message.matches("\nnext: ").count(), 1);
        assert!(
            refusal.message.contains(&root.display().to_string()),
            "{}",
            refusal.message
        );
        assert!(
            refusal.message.contains("is not a directory"),
            "{}",
            refusal.message
        );
        assert!(
            refusal
                .message
                .contains(&format!("check that `{}` is writable", root.display())),
            "{}",
            refusal.message
        );
    }

    /// An unwritable cache child is named in both the message and the remedy.
    #[test]
    fn an_unwritable_cache_child_is_named_in_the_message_and_the_remedy() {
        let root = lab_root();
        let child = root.join("images");
        let error = ImageError::Io {
            action: "create",
            path: child.clone(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };

        let refusal = cache_failure(&root, &error);

        assert_eq!(refusal.code, "image-store");
        assert!(
            refusal.message.contains(&format!(
                "cannot prepare the image cache: `{}`",
                child.display()
            )),
            "{}",
            refusal.message
        );
        assert!(
            refusal
                .message
                .contains(&format!("make `{}` writable", child.display())),
            "{}",
            refusal.message
        );
        assert_eq!(refusal.message.matches("\nnext: ").count(), 1);
    }

    /// The real `HostEnv` → `ImageLab` wiring resolves the data-home store and never sees `ENOENT`.
    #[test]
    fn for_project_resolves_and_creates_the_data_home_store() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock reads after the epoch")
            .as_nanos();
        let scratch = PathBuf::from("/var/tmp").join(format!(
            "slipfork-image-wiring-{}-{nanos}",
            std::process::id()
        ));
        let data_home = scratch.join("data-home");
        fs::create_dir_all(&data_home).expect("create the real data home");
        let host = HostEnv {
            exe: scratch.join("slipfork"),
            path: vec![PathBuf::from("/usr/bin")],
            runtime_dir: scratch.join("run"),
            state_dir: scratch.join("state"),
            mounts: vec![PathBuf::from("/")],
            arch: std::env::consts::ARCH,
            uid: 1000,
            gid: 1000,
            resolv_conf: PathBuf::from("/etc/resolv.conf"),
            home: Some(scratch.join("home")),
            install_root: PathBuf::from("/"),
            display: false,
            ssh: false,
            color: false,
            opener: None,
            env: BTreeMap::from([("XDG_DATA_HOME".to_owned(), data_home.display().to_string())]),
            config_dir: scratch.join("config"),
            registry_credentials: RegistryCredentials::default(),
        };
        let store_root = data_home.join("slipfork").join("store");
        assert!(
            !store_root.exists(),
            "the fallback store starts absent, so an ENOENT regression is visible"
        );

        let outcome = super::super::ImageLab::for_project(&host, Path::new("/srv/demo"));

        match outcome {
            Ok(_lab) => assert!(
                store_root.is_dir(),
                "a Btrfs host keeps the created store root"
            ),
            Err(refusal) => {
                assert_eq!(refusal.code, "image-store");
                assert!(
                    refusal.message.contains(&store_root.display().to_string()),
                    "the refusal pins the resolved store root: {}",
                    refusal.message
                );
                assert_eq!(refusal.message.matches("\nnext: ").count(), 1);
                assert!(
                    !refusal.message.contains("No such file")
                        && !refusal.message.contains("os error 2"),
                    "the store is created before it is opened: {}",
                    refusal.message
                );
            }
        }
        assert!(
            store_root.is_dir(),
            "the resolved fallback store is created either way"
        );

        fs::remove_dir_all(&scratch).expect("remove the wiring lab");
    }
}
