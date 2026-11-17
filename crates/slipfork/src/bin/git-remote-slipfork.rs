//! `git-remote-slipfork`: Git's remote-helper entry point for fork worlds (`ADR-0005` §3).
//!
//! Git runs `git-remote-slipfork <remote> <url>` in the repository and speaks the documented
//! `gitremote-helpers` line protocol over stdio. The URL is `slipfork://<fork>`; the committed
//! branch and the synthesized dirty view belong to the **daemon-owned** fork workspace, so this
//! binary resolves the repository and refs through the typed control port
//! (`DaemonControl::workspace_refs`, whose daemon side uses the injected `RepoRootResolver` and
//! `VcsWorkspace::resolve`) and delegates every protocol frame to
//! [`slipfork_vcs::remote_helper`].
//!
//! It never shells out to Git, never invents a ref, and fails closed when the fork world is absent
//! from `slipforkd`. No vault or world engine is linked here.

#![forbid(unsafe_code)]

use std::io::BufReader;
use std::path::{Path, PathBuf};

use slipfork::layout::Layout;
use slipfork_edge::daemon::WorkspaceRefsRequest;
use slipfork_edge::{DaemonControl, DaemonControlClient};
use slipfork_vcs::{VcsError, VcsWorkspace, WorkspaceChanges, WorkspaceRefs, remote_helper};
use slipfork_world::validate_fork_name;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(_remote), Some(url)) = (args.next(), args.next()) else {
        eprintln!(
            "git-remote-slipfork: expected <remote> <url> (configure a `slipfork://<fork>` remote)"
        );
        std::process::exit(1);
    };
    if let Err(message) = run(&url) {
        eprintln!("git-remote-slipfork: {message}");
        std::process::exit(1);
    }
}

fn run(url: &str) -> Result<(), String> {
    let fork = url.strip_prefix("slipfork://").ok_or_else(|| {
        format!("unsupported URL '{url}': expected `slipfork://<fork>` (for example `git remote add slipfork slipfork://feat-billing`)")
    })?;
    let fork = fork.trim_end_matches('/');
    validate_fork_name(fork, &[]).map_err(|err| err.to_string())?;

    let cwd = std::env::current_dir().map_err(|err| format!("cannot read cwd: {err}"))?;
    let layout = layout_from_env()?;
    let client = DaemonControlClient::bind(layout.runtime_dir());
    if !client.is_served() {
        return Err(format!(
            "no slipforkd answers {}; run `slipfork up --world {fork}` (or start slipforkd --state-dir <dir> --runtime-dir {}) first",
            client.socket_path().display(),
            layout.runtime_dir().display()
        ));
    }
    let refs = client
        .workspace_refs(&WorkspaceRefsRequest {
            cwd,
            fork: fork.to_owned(),
        })
        .map_err(|err| err.to_string())?;
    let workspace = RefWorkspace {
        fork: fork.to_owned(),
        path: refs.worktree,
        refs: WorkspaceRefs {
            base: refs.base,
            wip: refs.wip,
        },
        base_oid: refs.base_oid,
        wip_oid: refs.wip_oid,
    };

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    remote_helper::serve(&workspace, BufReader::new(stdin.lock()), stdout.lock())
        .map_err(|err| err.to_string())
}

/// Resolve the same explicit roots the thin CLI uses, without inventing a daemon address.
///
/// The control socket is always `<state_dir>/run/daemon.sock` (`ADR-0011` §2); `XDG_RUNTIME_DIR`
/// is deliberately not consulted, so a system `slipforkd.service` and a login shell agree.
fn layout_from_env() -> Result<Layout, String> {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            "HOME is not set; git-remote-slipfork needs the developer account's home directory to locate slipforkd".to_owned()
        })?;
    let xdg_cache = absolute_env("XDG_CACHE_HOME");
    let xdg_data = absolute_env("XDG_DATA_HOME");
    Ok(Layout::resolve(
        None,
        &home,
        xdg_cache.as_deref(),
        xdg_data.as_deref(),
    ))
}

fn absolute_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// Read-only view of the daemon-resolved refs.
///
/// `changes` and `forget` are impossible through a remote helper (fetch never mutates the
/// workspace), so they fail closed instead of pretending to succeed.
struct RefWorkspace {
    path: PathBuf,
    fork: String,
    refs: WorkspaceRefs,
    base_oid: String,
    wip_oid: String,
}

impl VcsWorkspace for RefWorkspace {
    fn fork_name(&self) -> &str {
        &self.fork
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn refs(&self) -> WorkspaceRefs {
        self.refs.clone()
    }

    fn resolve(&self, reference: &str) -> Result<String, VcsError> {
        if reference == self.refs.base {
            return Ok(self.base_oid.clone());
        }
        if reference == self.refs.wip {
            return Ok(self.wip_oid.clone());
        }
        Err(VcsError::UnexpectedOutput {
            command: "git-remote-slipfork".to_owned(),
            message: format!(
                "unadvertised ref '{reference}' (advertised: {}, {})",
                self.refs.base, self.refs.wip
            ),
        })
    }

    fn changes(&self) -> Result<WorkspaceChanges, VcsError> {
        Err(VcsError::UnexpectedOutput {
            command: "git-remote-slipfork".to_owned(),
            message:
                "the remote helper is a read-only view; use `slipfork exec` to inspect the world"
                    .to_owned(),
        })
    }

    fn forget(&self) -> Result<(), VcsError> {
        Err(VcsError::UnexpectedOutput {
            command: "git-remote-slipfork".to_owned(),
            message: "a fetch never tears down a fork workspace; use `slipfork drop <fork>`"
                .to_owned(),
        })
    }
}
