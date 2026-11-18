//! The version-agnostic `daemon.sock` handover that runs before `daemon install` enables units.
//!
//! `slipfork daemon install` must never let an older (or zero-setup) daemon keep the control
//! socket while systemd starts the installed one (`ADR-0011` §3). [`release_daemon`] runs one
//! order: stop the units this installer writes, ask a daemon that speaks the typed contract to shut
//! down and await it, then prove any remaining holder — peer pid from `SO_PEERCRED`, executable
//! basename from `/proc/<pid>/exe`, pinned with a `pidfd` — before a signal exists to send. A
//! holder that cannot be proven is refused in one line and never signalled.
//!
//! Every probe and the shutdown go through the injected [`UnitInstaller`], so one adapter sees one
//! socket at a time and the install sequence only decides the order.

#![forbid(unsafe_code)]

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::daemon_unit_install::{ManagedUnit, UnitInstaller};
use slipfork_edge::DaemonControlServer;
use slipfork_engine::linux_cow::{ProcessSignal, open_pidfd, peer_pid};

/// One daemon-socket budget shared by the handover and the CLI's auto-spawn wait.
pub const DAEMON_GRACE: Duration = Duration::from_secs(15);

/// Poll interval while waiting for `<runtime_dir>/daemon.sock` to answer or disappear.
pub const DAEMON_POLL: Duration = Duration::from_millis(50);

/// The handover's bounded-wait budget: one grace window plus the probe interval.
///
/// `daemon install` keeps time with plain [`Instant`] observations, and the production caller
/// passes [`DAEMON_HANDOVER`]; a test passes a zero budget and reaches the refusal immediately.
#[derive(Debug, Clone, Copy)]
pub struct HandoverBudget {
    /// How long a held socket may be awaited before the refusal.
    pub grace: Duration,
    /// Interval between probes while waiting.
    pub poll: Duration,
}

/// The production handover budget.
pub const DAEMON_HANDOVER: HandoverBudget = HandoverBudget {
    grace: DAEMON_GRACE,
    poll: DAEMON_POLL,
};

/// What one probe of `<runtime_dir>/daemon.sock` found.
///
/// `Absent` is the only "released" answer, and the handover re-probes [`UnitInstaller::is_served`]
/// after `Absent` and `Unprovable` before it trusts that: a probe that could not name a holder has
/// proven nothing, so an answering socket is still a held socket.
#[derive(Debug)]
pub enum SocketHolder {
    /// A process answers and is proven to be this host's `slipforkd`; `pidfd` pins that exact
    /// process, so a later signal can never reach a reused pid.
    ProvenSlipforkd {
        /// Peer pid the ownership proof named.
        pid: u32,
        /// `pidfd_open(2)` descriptor for that process, opened at proof time.
        pidfd: OwnedFd,
    },
    /// A named process answers, but its executable basename is not `slipforkd`.
    Foreign {
        /// Peer pid from `SO_PEERCRED`.
        pid: u32,
    },
    /// A process answers, but no pid this namespace can address could be read from it
    /// (`SO_PEERCRED` reported `0`) or its executable could not be inspected.
    Unprovable,
    /// Nothing answers the socket (a leftover socket file is not a holder).
    Absent,
}

/// Release `<runtime_dir>/daemon.sock` from any holder before the installed units start.
///
/// One function, one order, every step fail-closed (`ADR-0011` §3):
///
/// 1. the units this installer writes are stopped while they exist;
/// 2. a daemon that speaks the typed contract is asked to shut down, then awaited (bounded);
/// 3. whatever still answers must be proven to be a `slipforkd` by the adapter's
///    [`UnitInstaller::socket_holder`] probe — peer pid from `SO_PEERCRED`, `/proc/<pid>/exe`
///    basename, and a `pidfd` that pins that process — and is then `SIGTERM`ed through the pidfd,
///    with `SIGKILL` only after the grace window;
/// 4. a holder that cannot be proven is refused in one line: it is never signalled, and the socket
///    is re-probed so an answering socket never lets the install proceed.
///
/// Every probe and the shutdown go through `installer`, so one adapter sees one socket at a time.
/// No holder means no effect beyond the unit stop, so a second run converges with `[ok] unchanged`.
pub fn release_daemon(
    installer: &dyn UnitInstaller,
    budget: HandoverBudget,
    runtime_dir: &Path,
) -> Result<(), String> {
    let socket = DaemonControlServer::socket_path_in(runtime_dir);

    installer.stop_units(&[ManagedUnit::Socket, ManagedUnit::Service])?;

    // A leftover socket file nobody bound is not a holder: only a successful connect counts.
    if !installer.is_served(runtime_dir)? {
        return Ok(());
    }

    // (b) The typed contract first: ask the daemon to stop itself, then await its disappearance.
    if installer.shutdown(runtime_dir).is_ok() && await_release(installer, budget, runtime_dir)? {
        return Ok(());
    }

    // (c) Anything still answering is proven by the adapter before a signal exists to send. The
    // proof opens a pidfd, so both signals below address that process and never a reused pid.
    match installer.socket_holder(runtime_dir)? {
        SocketHolder::ProvenSlipforkd { pid, pidfd } => {
            for signal in [ProcessSignal::Terminate, ProcessSignal::Kill] {
                if let Err(err) = installer.signal_proven(&pidfd, signal) {
                    // The pinned process exited between the proof and the signal; the bounded
                    // await is still the single judge of whether the socket was released.
                    if await_release(installer, budget, runtime_dir)? {
                        return Ok(());
                    }
                    return Err(held_socket(
                        &socket,
                        pid,
                        &format!("could not be signalled ({err})"),
                    ));
                }
                if await_release(installer, budget, runtime_dir)? {
                    return Ok(());
                }
            }
            Err(held_socket(
                &socket,
                pid,
                "still holds the socket after `SIGTERM` and `SIGKILL`",
            ))
        }
        SocketHolder::Foreign { pid } => refuse_unless_released(
            installer,
            budget,
            runtime_dir,
            &socket,
            Some(pid),
            "is not a slipforkd this install can prove and stop",
        ),
        SocketHolder::Unprovable => refuse_unless_released(
            installer,
            budget,
            runtime_dir,
            &socket,
            None,
            "answers the socket, but this install cannot prove which process holds it",
        ),
        // The probe could not name a holder; if the socket still answers, a different one took it.
        SocketHolder::Absent => refuse_unless_released(
            installer,
            budget,
            runtime_dir,
            &socket,
            None,
            "answers the socket, but the probe could not name its holder",
        ),
    }
}

/// Probe `<runtime_dir>/daemon.sock` and prove who answers it (`SO_PEERCRED` + `/proc/<pid>/exe`).
///
/// This is the single place kernel credentials become an ownership fact: the pid comes from
/// `SO_PEERCRED`, the executable **basename** from `/proc/<pid>/exe` (an upgrade leaves
/// `slipforkd (deleted)` under a still-running daemon), and a proven holder is pinned with a
/// [`pidfd`](open_pidfd) so a later signal can never reach a reused pid.
///
/// `Absent` means the connect failed; it is **not** a release decision — the handover re-probes
/// [`UnitInstaller::is_served`] after `Absent` and `Unprovable`, so a probe that could not name a
/// holder can never let a held socket through.
pub fn probe_socket_holder(runtime_dir: &Path) -> Result<SocketHolder, String> {
    let socket = DaemonControlServer::socket_path_in(runtime_dir);
    let stream = match UnixStream::connect(&socket) {
        Ok(stream) => stream,
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(SocketHolder::Absent);
        }
        Err(err) => return Err(format!("cannot connect to {}: {err}", socket.display())),
    };
    let Ok(pid) = peer_pid(&stream) else {
        // A peer outside this PID namespace reports pid 0: it answers, but nothing here can be
        // proven about it.
        return Ok(SocketHolder::Unprovable);
    };
    let executable = match fs::read_link(format!("/proc/{pid}/exe")) {
        Ok(executable) => executable,
        // It exited between the connect and the read: the socket is re-probed before any action.
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(SocketHolder::Absent),
        Err(_) => return Ok(SocketHolder::Unprovable),
    };
    let name = executable
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    let basename = name.strip_suffix(" (deleted)").unwrap_or(name);
    if basename != "slipforkd" {
        return Ok(SocketHolder::Foreign { pid });
    }
    let pidfd =
        open_pidfd(pid).map_err(|err| format!("cannot pin the slipforkd pid {pid}: {err}"))?;
    Ok(SocketHolder::ProvenSlipforkd { pid, pidfd })
}

/// Await the control socket's disappearance, bounded by the caller's grace window.
fn await_release(
    installer: &dyn UnitInstaller,
    budget: HandoverBudget,
    runtime_dir: &Path,
) -> Result<bool, String> {
    let deadline = std::time::Instant::now() + budget.grace;
    loop {
        if !installer.is_served(runtime_dir)? {
            return Ok(true);
        }
        if std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(budget.poll);
    }
}

/// Refuse in one line unless the socket was really released (invariant `1d`).
///
/// `pid` is `None` when the probe itself could not name the holder (a peer outside this PID
/// namespace, or an unreadable executable); naming `sudo kill 0` would be worse than naming none.
fn refuse_unless_released(
    installer: &dyn UnitInstaller,
    budget: HandoverBudget,
    runtime_dir: &Path,
    socket: &Path,
    pid: Option<u32>,
    reason: &str,
) -> Result<(), String> {
    if await_release(installer, budget, runtime_dir)? {
        return Ok(());
    }
    Err(match pid {
        Some(pid) => held_socket(socket, pid, reason),
        None => format!(
            "{} {reason}; stop it manually with `sudo systemctl stop slipforkd.socket slipforkd.service`",
            socket.display()
        ),
    })
}

/// One line naming the socket, its holder, and the exact manual command (invariant `1d`).
fn held_socket(socket: &Path, pid: u32, reason: &str) -> String {
    format!(
        "{} is held by pid {pid}, which {reason}; stop it manually with `sudo kill {pid}`",
        socket.display()
    )
}
