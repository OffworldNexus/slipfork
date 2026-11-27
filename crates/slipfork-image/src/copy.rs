//! `COPY`/`ADD` filesystem application inside a writable step snapshot.

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::{chown, lchown, symlink};
use std::path::{Path, PathBuf};

use crate::context::{BuildContext, ContextEntry};
use crate::dockerfile::CopyOptions;
use crate::error::{Error, Result};
use crate::fetch;
use crate::ignore::SourcePattern;
use crate::metadata::{Mtime, set_mode, set_mtime};
use crate::runtime::rooted;

/// Where a `COPY`/`ADD` destination lands: the writable step rootfs plus the stage's current
/// `WORKDIR`, so a relative destination resolves exactly as Docker resolves it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Destination<'a> {
    rootfs: &'a Path,
    workdir: &'a str,
}

impl<'a> Destination<'a> {
    /// Binds the writable rootfs to the stage's current working directory.
    pub(crate) fn new(rootfs: &'a Path, workdir: &'a str) -> Self {
        Self { rootfs, workdir }
    }

    /// Resolves `dest` inside the rootfs: an absolute destination roots at `/`, a relative one
    /// joins `WORKDIR`, empty and `.` segments collapse, and any `..` is refused.
    pub(crate) fn resolve(&self, dest: &str) -> Result<PathBuf> {
        if dest.split('/').any(|segment| segment == "..") {
            return Err(step(format!("destination `{dest}` escapes the image root")));
        }
        let joined = if dest.starts_with('/') {
            dest.to_owned()
        } else {
            format!("{}/{dest}", self.workdir)
        };
        let mut path = self.rootfs.to_path_buf();
        for segment in joined.split('/') {
            if !segment.is_empty() && segment != "." {
                path.push(segment);
            }
        }
        Ok(path)
    }

    /// Whether `dest` names a directory given the full instruction's source count: a trailing
    /// slash, more than one source, or an already-existing directory makes every source keep its
    /// own file name inside `dest`.
    pub(crate) fn is_directory(&self, dest: &str, source_count: usize) -> Result<bool> {
        Ok(dest.ends_with('/') || source_count > 1 || self.resolve(dest)?.is_dir())
    }
}

/// Applies `COPY` from the build context.
pub(crate) fn copy_context(
    context: &BuildContext,
    sources: &[String],
    dest: &str,
    destination: Destination<'_>,
    options: CopyOptions,
) -> Result<()> {
    for source in sources {
        if let Some(scheme) = fetch::scheme(source) {
            return Err(step_next(
                format!("`COPY {source}` uses the `{scheme}:` scheme; `COPY` cannot fetch a URL"),
                "use `ADD` for remote sources, or copy a path from the build context",
            ));
        }
    }
    let sources: Vec<String> = sources.iter().map(|source| normalize(source)).collect();
    let dest_abs = destination.resolve(dest)?;
    let dest_is_dir = destination.is_directory(dest, sources.len())? || {
        let single = sources.first().map(String::as_str).unwrap_or(".");
        is_dir(context.entries(), single) || single == "."
    };

    let mut directories: Vec<(PathBuf, Mtime)> = Vec::new();
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
            copy_entry(&entry.absolute, &target, entry.is_dir, options)?;
            if entry.is_dir {
                directories.push((target, source_mtime(&entry.absolute)?));
            }
        }
    }
    // A directory's mtime lands after every child: writing a child re-stamps its parent.
    for (directory, mtime) in directories.iter().rev() {
        apply_chmod(directory, options.mode)?;
        set_mtime(directory, *mtime, true)?;
    }
    Ok(())
}

/// Applies `COPY --from=<stage>` from another stage's sealed rootfs.
pub(crate) fn copy_rootfs(
    source_root: &Path,
    sources: &[String],
    dest: &str,
    destination: Destination<'_>,
    options: CopyOptions,
) -> Result<()> {
    let sources: Vec<String> = sources.iter().map(|source| normalize(source)).collect();
    let dest_abs = destination.resolve(dest)?;
    let dest_is_dir = destination.is_directory(dest, sources.len())? || {
        let first = sources.first().map(String::as_str).unwrap_or("/");
        rooted(source_root, first).is_dir()
    };
    let mut directories: Vec<(PathBuf, Mtime)> = Vec::new();
    // An existing destination directory keeps its own mode; only directories this copy creates
    // receive `--chmod`.
    let dest_existed = dest_abs.exists();
    for source in &sources {
        let source_abs = rooted(source_root, source);
        let metadata = fs::symlink_metadata(&source_abs)
            .map_err(|_| step(format!("`{source}` does not exist in the source stage")))?;
        if metadata.is_dir() {
            let mut collected = Vec::new();
            walk(&source_abs, Path::new(""), &mut collected)?;
            if !dest_existed {
                directories.push((dest_abs.clone(), source_mtime(&source_abs)?));
            }
            for (relative, absolute, is_dir) in collected {
                let target = dest_abs.join(&relative);
                copy_entry(&absolute, &target, is_dir, options)?;
                if is_dir {
                    directories.push((target, source_mtime(&absolute)?));
                }
            }
        } else if dest_is_dir {
            copy_entry(
                &source_abs,
                &dest_abs.join(source_abs.file_name().unwrap_or_default()),
                false,
                options,
            )?;
        } else {
            copy_entry(&source_abs, &dest_abs, false, options)?;
        }
    }
    for (directory, mtime) in directories.iter().rev() {
        apply_chmod(directory, options.mode)?;
        set_mtime(directory, *mtime, true)?;
    }
    Ok(())
}

/// Applies `ADD`: local copies with tar archives extracted, plus streamed `http(s)://` sources.
///
/// Local archives keep their extraction semantics; a URL payload is written verbatim (never
/// unpacked). A URL layer is cached by instruction text alone — there is no checksum flag yet, so
/// an unchanged `ADD <url>` line reuses the sealed step even when the remote bytes change.
pub(crate) fn add_context(
    context: &BuildContext,
    sources: &[String],
    dest: &str,
    destination: Destination<'_>,
    options: CopyOptions,
) -> Result<()> {
    let mut plain = Vec::new();
    let mut urls = Vec::new();
    for source in sources {
        if fetch::is_http(source) {
            urls.push(source.as_str());
            continue;
        }
        if let Some(scheme) = fetch::scheme(source) {
            return Err(step_next(
                format!(
                    "`ADD {source}` uses the `{scheme}:` scheme, which the native builder cannot fetch"
                ),
                "use an `http://` or `https://` URL, or a path inside the build context",
            ));
        }
        let normalized = normalize(source);
        let path = context.root().join(&normalized);
        if path.is_file()
            && let Some(archive) = open_archive(&path)?
        {
            let dest_abs = destination.resolve(dest)?;
            let created = !dest_abs.exists();
            fs::create_dir_all(&dest_abs)
                .map_err(|source| Error::io("create", &dest_abs, source))?;
            let mut archive = tar::Archive::new(archive);
            // The `tar` crate restores header mtimes by default (`preserve_mtime: true`), so the
            // `ADD` archive path needs no metadata pass of its own.
            archive
                .unpack(&dest_abs)
                .map_err(|source| Error::io("extract", &dest_abs, source))?;
            chown(&dest_abs, Some(options.owner.uid), Some(options.owner.gid))
                .map_err(|source| Error::io("chown", &dest_abs, source))?;
            // A destination directory that already existed keeps its mode; every entry the
            // archive materialized still takes `--chmod`.
            apply_chmod_tree(&dest_abs, options.mode, created)?;
        } else {
            plain.push(source.clone());
        }
    }
    let dest_is_dir = destination.is_directory(dest, sources.len())?;
    if !plain.is_empty() {
        // A multi-source `ADD` (say one context file plus one URL) must treat `dest` as a
        // directory for the local copy too, even without a trailing slash.
        let local_dest = if dest_is_dir && !dest.ends_with('/') {
            format!("{dest}/")
        } else {
            dest.to_owned()
        };
        copy_context(context, &plain, &local_dest, destination, options)?;
    }
    if !urls.is_empty() {
        let dest_abs = destination.resolve(dest)?;
        for url in urls {
            let target = if dest_is_dir {
                fs::create_dir_all(&dest_abs)
                    .map_err(|source| Error::io("create", &dest_abs, source))?;
                dest_abs.join(fetch::remote_name(url)?)
            } else {
                dest_abs.clone()
            };
            fetch::fetch(url, &target, options)?;
        }
    }
    Ok(())
}

/// The canonical source resolution shared by `COPY`/`ADD` and per-instruction source keys.
///
/// The same `normalize` + wildcard/prefix + `.dockerignore`-filtered entry walk as
/// [`copy_context`], so a source Merkle can never disagree with what a copy actually writes.
pub(crate) fn matched_entries<'a>(
    context: &'a BuildContext,
    source: &str,
) -> Result<Vec<&'a ContextEntry>> {
    let source = normalize(source);
    let matches = select(context.entries(), &source)?;
    if matches.is_empty() {
        return Err(step(format!(
            "`{source}` matched no file in the build context"
        )));
    }
    Ok(matches)
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

fn copy_entry(source: &Path, dest: &Path, is_dir: bool, options: CopyOptions) -> Result<()> {
    let owner = options.owner;
    if is_dir {
        fs::create_dir_all(dest).map_err(|source| Error::io("create", dest, source))?;
        chown(dest, Some(owner.uid), Some(owner.gid))
            .map_err(|source| Error::io("chown", dest, source))?;
        // Directory mode and mtime are applied by the caller once their children are written, so a
        // `--chmod=555` directory never blocks its own contents and a child never re-stamps it.
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::io("create", parent, source))?;
    }
    let symlink_entry = fs::symlink_metadata(source)
        .map_err(|source_err| Error::io("inspect", source, source_err))?
        .file_type()
        .is_symlink();
    // Docker `COPY` preserves the source mtime; `fs::copy` and `symlink` stamp `now`, so the
    // source timestamp is restored explicitly (on a symlink without following it).
    let mtime = source_mtime(source)?;
    if symlink_entry {
        let target = fs::read_link(source)
            .map_err(|source_err| Error::io("readlink", source, source_err))?;
        let _ = fs::remove_file(dest);
        symlink(&target, dest).map_err(|source_err| Error::io("symlink", dest, source_err))?;
    } else {
        fs::copy(source, dest).map_err(|source_err| Error::io("copy", dest, source_err))?;
    }
    lchown(dest, Some(owner.uid), Some(owner.gid))
        .map_err(|source| Error::io("chown", dest, source))?;
    if !symlink_entry {
        apply_chmod(dest, options.mode)?;
    }
    set_mtime(dest, mtime, !symlink_entry)
}

/// The mtime a `COPY` must carry from `source` onto its destination.
fn source_mtime(source: &Path) -> Result<Mtime> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|source_err| Error::io("inspect", source, source_err))?;
    let modified = metadata
        .modified()
        .map_err(|source_err| Error::io("read the mtime of", source, source_err))?;
    Mtime::from_system_time(modified, source)
}

/// Applies a `--chmod` mode to one written entry; `None` leaves the copied mode alone.
fn apply_chmod(path: &Path, mode: Option<u32>) -> Result<()> {
    if let Some(mode) = mode {
        set_mode(path, mode)?;
    }
    Ok(())
}

/// Applies `--chmod` to every entry an archive extraction materialized, never through a symlink.
///
/// `root_is_new` is false when the destination directory predated the extraction, which keeps that
/// directory's own mode; its extracted children still take `--chmod`.
fn apply_chmod_tree(root: &Path, mode: Option<u32>, root_is_new: bool) -> Result<()> {
    if mode.is_none() {
        return Ok(());
    }
    if root_is_new {
        apply_chmod(root, mode)?;
    }
    let mut collected = Vec::new();
    walk(root, Path::new(""), &mut collected)?;
    for (_, absolute, _) in collected {
        let metadata = fs::symlink_metadata(&absolute)
            .map_err(|source| Error::io("inspect", &absolute, source))?;
        if !metadata.file_type().is_symlink() {
            apply_chmod(&absolute, mode)?;
        }
    }
    Ok(())
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
    step_next(
        message,
        "check the source path and the `.dockerignore` rules",
    )
}

fn step_next(message: String, next: impl Into<String>) -> Error {
    Error::StepFailed {
        line: 0,
        phase: "copy",
        instruction: String::new().into(),
        message,
        exit_code: None,
        runtime: None,
        next: next.into(),
    }
}

#[cfg(test)]
mod tests;
