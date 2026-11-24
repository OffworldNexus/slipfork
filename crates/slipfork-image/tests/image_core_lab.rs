//! Lab journey over the native image engine's pure edges: registry reference normalization, a real
//! build context on disk with `.dockerignore`, and a real layer unpack onto disk.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use slipfork_image::{
    Architecture, BuildArgs, BuildContext, BuildEvent, BuildRequest, Command, Credentials,
    DockerfileParser, Dockerignore, ImageConfig, ImageLayout, ImageReference, Instruction,
    LayerUnpacker, NativeImageBuilder, OciPuller, StepHost, StepLayout, StepLog, Variables,
    stage_walk,
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
    assert_eq!(
        layout.staging("sha256:0123456789abcdef"),
        Path::new("/store/images/oci/.partial-sha256-0123456789abcdef")
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

/// Dockerfile variables keep unknown references verbatim, and an unknown instruction fails closed.
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
        Credentials::Anonymous,
        Architecture::X86_64,
        "slipfork-tests/0.1".to_owned(),
    );
    let builder = NativeImageBuilder::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        puller,
        StepLayout::new(store.images_root()),
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
        .build(&request, &mut log)
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
        Credentials::Anonymous,
        Architecture::X86_64,
        "slipfork-tests/0.1".to_owned(),
    );
    let builder = NativeImageBuilder::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        puller,
        StepLayout::new(store.images_root()),
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
        .build(&request, &mut log)
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
