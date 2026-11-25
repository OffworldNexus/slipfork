//! Pure path/env/resource helpers for the world boot runtime.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::rootfs::{is_file_in_rootfs, shim_exists};
use slipfork_forkfile::{FeatureSet, direct_bin};
use slipfork_world::{Bind, BindSource, Identity, PodEnv, SnapshotStore, StoreLayout, WorldEngine};

use super::ImageHead;
use super::dev::DevMount;

const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// One `snapshots/<key>.json` node record, as far as the identity lookup needs it.
#[derive(serde::Deserialize)]
struct SealedRecord {
    kind: String,
    #[serde(default)]
    parents: Vec<String>,
    #[serde(default)]
    facts: Option<serde_json::Value>,
}

/// A sealed-image identity failure: one cause line and the one remedy that fixes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IdentityFailure {
    cause: String,
    remedy: String,
}

impl IdentityFailure {
    fn new(cause: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            cause: cause.into(),
            remedy: remedy.into(),
        }
    }

    /// The one-line cause (never carries a `next:` tail).
    pub(super) fn cause(&self) -> &str {
        &self.cause
    }

    /// The concrete fix, rendered as the failure's single `next:`.
    pub(super) fn remedy(&self) -> &str {
        &self.remedy
    }
}

impl From<slipfork_world::IdentityError> for IdentityFailure {
    fn from(error: slipfork_world::IdentityError) -> Self {
        Self::new(error.cause(), error.remedy())
    }
}

/// Resolves the identity a sealed image must run as (default container root).
///
/// Two records describe one sealed image: the head key names the last *materialized* step and the
/// image node that wraps it carries the final-stage `ImageConfig`, whose `user` is the `Dockerfile`
/// `USER`. A pulled image seals no `user` fact, which is Docker's `0:0` default.
pub(super) fn sealed_identity(
    layout: &StoreLayout,
    engine: &dyn WorldEngine,
    built: &ImageHead,
) -> Result<Identity, IdentityFailure> {
    let store = SnapshotStore::new(layout, engine);
    let user = sealed_user(&store, built)?;
    // The one resolver: build `RUN` steps resolve the same `ImageConfig.user` for the same rootfs
    // through this identical `Identity::resolve` call (`slipfork-image/src/runtime.rs`).
    Identity::resolve(&built.rootfs, user.as_deref()).map_err(IdentityFailure::from)
}

/// The `USER` fact of the image node that wraps the sealed head, if the image names one.
///
/// A Dockerfile-built head must have that image record; a pulled image has none and keeps Docker's
/// `0:0` default, so neither case can silently fall back to the host identity.
fn sealed_user(
    store: &SnapshotStore<'_>,
    built: &ImageHead,
) -> Result<Option<String>, IdentityFailure> {
    if let Some(record) = read_record(&store.record_path(&built.key))?
        && record.kind == "image"
        && let Some(facts) = record.facts.as_ref()
    {
        return Ok(field(facts));
    }
    // The image node is the head's single child: its key hashes the head's key as parent.
    let parent = built.key.as_hex();
    let entries = fs::read_dir(store.snapshots_root()).map_err(|error| {
        IdentityFailure::new(
            format!(
                "cannot list the sealed snapshots in `{}`: {error}",
                store.snapshots_root().display()
            ),
            "run `slipfork up` to rebuild this world",
        )
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("json") {
            continue;
        }
        let Some(record) = read_record(&path)? else {
            continue;
        };
        if record.kind == "image"
            && record.parents.as_slice() == [parent.as_str()]
            && let Some(facts) = record.facts.as_ref()
        {
            return Ok(field(facts));
        }
    }
    if built.state == "pulled" {
        return Ok(None);
    }
    Err(IdentityFailure::new(
        format!(
            "the sealed image `{}` has no recorded `USER`",
            built.key.as_hex()
        ),
        "run `slipfork up` to rebuild this world",
    ))
}

/// The `facts.user` string of one image config, when it names a `USER`.
fn field(facts: &serde_json::Value) -> Option<String> {
    facts.get("user")?.as_str().map(str::to_owned)
}

/// Reads one node record, mapping every unreadable-or-corrupt case to one remedy line.
fn read_record(path: &Path) -> Result<Option<SealedRecord>, IdentityFailure> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
            IdentityFailure::new(
                format!(
                    "cannot parse the sealed image record `{}`: {error}",
                    path.display()
                ),
                "run `slipfork up` to rebuild this world",
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(IdentityFailure::new(
            format!(
                "cannot read the sealed image record `{}`: {error}",
                path.display()
            ),
            "run `slipfork up` to rebuild this world",
        )),
    }
}

pub(super) fn project_bind(project: &Path) -> Bind {
    Bind {
        source: BindSource::Host(project.to_path_buf()),
        destination: "/project".to_owned(),
        read_only: false,
    }
}

/// The `/project` bind a pod gets: only a `dev` world sees the live checkout.
pub(super) fn project_binds(features: &FeatureSet, project: &Path) -> Vec<Bind> {
    if features.contains("dev") {
        vec![project_bind(project)]
    } else {
        Vec::new()
    }
}

/// The binds a `dev` pod gets: the checkout at `/project` plus every Dockerfile destination the
/// stage's live context must overlay (`WORKDIR`, whole-context `COPY` destination). `/` is never
/// a bind target.
pub(super) fn dev_binds(
    features: &FeatureSet,
    project: &Path,
    mount: Option<&DevMount>,
) -> Vec<Bind> {
    let mut binds = project_binds(features, project);
    if let Some(mount) = mount {
        for destination in mount.overlays.iter().filter(|dest| dest.as_str() != "/") {
            binds.push(Bind {
                source: BindSource::Host(mount.context.clone()),
                destination: destination.clone(),
                read_only: false,
            });
        }
    }
    binds
}

pub(super) fn merge_env(
    resolved: &BTreeMap<String, BTreeMap<String, (String, bool)>>,
    subject: &str,
    image: Option<&str>,
    port: Option<u16>,
) -> Vec<PodEnv> {
    let mut merged = BTreeMap::new();
    for component in image.into_iter().chain(std::iter::once(subject)) {
        if let Some(vars) = resolved.get(component) {
            for (key, (value, _)) in vars {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    if let Some(port) = port {
        merged.insert("PORT".to_owned(), port.to_string());
    }
    merged
        .into_iter()
        .map(|(key, value)| PodEnv::new(&key, &value))
        .collect()
}

pub(super) fn shell_command(command: &str) -> String {
    command.strip_prefix('@').unwrap_or(command).to_owned()
}

/// The effective launch `PATH`: a `Forkfile` `env` entry wins over the image `ENV`, which wins
/// over the sandbox default.
pub(super) fn merge_path(pod_env: &[PodEnv], inherited_env: &[String]) -> String {
    pod_env
        .iter()
        .find(|entry| entry.key == "PATH")
        .map(|entry| entry.value.clone())
        .or_else(|| {
            inherited_env
                .iter()
                .find_map(|pair| pair.strip_prefix("PATH=").map(str::to_owned))
        })
        .unwrap_or_else(|| DEFAULT_PATH.to_owned())
}

/// Replaces `PATH`/`VIRTUAL_ENV` in the merged pod env instead of appending duplicate keys.
pub(super) fn upsert_env(env: &mut Vec<PodEnv>, entries: Vec<PodEnv>) {
    for entry in entries {
        env.retain(|existing| existing.key != entry.key);
        env.push(entry);
    }
}

/// A resolved service launch: the exact argv to exec plus any additive env entries that point
/// package managers at the project's own shims.
pub(super) struct ServiceLaunch {
    /// Executable argv handed to the pod (`/project/.venv/bin/uvicorn …` when resolvable).
    pub(super) argv: Vec<String>,
    /// Additive `PATH`/`VIRTUAL_ENV` entries, empty when the wrapper could not be removed.
    pub(super) env: Vec<PodEnv>,
}

/// One `@tool exec` prefix already rewritten away from its `corepack`/`uv` wrapper.
struct ResolvedPrefix {
    /// Container-absolute path of the real binary, e.g. `/project/.venv/bin/uvicorn`.
    program: String,
    /// Verbatim remainder after the binary token, including its leading separator.
    rest: String,
    /// Container directory prepended to `PATH`.
    bin_dir: String,
    /// `VIRTUAL_ENV` for a Python venv wrapper.
    venv: Option<String>,
}

impl ServiceLaunch {
    /// Resolves a service `run:` command directly to the project's installed binary, removing the
    /// `@uv run`/`@pnpm exec` startup cost only when the host-side shim really exists.
    ///
    /// Commands that still carry shell metacharacters run through `/bin/sh -c` — now without the
    /// wrapper when the shim resolved — so `$port` substitutions and quoting stay byte-for-byte.
    pub(super) fn resolve(
        project: &Path,
        container_workdir: &str,
        rootfs: &Path,
        path: &str,
        command: &str,
    ) -> Self {
        let (text, trusted_absolute, env) =
            match resolve_wrapper(project, container_workdir, rootfs, command) {
                Some(prefix) => {
                    let env = prefix_env(&prefix, path);
                    (format!("{}{}", prefix.program, prefix.rest), true, env)
                }
                None => (shell_command(command), false, Vec::new()),
            };
        Self {
            argv: exec_argv(rootfs, path, &text, trusted_absolute),
            env,
        }
    }
}

/// Maps `built.workdir` back to the host project directory that the `/project` bind exposes.
fn host_workdir(project: &Path, container_workdir: &str) -> Option<PathBuf> {
    match container_workdir.strip_prefix("/project/") {
        Some(rest) if !rest.is_empty() => Some(project.join(rest)),
        _ if container_workdir == "/project" => Some(project.to_path_buf()),
        _ => None,
    }
}

/// Rewrites `@uv run <bin> …` / `@pnpm exec <bin> …` to the in-project binary when it exists.
fn resolve_wrapper(
    project: &Path,
    container_workdir: &str,
    rootfs: &Path,
    command: &str,
) -> Option<ResolvedPrefix> {
    let host_dir = host_workdir(project, container_workdir)?;
    let direct = direct_bin(command)?;
    if !shim_exists(&host_dir.join(direct.exec.bin_dir).join(direct.bin), rootfs) {
        return None;
    }
    Some(ResolvedPrefix {
        program: direct.program(container_workdir),
        rest: direct.rest.to_owned(),
        bin_dir: direct.bin_dir(container_workdir),
        venv: (direct.tool.name == "uv").then(|| format!("{container_workdir}/.venv")),
    })
}

/// Prepends the shim directory to the pod's effective `PATH` and, for `uv`, exports `VIRTUAL_ENV`.
fn prefix_env(prefix: &ResolvedPrefix, path: &str) -> Vec<PodEnv> {
    let mut env = vec![PodEnv::new("PATH", &format!("{}:{path}", prefix.bin_dir))];
    if let Some(venv) = &prefix.venv {
        env.push(PodEnv::new("VIRTUAL_ENV", venv));
    }
    env
}

/// Builds the final argv, falling back to `/bin/sh -c` for shell syntax or an unresolved binary.
fn exec_argv(rootfs: &Path, path: &str, text: &str, trusted_absolute: bool) -> Vec<String> {
    if !has_shell_meta(text) {
        let parts: Vec<&str> = text.split_ascii_whitespace().collect();
        if let Some((program, rest)) = parts.split_first() {
            if trusted_absolute {
                let mut argv = Vec::with_capacity(parts.len());
                argv.push((*program).to_owned());
                argv.extend(rest.iter().map(|arg| (*arg).to_owned()));
                return argv;
            }
            if let Some(resolved) = resolve_in_rootfs(rootfs, path, program) {
                let mut argv = Vec::with_capacity(parts.len());
                argv.push(resolved);
                argv.extend(rest.iter().map(|arg| (*arg).to_owned()));
                return argv;
            }
        }
    }
    vec!["/bin/sh".to_owned(), "-c".to_owned(), text.to_owned()]
}

fn has_shell_meta(text: &str) -> bool {
    text.bytes().any(|byte| {
        matches!(
            byte,
            b'|' | b'&'
                | b';'
                | b'<'
                | b'>'
                | b'('
                | b')'
                | b'$'
                | b'`'
                | b'"'
                | b'\''
                | b'\\'
                | b'*'
                | b'?'
                | b'['
                | b']'
                | b'#'
                | b'~'
                | b'='
                | b'%'
                | b'{'
                | b'}'
                | b'!'
                | b'\n'
                | b'\r'
        )
    })
}

fn resolve_in_rootfs(rootfs: &Path, path: &str, program: &str) -> Option<String> {
    if program.contains('/') {
        let relative = program.trim_start_matches('/');
        return is_file_in_rootfs(rootfs, relative).then(|| {
            if program.starts_with('/') {
                program.to_owned()
            } else {
                format!("/{program}")
            }
        });
    }
    for dir in path.split(':').filter(|dir| !dir.is_empty()) {
        let relative = format!("{}/{program}", dir.trim_start_matches('/'));
        if is_file_in_rootfs(rootfs, &relative) {
            return Some(format!("/{relative}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::{ServiceLaunch, project_binds};
    use slipfork_world::PodEnv;

    /// `/project` is a dev-only bind: dropping `dev` leaves pods on the built rootfs alone.
    #[test]
    fn only_a_dev_world_binds_the_live_project() {
        let source = "# forkfile: 0.1\n\nimage base from python:3.13-slim {\n  service api {\n    run: sleep 3600\n  }\n}\n";
        let checked = slipfork_forkfile::compile(source).expect("compile the fixture");
        let dev = checked.document.default_features();
        assert!(dev.contains("dev"));
        let binds = project_binds(&dev, std::path::Path::new("/var/tmp/demo"));
        assert_eq!(binds.len(), 1);
        assert_eq!(binds[0].destination, "/project");

        let bare = checked
            .document
            .resolve_features(true, &[])
            .expect("resolve an empty base");
        assert!(!bare.contains("dev"));
        assert!(
            project_binds(&bare, std::path::Path::new("/var/tmp/demo")).is_empty(),
            "a non-dev world must not see the live checkout"
        );
    }

    /// A `WORKDIR /app` + `COPY . /srv` stage overlays the context at both destinations; a
    /// `COPY . .` stage keeps one `/app` overlay.
    #[test]
    fn a_dev_mount_binds_the_context_at_the_workdir_and_every_copy_destination() {
        let checked = slipfork_forkfile::compile("# forkfile: 0.1\n\nfeatures {\n  dev\n}\n")
            .expect("compile the fixture");
        let dev = checked.document.default_features();
        let project = std::path::Path::new("/var/tmp/demo");
        let split = super::DevMount {
            workdir: "/app".to_owned(),
            context: std::path::PathBuf::from("/var/tmp/demo"),
            overlays: vec!["/app".to_owned(), "/srv".to_owned()],
        };
        let binds = super::dev_binds(&dev, project, Some(&split));
        let destinations: Vec<&str> = binds.iter().map(|bind| bind.destination.as_str()).collect();
        assert_eq!(destinations, ["/project", "/app", "/srv"]);
        assert!(
            binds.iter().all(|bind| bind.destination != "/"),
            "a dev bind must never overlay the rootfs root"
        );

        let same = super::DevMount {
            overlays: vec!["/app".to_owned()],
            ..split
        };
        let same_binds = super::dev_binds(&dev, project, Some(&same));
        let destinations: Vec<&str> = same_binds
            .iter()
            .map(|bind| bind.destination.as_str())
            .collect();
        assert_eq!(destinations, ["/project", "/app"]);
    }

    fn touch(root: &TempDir, relative: &str) {
        let path = root.path().join(relative);
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture dirs");
        fs::write(path, b"#!/bin/sh\n").expect("write fixture binary");
    }

    fn path_of(launch: &ServiceLaunch) -> String {
        launch
            .env
            .iter()
            .find(|entry| entry.key == "PATH")
            .map(|entry| entry.value.clone())
            .expect("resolved launch carries PATH")
    }

    #[test]
    fn merge_path_prefers_a_declared_forkfile_path_then_the_image_path() {
        let declared = vec![PodEnv::new("PATH", "/declared/bin")];
        let image = vec!["PATH=/image/bin".to_owned()];
        assert_eq!(super::merge_path(&declared, &image), "/declared/bin");
        assert_eq!(super::merge_path(&[], &image), "/image/bin");
        assert_eq!(super::merge_path(&[], &[]), super::DEFAULT_PATH);

        let mut env = vec![
            PodEnv::new("PATH", "/declared/bin"),
            PodEnv::new("HOME", "/root"),
        ];
        super::upsert_env(&mut env, vec![PodEnv::new("PATH", "/shim:/declared/bin")]);
        assert_eq!(env.iter().filter(|entry| entry.key == "PATH").count(), 1);
        assert_eq!(env[1].value, "/shim:/declared/bin");
    }

    #[test]
    fn uv_run_resolves_to_the_project_venv_binary_without_the_wrapper() {
        let project = TempDir::new().expect("project");
        let rootfs = TempDir::new().expect("rootfs");
        touch(&project, ".venv/bin/uvicorn");
        let launch = ServiceLaunch::resolve(
            project.path(),
            "/project",
            rootfs.path(),
            super::DEFAULT_PATH,
            "@uv run uvicorn app.main:app --reload --host 0.0.0.0 --port 8000",
        );
        assert_eq!(
            launch.argv,
            [
                "/project/.venv/bin/uvicorn",
                "app.main:app",
                "--reload",
                "--host",
                "0.0.0.0",
                "--port",
                "8000"
            ]
        );
        assert!(path_of(&launch).starts_with("/project/.venv/bin:"));
        assert!(path_of(&launch).ends_with(super::DEFAULT_PATH));
        assert!(
            launch
                .env
                .iter()
                .any(|entry| entry.key == "VIRTUAL_ENV" && entry.value == "/project/.venv")
        );
    }

    #[test]
    fn pnpm_exec_resolves_to_the_workspace_shim_in_a_subdirectory() {
        let project = TempDir::new().expect("project");
        let rootfs = TempDir::new().expect("rootfs");
        touch(&project, "front/node_modules/.bin/vite");
        let launch = ServiceLaunch::resolve(
            project.path(),
            "/project/front",
            rootfs.path(),
            "/image/bin",
            "@pnpm exec vite dev --host 0.0.0.0 --port 5173",
        );
        assert_eq!(
            launch.argv,
            [
                "/project/front/node_modules/.bin/vite",
                "dev",
                "--host",
                "0.0.0.0",
                "--port",
                "5173"
            ]
        );
        assert_eq!(
            path_of(&launch),
            "/project/front/node_modules/.bin:/image/bin"
        );
        assert!(
            !launch.env.iter().any(|entry| entry.key == "VIRTUAL_ENV"),
            "a node wrapper must not export a Python venv"
        );
    }

    #[test]
    fn a_missing_shim_keeps_the_original_wrapper() {
        let project = TempDir::new().expect("project");
        let rootfs = TempDir::new().expect("rootfs");
        let launch = ServiceLaunch::resolve(
            project.path(),
            "/project",
            rootfs.path(),
            super::DEFAULT_PATH,
            "@uv run python -m app.worker",
        );
        assert_eq!(
            launch.argv,
            ["/bin/sh", "-c", "uv run python -m app.worker"]
        );
        assert!(launch.env.is_empty());
    }

    #[test]
    fn a_flag_first_command_and_a_foreign_workdir_both_fall_back() {
        let project = TempDir::new().expect("project");
        let rootfs = TempDir::new().expect("rootfs");
        touch(&project, ".venv/bin/uvicorn");
        let flag_first = ServiceLaunch::resolve(
            project.path(),
            "/project",
            rootfs.path(),
            super::DEFAULT_PATH,
            "@uv run -m uvicorn",
        );
        assert_eq!(flag_first.argv, ["/bin/sh", "-c", "uv run -m uvicorn"]);
        let foreign = ServiceLaunch::resolve(
            project.path(),
            "/opt/app",
            rootfs.path(),
            super::DEFAULT_PATH,
            "@uv run uvicorn app.main:app",
        );
        assert_eq!(
            foreign.argv,
            ["/bin/sh", "-c", "uv run uvicorn app.main:app"]
        );
    }

    /// One sealed head in a real snapshot store: the head record plus the image node that wraps it.
    fn sealed_head(
        disk: &TempDir,
        seed: u8,
        user: Option<&str>,
        state: &str,
    ) -> (slipfork_world::StoreLayout, super::ImageHead) {
        use slipfork_world::{NodeKey, StoreLayout};
        let layout = StoreLayout::new(disk.path());
        let snapshots = layout.snapshots_root();
        fs::create_dir_all(&snapshots).expect("create the snapshots root");
        let head = NodeKey::from_digest([seed; 32]);
        fs::write(
            snapshots.join(format!("{}.json", head.as_hex())),
            r#"{"v":1,"kind":"step","parents":[],"canon":"WORKDIR /app","line":2,"created_ms":0,"last_used_ms":0,"facts":null}"#,
        )
        .expect("write the head record");
        if let Some(user) = user {
            fs::write(
                snapshots.join(format!("{}.json", "a".repeat(64))),
                format!(
                    r#"{{"v":1,"kind":"image","parents":["{}"],"canon":"FROM scratch","line":1,"created_ms":0,"last_used_ms":0,"facts":{{"user":{}}}}}"#,
                    head.as_hex(),
                    serde_json::to_string(user).expect("encode the USER fact"),
                ),
            )
            .expect("write the image record");
        }
        let rootfs = disk.path().join("rootfs");
        fs::create_dir_all(rootfs.join("etc")).expect("seed the image etc");
        fs::write(
            rootfs.join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/sh\napp:x:1000:1000:App:/home/app:/bin/sh\n",
        )
        .expect("write the image passwd");
        let head = super::ImageHead {
            key: head,
            rootfs,
            inherited_env: Vec::new(),
            argv: vec!["/bin/true".to_owned()],
            workdir: "/app".to_owned(),
            state: state.to_owned(),
        };
        (layout, head)
    }

    /// A sealed image's `USER` resolves through the same `Identity::resolve` the build uses.
    #[test]
    fn a_sealed_image_user_resolves_and_an_unresolvable_one_fails_closed() {
        use slipfork_world::BtrfsEngine;
        let disk = TempDir::new_in("/var/tmp").expect("real disk store");
        let engine = BtrfsEngine::new(slipfork_world::StoreLayout::new(disk.path()));

        let (layout, head) = sealed_head(&disk, 1, Some("1000:1000"), "built");
        assert_eq!(
            super::sealed_identity(&layout, &engine, &head).expect("a numeric USER resolves"),
            slipfork_world::Identity {
                uid: 1000,
                gid: 1000
            }
        );

        let (layout, head) = sealed_head(&disk, 2, Some("ghost"), "built");
        let error = super::sealed_identity(&layout, &engine, &head)
            .expect_err("an unresolvable USER must fail closed");
        assert!(
            error.cause().contains("`ghost`") && error.cause().contains("/etc/passwd"),
            "the cause names the account and its file: {}",
            error.cause()
        );
        assert!(
            error.remedy().contains("useradd"),
            "the remedy names the fix: {}",
            error.remedy()
        );
        assert!(!error.cause().contains("next:"), "no embedded remedy");
    }

    /// A Dockerfile-built head with no image record fails closed instead of booting as root.
    #[test]
    fn a_built_head_without_its_image_record_is_refused_but_a_pulled_image_is_root() {
        use slipfork_world::BtrfsEngine;
        let disk = TempDir::new_in("/var/tmp").expect("real disk store");
        let engine = BtrfsEngine::new(slipfork_world::StoreLayout::new(disk.path()));

        let (layout, head) = sealed_head(&disk, 3, None, "built");
        let error = super::sealed_identity(&layout, &engine, &head).expect_err("fail closed");
        assert!(
            error.cause().contains("no recorded `USER`"),
            "{}",
            error.cause()
        );
        assert!(error.remedy().contains("slipfork up"), "{}", error.remedy());

        let (layout, head) = sealed_head(&disk, 4, None, "pulled");
        assert_eq!(
            super::sealed_identity(&layout, &engine, &head).expect("a pulled image is 0:0"),
            slipfork_world::Identity::ROOT
        );
    }
}
