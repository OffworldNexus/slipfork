//! `slipfork`: Single Composition Root CLI.
//!
//! Wires OS environment, local `0600` vaults, filesystem Merkle hashers, and CoW `WorldEngine`s
//! into pure domain services (`slipfork-snow`, `slipfork-dag`, `slipfork-engine`).

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

use clap::{Parser, Subcommand};
use serde_json::json;
use slipfork_dag::{DagConfig, DagPlanner, FsDigestCache, FsMerkleHasher};
use slipfork_engine::{
    ExecSpec, HostBindMount, ReflinkNetnsEngine, SnapshotId, WorldEngine, WorldHandle,
    current_uid_gid, probe_reflink_dir,
};
use slipfork_snow::{FileVaultProvider, OsCsprngEntropy, ResourceCoordinates, SnowFile};

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
    /// Clone a CoW world in milliseconds (and optionally create `.worktrees/<name>`).
    Fork {
        /// Name of the forked world / worktree.
        name: String,
        /// Source snapshot label.
        #[arg(long, default_value = "canonical")]
        from: String,
        /// Workspace path to bind under host $UID:$GID.
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
        /// Path to `slipfork.toml`.
        #[arg(long, default_value = "slipfork.toml")]
        file: PathBuf,
        /// Repository root directory.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
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

fn default_vault_path(project_slug: &str) -> PathBuf {
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

fn project_slug_from(path: &Path) -> String {
    path.canonicalize()
        .ok()
        .and_then(|p| {
            p.parent()
                .and_then(|d| d.file_name())
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "default".to_owned())
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
            let raw = fs::read_to_string(&file)
                .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
            let snow = SnowFile::parse_yaml(&raw).map_err(|e| e.to_string())?;
            let slug = project_slug_from(&file);
            let mut vault =
                FileVaultProvider::open(default_vault_path(&slug)).map_err(|e| e.to_string())?;
            let coords = ResourceCoordinates::local_defaults(&snow, &slug);
            let mut entropy = OsCsprngEntropy;
            let resolved = snow
                .resolve_mode(
                    &mode,
                    component.as_deref(),
                    &mut vault,
                    &coords,
                    &mut entropy,
                )
                .map_err(|e| e.to_string())?;

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
            let raw = fs::read_to_string(&file)
                .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
            let cfg = DagConfig::parse_toml(&raw).map_err(|e| e.to_string())?;
            let hasher = FsMerkleHasher::new(&repo);
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
            if let Some(sf) = snow_file
                && sf.exists()
            {
                let raw = fs::read_to_string(&sf).map_err(|e| e.to_string())?;
                let snow = SnowFile::parse_yaml(&raw).map_err(|e| e.to_string())?;
                let slug = project_slug_from(&sf);
                let mut vault = FileVaultProvider::open(default_vault_path(&slug))
                    .map_err(|e| e.to_string())?;
                let coords = ResourceCoordinates::local_defaults(&snow, &slug);
                let mut entropy = OsCsprngEntropy;
                let resolved = snow
                    .resolve_mode("local", None, &mut vault, &coords, &mut entropy)
                    .map_err(|e| e.to_string())?;
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
