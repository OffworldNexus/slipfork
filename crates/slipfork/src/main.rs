//! `slipfork`: Single Composition Root CLI.
//!
//! Wires OS environment, local `0600` vaults, filesystem Merkle hashers, and CoW `WorldEngine`s
//! into pure domain services (`slipfork-snow`, `slipfork-dag`, `slipfork-engine`).

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

use clap::{Parser, Subcommand};
use serde_json::json;
use slipfork_dag::{DagPlanner, FsDigestCache, FsMerkleHasher, SlipforkConfig};
use slipfork_engine::{
    ExecSpec, HostBindMount, ReflinkNetnsEngine, SnapshotId, WorldEngine, WorldHandle,
    current_uid_gid, probe_reflink_dir,
};
use slipfork_snow::{
    FileVaultProvider, OsCsprngEntropy, ResolvedEnvMap, ResourceCoordinates, SnowFile,
};
use slipfork_world::{ProjectSlug, WorldTarget};

#[derive(Parser)]
#[command(
    name = "slipfork",
    version,
    about = "Instant CoW dev worlds & parallel test DAG orchestrator"
)]
struct Cli {
    /// Emit machine-readable JSON output.
    #[arg(long, global = true)]
    json: bool,

    /// Override state directory (defaults to $XDG_CACHE_HOME/slipfork or ~/.cache/slipfork).
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Probe host CoW reflink latency, Incus socket availability, and unprivileged netns support.
    Doctor,
    /// Validate and resolve `snow.yml` modes & `!!` auto-generated secrets.
    Snow {
        #[command(subcommand)]
        action: SnowAction,
    },
    /// Compute the orthogonal Artifact + Fixture Snapshot DAG execution plan.
    Dag {
        #[command(subcommand)]
        action: DagAction,
    },
    /// Capture a named CoW snapshot from a source state directory.
    Snap {
        /// Source state directory to snapshot.
        #[arg(long)]
        source: PathBuf,
        /// Snapshot label.
        label: String,
    },
    /// Clone a CoW world in milliseconds and bind `--workspace` under host $UID:$GID.
    Fork {
        /// Name of the forked world.
        name: String,
        /// Source snapshot label.
        #[arg(long, default_value = "canonical")]
        from: String,
        /// Workspace path to bind under host $UID:$GID (defaults to current directory).
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Execute a command inside an isolated CoW world with `snow.yml` envs injected.
    Exec {
        /// Target world name.
        #[arg(long)]
        world: String,
        /// Working directory inside the mounted workspace.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Optional `snow.yml` path to inject resolved `local` mode environment variables.
        #[arg(long)]
        snow_file: Option<PathBuf>,
        /// Command and arguments to execute.
        #[arg(required = true, trailing_var_arg = true)]
        argv: Vec<String>,
    },
    /// Destroy a cloned CoW world and free its state.
    Drop {
        /// World name to destroy.
        name: String,
    },
}

#[derive(Subcommand)]
enum SnowAction {
    /// Validate `snow.yml`, persist missing `!!` secrets, and output resolved/redacted variables.
    Resolve {
        /// Path to `snow.yml`.
        #[arg(long, default_value = "snow.yml")]
        file: PathBuf,
        /// Active mode (`local`, `cloud`, etc.).
        #[arg(long, default_value = "local")]
        mode: String,
        /// Optional component ID filter.
        #[arg(long)]
        component: Option<String>,
        /// Expose plaintext secrets (only for internal child-process wiring; redacted by default).
        #[arg(long)]
        expose_secrets: bool,
    },
}

#[derive(Subcommand)]
enum DagAction {
    /// Hash repo inputs and print cached vs rebuild waves.
    Plan {
        /// Path to `slipfork.toml` (defaults to `<vcs_root>/slipfork.toml`).
        #[arg(long)]
        file: Option<PathBuf>,
        /// Repository root directory walked for input globs (defaults to `<vcs_root>`).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

fn default_state_root() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("slipfork");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(home).join(".cache").join("slipfork")
}

fn default_vault_path(project_slug: &ProjectSlug) -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg)
            .join("slipfork")
            .join("vaults")
            .join(format!("{project_slug}.json"));
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("slipfork")
        .join("vaults")
        .join(format!("{project_slug}.json"))
}

fn load_slipfork_config(path: &Path) -> Result<SlipforkConfig, String> {
    let raw =
        fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    SlipforkConfig::parse_toml(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

enum VcsResolution {
    /// Canonical checkout/workspace root: `<0>/slipfork.toml` owns config and `<0>`'s basename
    /// is a valid automatic `ProjectSlug` fallback when `project` is omitted.
    Canonical(PathBuf),
    /// Recognized workspace where `<config_root>/slipfork.toml` owns config, but automatic
    /// directory-name slug derivation must fail closed unless `project = "<slug>"` is set.
    RequireProjectOverride {
        config_root: PathBuf,
        override_required: String,
    },
}

impl VcsResolution {
    fn into_config_root(self) -> PathBuf {
        match self {
            Self::Canonical(root) => root,
            Self::RequireProjectOverride { config_root, .. } => config_root,
        }
    }
}

enum VcsProbe {
    Found(VcsResolution),
    NotARepo,
    GitBinaryMissing(io::Error),
    Failed(String),
}

fn vcs_config_root(start: &Path) -> Result<PathBuf, String> {
    resolve_vcs(start).map(VcsResolution::into_config_root)
}

fn probe_vcs_config_root(canonical_start: &Path) -> Result<Option<PathBuf>, String> {
    Ok(probe_vcs(canonical_start)?.map(VcsResolution::into_config_root))
}

fn resolve_dag_paths(
    file: Option<PathBuf>,
    repo: Option<PathBuf>,
) -> Result<(PathBuf, PathBuf), String> {
    match (file, repo) {
        (None, None) => {
            let vcs_root = vcs_config_root(Path::new("."))?;
            Ok((vcs_root.join("slipfork.toml"), vcs_root))
        }
        (None, Some(explicit_repo)) => {
            let canonical_repo = fs::canonicalize(&explicit_repo)
                .map_err(|e| format!("cannot access --repo {}: {e}", explicit_repo.display()))?;
            let vcs_root = vcs_config_root(&canonical_repo)?;
            if canonical_repo != vcs_root {
                let relation = if canonical_repo.starts_with(&vcs_root) {
                    "is narrower than"
                } else {
                    "diverges from"
                };
                return Err(format!(
                    "--repo {} {relation} VCS root {} (which owns {}); omit --repo (or pass --repo {}) so config and input globs share the VCS root",
                    explicit_repo.display(),
                    vcs_root.display(),
                    vcs_root.join("slipfork.toml").display(),
                    vcs_root.display(),
                ));
            }
            Ok((vcs_root.join("slipfork.toml"), vcs_root))
        }
        (Some(explicit_file), None) => {
            if let Err(e) = fs::metadata(&explicit_file) {
                return Err(format!("cannot read {}: {e}", explicit_file.display()));
            }
            let vcs_root = vcs_config_root(&parent_dir(&explicit_file))?;
            Ok((explicit_file, vcs_root))
        }
        (Some(explicit_file), Some(explicit_repo)) => {
            if let Err(e) = fs::metadata(&explicit_file) {
                return Err(format!("cannot read {}: {e}", explicit_file.display()));
            }
            let canonical_file_dir = fs::canonicalize(parent_dir(&explicit_file))
                .map_err(|e| format!("cannot access {}: {e}", explicit_file.display()))?;
            let canonical_repo = fs::canonicalize(&explicit_repo)
                .map_err(|e| format!("cannot access --repo {}: {e}", explicit_repo.display()))?;

            match (
                probe_vcs_config_root(&canonical_file_dir)?,
                probe_vcs_config_root(&canonical_repo)?,
            ) {
                (Some(file_vcs_root), _) => {
                    if canonical_repo != file_vcs_root {
                        return Err(format!(
                            "--repo {} is not the VCS root ({}) of --file {} (rule: --repo must match the canonical VCS root of --file so config and input globs share one root); pass --repo {} or omit --repo",
                            explicit_repo.display(),
                            file_vcs_root.display(),
                            explicit_file.display(),
                            file_vcs_root.display(),
                        ));
                    }
                    Ok((explicit_file, canonical_repo))
                }
                (None, Some(repo_vcs_root)) => Err(format!(
                    "--file {} is outside any git or jj repository while --repo {} belongs to VCS root {} (rule: --repo must match the VCS root of --file so config and input globs share one root)",
                    explicit_file.display(),
                    explicit_repo.display(),
                    repo_vcs_root.display(),
                )),
                (None, None) => {
                    if canonical_repo != canonical_file_dir {
                        return Err(format!(
                            "--repo {} does not match the directory ({}) of --file {} outside any git or jj repository (rule: outside a VCS repository, --repo must match the parent directory of --file so config and input globs share one root)",
                            explicit_repo.display(),
                            canonical_file_dir.display(),
                            explicit_file.display(),
                        ));
                    }
                    Ok((explicit_file, canonical_repo))
                }
            }
        }
    }
}

fn project_slug_for(dir: &Path) -> Result<ProjectSlug, String> {
    match resolve_vcs(dir)? {
        VcsResolution::Canonical(root) => {
            if let Some(slug) = slipfork_toml_project(&root)? {
                return Ok(slug);
            }
            let name = root.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
                format!(
                    "cannot derive a project slug from repository root {}; set `project = \"<slug>\"` in {}",
                    root.display(),
                    root.join("slipfork.toml").display()
                )
            })?;
            ProjectSlug::parse(name).map_err(|err| {
                format!(
                    "{err}; rename the repository directory or set `project = \"<slug>\"` in {}",
                    root.join("slipfork.toml").display()
                )
            })
        }
        VcsResolution::RequireProjectOverride {
            config_root,
            override_required,
        } => match slipfork_toml_project(&config_root)? {
            Some(slug) => Ok(slug),
            None => Err(override_required),
        },
    }
}

fn slipfork_toml_project(root: &Path) -> Result<Option<ProjectSlug>, String> {
    let path = root.join("slipfork.toml");
    match fs::metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        Ok(_) => Ok(load_slipfork_config(&path)?.project),
    }
}

fn has_git_marker_ancestor(start: &Path) -> bool {
    start
        .ancestors()
        .any(|dir| fs::symlink_metadata(dir.join(".git")).is_ok())
}

fn probe_vcs(canonical_start: &Path) -> Result<Option<VcsResolution>, String> {
    let git_missing = match git_root(canonical_start) {
        VcsProbe::Found(res) => return Ok(Some(res)),
        VcsProbe::Failed(err) => return Err(err),
        VcsProbe::NotARepo => None,
        VcsProbe::GitBinaryMissing(err) => Some(err),
    };

    if let Some(err) = git_missing
        && (has_git_marker_ancestor(canonical_start) || find_jj_ancestor(canonical_start).is_none())
    {
        return Err(format!(
            "cannot resolve repository root in {}: 'git' executable was not found on PATH ({err}); install 'git' and ensure it is on PATH",
            canonical_start.display()
        ));
    }

    match jj_root(canonical_start) {
        VcsProbe::Found(res) => Ok(Some(res)),
        VcsProbe::Failed(err) => Err(err),
        VcsProbe::NotARepo | VcsProbe::GitBinaryMissing(_) => Ok(None),
    }
}

fn resolve_vcs(start: &Path) -> Result<VcsResolution, String> {
    let canonical_start =
        fs::canonicalize(start).map_err(|e| format!("cannot access {}: {e}", start.display()))?;
    probe_vcs(&canonical_start)?.ok_or_else(|| {
        format!(
            "cannot derive a project slug: {} is not inside a git or jj repository; initialize a repository ('git init' or 'jj git init') at your project root",
            canonical_start.display()
        )
    })
}

fn parse_trimmed_stdout(stdout: Vec<u8>, cmd_label: &str) -> Result<String, String> {
    let raw = String::from_utf8(stdout)
        .map_err(|e| format!("{cmd_label} returned non-UTF-8 output: {e}"))?;
    Ok(raw.trim().to_owned())
}

fn canonicalize_command_path(stdout: Vec<u8>, cmd_label: &str) -> Result<PathBuf, String> {
    let trimmed = parse_trimmed_stdout(stdout, cmd_label)?;
    if trimmed.is_empty() {
        return Err(format!("{cmd_label} returned an empty path"));
    }
    let path = fs::canonicalize(&trimmed)
        .map_err(|e| format!("cannot canonicalize {cmd_label} path '{trimmed}': {e}"))?;
    if !path.is_dir() {
        return Err(format!(
            "{cmd_label} path '{}' is not a directory",
            path.display()
        ));
    }
    Ok(path)
}

fn run_git_rev_parse(dir: &Path, args: &[&str]) -> io::Result<std::process::Output> {
    Command::new("git")
        .env("LC_ALL", "C")
        .env("LANGUAGE", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .arg("-C")
        .arg(dir)
        .arg("rev-parse")
        .args(args)
        .output()
}

fn run_git_rev_parse_raw(dir: &Path, args: &[&str], label: &str) -> Result<Vec<u8>, String> {
    let out = run_git_rev_parse(dir, args)
        .map_err(|e| format!("failed to execute '{label}' in {}: {e}", dir.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(format!("{label} failed in {}: {stderr}", dir.display()));
    }
    Ok(out.stdout)
}

fn git_rev_parse_canonical_path(dir: &Path, args: &[&str], label: &str) -> Result<PathBuf, String> {
    canonicalize_command_path(run_git_rev_parse_raw(dir, args, label)?, label)
}

/// Canonical Git repository root derived strictly from `git rev-parse --show-toplevel`,
/// failing closed on bare repositories, submodules, and `--separate-git-dir` indirection.
fn git_root(canonical_start: &Path) -> VcsProbe {
    let bare_out = match run_git_rev_parse(canonical_start, &["--is-bare-repository"]) {
        Ok(out) => out,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return VcsProbe::GitBinaryMissing(e),
        Err(e) => {
            return VcsProbe::Failed(format!(
                "failed to execute 'git rev-parse --is-bare-repository' in {}: {e}",
                canonical_start.display()
            ));
        }
    };
    if !bare_out.status.success() {
        let stderr = String::from_utf8_lossy(&bare_out.stderr).trim().to_owned();
        if stderr
            .to_ascii_lowercase()
            .starts_with("fatal: not a git repository (or any ")
        {
            return VcsProbe::NotARepo;
        }
        return VcsProbe::Failed(format!(
            "git rev-parse failed in {}: {stderr}",
            canonical_start.display()
        ));
    }
    match parse_trimmed_stdout(bare_out.stdout, "git rev-parse --is-bare-repository") {
        Ok(val) if val == "true" => {
            let bare_dir = git_rev_parse_canonical_path(
                canonical_start,
                &["--path-format=absolute", "--git-dir"],
                "git rev-parse --git-dir",
            )
            .unwrap_or_else(|_| canonical_start.to_path_buf());
            return VcsProbe::Failed(format!(
                "cannot derive a project slug inside bare git repository {}: bare repositories have no working tree; clone into a working tree (`git clone {} <dir>`) and, if needed, set `project = \"<slug>\"` in `<dir>/slipfork.toml`",
                bare_dir.display(),
                bare_dir.display(),
            ));
        }
        Ok(_) => {}
        Err(err) => return VcsProbe::Failed(err),
    }

    let toplevel = match git_rev_parse_canonical_path(
        canonical_start,
        &["--show-toplevel"],
        "git rev-parse --show-toplevel",
    ) {
        Ok(path) => path,
        Err(err) => return VcsProbe::Failed(err),
    };

    let superproject = match run_git_rev_parse_raw(
        &toplevel,
        &["--show-superproject-working-tree"],
        "git rev-parse --show-superproject-working-tree",
    )
    .and_then(|stdout| {
        parse_trimmed_stdout(stdout, "git rev-parse --show-superproject-working-tree")
    }) {
        Ok(text) => text,
        Err(err) => return VcsProbe::Failed(err),
    };
    if !superproject.is_empty() {
        return VcsProbe::Found(VcsResolution::RequireProjectOverride {
            override_required: format!(
                "cannot derive a project slug automatically inside git submodule {} (superproject {superproject}): set `project = \"<slug>\"` in {}",
                toplevel.display(),
                toplevel.join("slipfork.toml").display()
            ),
            config_root: toplevel,
        });
    }

    let git_dir = match git_rev_parse_canonical_path(
        &toplevel,
        &["--path-format=absolute", "--git-dir"],
        "git rev-parse --git-dir",
    ) {
        Ok(path) => path,
        Err(err) => return VcsProbe::Failed(err),
    };

    let dot_git = toplevel.join(".git");
    if fs::symlink_metadata(&dot_git).is_ok_and(|m| m.is_dir())
        && fs::canonicalize(&dot_git).ok().as_ref() == Some(&git_dir)
    {
        return VcsProbe::Found(VcsResolution::Canonical(toplevel));
    }

    // Linked-worktree canonicalization: `<toplevel>/.git` is a regular file pointing into the main
    // checkout's `.git/worktrees/<id>` (`git_dir != common` and `common` is `<main_root>/.git`).
    if fs::symlink_metadata(&dot_git).is_ok_and(|m| m.is_file()) {
        let common = match git_rev_parse_canonical_path(
            &toplevel,
            &["--path-format=absolute", "--git-common-dir"],
            "git rev-parse --git-common-dir",
        ) {
            Ok(path) => path,
            Err(err) => return VcsProbe::Failed(err),
        };
        if git_dir != common
            && common.file_name() == Some(OsStr::new(".git"))
            && let Some(main_root) = common.parent()
            && git_rev_parse_canonical_path(
                main_root,
                &["--show-toplevel"],
                "git rev-parse --show-toplevel",
            )
            .ok()
            .as_deref()
                == Some(main_root)
        {
            return VcsProbe::Found(VcsResolution::Canonical(main_root.to_path_buf()));
        }
        return VcsProbe::Found(VcsResolution::RequireProjectOverride {
            override_required: format!(
                "cannot derive a project slug automatically for git checkout {} using a separate git directory ({}): set `project = \"<slug>\"` in {}",
                toplevel.display(),
                common.display(),
                toplevel.join("slipfork.toml").display()
            ),
            config_root: toplevel,
        });
    }

    VcsProbe::Found(VcsResolution::RequireProjectOverride {
        override_required: format!(
            "cannot derive a project slug automatically for git checkout {} using a separate git directory or gitdir indirection: set `project = \"<slug>\"` in {}",
            toplevel.display(),
            toplevel.join("slipfork.toml").display()
        ),
        config_root: toplevel,
    })
}

fn find_jj_ancestor(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(".jj").is_dir())
        .map(Path::to_path_buf)
}

fn jj_override_message(jj_dir: &Path, cause: &str) -> String {
    format!(
        "cannot derive a project slug for Jujutsu workspace {}: {cause} ('jj' is required for automatic jj-workspace detection, and because 'jj root' is workspace-scoped, '.worktrees/<fork>' workspaces also require an explicit override); set `project = \"<slug>\"` in {}",
        jj_dir.display(),
        jj_dir.join("slipfork.toml").display()
    )
}

/// Jujutsu workspace root via the real `jj root` CLI when `.jj/` exists in an ancestor directory
/// (zero `.jj` internal pointer layout parsing).
fn jj_root(canonical_start: &Path) -> VcsProbe {
    let Some(jj_dir) = find_jj_ancestor(canonical_start) else {
        return VcsProbe::NotARepo;
    };

    let output = match Command::new("jj")
        .env("LC_ALL", "C")
        .env("LANGUAGE", "C")
        .arg("root")
        .current_dir(&jj_dir)
        .output()
    {
        Ok(out) => out,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return VcsProbe::Found(VcsResolution::RequireProjectOverride {
                override_required: jj_override_message(
                    &jj_dir,
                    "'jj' executable was not found on PATH",
                ),
                config_root: jj_dir,
            });
        }
        Err(e) => {
            return VcsProbe::Found(VcsResolution::RequireProjectOverride {
                override_required: jj_override_message(
                    &jj_dir,
                    &format!("failed to execute 'jj root': {e}"),
                ),
                config_root: jj_dir,
            });
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return VcsProbe::Found(VcsResolution::RequireProjectOverride {
            override_required: jj_override_message(
                &jj_dir,
                &format!("'jj root' exited with error: {stderr}"),
            ),
            config_root: jj_dir,
        });
    }
    match canonicalize_command_path(output.stdout, "jj root") {
        Ok(root) => VcsProbe::Found(VcsResolution::Canonical(root)),
        Err(err) => VcsProbe::Found(VcsResolution::RequireProjectOverride {
            override_required: jj_override_message(&jj_dir, &err),
            config_root: jj_dir,
        }),
    }
}

fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn resolve_snow_file(
    file: &Path,
    mode: &str,
    component: Option<&str>,
) -> Result<ResolvedEnvMap, String> {
    let raw =
        fs::read_to_string(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    let snow = SnowFile::parse_yaml(&raw).map_err(|e| e.to_string())?;
    let slug = project_slug_for(&parent_dir(file))?;
    let mut vault =
        FileVaultProvider::open(default_vault_path(&slug)).map_err(|e| e.to_string())?;
    let coords = ResourceCoordinates::local_for_world(&snow, &slug, &WorldTarget::Canonical)
        .map_err(|e| e.to_string())?;
    let mut entropy = OsCsprngEntropy;
    snow.resolve_mode(mode, component, &mut vault, &coords, &mut entropy)
        .map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let state_root = cli.state_dir.unwrap_or_else(default_state_root);
    match run_cli(cli.json, &state_root, cli.command) {
        Ok(code) => code,
        Err(msg) => {
            if cli.json {
                println!("{}", json!({ "ok": false, "error": msg }));
            } else {
                eprintln!("slipfork: {msg}");
            }
            ExitCode::from(1)
        }
    }
}

fn run_cli(json_out: bool, state_root: &Path, command: Commands) -> Result<ExitCode, String> {
    let (uid, gid) = current_uid_gid();
    let engine = ReflinkNetnsEngine::new(state_root, uid, gid);

    match command {
        Commands::Doctor => {
            let incus_sock = Path::new("/var/lib/incus/unix.socket").exists();
            let reflink_us = probe_reflink_dir(state_root).map_err(|e| e.to_string())?;
            let netns_ok = Command::new("unshare")
                .args([
                    &format!("--map-user={uid}"),
                    &format!("--map-group={gid}"),
                    "--keep-caps",
                    "--net",
                    "--pid",
                    "--fork",
                    "sh",
                    "-c",
                    "ip link set lo up",
                ])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);

            if json_out {
                println!(
                    "{}",
                    json!({
                        "ok": netns_ok,
                        "uid": uid,
                        "gid": gid,
                        "state_root": state_root,
                        "incus_daemon_socket": incus_sock,
                        "reflink_64k_micros": reflink_us,
                        "isolated_netns": netns_ok,
                    })
                );
            } else {
                println!("state_root: {}", state_root.display());
                println!(
                    "incus:      {}",
                    if incus_sock {
                        "available"
                    } else {
                        "absent (using reflink-netns)"
                    }
                );
                println!("reflink:    ok (64 KiB FICLONE in {reflink_us} us)");
                println!(
                    "netns:      {} (uid={uid}:{gid})",
                    if netns_ok { "ok" } else { "FAILED" }
                );
            }
            Ok(if netns_ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }

        Commands::Snow {
            action:
                SnowAction::Resolve {
                    file,
                    mode,
                    component,
                    expose_secrets,
                },
        } => {
            let resolved = resolve_snow_file(&file, &mode, component.as_deref())?;

            let out_map: BTreeMap<String, String> = resolved
                .iter()
                .map(|(k, v)| {
                    let text = if expose_secrets {
                        v.expose()
                    } else {
                        v.redacted()
                    };
                    (k.clone(), text.to_owned())
                })
                .collect();

            if json_out {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&out_map).unwrap_or_default()
                );
            } else {
                for (k, v) in &out_map {
                    println!("{k}={v}");
                }
            }
            Ok(ExitCode::SUCCESS)
        }

        Commands::Dag {
            action: DagAction::Plan { file, repo },
        } => {
            let (config_path, repo_root) = resolve_dag_paths(file, repo)?;
            let cfg = load_slipfork_config(&config_path)?;
            let hasher = FsMerkleHasher::new(&repo_root);
            let cache = FsDigestCache::new(state_root);
            let plan = DagPlanner::plan(&cfg, &BTreeMap::new(), &BTreeMap::new(), &hasher, &cache)
                .map_err(|e| e.to_string())?;
            if json_out {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&plan).unwrap_or_default()
                );
            } else {
                for a in &plan.artifacts {
                    println!("artifact {:16} {:?} ({})", a.id, a.action, &a.digest[..12]);
                }
                for (w_idx, wave) in plan.fixture_waves.iter().enumerate() {
                    for f in wave {
                        println!(
                            "wave {w_idx} fixture {:12} {:?} ({})",
                            f.id,
                            f.action,
                            &f.digest[..12]
                        );
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }

        Commands::Snap { source, label } => {
            let t0 = Instant::now();
            let snap = engine
                .create_snapshot(&source, &label, false)
                .map_err(|e| e.to_string())?;
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            if json_out {
                println!(
                    "{}",
                    json!({ "ok": true, "snapshot": snap.0, "elapsed_ms": ms })
                );
            } else {
                println!("snapshot '{}' created in {ms:.2} ms", snap.0);
            }
            Ok(ExitCode::SUCCESS)
        }

        Commands::Fork {
            name,
            from,
            workspace,
        } => {
            let ws = workspace
                .or_else(|| std::env::current_dir().ok())
                .ok_or_else(|| "cannot resolve workspace directory".to_owned())?;
            let t0 = Instant::now();
            let handle = engine
                .clone_world(&SnapshotId(from), &name, &HostBindMount { host_path: ws })
                .map_err(|e| e.to_string())?;
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            if json_out {
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "world": handle.id,
                        "state_dir": handle.state_dir,
                        "workspace_dir": handle.workspace_dir,
                        "elapsed_ms": ms,
                    })
                );
            } else {
                println!(
                    "forked world '{}' -> {} in {ms:.2} ms",
                    handle.id,
                    handle.state_dir.display()
                );
            }
            Ok(ExitCode::SUCCESS)
        }

        Commands::Exec {
            world,
            cwd,
            snow_file,
            argv,
        } => {
            let cwd = cwd
                .or_else(|| std::env::current_dir().ok())
                .ok_or_else(|| "cannot resolve cwd".to_owned())?;
            let mut env = BTreeMap::new();
            if let Some(sf) = snow_file {
                let resolved = resolve_snow_file(&sf, "local", None)?;
                for (k, v) in resolved {
                    env.insert(k, v.expose().to_owned());
                }
            }
            let state_dir = state_root.join("worlds").join(&world).join("state");
            if !state_dir.exists() {
                return Err(format!(
                    "world '{world}' does not exist under {}",
                    state_root.display()
                ));
            }
            let handle = WorldHandle {
                id: world,
                state_dir,
                workspace_dir: cwd.clone(),
            };
            let out = engine
                .exec_in_world(&handle, &ExecSpec { argv, cwd, env })
                .map_err(|e| e.to_string())?;
            if !out.stdout.is_empty() {
                print!("{}", out.stdout);
            }
            if !out.stderr.is_empty() {
                eprint!("{}", out.stderr);
            }
            Ok(ExitCode::from(out.exit_code.clamp(0, 255) as u8))
        }

        Commands::Drop { name } => {
            let handle = WorldHandle {
                id: name.clone(),
                state_dir: state_root.join("worlds").join(&name).join("state"),
                workspace_dir: PathBuf::new(),
            };
            engine.destroy_world(handle).map_err(|e| e.to_string())?;
            if json_out {
                println!("{}", json!({ "ok": true, "dropped": name }));
            } else {
                println!("dropped world '{name}'");
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}
