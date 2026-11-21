//! `COPY`/`ADD` filesystem application inside a writable step snapshot.

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::{chown, lchown, symlink};
use std::path::{Path, PathBuf};

use crate::context::{BuildContext, ContextEntry};
use crate::dockerfile::Owner;
use crate::error::{Error, Result};
use crate::ignore::SourcePattern;
use crate::runtime::rooted;

/// Applies `COPY` from the build context.
pub(crate) fn copy_context(
    context: &BuildContext,
    sources: &[String],
    dest: &str,
    rootfs: &Path,
    owner: Owner,
) -> Result<()> {
    let sources: Vec<String> = sources.iter().map(|source| normalize(source)).collect();
    let dest_abs = resolve_dest(rootfs, dest)?;
    let dest_is_dir = dest.ends_with('/') || sources.len() > 1 || {
        let single = sources.first().map(String::as_str).unwrap_or(".");
        is_dir(context.entries(), single) || single == "."
    };

    for source in &sources {
        let matches = select(context.entries(), source)?;
        if matches.is_empty() {
            return Err(step(format!(
                "`{source}` matched no file in the build context"
            )));
        }
        let source_is_file =
            matches.len() == 1 && !matches[0].is_dir && matches[0].relative == Path::new(source);
        for entry in matches {
            let target = if source_is_file && !dest_is_dir {
                dest_abs.clone()
            } else if source_is_file {
                dest_abs.join(entry.relative.file_name().unwrap_or_default())
            } else {
                let stripped = entry
                    .relative
                    .strip_prefix(source)
                    .unwrap_or(&entry.relative);
                dest_abs.join(stripped)
            };
            copy_entry(&entry.absolute, &target, entry.is_dir, owner)?;
        }
    }
    Ok(())
}

/// Applies `COPY --from=<stage>` from another stage's sealed rootfs.
pub(crate) fn copy_rootfs(
    source_root: &Path,
    sources: &[String],
    dest: &str,
    rootfs: &Path,
    owner: Owner,
) -> Result<()> {
    let sources: Vec<String> = sources.iter().map(|source| normalize(source)).collect();
    let dest_abs = resolve_dest(rootfs, dest)?;
    let dest_is_dir = dest.ends_with('/') || sources.len() > 1 || {
        let first = sources.first().map(String::as_str).unwrap_or("/");
        rooted(source_root, first).is_dir()
    };
    for source in &sources {
        let source_abs = rooted(source_root, source);
        let metadata = fs::symlink_metadata(&source_abs)
            .map_err(|_| step(format!("`{source}` does not exist in the source stage")))?;
        if metadata.is_dir() {
            let mut collected = Vec::new();
            walk(&source_abs, Path::new(""), &mut collected)?;
            for (relative, absolute, is_dir) in collected {
                copy_entry(&absolute, &dest_abs.join(relative), is_dir, owner)?;
            }
        } else if dest_is_dir {
            copy_entry(
                &source_abs,
                &dest_abs.join(source_abs.file_name().unwrap_or_default()),
                false,
                owner,
            )?;
        } else {
            copy_entry(&source_abs, &dest_abs, false, owner)?;
        }
    }
    Ok(())
}

/// Applies `ADD`: local copies, with tar archives extracted into the destination.
pub(crate) fn add_context(
    context: &BuildContext,
    sources: &[String],
    dest: &str,
    rootfs: &Path,
    owner: Owner,
) -> Result<()> {
    let mut plain = Vec::new();
    for source in sources {
        if source.starts_with("http://") || source.starts_with("https://") {
            return Err(step(format!(
                "`ADD {source}` fetches a URL, which the native builder does not support"
            )));
        }
        let normalized = normalize(source);
        let path = context.root().join(&normalized);
        if path.is_file()
            && let Some(archive) = open_archive(&path)?
        {
            let dest_abs = resolve_dest(rootfs, dest)?;
            fs::create_dir_all(&dest_abs)
                .map_err(|source| Error::io("create", &dest_abs, source))?;
            let mut archive = tar::Archive::new(archive);
            archive
                .unpack(&dest_abs)
                .map_err(|source| Error::io("extract", &dest_abs, source))?;
            chown(&dest_abs, Some(owner.uid), Some(owner.gid))
                .map_err(|source| Error::io("chown", &dest_abs, source))?;
        } else {
            plain.push(source.clone());
        }
    }
    if !plain.is_empty() {
        copy_context(context, &plain, dest, rootfs, owner)?;
    }
    Ok(())
}

fn select<'a>(entries: &'a [ContextEntry], source: &str) -> Result<Vec<&'a ContextEntry>> {
    if source == "." || source == "./" {
        return Ok(entries.iter().collect());
    }
    if let Some(pattern) = SourcePattern::parse(source)
        && pattern.is_glob()
    {
        return Ok(entries
            .iter()
            .filter(|entry| pattern.matches(&entry.relative))
            .collect());
    }
    Ok(entries
        .iter()
        .filter(|entry| {
            entry.relative == Path::new(source) || entry.relative.starts_with(format!("{source}/"))
        })
        .collect())
}

fn is_dir(entries: &[ContextEntry], source: &str) -> bool {
    entries
        .iter()
        .any(|entry| entry.relative == Path::new(source) && entry.is_dir)
}

fn copy_entry(source: &Path, dest: &Path, is_dir: bool, owner: Owner) -> Result<()> {
    if is_dir {
        fs::create_dir_all(dest).map_err(|source| Error::io("create", dest, source))?;
    } else {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|source| Error::io("create", parent, source))?;
        }
        let metadata = fs::symlink_metadata(source)
            .map_err(|source_err| Error::io("inspect", source, source_err))?;
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(source)
                .map_err(|source_err| Error::io("readlink", source, source_err))?;
            let _ = fs::remove_file(dest);
            symlink(&target, dest).map_err(|source_err| Error::io("symlink", dest, source_err))?;
        } else {
            fs::copy(source, dest).map_err(|source_err| Error::io("copy", dest, source_err))?;
        }
    }
    let result = if is_dir {
        chown(dest, Some(owner.uid), Some(owner.gid))
    } else {
        lchown(dest, Some(owner.uid), Some(owner.gid))
    };
    result.map_err(|source| Error::io("chown", dest, source))
}

fn walk(root: &Path, relative: &Path, out: &mut Vec<(PathBuf, PathBuf, bool)>) -> Result<()> {
    let directory = root.join(relative);
    for child in fs::read_dir(&directory).map_err(|source| Error::io("read", &directory, source))? {
        let child = child.map_err(|source| Error::io("read", &directory, source))?;
        let relative = relative.join(child.file_name());
        let absolute = child.path();
        let metadata = fs::symlink_metadata(&absolute)
            .map_err(|source| Error::io("inspect", &absolute, source))?;
        let is_dir = metadata.is_dir();
        out.push((relative.clone(), absolute, is_dir));
        if is_dir {
            walk(root, &relative, out)?;
        }
    }
    Ok(())
}

fn resolve_dest(rootfs: &Path, dest: &str) -> Result<PathBuf> {
    if dest.split('/').any(|segment| segment == "..") {
        return Err(step(format!("destination `{dest}` escapes the image root")));
    }
    Ok(rooted(rootfs, dest))
}

fn normalize(source: &str) -> String {
    let source = source.trim_start_matches("./");
    let source = source.trim_end_matches('/');
    if source.is_empty() {
        ".".to_owned()
    } else {
        source.to_owned()
    }
}

/// Opens `path` when it is a gzip/zstd/plain tar archive; `None` for an ordinary file.
///
/// One magic probe decides both "is this an archive" and which decompressor to wrap, so `ADD`
/// never sniffs the same header twice.
fn open_archive(path: &Path) -> Result<Option<Box<dyn Read>>> {
    let mut probe = fs::File::open(path).map_err(|source| Error::io("open", path, source))?;
    let mut magic = [0u8; 512];
    let read = io::Read::read(&mut probe, &mut magic).unwrap_or(0);
    let head = &magic[..read];
    let gzip = head.starts_with(&[0x1f, 0x8b]);
    let zstd = head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]);
    let tar = head.len() >= 262 && &head[257..262] == b"ustar";
    if !(gzip || zstd || tar) {
        return Ok(None);
    }
    let file = fs::File::open(path).map_err(|source| Error::io("open", path, source))?;
    let reader: Box<dyn Read> = if gzip {
        Box::new(flate2::read::GzDecoder::new(file))
    } else if zstd {
        Box::new(
            zstd::stream::read::Decoder::new(file)
                .map_err(|source| Error::io("open", path, source))?,
        )
    } else {
        Box::new(file)
    };
    Ok(Some(reader))
}

fn step(message: String) -> Error {
    Error::StepFailed {
        line: 0,
        instruction: "COPY/ADD".to_owned(),
        message,
        next: "check the source path and the `.dockerignore` rules".to_owned(),
    }
}
