//! Destination resolution, `--chmod`, and mtime-preserving `COPY`/`ADD` lab tests.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use crate::dockerfile::{CopyOptions, ImageConfig};
use crate::metadata::{Mtime, set_mtime};

use super::super::{Destination, add_context, apply_chmod_tree, copy_context, copy_rootfs};
use super::{context, lab, owner};

/// A fixed instant every `COPY` fixture source is stamped with, so a copy that writes `now`
/// cannot pass unnoticed.
const COPY_INSTANT: u64 = 1_800_000_000;

/// Pins one fixture entry's mtime; `File::open` also opens a directory for `set_times`.
fn stamp(path: &Path) {
    fs::File::open(path)
        .expect("open a fixture entry")
        .set_times(
            fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(COPY_INSTANT)),
        )
        .expect("stamp the fixture mtime");
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
    for relative in ["bin", "bin/nested", "bin/run.sh", "bin/nested/x.txt"] {
        stamp(&context_root.join(relative));
    }
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
    // The copy stamped `now`; both the file and the deferred directory loop must restore the
    // source's instant instead.
    let expected = UNIX_EPOCH + Duration::from_secs(COPY_INSTANT);
    for relative in ["opt", "opt/nested", "opt/run.sh"] {
        assert_eq!(
            fs::metadata(rootfs.join(relative))
                .expect("stat a written entry")
                .modified()
                .expect("read the copied mtime"),
            expected,
            "`{relative}` carries the source mtime"
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
    let instant = Mtime::from_unix_secs(COPY_INSTANT, &tree.join("link"))
        .expect("a fixed fixture instant fits `utimensat`");
    set_mtime(&tree.join("link"), instant, false).expect("stamp the symlink fixture");

    apply_chmod_tree(&tree, Some(0o750), true).expect("apply --chmod to the extracted tree");

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

    // `COPY --chmod` of a symlink entry must skip the mode write entirely: `set_permissions`
    // follows the link, so a container-absolute target would resolve inside the host root.
    let rootfs = root.join("rootfs");
    fs::create_dir_all(&rootfs).expect("create the copy destination rootfs");
    copy_rootfs(
        &tree,
        &["link".to_owned()],
        "/copied",
        Destination::new(&rootfs, "/"),
        CopyOptions::new(owner(&rootfs), Some(0o700)),
    )
    .expect("COPY --chmod a symlink entry");
    let copied = fs::symlink_metadata(rootfs.join("copied")).expect("stat the copied link");
    assert!(copied.file_type().is_symlink(), "the entry stays a symlink");
    assert_eq!(
        copied.modified().expect("read the copied link mtime"),
        UNIX_EPOCH + Duration::from_secs(COPY_INSTANT),
        "the copied symlink keeps its source mtime, read without following"
    );
    assert_eq!(
        fs::metadata(&outside)
            .expect("stat the symlink target")
            .mode()
            & 0o777,
        0o600,
        "`--chmod` of a symlink must never follow it onto its target"
    );

    let _ = fs::remove_dir_all(&root);
}
