//! Lab journey over the native image engine's pure edges: registry reference normalization, a real
//! build context on disk with `.dockerignore`, and a real layer unpack onto disk.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use slipfork_image::{
    Architecture, BuildArgs, BuildContext, BuildEvent, BuildRequest, Command, DockerfileParser,
    Dockerignore, ImageConfig, ImageLayout, ImageReference, Instruction, LayerUnpacker,
    NativeImageBuilder, OciPuller, RegistryCredentials, StepHost, StepLog, Variables,
    build_inputs_digest, stage_walk,
};
use slipfork_world::{BtrfsEngine, StoreLayout, WorldEngine};

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

#[test]
fn docker_hub_references_normalize_and_hostile_names_are_refused() {
    let reference = ImageReference::parse("postgres:18").expect("valid reference");
    assert_eq!(reference.registry(), "registry-1.docker.io");
    assert_eq!(reference.repository(), "library/postgres");
    assert_eq!(reference.tag(), Some("18"));
    assert_eq!(
        reference.to_string(),
        "registry-1.docker.io/library/postgres:18"
    );
    for raw in [
        "docker.io/library/alpine:3.20",
        "index.docker.io/library/alpine:3.20",
        "registry.hub.docker.com/library/alpine:3.20",
    ] {
        let reference = ImageReference::parse(raw).expect("valid Hub reference");
        assert_eq!(reference.registry(), "registry-1.docker.io", "{raw}");
        assert_eq!(reference.repository(), "library/alpine", "{raw}");
    }
    let digest = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let pinned = ImageReference::parse(&format!("quay.io/o/i@{digest}")).expect("digest ref");
    assert_eq!(pinned.registry(), "quay.io");
    assert_eq!(pinned.tag(), None);
    assert_eq!(pinned.digest(), Some(digest));
    for bad in [
        "",
        "Postgres",
        "postgres:",
        "docker.io/",
        "docker.io//x",
        "repo@sha256:XYZ",
        "repo@sha256",
        "repo:tag:bad",
        "repo:with space",
        "..",
    ] {
        assert!(ImageReference::parse(bad).is_err(), "`{bad}`");
    }
    let layout = ImageLayout::new("/store/images");
    assert_eq!(layout.root(), Path::new("/store/images/oci"));
    assert_eq!(
        layout.image("sha256:0123456789abcdef"),
        Path::new("/store/images/oci/sha256-0123456789abcdef")
    );
}

#[test]
fn a_real_build_context_hashes_stably_and_honours_dockerignore() {
    let root = lab("context");
    fs::create_dir_all(root.join("src")).expect("create the context");
    fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("write source");
    fs::write(root.join("debug.log"), "noise\n").expect("write log");
    fs::write(root.join(".dockerignore"), "*.log\n").expect("write ignore");
    fs::create_dir_all(root.join(".slipfork-store/images/oci/sha256-x")).expect("seed store");
    fs::create_dir_all(root.join(".forks/feat")).expect("seed forks");

    let context = BuildContext::load(&root).expect("load the context");
    let paths: Vec<String> = context
        .entries()
        .iter()
        .map(|entry| entry.relative.to_string_lossy().into_owned())
        .collect();
    assert!(paths.contains(&"src/main.rs".to_owned()), "{paths:?}");
    assert!(!paths.contains(&"debug.log".to_owned()), "{paths:?}");
    assert!(!paths.contains(&".dockerignore".to_owned()), "{paths:?}");
    assert!(
        !paths
            .iter()
            .any(|path| path.starts_with(".slipfork") || path.starts_with(".forks")),
        "managed state is never part of the context: {paths:?}"
    );
    let first = context.hash().expect("hash");
    assert_eq!(first, context.hash().expect("hash again"));
    fs::write(root.join("src/main.rs"), "fn main() { }\n").expect("edit the source");
    let edited = BuildContext::load(&root).expect("reload");
    assert_ne!(first, edited.hash().expect("new hash"));

    let ignore = Dockerignore::parse(
        "# comment lines are not patterns\nnode_modules/\nsrc/[a-c]*.rs\n/root.txt\n*.log\n!keep.log\n**/*.tmp\n",
    );
    assert!(ignore.is_ignored(Path::new("node_modules/react/index.js")));
    let question = Dockerignore::parse("debug?.log\n");
    assert!(question.is_ignored(Path::new("debug1.log")));
    assert!(
        !question.is_ignored(Path::new("debug10.log")),
        "`?` matches exactly one character"
    );
    assert!(ignore.is_ignored(Path::new("src/apple.rs")));
    assert!(!ignore.is_ignored(Path::new("src/zebra.rs")));
    assert!(
        ignore.is_ignored(Path::new("root.txt")),
        "a leading `/` anchors at the context root"
    );
    assert!(
        !ignore.is_ignored(Path::new("vendor/root.txt")),
        "an anchored pattern never matches a nested copy"
    );
    assert!(ignore.is_ignored(Path::new("app/debug.log")));
    assert!(!ignore.is_ignored(Path::new("app/keep.log")));
    assert!(ignore.is_ignored(Path::new("src/a/b/c.tmp")));
    assert!(!ignore.is_ignored(Path::new("src/main.rs")));
    assert!(!ignore.is_ignored(Path::new("comment")));

    // The demo's nested-output rules must prune a dependency tree that lives below the root.
    let nested = Dockerignore::parse(
        "**/.venv/**\n**/node_modules/**\n**/build/**\n**/.svelte-kit/**\n**/__pycache__/**\n**/*.pyc\n",
    );
    for path in [
        ".venv/lib/python3.13/site-packages/x.py",
        "front/node_modules/svelte/index.js",
        "front/build/index.js",
        "front/.svelte-kit/output/server.js",
        "app/__pycache__/main.cpython-313.pyc",
        "app/legacy.pyc",
    ] {
        assert!(nested.is_ignored(Path::new(path)), "{path} must be ignored");
    }
    for path in [
        "front/package.json",
        "front/src/routes/+page.svelte",
        "app/main.py",
    ] {
        assert!(!nested.is_ignored(Path::new(path)), "{path} must survive");
    }
    let _ = fs::remove_dir_all(&root);
}

/// The canonical build-input digest tracks the `Dockerfile` and copied context, and ignores a
/// checkout edit for an image that copies nothing (so bind-mounted sources never force rebuilds).
#[test]
fn build_inputs_digest_tracks_the_dockerfile_copied_context_and_args() {
    let root = lab("inputs-digest");
    fs::create_dir_all(root.join("src")).expect("create the context");
    fs::write(root.join("src/app.txt"), "one").expect("write source");
    fs::write(root.join("debug.log"), "noise").expect("write ignored log");
    fs::write(root.join(".dockerignore"), "*.log\n").expect("write ignore");
    let dockerfile = root.join("Dockerfile");
    fs::write(
        &dockerfile,
        "FROM alpine:3.20\nCOPY src /src\nRUN echo one\n",
    )
    .expect("write the Dockerfile");

    let first = build_inputs_digest(&root, &dockerfile, None, &BuildArgs::new()).expect("digest");
    assert_eq!(
        first,
        build_inputs_digest(&root, &dockerfile, None, &BuildArgs::new()).expect("digest again")
    );
    // An ignored file is not a build input.
    fs::write(root.join("debug.log"), "more noise").expect("edit the ignored log");
    assert_eq!(
        first,
        build_inputs_digest(&root, &dockerfile, None, &BuildArgs::new())
            .expect("ignored edit keeps the digest")
    );
    // A copied context file is.
    fs::write(root.join("src/app.txt"), "two").expect("edit the copied source");
    let context_edited =
        build_inputs_digest(&root, &dockerfile, None, &BuildArgs::new()).expect("context digest");
    assert_ne!(
        first, context_edited,
        "a copied file edit must change the key"
    );
    // So is the Dockerfile itself.
    fs::write(
        &dockerfile,
        "FROM alpine:3.20\nCOPY src /src\nRUN echo two\n",
    )
    .expect("edit the Dockerfile");
    let dockerfile_edited = build_inputs_digest(&root, &dockerfile, None, &BuildArgs::new())
        .expect("Dockerfile digest");
    assert_ne!(
        context_edited, dockerfile_edited,
        "a Dockerfile edit must change the key"
    );
    // A `Dockerfile` with no host `COPY`/`ADD` never walks the checkout.
    fs::write(&dockerfile, "FROM alpine:3.20\nRUN echo three\n").expect("drop the COPY");
    let no_context = build_inputs_digest(&root, &dockerfile, None, &BuildArgs::new())
        .expect("no-context digest");
    fs::write(root.join("src/app.txt"), "three").expect("edit an uncopied source");
    assert_eq!(
        no_context,
        build_inputs_digest(&root, &dockerfile, None, &BuildArgs::new())
            .expect("uncopied edit keeps the digest")
    );
    let _ = fs::remove_dir_all(&root);
}

/// M3.166: the resolved build `arg` map is a build input. Identical args keep the digest, a `PORT`
/// value change moves it, and an empty map differs from a populated one.
#[test]
fn build_inputs_digest_tracks_resolved_build_args() {
    let root = lab("inputs-args");
    fs::create_dir_all(root.join("src")).expect("create the context");
    fs::write(root.join("src/app.txt"), "one").expect("write source");
    let dockerfile = root.join("Dockerfile");
    fs::write(
        &dockerfile,
        "FROM alpine:3.20\nCOPY src /src\nARG PORT=3000\n",
    )
    .expect("write the Dockerfile");

    let empty = build_inputs_digest(&root, &dockerfile, None, &BuildArgs::new()).expect("digest");
    let mut port_3000 = BuildArgs::new();
    port_3000.declare("PORT", "3000");
    let sealed = build_inputs_digest(&root, &dockerfile, None, &port_3000).expect("args digest");
    assert_ne!(empty, sealed, "an empty and a populated args map differ");

    let mut same = BuildArgs::new();
    same.declare("PORT", "3000");
    assert_eq!(
        sealed,
        build_inputs_digest(&root, &dockerfile, None, &same).expect("same args keep the digest")
    );

    let mut port_5173 = BuildArgs::new();
    port_5173.declare("PORT", "5173");
    assert_ne!(
        sealed,
        build_inputs_digest(&root, &dockerfile, None, &port_5173).expect("changed arg digest"),
        "a changed `PORT` value must move the digest"
    );
    let _ = fs::remove_dir_all(&root);
}

/// M3.129: VCS metadata is never a build input. A `fork` branch switch rewrites `.git/HEAD`,
/// `.git/index`, and `.git/logs`, so a root-context image (`COPY . .`) must keep its digest and
/// `.git` must never reach the `COPY` payload (a root `.git` directory, a linked worktree's
/// `.git` file, and a nested repository alike). A real copied-file edit still moves the digest.
#[test]
fn a_git_only_change_never_moves_the_build_input_digest_or_reaches_copy() {
    let root = lab("git-context");
    fs::create_dir_all(root.join("src")).expect("create the context");
    fs::write(root.join("src/app.txt"), "one").expect("write source");
    fs::create_dir_all(root.join(".git/logs")).expect("create the checkout metadata");
    fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").expect("write HEAD");
    fs::write(root.join(".git/index"), "index-main").expect("write the index");
    fs::write(root.join(".git/logs/HEAD"), "main\n").expect("write the reflog");
    fs::create_dir_all(root.join("vendor/dep/.git")).expect("create a nested repository");
    fs::write(root.join("vendor/dep/.git/HEAD"), "nested\n").expect("write the nested HEAD");
    fs::create_dir_all(root.join("vendor/wt")).expect("create a linked worktree");
    fs::write(
        root.join("vendor/wt/.git"),
        "gitdir: ../../.git/worktrees/wt\n",
    )
    .expect("write a worktree `.git` file");
    fs::write(root.join("vendor/wt/real.txt"), "real\n").expect("write a real worktree file");
    let dockerfile = root.join("Dockerfile");
    fs::write(&dockerfile, "FROM alpine:3.20\nCOPY . /code\n").expect("write the Dockerfile");

    let first = build_inputs_digest(&root, &dockerfile, None, &BuildArgs::new()).expect("digest");
    let paths: Vec<String> = BuildContext::load(&root)
        .expect("load the real build context")
        .entries()
        .iter()
        .map(|entry| entry.relative.to_string_lossy().into_owned())
        .collect();
    assert!(
        !paths
            .iter()
            .any(|path| path.split('/').any(|part| part == ".git")),
        "VCS metadata must never reach `COPY`: {paths:?}"
    );
    assert!(paths.contains(&"src/app.txt".to_owned()), "{paths:?}");
    assert!(
        paths.contains(&"vendor/wt/real.txt".to_owned()),
        "a worktree's real files still ship: {paths:?}"
    );

    // Exactly what `VcsWorkspace::switch` rewrites when the fork checks out another branch.
    fs::write(root.join(".git/HEAD"), "ref: refs/heads/feat\n").expect("switch HEAD");
    fs::write(root.join(".git/index"), "index-feat").expect("rewrite the index");
    fs::write(root.join(".git/logs/HEAD"), "main\nfeat\n").expect("append the reflog");
    fs::write(root.join("vendor/dep/.git/HEAD"), "nested-feat\n").expect("switch the submodule");
    fs::write(
        root.join("vendor/wt/.git"),
        "gitdir: ../../.git/worktrees/wt2\n",
    )
    .expect("switch the worktree");
    assert_eq!(
        first,
        build_inputs_digest(&root, &dockerfile, None, &BuildArgs::new())
            .expect("branch-switch digest"),
        "a branch switch must not change a root-context image's inputs"
    );

    // A real copied source edit and the Dockerfile still move the digest.
    fs::write(root.join("src/app.txt"), "two").expect("edit the copied source");
    let edited =
        build_inputs_digest(&root, &dockerfile, None, &BuildArgs::new()).expect("edited digest");
    assert_ne!(first, edited, "a copied file edit must rebuild");
    fs::write(&dockerfile, "FROM alpine:3.20\nCOPY . /code\nRUN true\n")
        .expect("edit the Dockerfile");
    assert_ne!(
        edited,
        build_inputs_digest(&root, &dockerfile, None, &BuildArgs::new())
            .expect("Dockerfile digest"),
        "a Dockerfile edit must rebuild"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn dockerfile_variables_keep_unknown_refs_and_bogus_instructions_error() {
    let mut args = BuildArgs::new();
    args.declare("VITE_CDN_URL", "http://static.localhost");
    let mut env = BTreeMap::new();
    env.insert("APP_MODE".to_owned(), "production".to_owned());
    let variables = Variables::new(&args, &env);
    assert_eq!(
        variables.expand("$VITE_CDN_URL/${APP_MODE}/$MISSING"),
        "http://static.localhost/production/$MISSING"
    );
    assert_eq!(
        variables.expand("${UNSET}/x"),
        "${UNSET}/x",
        "unknown `${{...}}` references stay verbatim"
    );
    assert_eq!(
        variables.expand("$UNSET/x"),
        "$UNSET/x",
        "unknown `$NAME` references stay verbatim"
    );

    let error = DockerfileParser::parse("FROM base\nFROBNICATE x\n")
        .expect_err("an unknown instruction must fail");
    assert!(
        error
            .to_string()
            .contains("unknown instruction `FROBNICATE`"),
        "{error}"
    );
}

/// `COPY --chmod` parses both `755` and `0o755` to the same octal mode, renders without the `0o`
/// prefix, and refuses a non-octal value with a line-decorated `Dockerfile` error.
#[test]
fn copy_chmod_parses_octal_and_renders_canonically() {
    for source in [
        "FROM base\nCOPY --chmod=755 src /dst\n",
        "FROM base\nCOPY --chmod=0o755 src /dst\n",
    ] {
        let stages = DockerfileParser::parse(source).expect("a valid octal `--chmod`");
        let parsed = &stages[0].instructions[0];
        let Instruction::Copy(spec) = &parsed.instruction else {
            panic!("expected COPY, saw {:?}", parsed.instruction);
        };
        assert_eq!(spec.mode, Some(0o755), "{source}");
        assert_eq!(
            parsed.instruction.render(),
            "COPY --chmod=755 src /dst",
            "render prints the bare octal mode"
        );
    }

    let error = DockerfileParser::parse("FROM base\nCOPY --chmod=999 src /dst\n")
        .expect_err("`999` is not octal");
    let rendered = error.to_string();
    assert!(
        rendered.contains("Dockerfile:2:"),
        "the line is named: {rendered}"
    );
    assert!(
        rendered.contains("--chmod=999"),
        "the flag is named: {rendered}"
    );
}

/// A missing `crun` is a typed failure before any build step touches a subvolume.
#[test]
fn a_missing_crun_fails_typed_before_any_step_runs() {
    let root = lab("crun");
    let dockerfile = root.join("Dockerfile");
    fs::write(&dockerfile, "FROM scratch\nRUN true\n").expect("write the Dockerfile");
    let store = StoreLayout::new(root.join("store"));
    let engine = Arc::new(BtrfsEngine::new(store.clone()));
    let puller = OciPuller::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        ImageLayout::new(store.images_root()),
        RegistryCredentials::default(),
        Architecture::X86_64,
        "slipfork-tests/0.1".to_owned(),
    );
    let builder = NativeImageBuilder::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        puller,
        store.clone(),
        Architecture::X86_64,
        StepHost::new(
            "/nonexistent/crun",
            "/etc/resolv.conf",
            root.join("bundles"),
            1000,
            1000,
        ),
    );
    let request = BuildRequest {
        context: root.clone(),
        dockerfile,
        args: BuildArgs::new(),
        target: None,
        externals: std::collections::BTreeMap::new(),
    };
    let mut events = |_: BuildEvent<'_>| {};
    let mut log = StepLog::with_events(&mut events);
    let error = builder
        .build(&request, &mut log, 1_000)
        .expect_err("a missing crun must fail closed");
    assert!(
        matches!(error, slipfork_image::Error::CrunMissing { .. }),
        "unexpected error: {error}"
    );
    assert!(
        !store.images_root().join("oci").exists(),
        "zero build steps may run before the runtime check"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_real_tar_layer_unpacks_a_dot_root_entry_onto_disk() {
    let root = lab("unpack");
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
        builder.append(&header, body).expect("append the tar entry");
    }
    let bytes = builder.into_inner().expect("tar bytes");
    LayerUnpacker::new(root.join("rootfs"))
        .apply(&mut &bytes[..], "application/vnd.oci.image.layer.v1.tar")
        .expect("a leading `./` root entry must unpack");
    assert_eq!(
        fs::read_to_string(root.join("rootfs/usr/bin/python")).expect("read the unpacked file"),
        "#!/bin/sh\n"
    );
    let _ = fs::remove_dir_all(&root);
}

/// M3.143: every materialized entry keeps the mtime its layer header states. Without this, Debian's
/// baked `/var/lib/apt/lists/*InRelease` looks freshly fetched, the mirror answers `304`, and `apt`
/// rejects the image-build-era `Valid-Until`. A file written after unpacking (a `RUN`) keeps `now`.
#[test]
fn unpacked_layers_keep_header_mtimes_and_later_writes_stay_fresh() {
    const BAKED: u64 = 1_793_836_800; // 2026-11-05T00:00:00Z
    const PATCHED: u64 = 1_793_577_600; // 2026-11-02T00:00:00Z
    let start = SystemTime::now();
    let root = lab("unpack-mtime");
    let rootfs = root.join("rootfs");

    let mut base = tar::Builder::new(Vec::new());
    let mut dir = tar::Header::new_gnu();
    dir.set_entry_type(tar::EntryType::Directory);
    dir.set_size(0);
    dir.set_mode(0o755);
    dir.set_mtime(BAKED);
    dir.set_path("opt/").expect("directory path");
    dir.set_cksum();
    base.append(&dir, &b""[..]).expect("append the directory");

    let mut file = tar::Header::new_gnu();
    file.set_entry_type(tar::EntryType::Regular);
    file.set_size(6);
    file.set_mode(0o644);
    file.set_mtime(BAKED);
    file.set_path("opt/state.txt").expect("file path");
    file.set_cksum();
    base.append(&file, &b"baked\n"[..])
        .expect("append the file");

    let mut link = tar::Header::new_gnu();
    link.set_entry_type(tar::EntryType::Symlink);
    link.set_size(0);
    link.set_mode(0o777);
    link.set_mtime(BAKED);
    link.set_path("opt/current").expect("link path");
    link.set_link_name("state.txt").expect("link target");
    link.set_cksum();
    base.append(&link, &b""[..]).expect("append the symlink");

    let mut patch = tar::Builder::new(Vec::new());
    let mut later = tar::Header::new_gnu();
    later.set_entry_type(tar::EntryType::Regular);
    later.set_size(7);
    later.set_mode(0o644);
    later.set_mtime(PATCHED);
    later.set_path("opt/later.txt").expect("file path");
    later.set_cksum();
    patch
        .append(&later, &b"later\n\n"[..])
        .expect("append the patched file");

    let unpacker = LayerUnpacker::new(&rootfs);
    let base_layer = gzip_layer(&base.into_inner().expect("base tar"));
    unpacker
        .apply(
            &mut base_layer.as_slice(),
            "application/vnd.oci.image.layer.v1.tar+gzip",
        )
        .expect("unpack the base layer");
    let baked = UNIX_EPOCH + Duration::from_secs(BAKED);
    for path in ["opt", "opt/state.txt", "opt/current"] {
        let entry = fs::symlink_metadata(rootfs.join(path)).expect("stat the unpacked entry");
        assert_eq!(
            entry.modified().expect("read the mtime"),
            baked,
            "`{path}` must keep the layer header's mtime (a symlink via nofollow)"
        );
    }

    let patch_layer = gzip_layer(&patch.into_inner().expect("patch tar"));
    unpacker
        .apply(
            &mut patch_layer.as_slice(),
            "application/vnd.oci.image.layer.v1.tar+gzip",
        )
        .expect("unpack the patch layer");
    let patched = UNIX_EPOCH + Duration::from_secs(PATCHED);
    assert_eq!(
        fs::symlink_metadata(rootfs.join("opt/later.txt"))
            .expect("stat the patched file")
            .modified()
            .expect("read the mtime"),
        patched,
        "each layer keeps its own header mtime"
    );
    assert_eq!(
        fs::symlink_metadata(rootfs.join("opt/state.txt"))
            .expect("stat the untouched file")
            .modified()
            .expect("read the mtime"),
        baked,
        "a later layer never re-stamps an untouched file"
    );

    // A `RUN` writes after unpacking: the resulting file keeps `now`, never a layer timestamp.
    fs::write(rootfs.join("opt/written.txt"), b"run\n").expect("write a run artifact");
    let written = fs::metadata(rootfs.join("opt/written.txt"))
        .expect("stat the run artifact")
        .modified()
        .expect("read the mtime");
    // The kernel stamps mtimes from its coarse tick, which may trail `Clock::now` by a jiffy.
    let earliest = start - Duration::from_secs(1);
    assert!(
        written >= earliest,
        "a file written after unpacking keeps `now`, saw {written:?} before {earliest:?}"
    );
    let _ = fs::remove_dir_all(&root);
}

fn gzip_layer(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write as _;

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).expect("gzip the layer");
    encoder.finish().expect("finish the gzip layer")
}

/// Real `--target` semantics: stages walk in declaration order and stop right after the requested
/// one, so a later stage's failing `RUN` is never reached.
#[test]
fn a_target_stage_stops_the_walk_before_later_failing_stages() {
    let source = "FROM scratch AS dev\n\
                  ENV APP_MODE=dev\n\
                  FROM scratch AS builder\n\
                  RUN echo built > /out.txt\n\
                  FROM scratch AS prod\n\
                  RUN sh -c 'pnpm run build'\n";
    let stages = DockerfileParser::parse(source).expect("the three-stage fixture parses");
    assert_eq!(stages.len(), 3);
    let prod = stages.last().expect("three stages");
    assert_eq!(prod.alias.as_deref(), Some("prod"));
    assert!(
        prod.instructions
            .iter()
            .any(|parsed| matches!(parsed.instruction, Instruction::Run(_))),
        "the fixture's final stage must carry the failing RUN"
    );

    let dev = stage_walk(&stages, Some("dev")).expect("`dev` is declared");
    assert_eq!(dev.end, 0, "a `dev` target must stop after the first stage");
    assert!(
        dev.end < stages.len() - 1,
        "the walk must never reach the prod RUN"
    );

    // Targeting the last stage still walks there, exactly like a targetless build.
    assert_eq!(
        stage_walk(&stages, Some("prod"))
            .expect("`prod` is declared")
            .end,
        2
    );
    assert_eq!(
        stage_walk(&stages, None)
            .expect("a Dockerfile has a final stage")
            .end,
        2
    );
    assert_eq!(
        stage_walk(&stages, Some("DEV"))
            .expect("aliases match case-insensitively")
            .end,
        0
    );

    let error = stage_walk(&stages, Some("staging")).expect_err("an absent alias must fail");
    assert!(
        matches!(error, slipfork_image::Error::UnknownStage { ref name } if name == "staging"),
        "unexpected error: {error}"
    );
}

/// The builder rejects an absent `--target` before `crun`, the context, or any base image is touched,
/// even when a later, unreachable stage carries a `RUN`.
#[test]
fn an_absent_target_alias_fails_before_any_build_work() {
    let root = lab("target");
    let dockerfile = root.join("Dockerfile");
    fs::write(
        &dockerfile,
        "FROM scratch AS dev\nENV APP_MODE=dev\nFROM scratch AS prod\nRUN true\n",
    )
    .expect("write the Dockerfile");
    let store = StoreLayout::new(root.join("store"));
    let engine = Arc::new(BtrfsEngine::new(store.clone()));
    let puller = OciPuller::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        ImageLayout::new(store.images_root()),
        RegistryCredentials::default(),
        Architecture::X86_64,
        "slipfork-tests/0.1".to_owned(),
    );
    let builder = NativeImageBuilder::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        puller,
        store.clone(),
        Architecture::X86_64,
        StepHost::new(
            "/nonexistent/crun",
            "/etc/resolv.conf",
            root.join("bundles"),
            1000,
            1000,
        ),
    );
    let request = BuildRequest {
        context: root.clone(),
        dockerfile,
        args: BuildArgs::new(),
        target: Some("staging".to_owned()),
        externals: std::collections::BTreeMap::new(),
    };
    let mut events = |_: BuildEvent<'_>| {};
    let mut log = StepLog::with_events(&mut events);
    let error = builder
        .build(&request, &mut log, 1_000)
        .expect_err("an absent target alias must fail closed");
    assert!(
        matches!(error, slipfork_image::Error::UnknownStage { ref name } if name == "staging"),
        "unexpected error: {error}"
    );
    assert!(
        !store.images_root().join("oci").exists(),
        "resolving the target must precede every pull and step"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn entrypoint_and_cmd_compose_into_one_exec_argv() {
    let mut config = ImageConfig::default();
    assert!(config.exec_argv().is_empty());
    config.apply(&Instruction::Entrypoint(Command::Exec(vec![
        "node".to_owned(),
        "server.js".to_owned(),
    ])));
    config.apply(&Instruction::Cmd(Command::Exec(vec![
        "--port".to_owned(),
        "3000".to_owned(),
    ])));
    assert_eq!(config.exec_argv(), ["node", "server.js", "--port", "3000"]);
    config.apply(&Instruction::Entrypoint(Command::Shell(
        "exec python3".to_owned(),
    )));
    assert_eq!(config.exec_argv(), ["/bin/sh", "-c", "exec python3"]);
}
