//! Pure path/env/resource helpers for the world boot runtime.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use slipfork_forkfile::{direct_bin, no_sync_uv, tool_prefix};
use slipfork_world::{Bind, BindSource, PodEnv};

const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

pub(super) fn project_bind(project: &Path) -> Bind {
    Bind {
        source: BindSource::Host(project.to_path_buf()),
        destination: "/project".to_owned(),
        read_only: false,
    }
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

/// The `@<tool>` name of a declaration (`None` for a plain shell command).
pub(super) fn tool(command: &str) -> Option<&str> {
    tool_prefix(command).map(|(tool, _)| tool)
}

/// Environment that suppresses a tool's implicit sync step inside a service pod.
///
/// The `@`-setup node still owns syncing; a service that merely runs through `@uv` must not
/// re-check the lockfile on every boot (`specs/03` §3).
pub(super) fn tool_env(command: &str) -> Vec<PodEnv> {
    match tool(command) {
        Some("uv") => vec![PodEnv::new("UV_NO_SYNC", "1")],
        _ => Vec::new(),
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
            match resolve_wrapper(project, container_workdir, command) {
                Some(prefix) => {
                    let env = prefix_env(&prefix, path);
                    (format!("{}{}", prefix.program, prefix.rest), true, env)
                }
                None => {
                    let text = match tool(command) {
                        Some("uv") => shell_command(&no_sync_uv(command)),
                        _ => shell_command(command),
                    };
                    (text, false, Vec::new())
                }
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
    command: &str,
) -> Option<ResolvedPrefix> {
    let host_dir = host_workdir(project, container_workdir)?;
    let direct = direct_bin(command)?;
    if !host_dir
        .join(direct.exec.bin_dir)
        .join(direct.bin)
        .is_file()
    {
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
        return rootfs.join(relative).is_file().then(|| {
            if program.starts_with('/') {
                program.to_owned()
            } else {
                format!("/{program}")
            }
        });
    }
    for dir in path.split(':').filter(|dir| !dir.is_empty()) {
        let relative = format!("{}/{program}", dir.trim_start_matches('/'));
        if rootfs.join(&relative).is_file() {
            return Some(format!("/{relative}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::ServiceLaunch;
    use slipfork_world::PodEnv;

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
            ["/bin/sh", "-c", "uv run --no-sync python -m app.worker"]
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
        assert_eq!(
            flag_first.argv,
            ["/bin/sh", "-c", "uv run --no-sync -m uvicorn"]
        );
        let foreign = ServiceLaunch::resolve(
            project.path(),
            "/opt/app",
            rootfs.path(),
            super::DEFAULT_PATH,
            "@uv run uvicorn app.main:app",
        );
        assert_eq!(
            foreign.argv,
            ["/bin/sh", "-c", "uv run --no-sync uvicorn app.main:app"]
        );
    }
}
