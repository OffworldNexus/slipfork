//! Layer extraction: gzip/zstd decoding, tar entry application and whiteout semantics.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

/// Compression wrapper of a layer blob, derived from its OCI media type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compression {
    Gzip,
    Zstd,
    Plain,
}

impl Compression {
    fn from_media_type(media_type: &str) -> Option<Self> {
        if media_type.ends_with("+gzip") || media_type.ends_with(".gzip") {
            Some(Self::Gzip)
        } else if media_type.ends_with("+zstd") || media_type.ends_with(".zstd") {
            Some(Self::Zstd)
        } else if media_type.ends_with("+tar") || media_type.ends_with(".tar") {
            Some(Self::Plain)
        } else {
            None
        }
    }
}

/// A whiteout entry: one path removal or an opaque-directory clear.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Whiteout {
    Opaque,
    Remove(String),
}

impl Whiteout {
    fn parse(name: &str) -> Option<Self> {
        if name == ".wh..wh..opq" {
            return Some(Self::Opaque);
        }
        name.strip_prefix(".wh.")
            .filter(|rest| !rest.is_empty())
            .map(|rest| Self::Remove(rest.to_owned()))
    }
}

/// Unpacks ordered layers into one rootfs directory with whiteout semantics.
pub struct LayerUnpacker {
    root: PathBuf,
}

impl LayerUnpacker {
    /// Unpacks into `root` (a fresh read-write subvolume owned by the caller).
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Applies one compressed layer; `compressed` is the raw registry blob stream.
    pub fn apply(&self, compressed: &mut dyn Read, media_type: &str) -> Result<()> {
        fs::create_dir_all(&self.root).map_err(|source| Error::io("create", &self.root, source))?;
        let compression =
            Compression::from_media_type(media_type).ok_or_else(|| Error::UnsupportedLayer {
                media_type: media_type.to_owned(),
            })?;
        match compression {
            Compression::Gzip => self.apply_tar(flate2::read::MultiGzDecoder::new(compressed)),
            Compression::Zstd => {
                let decoder = zstd::stream::read::Decoder::new(compressed)
                    .map_err(|source| Error::io("decompress", &self.root, source))?;
                self.apply_tar(decoder)
            }
            Compression::Plain => self.apply_tar(compressed),
        }
    }

    fn apply_tar<R: Read>(&self, reader: R) -> Result<()> {
        let mut archive = tar::Archive::new(reader);
        let entries = archive
            .entries()
            .map_err(|source| Error::io("read a layer archive", &self.root, source))?;
        let mut dir_modes: Vec<(PathBuf, u32)> = Vec::new();
        for entry in entries {
            let mut entry =
                entry.map_err(|source| Error::io("read a layer entry", &self.root, source))?;
            self.apply_entry(&mut entry, &mut dir_modes)?;
        }
        // Directory modes land after every child exists; a sealed `/root` would block its own tree.
        for (path, mode) in dir_modes {
            set_mode(&path, mode)?;
        }
        Ok(())
    }

    fn apply_entry<R: Read>(
        &self,
        entry: &mut tar::Entry<'_, R>,
        dir_modes: &mut Vec<(PathBuf, u32)>,
    ) -> Result<()> {
        let path = entry
            .path()
            .map_err(|source| Error::io("read a layer entry name", &self.root, source))?;
        let rel = sanitize(&path)?;
        if let Some(name) = rel.file_name().and_then(OsStr::to_str) {
            if let Some(whiteout) = Whiteout::parse(name) {
                let parent = rel.parent().unwrap_or_else(|| Path::new(""));
                return self.apply_whiteout(parent, &whiteout);
            }
        }
        let mode = entry.header().mode().unwrap_or(0o644);
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                let dir = self.ensure_dir(&rel)?;
                dir_modes.push((dir, mode));
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                self.write_file(&rel, mode, entry)?;
            }
            tar::EntryType::Symlink => {
                let target = self.link_target(entry, &rel)?;
                self.write_symlink(&rel, &target)?;
            }
            tar::EntryType::Link => {
                let target = self.link_target(entry, &rel)?;
                self.write_hardlink(&rel, &target)?;
            }
            other => {
                return Err(Error::UnsupportedEntry {
                    entry: rel.display().to_string(),
                    kind: kind_label(other),
                });
            }
        }
        Ok(())
    }

    fn link_target<R: Read>(&self, entry: &tar::Entry<'_, R>, rel: &Path) -> Result<PathBuf> {
        entry
            .link_name()
            .map_err(|source| Error::io("read a link target", &self.root, source))?
            .map(|target| target.into_owned())
            .ok_or_else(|| Error::UnsafeLink {
                entry: rel.display().to_string(),
                target: String::new(),
            })
    }

    fn write_file<R: Read>(
        &self,
        rel: &Path,
        mode: u32,
        entry: &mut tar::Entry<'_, R>,
    ) -> Result<()> {
        let path = self.entry_path(rel)?;
        remove_path(&path)?;
        let mut file =
            fs::File::create(&path).map_err(|source| Error::io("create", &path, source))?;
        io::copy(entry, &mut file).map_err(|source| Error::io("unpack", &path, source))?;
        set_mode(&path, mode)
    }

    fn write_symlink(&self, rel: &Path, target: &Path) -> Result<()> {
        validate_symlink_target(rel, target)?;
        let path = self.entry_path(rel)?;
        remove_path(&path)?;
        std::os::unix::fs::symlink(target, &path).map_err(|source| Error::io("link", &path, source))
    }

    fn write_hardlink(&self, rel: &Path, target: &Path) -> Result<()> {
        let source = self.archive_path(rel, target)?;
        let path = self.entry_path(rel)?;
        remove_path(&path)?;
        fs::hard_link(&source, &path).map_err(|source_err| Error::io("link", &path, source_err))
    }

    /// Resolves a hardlink target: root-relative even when tar wrote it absolute.
    fn archive_path(&self, rel: &Path, target: &Path) -> Result<PathBuf> {
        let rooted = hardlink_target(&self.root, rel, target)?;
        let clean = rooted
            .strip_prefix(&self.root)
            .expect("hardlink target is rooted");
        let mut current = self.root.clone();
        for component in clean.components() {
            current.push(component);
            let metadata = fs::symlink_metadata(&current)
                .map_err(|source| Error::io("inspect", &current, source))?;
            if metadata.file_type().is_symlink() {
                return Err(Error::UnsafeLink {
                    entry: rel.display().to_string(),
                    target: target.display().to_string(),
                });
            }
        }
        Ok(current)
    }

    fn apply_whiteout(&self, parent: &Path, whiteout: &Whiteout) -> Result<()> {
        let Some(dir) = self.existing_dir(parent) else {
            return Ok(());
        };
        match whiteout {
            Whiteout::Opaque => {
                let entries =
                    fs::read_dir(&dir).map_err(|source| Error::io("read", &dir, source))?;
                for entry in entries {
                    let entry = entry.map_err(|source| Error::io("read", &dir, source))?;
                    remove_path(&entry.path())?;
                }
                Ok(())
            }
            Whiteout::Remove(name) => remove_path(&dir.join(name)),
        }
    }

    /// Creates every directory component of `rel`, refusing to follow a symlink.
    fn ensure_dir(&self, rel: &Path) -> Result<PathBuf> {
        let mut current = self.root.clone();
        for component in rel.components() {
            current.push(component);
            match fs::symlink_metadata(&current) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() || !metadata.is_dir() {
                        return Err(Error::UnsafePath {
                            entry: rel.display().to_string(),
                        });
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir(&current)
                        .map_err(|source| Error::io("create", &current, source))?;
                }
                Err(err) => return Err(Error::io("inspect", &current, err)),
            }
        }
        Ok(current)
    }

    /// Resolves an existing directory chain without following any symlink.
    fn existing_dir(&self, rel: &Path) -> Option<PathBuf> {
        let mut current = self.root.clone();
        for component in rel.components() {
            current.push(component);
            let metadata = fs::symlink_metadata(&current).ok()?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return None;
            }
        }
        Some(current)
    }

    /// Creates every parent of `rel` inside the root before a non-directory entry is written.
    ///
    /// Layers routinely omit `dir` entries or list a link before its directory, so parents are
    /// created on demand: `ensure_dir` walks one sanitized component at a time and never follows
    /// a symlink out of the root.
    fn entry_path(&self, rel: &Path) -> Result<PathBuf> {
        let name = rel.file_name().ok_or_else(|| Error::UnsafePath {
            entry: rel.display().to_string(),
        })?;
        let parent = self.ensure_dir(rel.parent().unwrap_or_else(|| Path::new("")))?;
        Ok(parent.join(name))
    }
}

/// Human label for a tar entry kind the unpacker refuses to materialize.
fn kind_label(entry_type: tar::EntryType) -> String {
    match entry_type {
        tar::EntryType::Char => "character device".to_owned(),
        tar::EntryType::Block => "block device".to_owned(),
        tar::EntryType::Fifo => "fifo".to_owned(),
        tar::EntryType::GNUSparse => "sparse file".to_owned(),
        other => format!("tar entry type 0x{:02x}", other.as_byte()),
    }
}

/// Normalizes one tar entry name into a path relative to the image root.
///
/// Real archives carry a leading `./` and a `./` (or `.`) entry for the root itself, so empty and
/// `.` components are dropped and an all-`.` name normalizes to the empty path (the root).
/// Absolute names and any `..` component are still refused, and `usr//bin` folds to `usr/bin`.
fn sanitize(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(Error::UnsafePath {
            entry: path.display().to_string(),
        });
    }
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => clean.push(name),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::UnsafePath {
                    entry: path.display().to_string(),
                });
            }
        }
    }
    Ok(clean)
}

/// Rebases a tar hardlink target onto the image root: `/bin/busybox` -> `<root>/bin/busybox`.
fn hardlink_target(root: &Path, entry: &Path, target: &Path) -> Result<PathBuf> {
    let relative = target.strip_prefix("/").unwrap_or(target);
    let clean = sanitize(relative).map_err(|_| Error::UnsafeLink {
        entry: entry.display().to_string(),
        target: target.display().to_string(),
    })?;
    Ok(root.join(clean))
}

/// Symlink targets are container-absolute and stored verbatim; only empty or NUL is invalid.
fn validate_symlink_target(entry: &Path, target: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let bytes = target.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.contains(&0) {
        return Err(Error::UnsafeLink {
            entry: entry.display().to_string(),
            target: target.display().to_string(),
        });
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                fs::remove_dir_all(path).map_err(|source| Error::io("remove", path, source))
            } else {
                fs::remove_file(path).map_err(|source| Error::io("remove", path, source))
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(Error::io("remove", path, err)),
    }
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777))
        .map_err(|source| Error::io("chmod", path, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whiteout_names_split_into_opaque_and_removals() {
        assert_eq!(Whiteout::parse(".wh..wh..opq"), Some(Whiteout::Opaque));
        assert_eq!(
            Whiteout::parse(".wh.old.txt"),
            Some(Whiteout::Remove("old.txt".to_owned()))
        );
        assert_eq!(Whiteout::parse("old.txt"), None);
        assert_eq!(Whiteout::parse(".wh."), None);
    }

    #[test]
    fn media_types_select_the_decompressor() {
        assert_eq!(
            Compression::from_media_type("application/vnd.oci.image.layer.v1.tar+gzip"),
            Some(Compression::Gzip)
        );
        assert_eq!(
            Compression::from_media_type("application/vnd.docker.image.rootfs.diff.tar.gzip"),
            Some(Compression::Gzip)
        );
        assert_eq!(
            Compression::from_media_type("application/vnd.oci.image.layer.v1.tar+zstd"),
            Some(Compression::Zstd)
        );
        assert_eq!(
            Compression::from_media_type("application/vnd.oci.image.layer.v1.tar"),
            Some(Compression::Plain)
        );
        assert_eq!(Compression::from_media_type("application/json"), None);
    }

    #[test]
    fn leading_dot_entries_normalize_to_the_image_root() {
        for root in [".", "./", "././"] {
            assert_eq!(
                sanitize(Path::new(root)).expect("root directory entry"),
                PathBuf::new(),
                "{root}"
            );
        }
        assert_eq!(
            sanitize(Path::new("./usr/")).expect("leading dot directory"),
            PathBuf::from("usr")
        );
        assert_eq!(
            sanitize(Path::new("./usr/./bin")).expect("nested leading dots"),
            PathBuf::from("usr/bin")
        );
        assert_eq!(
            sanitize(Path::new("usr//bin")).expect("double slash"),
            PathBuf::from("usr/bin")
        );
        for escape in ["../x", "usr/../../x", "/etc/passwd"] {
            assert!(
                sanitize(Path::new(escape)).is_err(),
                "`{escape}` must be refused"
            );
        }
    }

    #[test]
    fn a_python_style_archive_with_a_dot_root_entry_unpacks() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after the epoch")
            .as_nanos();
        let root =
            Path::new("/var/tmp").join(format!("slipfork-unpack-{}-{nanos}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut builder = tar::Builder::new(Vec::new());
        for (name, kind, body) in [
            ("./", tar::EntryType::Directory, &b""[..]),
            ("./usr/", tar::EntryType::Directory, &b""[..]),
            ("./usr/bin/python", tar::EntryType::Regular, b"#!/bin/sh\n"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_path(name).expect("tar entry name");
            header.set_cksum();
            builder.append(&header, body).expect("append a tar entry");
        }
        let bytes = builder.into_inner().expect("tar bytes");
        LayerUnpacker::new(&root)
            .apply(&mut &bytes[..], "application/vnd.oci.image.layer.v1.tar")
            .expect("a leading `./` root entry must unpack");
        assert_eq!(
            fs::read_to_string(root.join("usr/bin/python")).expect("read the unpacked file"),
            "#!/bin/sh\n"
        );
        fs::remove_dir_all(&root).expect("remove the unpack scratch tree");
    }

    #[test]
    fn entry_paths_must_stay_inside_the_root() {
        assert_eq!(
            sanitize(Path::new("app/./keep.txt")).expect("clean path"),
            PathBuf::from("app/keep.txt")
        );
        assert!(sanitize(Path::new("../escape.txt")).is_err());
        assert!(sanitize(Path::new("/etc/passwd")).is_err());
        assert!(sanitize(Path::new("app/../../escape")).is_err());
        assert!(sanitize(Path::new("")).is_err());
    }

    #[test]
    fn unsupported_entry_kinds_are_named() {
        assert_eq!(kind_label(tar::EntryType::Char), "character device");
        assert_eq!(kind_label(tar::EntryType::Block), "block device");
        assert_eq!(kind_label(tar::EntryType::Fifo), "fifo");
    }

    #[test]
    fn absolute_hardlinks_rebase_onto_the_image_root() {
        let root = Path::new("/rootfs");
        for target in ["/bin/busybox", "bin/busybox"] {
            assert_eq!(
                hardlink_target(root, Path::new("bin/arch"), Path::new(target))
                    .expect("a root-relative target"),
                PathBuf::from("/rootfs/bin/busybox"),
                "{target}"
            );
        }
        assert!(
            hardlink_target(root, Path::new("bin/evil"), Path::new("../../etc/shadow")).is_err()
        );
        assert!(hardlink_target(root, Path::new("bin/evil"), Path::new("/../etc/shadow")).is_err());
    }

    #[test]
    fn symlink_targets_are_container_absolute_and_stored_verbatim() {
        assert!(validate_symlink_target(Path::new("bin/sh"), Path::new("/bin/busybox")).is_ok());
        assert!(validate_symlink_target(Path::new("app/link"), Path::new("../../etc")).is_ok());
        assert!(validate_symlink_target(Path::new("app/link"), Path::new("")).is_err());
        assert!(validate_symlink_target(Path::new("app/link"), Path::new("a\0b")).is_err());
    }
}
