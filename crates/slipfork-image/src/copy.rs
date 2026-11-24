//! `COPY`/`ADD` filesystem application inside a writable step snapshot.

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::{PermissionsExt, chown, lchown, symlink};
use std::path::{Path, PathBuf};

use crate::context::{BuildContext, ContextEntry};
use crate::dockerfile::CopyOptions;
use crate::error::{Error, Result};
use crate::fetch;
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

    let mut directories = Vec::new();
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
                directories.push(target);
            }
        }
    }
    for directory in directories.iter().rev() {
        set_mode(directory, options.mode)?;
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
    let mut directories = Vec::new();
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
                directories.push(dest_abs.clone());
            }
            for (relative, absolute, is_dir) in collected {
                let target = dest_abs.join(&relative);
                copy_entry(&absolute, &target, is_dir, options)?;
                if is_dir {
                    directories.push(target);
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
    for directory in directories.iter().rev() {
        set_mode(directory, options.mode)?;
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
            archive
                .unpack(&dest_abs)
                .map_err(|source| Error::io("extract", &dest_abs, source))?;
            chown(&dest_abs, Some(options.owner.uid), Some(options.owner.gid))
                .map_err(|source| Error::io("chown", &dest_abs, source))?;
            // A destination directory that already existed keeps its mode; every entry the
            // archive materialized still takes `--chmod`.
            set_mode_tree(&dest_abs, options.mode, created)?;
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
    let mut symlink_entry = false;
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
            symlink_entry = true;
        } else {
            fs::copy(source, dest).map_err(|source_err| Error::io("copy", dest, source_err))?;
        }
    }
    let owner = options.owner;
    let result = if is_dir {
        chown(dest, Some(owner.uid), Some(owner.gid))
    } else {
        lchown(dest, Some(owner.uid), Some(owner.gid))
    };
    result.map_err(|source| Error::io("chown", dest, source))?;
    if symlink_entry || is_dir {
        // Directory modes are applied by the caller once their children are written, so a
        // `--chmod=555` directory never blocks its own contents.
        return Ok(());
    }
    set_mode(dest, options.mode)
}

/// Applies a `--chmod` mode to one written entry; `None` leaves the copied mode alone.
fn set_mode(path: &Path, mode: Option<u32>) -> Result<()> {
    if let Some(mode) = mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777))
            .map_err(|source| Error::io("chmod", path, source))?;
    }
    Ok(())
}

/// Applies `--chmod` to every entry an archive extraction materialized, never through a symlink.
///
/// `root_is_new` is false when the destination directory predated the extraction, which keeps that
/// directory's own mode; its extracted children still take `--chmod`.
fn set_mode_tree(root: &Path, mode: Option<u32>, root_is_new: bool) -> Result<()> {
    if mode.is_none() {
        return Ok(());
    }
    if root_is_new {
        set_mode(root, mode)?;
    }
    let mut collected = Vec::new();
    walk(root, Path::new(""), &mut collected)?;
    for (_, absolute, _) in collected {
        let metadata = fs::symlink_metadata(&absolute)
            .map_err(|source| Error::io("inspect", &absolute, source))?;
        if !metadata.file_type().is_symlink() {
            set_mode(&absolute, mode)?;
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
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::PathBuf;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::dockerfile::{CopyOptions, ImageConfig, Owner};
    use crate::error::Error;

    use super::{
        BuildContext, Destination, add_context, copy_context, copy_rootfs, fs, set_mode_tree,
    };

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
            CopyOptions::new(owner, None),
        )
        .expect("COPY package.json ./");
        copy_context(
            &context,
            &["src".to_owned()],
            "./src",
            Destination::new(&rootfs, &config.workdir),
            CopyOptions::new(owner, None),
        )
        .expect("COPY src ./src");
        copy_context(
            &context,
            &["README.md".to_owned()],
            "docs/",
            Destination::new(&rootfs, &config.workdir),
            CopyOptions::new(owner, None),
        )
        .expect("a later COPY still uses /app");
        add_context(
            &context,
            &["package.json".to_owned()],
            "added/",
            Destination::new(&rootfs, &config.workdir),
            CopyOptions::new(owner, None),
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
            CopyOptions::new(owner, None),
        )
        .expect("COPY --from file");
        copy_rootfs(
            &source,
            &["dir".to_owned()],
            "./vendor",
            Destination::new(&rootfs, &config.workdir),
            CopyOptions::new(owner, None),
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
            CopyOptions::new(owner, None),
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
            let error = copy_context(
                &context,
                &["app.conf".to_owned()],
                dest,
                destination,
                CopyOptions::new(owner, None),
            )
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

    /// `--chmod` reaches every written entry — files and directories — and a read-only directory
    /// still receives its children before the mode lands.
    #[test]
    fn copy_chmod_applies_to_every_written_entry() {
        let (context_root, context) =
            context(&[("bin/run.sh", "#!/bin/sh\n"), ("bin/nested/x.txt", "x\n")]);
        let rootfs = lab("rootfs-chmod");
        let options = CopyOptions::new(owner(&rootfs), Some(0o555));

        copy_context(
            &context,
            &["bin".to_owned()],
            "/opt/",
            Destination::new(&rootfs, "/"),
            options,
        )
        .expect("COPY --chmod=555 bin /opt/");

        for relative in ["opt", "opt/nested", "opt/run.sh"] {
            assert_eq!(
                fs::metadata(rootfs.join(relative))
                    .expect("stat a written entry")
                    .mode()
                    & 0o777,
                0o555,
                "`{relative}` carries `--chmod`"
            );
        }
        assert_eq!(
            fs::read_to_string(rootfs.join("opt/nested/x.txt")).expect("the nested file landed"),
            "x\n"
        );

        let _ = fs::remove_dir_all(&context_root);
        let _ = fs::remove_dir_all(&rootfs);
    }

    /// An extracted tree takes `--chmod` on its root and children, but a symlink is never followed.
    #[test]
    fn archive_chmod_tree_applies_to_entries_and_never_follows_a_symlink() {
        let root = lab("rootfs-chmod-tree");
        let tree = root.join("extracted");
        fs::create_dir_all(tree.join("nested")).expect("create the extracted tree");
        fs::write(tree.join("run.sh"), "#!/bin/sh\n").expect("write an extracted file");
        fs::write(tree.join("nested/x.txt"), "x\n").expect("write a nested extracted file");
        let outside = root.join("outside.txt");
        fs::write(&outside, "keep\n").expect("write the symlink target");
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o600))
            .expect("set the symlink target mode");
        symlink(&outside, tree.join("link")).expect("plant a symlink inside the tree");

        set_mode_tree(&tree, Some(0o750), true).expect("apply --chmod to the extracted tree");

        for relative in ["", "nested", "run.sh", "nested/x.txt"] {
            assert_eq!(
                fs::metadata(tree.join(relative))
                    .expect("stat an extracted entry")
                    .mode()
                    & 0o7777,
                0o750,
                "`{relative}` carries `--chmod`"
            );
        }
        let link = fs::symlink_metadata(tree.join("link")).expect("stat the symlink");
        assert!(link.file_type().is_symlink(), "the symlink stays a symlink");
        assert_eq!(
            fs::metadata(&outside)
                .expect("stat the symlink target")
                .mode()
                & 0o777,
            0o600,
            "`--chmod` must never follow a symlink out of the tree"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// Serves exactly one HTTP/1.1 response on a loopback ephemeral port, then closes the socket.
    fn serve_once(status: u16, body: &[u8]) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the loopback URL server");
        let port = listener
            .local_addr()
            .expect("read the loopback address")
            .port();
        let body = body.to_vec();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept the loopback request");
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("read the request head");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            let head = format!(
                "HTTP/1.1 {status} slipfork\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(head.as_bytes())
                .expect("write the response head");
            stream.write_all(&body).expect("write the response body");
            stream.flush().expect("flush the response");
        });
        (format!("127.0.0.1:{port}"), handle)
    }

    /// `ADD <url> /opt/data/` streams the served bytes into the directory as a 0644 file.
    #[test]
    fn an_add_url_streams_into_a_directory_destination() {
        const BODY: &[u8] = b"#!/bin/sh\necho offworld\n";
        let (addr, server) = serve_once(200, BODY);
        let (context_root, context) = context(&[]);
        let rootfs = lab("rootfs-url-dir");
        let owner = owner(&rootfs);

        add_context(
            &context,
            &[format!("http://{addr}/install.sh")],
            "/opt/data/",
            Destination::new(&rootfs, "/"),
            CopyOptions::new(owner, None),
        )
        .expect("ADD http://127.0.0.1/install.sh /opt/data/");
        server.join().expect("the loopback server exits");

        let written = rootfs.join("opt/data/install.sh");
        assert_eq!(fs::read(&written).expect("read the served payload"), BODY);
        assert_eq!(
            fs::metadata(&written).expect("stat the payload").mode() & 0o777,
            0o644,
            "a URL payload lands 0644"
        );

        let _ = fs::remove_dir_all(&context_root);
        let _ = fs::remove_dir_all(&rootfs);
    }

    /// An exact destination writes that path, ignoring the URL's query string.
    #[test]
    fn an_add_url_exact_destination_writes_the_named_file() {
        const BODY: &[u8] = b"spool\n";
        let (addr, server) = serve_once(200, BODY);
        let (context_root, context) = context(&[]);
        let rootfs = lab("rootfs-url-file");
        let owner = owner(&rootfs);

        add_context(
            &context,
            &[format!("http://{addr}/download?raw=1")],
            "/usr/local/bin/install.sh",
            Destination::new(&rootfs, "/"),
            CopyOptions::new(owner, None),
        )
        .expect("ADD url to an exact file path");
        server.join().expect("the loopback server exits");

        let written = rootfs.join("usr/local/bin/install.sh");
        assert_eq!(fs::read(&written).expect("read the served payload"), BODY);
        assert_eq!(
            fs::metadata(&written).expect("stat the payload").mode() & 0o777,
            0o644
        );
        assert!(
            !rootfs.join("usr/local/bin/download").exists(),
            "an exact destination never appends the URL file name"
        );

        let _ = fs::remove_dir_all(&context_root);
        let _ = fs::remove_dir_all(&rootfs);
    }

    /// A non-2xx answer fails closed with the typed HTTP error and leaves no temp file behind.
    #[test]
    fn a_non_success_url_fails_closed_and_cleans_its_temp_file() {
        let (addr, server) = serve_once(404, b"missing\n");
        let (context_root, context) = context(&[]);
        let rootfs = lab("rootfs-url-404");
        let owner = owner(&rootfs);

        let error = add_context(
            &context,
            &[format!("http://{addr}/install.sh")],
            "/opt/data/",
            Destination::new(&rootfs, "/"),
            CopyOptions::new(owner, None),
        )
        .expect_err("a 404 must fail closed");
        server.join().expect("the loopback server exits");

        assert!(
            matches!(&error, Error::Http { status: 404, .. }),
            "a non-2xx is the typed HTTP error: {error}"
        );
        let leftovers = fs::read_dir(rootfs.join("opt/data"))
            .expect("the destination directory exists")
            .count();
        assert_eq!(leftovers, 0, "a failed fetch removes its temp file");

        let _ = fs::remove_dir_all(&context_root);
        let _ = fs::remove_dir_all(&rootfs);
    }

    /// Only `ADD` fetches, and only `http(s)://`; every other scheme fails closed with a remedy.
    #[test]
    fn only_add_fetches_http_urls() {
        let (context_root, context) = context(&[("app.conf", "k=v\n")]);
        let rootfs = lab("rootfs-url-refuse");
        let owner = owner(&rootfs);
        let destination = Destination::new(&rootfs, "/");

        let copy_error = copy_context(
            &context,
            &["http://127.0.0.1:9/x".to_owned()],
            "/opt/",
            destination,
            CopyOptions::new(owner, None),
        )
        .expect_err("COPY must never fetch a URL");
        assert!(
            copy_error.to_string().contains("use `ADD`"),
            "COPY points at ADD: {copy_error}"
        );

        let add_error = add_context(
            &context,
            &["ftp://127.0.0.1:9/x".to_owned()],
            "/opt/",
            destination,
            CopyOptions::new(owner, None),
        )
        .expect_err("only http(s) is fetchable");
        let rendered = add_error.to_string();
        assert!(
            rendered.contains("`ftp:`"),
            "the scheme is named: {rendered}"
        );
        assert!(rendered.contains("next:"), "one line plus next: {rendered}");

        let _ = fs::remove_dir_all(&context_root);
        let _ = fs::remove_dir_all(&rootfs);
    }

    /// One context file plus one URL into a slash-less `dest` still resolves as a directory, so
    /// both sources keep their own names instead of the local copy claiming `dest` itself.
    #[test]
    fn a_mixed_add_puts_the_local_file_and_the_url_in_one_directory() {
        const BODY: &[u8] = b"#!/bin/sh\necho served\n";
        let (addr, server) = serve_once(200, BODY);
        let (context_root, context) = context(&[("note.txt", "local note\n")]);
        let rootfs = lab("rootfs-url-mixed");
        let owner = owner(&rootfs);

        add_context(
            &context,
            &["note.txt".to_owned(), format!("http://{addr}/install.sh")],
            "/opt",
            Destination::new(&rootfs, "/"),
            CopyOptions::new(owner, None),
        )
        .expect("ADD a context file and a URL into one directory");
        server.join().expect("the loopback server exits");

        assert_eq!(
            fs::read_to_string(rootfs.join("opt/note.txt")).expect("read the local copy"),
            "local note\n"
        );
        assert_eq!(
            fs::read(rootfs.join("opt/install.sh")).expect("read the served payload"),
            BODY
        );

        let _ = fs::remove_dir_all(&context_root);
        let _ = fs::remove_dir_all(&rootfs);
    }

    /// A gzipped body is an opaque URL payload: it lands byte-for-byte and is never unpacked.
    #[test]
    fn a_gzipped_url_payload_is_written_verbatim_never_extracted() {
        let raw = b"#!/bin/sh\necho gz\n";
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(raw).expect("write the gzip payload");
        let gz = encoder.finish().expect("finish the gzip payload");
        let (addr, server) = serve_once(200, &gz);
        let (context_root, context) = context(&[]);
        let rootfs = lab("rootfs-url-gzip");
        let owner = owner(&rootfs);

        add_context(
            &context,
            &[format!("http://{addr}/bundle.tar.gz")],
            "/opt/data/",
            Destination::new(&rootfs, "/"),
            CopyOptions::new(owner, None),
        )
        .expect("ADD a gzipped url payload");
        server.join().expect("the loopback server exits");

        assert_eq!(
            fs::read(rootfs.join("opt/data/bundle.tar.gz")).expect("read the payload"),
            gz,
            "a URL payload is never tar-extracted"
        );
        assert!(
            !rootfs.join("opt/data/bundle.tar").exists(),
            "no archive is unpacked from a URL"
        );

        let _ = fs::remove_dir_all(&context_root);
        let _ = fs::remove_dir_all(&rootfs);
    }
}
