use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::{Error, NameKind, Result};

/// Name of the store directory Slipfork owns on the Btrfs mount.
pub const STORE_DIR: &str = ".slipfork-store";
/// Read-only image/build-step subvolumes live here.
pub const IMAGES_DIR: &str = "images";
/// Per-world data subvolumes live here.
pub const WORLDS_DIR: &str = "worlds";
/// Project-relative directory holding project snapshots.
pub const FORKS_DIR: &str = ".forks";

/// Absolute Btrfs paths owned by Slipfork, derived from one injected mount root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreLayout {
    btrfs_mount: PathBuf,
}

impl StoreLayout {
    /// Builds a layout rooted at `btrfs_mount` (a filesystem root or any directory on it).
    #[must_use]
    pub fn new(btrfs_mount: impl Into<PathBuf>) -> Self {
        Self {
            btrfs_mount: btrfs_mount.into(),
        }
    }

    /// The Btrfs mount every other path is derived from.
    #[must_use]
    pub fn btrfs_mount(&self) -> &Path {
        &self.btrfs_mount
    }

    /// `<mount>/.slipfork-store`.
    #[must_use]
    pub fn store_root(&self) -> PathBuf {
        self.btrfs_mount.join(STORE_DIR)
    }

    /// `<mount>/.slipfork-store/images`.
    #[must_use]
    pub fn images_root(&self) -> PathBuf {
        self.store_root().join(IMAGES_DIR)
    }

    /// `<mount>/.slipfork-store/worlds`.
    #[must_use]
    pub fn worlds_root(&self) -> PathBuf {
        self.store_root().join(WORLDS_DIR)
    }

    /// `<mount>/.slipfork-store/worlds/<world>`.
    #[must_use]
    pub fn world(&self, world: &WorldName) -> PathBuf {
        self.worlds_root().join(world.as_str())
    }

    /// `<mount>/.slipfork-store/worlds/<fork>--<world>`.
    #[must_use]
    pub fn fork_world(&self, world: &WorldName, fork: &ForkName) -> PathBuf {
        self.worlds_root()
            .join(Self::fork_world_name(world, fork).as_str())
    }

    /// `<project>/.forks/<fork>`: the fork's own checkout snapshot.
    #[must_use]
    pub fn project_fork(project: &Path, fork: &ForkName) -> PathBuf {
        project_fork_path(project, fork)
    }

    /// `<fork>--<world>`: the world id a fork boots under.
    #[must_use]
    pub fn fork_world_name(world: &WorldName, fork: &ForkName) -> WorldName {
        WorldName(format!("{}--{}", fork.as_str(), world.as_str()))
    }

    /// Splits a `worlds/` entry name back into its `<fork>--<world>` pair.
    ///
    /// The separator and both name validators live here, so no caller re-scans entry names.
    /// Splitting on the **last** `--` keeps fork names that themselves contain `-` intact.
    #[must_use]
    pub fn parse_fork_world(entry: &std::ffi::OsStr) -> Option<(ForkName, WorldName)> {
        let (fork, world) = entry.to_str()?.rsplit_once("--")?;
        Some((ForkName::parse(fork).ok()?, WorldName::parse(world).ok()?))
    }
}

/// A validated `worlds/<world>` data subvolume name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorldName(String);

impl WorldName {
    /// Parses `raw` (`[a-z0-9][a-z0-9._-]*`) into a world name.
    pub fn parse(raw: &str) -> Result<Self> {
        validate_name(NameKind::World, raw).map(Self)
    }

    /// The validated name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorldName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A validated fork name shared by both snapshots of a fork.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ForkName(String);

impl ForkName {
    /// Parses `raw` (`[a-z0-9][a-z0-9._-]*`) into a fork name.
    pub fn parse(raw: &str) -> Result<Self> {
        validate_name(NameKind::Fork, raw).map(Self)
    }

    /// The validated name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ForkName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub(crate) fn validate_name(kind: NameKind, raw: &str) -> Result<String> {
    let mut bytes = raw.bytes();
    let starts_well = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    let rest_well = bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    });
    if !starts_well || !rest_well || matches!(raw, "." | "..") {
        return Err(Error::InvalidName {
            kind,
            name: raw.to_owned(),
        });
    }
    Ok(raw.to_owned())
}

/// `<project>/.forks`.
#[must_use]
pub(crate) fn project_forks_dir(project: &Path) -> PathBuf {
    project.join(FORKS_DIR)
}

/// `<project>/.forks/<fork>`.
#[must_use]
pub(crate) fn project_fork_path(project: &Path, fork: &ForkName) -> PathBuf {
    project_forks_dir(project).join(fork.as_str())
}

/// Splits `path` into its parent directory and single-component file name.
pub(crate) fn split_dir(path: &Path) -> Result<(PathBuf, OsString)> {
    let name = path
        .file_name()
        .ok_or_else(|| Error::UnsupportedPath {
            path: path.to_path_buf(),
        })?
        .to_owned();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| Error::UnsupportedPath {
            path: path.to_path_buf(),
        })?;
    Ok((parent.to_path_buf(), name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_derives_every_store_path_from_the_mount() {
        let layout = StoreLayout::new("/mnt/btrfs");
        assert_eq!(layout.store_root(), Path::new("/mnt/btrfs/.slipfork-store"));
        assert_eq!(
            layout.images_root(),
            Path::new("/mnt/btrfs/.slipfork-store/images")
        );
        assert_eq!(
            layout.worlds_root(),
            Path::new("/mnt/btrfs/.slipfork-store/worlds")
        );
        let world = WorldName::parse("demo").expect("valid world");
        let fork = ForkName::parse("feat-auth").expect("valid fork");
        assert_eq!(
            layout.world(&world),
            Path::new("/mnt/btrfs/.slipfork-store/worlds/demo")
        );
        assert_eq!(
            layout.fork_world(&world, &fork),
            Path::new("/mnt/btrfs/.slipfork-store/worlds/feat-auth--demo")
        );
        assert_eq!(
            project_fork_path(Path::new("/mnt/btrfs/projects/demo"), &fork),
            Path::new("/mnt/btrfs/projects/demo/.forks/feat-auth")
        );
    }

    #[test]
    fn names_reject_escaping_and_uppercase_components() {
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "Upper",
            "-lead",
            "dot.dot/x",
            "with space",
        ] {
            assert!(
                WorldName::parse(bad).is_err(),
                "`{bad}` must not be a world name"
            );
            assert!(
                ForkName::parse(bad).is_err(),
                "`{bad}` must not be a fork name"
            );
        }
    }

    #[test]
    fn names_accept_lowercase_digits_dots_dashes() {
        for good in ["demo", "s3", "feat-auth", "a.b_c-d", "9lives"] {
            assert!(WorldName::parse(good).is_ok(), "`{good}` must be valid");
            assert!(ForkName::parse(good).is_ok(), "`{good}` must be valid");
        }
    }

    #[test]
    fn fork_world_entries_split_on_the_last_separator() {
        let entry = std::ffi::OsStr::new("feat-auth--demo");
        let (fork, world) = StoreLayout::parse_fork_world(entry).expect("valid fork/world pair");
        assert_eq!(fork.as_str(), "feat-auth");
        assert_eq!(world.as_str(), "demo");
        // A fork name containing `-` must not be mistaken for another world's snapshot.
        let (long_fork, long_world) =
            StoreLayout::parse_fork_world(std::ffi::OsStr::new("x--a--b")).expect("valid pair");
        assert_eq!(long_fork.as_str(), "x--a");
        assert_eq!(long_world.as_str(), "b");
        assert!(StoreLayout::parse_fork_world(std::ffi::OsStr::new("demo")).is_none());
        assert!(StoreLayout::parse_fork_world(std::ffi::OsStr::new("--demo")).is_none());
        assert!(StoreLayout::parse_fork_world(std::ffi::OsStr::new("feat--Upper")).is_none());
    }

    #[test]
    fn split_dir_needs_a_real_final_component() {
        assert!(split_dir(Path::new("/")).is_err());
        assert!(split_dir(Path::new("..")).is_err());
        assert_eq!(
            split_dir(Path::new("/mnt/demo")).expect("splittable"),
            (PathBuf::from("/mnt"), OsString::from("demo"))
        );
    }
}
