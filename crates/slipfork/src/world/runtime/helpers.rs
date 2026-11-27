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
/// `USER`. A pulled or cached-from image seals no `user` fact, which is Docker's `0:0` default.
pub(super) fn sealed_identity(
    layout: &StoreLayout,
    engine: &dyn WorldEngine,
    built: &ImageHead,
) -> Result<Identity, IdentityFailure> {
    // A pulled image seals its OCI rootfs under `images/oci/<digest>` and records no OCI config
    // `USER`, so Docker's `0:0` default applies. Node records must never be consulted here: a
    // from-only image shares its base key with any same-digest Dockerfile build, whose `USER`
    // would otherwise alias onto the pull.
    if built.rootfs.starts_with(layout.images_root()) {
        return Ok(Identity::ROOT);
    }
    let store = SnapshotStore::new(layout, engine);
    let user = sealed_user(&store, built)?;
    // The one resolver: build `RUN` steps resolve the same `ImageConfig.user` for the same rootfs
    // through this identical `Identity::resolve` call (`slipfork-image/src/runtime.rs`).
    Identity::resolve(&built.rootfs, user.as_deref()).map_err(IdentityFailure::from)
}

/// The `USER` fact of the image node that wraps the sealed head, if the image names one.
///
/// Provenance decides the fallback: a head with no node record at all was pulled (`pulled` on its
/// first `up`, `cached` thereafter) and keeps Docker's `0:0` default. Any head that *does* have a
/// record is a Dockerfile build (`built`, or `cached` on a 0-miss rebuild) and must still find its
/// image record, even if GC swept it, so an explicit `USER` never silently boots root.
fn sealed_user(
    store: &SnapshotStore<'_>,
    built: &ImageHead,
) -> Result<Option<String>, IdentityFailure> {
    let head_record = read_record(&store.record_path(&built.key))?;
    if let Some(record) = head_record.as_ref()
        && record.kind == "image"
        && let Some(facts) = record.facts.as_ref()
    {
        return Ok(field(facts));
    }
    // The image node is the head's single child: its key hashes the head's key as parent. A pure
    // pull writes no node record, so the snapshots root may not exist at all: that is no records,
    // not a failure.
    let parent = built.key.as_hex();
    let snapshots = store.snapshots_root();
    if snapshots.is_dir() {
        let entries = fs::read_dir(snapshots).map_err(|error| {
            IdentityFailure::new(
                format!(
                    "cannot list the sealed snapshots in `{}`: {error}",
                    snapshots.display()
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
    }
    // A pulled head writes no node record at all; its absent `USER` is Docker's root default. A
    // recorded head that lost its image record is a Dockerfile build that must fail closed.
    if built.state == "pulled" || head_record.is_none() {
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
mod tests;
