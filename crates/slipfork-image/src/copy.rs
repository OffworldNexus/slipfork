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
}

/// Applies `COPY` from the build context.
pub(crate) fn copy_context(
    context: &BuildContext,
    sources: &[String],
    dest: &str,
    destination: Destination<'_>,
    owner: Owner,
) -> Result<()> {
    let sources: Vec<String> = sources.iter().map(|source| normalize(source)).collect();
    let dest_abs = destination.resolve(dest)?;
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
    destination: Destination<'_>,
    owner: Owner,
) -> Result<()> {
    let sources: Vec<String> = sources.iter().map(|source| normalize(source)).collect();
    let dest_abs = destination.resolve(dest)?;
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
    destination: Destination<'_>,
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
            let dest_abs = destination.resolve(dest)?;
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
        copy_context(context, &plain, dest, destination, owner)?;
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::dockerfile::ImageConfig;

    use super::{BuildContext, Destination, Owner, add_context, copy_context, copy_rootfs, fs};

    fn lab(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let root = PathBuf::from("/var/tmp").join(format!(
            "slipfork-image-{prefix}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("the lab needs real disk under /var/tmp");
        root
    }

    fn owner(root: &std::path::Path) -> Owner {
        let metadata = fs::metadata(root).expect("stat the lab root");
        Owner {
            uid: metadata.uid(),
            gid: metadata.gid(),
        }
    }

    fn context(files: &[(&str, &str)]) -> (PathBuf, BuildContext) {
        let root = lab("context");
        for (relative, body) in files {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create a context directory");
            }
            fs::write(&path, body).expect("write a context file");
        }
        let context = BuildContext::load(&root).expect("load the real build context");
        (root, context)
    }

    /// `COPY package.json ./` and `COPY src ./src` land under `WORKDIR /app`, a later instruction
    /// still resolves against `/app`, and `ADD` follows the same rule.
    #[test]
    fn a_relative_destination_lands_under_the_stage_workdir() {
        let (context_root, context) = context(&[
            ("package.json", "{}\n"),
            ("src/app.py", "print('hi')\n"),
            ("README.md", "docs\n"),
        ]);
        let rootfs = lab("rootfs-app");
        let owner = owner(&rootfs);
        let mut config = ImageConfig::default();
        assert_eq!(config.set_workdir("/app"), "/app");

        copy_context(
            &context,
            &["package.json".to_owned()],
            "./",
            Destination::new(&rootfs, &config.workdir),
            owner,
        )
        .expect("COPY package.json ./");
        copy_context(
            &context,
            &["src".to_owned()],
            "./src",
            Destination::new(&rootfs, &config.workdir),
            owner,
        )
        .expect("COPY src ./src");
        copy_context(
            &context,
            &["README.md".to_owned()],
            "docs/",
            Destination::new(&rootfs, &config.workdir),
            owner,
        )
        .expect("a later COPY still uses /app");
        add_context(
            &context,
            &["package.json".to_owned()],
            "added/",
            Destination::new(&rootfs, &config.workdir),
            owner,
        )
        .expect("ADD follows WORKDIR too");

        assert_eq!(
            fs::read_to_string(rootfs.join("app/package.json")).expect("package.json under /app"),
            "{}\n"
        );
        assert_eq!(
            fs::read_to_string(rootfs.join("app/src/app.py")).expect("src under /app"),
            "print('hi')\n"
        );
        assert_eq!(
            fs::read_to_string(rootfs.join("app/docs/README.md"))
                .expect("the later COPY still sees /app"),
            "docs\n"
        );
        assert_eq!(
            fs::read_to_string(rootfs.join("app/added/package.json")).expect("ADD under /app"),
            "{}\n"
        );
        assert!(
            !rootfs.join("package.json").exists(),
            "a relative dest must never land at the rootfs top"
        );
        assert_eq!(
            config.workdir, "/app",
            "later instructions keep the WORKDIR"
        );

        let _ = fs::remove_dir_all(&context_root);
        let _ = fs::remove_dir_all(&rootfs);
    }

    /// `COPY --from=<stage>` resolves a relative destination against the same `WORKDIR`.
    #[test]
    fn copy_from_another_stage_follows_the_same_workdir_rule() {
        let source = lab("stage-src");
        fs::write(source.join("pkg.txt"), "from stage\n").expect("write a stage file");
        fs::create_dir_all(source.join("dir")).expect("create a stage directory");
        fs::write(source.join("dir/x.txt"), "x\n").expect("write a nested stage file");
        let rootfs = lab("rootfs-from");
        let owner = owner(&rootfs);
        let mut config = ImageConfig::default();
        assert_eq!(config.set_workdir("/app"), "/app");

        copy_rootfs(
            &source,
            &["pkg.txt".to_owned()],
            "./",
            Destination::new(&rootfs, &config.workdir),
            owner,
        )
        .expect("COPY --from file");
        copy_rootfs(
            &source,
            &["dir".to_owned()],
            "./vendor",
            Destination::new(&rootfs, &config.workdir),
            owner,
        )
        .expect("COPY --from directory");

        assert_eq!(
            fs::read_to_string(rootfs.join("app/pkg.txt")).expect("stage file under /app"),
            "from stage\n"
        );
        assert_eq!(
            fs::read_to_string(rootfs.join("app/vendor/x.txt")).expect("stage dir under /app"),
            "x\n"
        );

        let _ = fs::remove_dir_all(&source);
        let _ = fs::remove_dir_all(&rootfs);
    }

    /// An absolute destination roots at `/` even with a `WORKDIR`, and repeated slashes collapse.
    #[test]
    fn an_absolute_destination_still_roots_at_the_image_root() {
        let (context_root, context) = context(&[("app.conf", "k=v\n")]);
        let rootfs = lab("rootfs-abs");
        let owner = owner(&rootfs);
        let mut config = ImageConfig::default();
        assert_eq!(config.set_workdir("/app"), "/app");

        copy_context(
            &context,
            &["app.conf".to_owned()],
            "//etc//app/app.conf",
            Destination::new(&rootfs, &config.workdir),
            owner,
        )
        .expect("absolute COPY");

        assert_eq!(
            fs::read_to_string(rootfs.join("etc/app/app.conf")).expect("absolute dest at /etc"),
            "k=v\n"
        );
        assert!(
            !rootfs.join("app/etc").exists(),
            "an absolute dest ignores WORKDIR"
        );

        let _ = fs::remove_dir_all(&context_root);
        let _ = fs::remove_dir_all(&rootfs);
    }

    /// Any `..` segment is refused, relative or absolute, so a dest can never escape the rootfs.
    #[test]
    fn a_parent_segment_in_a_destination_is_refused() {
        let (context_root, context) = context(&[("app.conf", "k=v\n")]);
        let rootfs = lab("rootfs-escape");
        let owner = owner(&rootfs);
        let destination = Destination::new(&rootfs, "/app");

        for dest in ["../escape", "/etc/../escape", "./../../escape"] {
            let error = copy_context(&context, &["app.conf".to_owned()], dest, destination, owner)
                .expect_err("a `..` segment must fail closed");
            assert!(
                error.to_string().contains("escapes the image root"),
                "{dest}: {error}"
            );
        }
        assert!(!rootfs.join("escape").exists());

        let _ = fs::remove_dir_all(&context_root);
        let _ = fs::remove_dir_all(&rootfs);
    }
}
