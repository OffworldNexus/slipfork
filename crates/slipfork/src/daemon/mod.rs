//! The single per-user daemon: `<runtime_dir>/daemon.sock`, NDJSON control, lifecycles, ingress.
//!
//! One `slipfork` binary is both the CLI and the daemon (`slipfork __daemon`). The daemon owns every
//! world under the invoking UID, relays boot progress as `event` frames, and reaches world pods only
//! through each world's `<pods>/control.sock` `Dial` request. No second socket exists.

mod boot;
mod client;
mod converge;
mod frames;
mod handoff;
mod host;
mod identity;
pub(crate) mod image;
mod ingress;
mod lease;
mod lifecycle;
mod lock;
mod reconcile;
mod rpc;
mod serving;
mod spawn;
mod view;
mod wire;

pub use client::{DaemonClient, DaemonError};
pub(crate) use serving::Daemon;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use slipfork_vcs::VcsWorkspace;
use slipfork_world::LedgerError;

use crate::world::HostEnv;

/// Wall clock for ledger timestamps; the daemon is a composition root.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// A typed control-plane failure rendered as an `error` frame with a bare message.
#[derive(Debug, Clone)]
pub(crate) struct Failure {
    code: &'static str,
    message: String,
}

impl Failure {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// One daemon startup failure; the CLI prints exactly one `next:` line.
struct Startup {
    message: String,
    next: String,
}

impl Startup {
    fn new(message: impl Into<String>, next: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            next: next.into(),
        }
    }
}

/// Runs the socket-activated `:80` handoff in this process; called from the composition root.
pub use handoff::{Mode as IngressMode, run as run_handoff};

/// `<runtime_dir>/daemon.sock`.
#[must_use]
pub fn socket_path(host: &HostEnv) -> PathBuf {
    host.runtime_dir.join("daemon.sock")
}

/// Runs the daemon in this process; returns only when startup fails.
pub fn run(host: HostEnv, vcs: Arc<dyn VcsWorkspace>) -> ExitCode {
    match Daemon::start(host, vcs) {
        Ok(Some(daemon)) => match daemon.serve() {
            Ok(()) => ExitCode::SUCCESS,
            Err(startup) => {
                report(&startup);
                ExitCode::FAILURE
            }
        },
        Ok(None) => ExitCode::SUCCESS,
        Err(startup) => {
            report(&startup);
            ExitCode::FAILURE
        }
    }
}

fn report(startup: &Startup) {
    eprintln!("error: {}", startup.message);
    eprintln!("next: {}", startup.next);
}

/// Maps a ledger open refusal to the daemon's one startup line and one `next:` remedy.
fn startup_ledger(error: LedgerError) -> Startup {
    match error {
        LedgerError::SchemaNewer { version } => Startup::new(
            format!("state.db was written by a newer slipfork (migration {version})"),
            "upgrade slipfork",
        ),
        LedgerError::WriterBusy => Startup::new(
            "another slipfork daemon holds the ledger writer lock",
            "stop the other slipfork daemon",
        ),
        other => Startup::new(
            format!(
                "open the world ledger: {}",
                other.to_string().lines().next().unwrap_or_default()
            ),
            "check that the state directory is writable",
        ),
    }
}
