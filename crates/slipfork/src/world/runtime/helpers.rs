//! Pure path/env/resource helpers for the world boot runtime.

use std::collections::BTreeMap;
use std::path::Path;

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

/// Resolves a metacharacter-free service `run:` command directly to an in-rootfs executable
/// argv, avoiding an extra `/bin/sh -c` process while falling back to `/bin/sh -c` for shell syntax.
pub(super) fn service_argv(rootfs: &Path, inherited_env: &[String], command: &str) -> Vec<String> {
    let trimmed = shell_command(command);
    let has_shell_meta = trimmed.bytes().any(|byte| {
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
    });
    if !has_shell_meta {
        let parts: Vec<&str> = trimmed.split_ascii_whitespace().collect();
        if let Some((program, rest)) = parts.split_first()
            && let Some(resolved) = resolve_in_rootfs(rootfs, inherited_env, program)
        {
            let mut argv = Vec::with_capacity(parts.len());
            argv.push(resolved);
            argv.extend(rest.iter().map(|arg| (*arg).to_owned()));
            return argv;
        }
    }
    vec!["/bin/sh".to_owned(), "-c".to_owned(), trimmed]
}

fn resolve_in_rootfs(rootfs: &Path, inherited_env: &[String], program: &str) -> Option<String> {
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
    let path_var = inherited_env
        .iter()
        .find_map(|entry| entry.strip_prefix("PATH="))
        .unwrap_or(DEFAULT_PATH);
    for dir in path_var.split(':').filter(|dir| !dir.is_empty()) {
        let relative = format!("{}/{program}", dir.trim_start_matches('/'));
        if rootfs.join(&relative).is_file() {
            return Some(format!("/{relative}"));
        }
    }
    None
}
