//! `slipfork daemon install`: the single installer for `:80` system socket activation.
//!
//! `systemd` (PID 1) is the only component allowed to bind `127.0.0.1:80` and `[::1]:80` on a host
//! whose `net.ipv4.ip_unprivileged_port_start` is `1024`; neither an unprivileged process nor
//! `systemd --user` may do it (`docs/research/tunnel-weaver-and-systemd-facts.md` §5). This module
//! owns the pure install orchestration (`ADR-0011` §1): it renders the **exact bytes**
//! `slipfork daemon unit` prints through the `slipfork-edge` renderers, resolves the target
//! developer account, and drives a narrow [`UnitInstaller`] port. Every filesystem and
//! unit-management effect happens in the adapter (`OsUnitInstaller` in `main.rs`); the one
//! ownership proof ([`probe_socket_holder`]) lives here because it is pure over an explicit
//! runtime dir and is exercised end to end by the tests. There is no stringly
//! `run(program, args)` bag, no `--force`, and no user-unit fallback.
//!
//! Idempotency is byte equality: a file is rewritten only when its content differs, so a second
//! `daemon install` performs zero writes while still running `enable --now`.
//!
//! The socket handover ([`release_daemon`]) is **version-agnostic** and runs one order: stop the
//! units this module writes, ask the daemon to shut itself down and await it, then — if anything
//! still answers — [`probe_socket_holder`] must prove the holder is a `slipforkd` from
//! `SO_PEERCRED` plus the `/proc/<pid>/exe` basename, **pin it with a pidfd**, and signal only
//! through that descriptor, so `SIGKILL` can never reach a recycled pid. The probe and the shutdown
//! both go through the injected [`UnitInstaller`], so there is one socket path, one adapter, and no
//! second client racing it. An unowned holder is refused in one line and never signalled.

#![forbid(unsafe_code)]

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::apparmor::render_userns_profile;
use crate::layout::Layout;
use crate::user::{ResolvedAccount, UNPRIVILEGED_ACCOUNT_REMEDY, UserIdentity};
use slipfork_edge::{
    DaemonControlServer, INGRESS_ADDRS, UnitSpec, render_service_unit, render_socket_unit,
};
use slipfork_engine::MonotonicClock;
use slipfork_engine::linux_cow::{ProcessSignal, open_pidfd, peer_pid};

/// `/etc/systemd/system/slipforkd.socket` — PID 1 binds the ingress/forward ports here.
pub const SOCKET_UNIT_PATH: &str = "/etc/systemd/system/slipforkd.socket";
/// `/etc/systemd/system/slipforkd.service` — the unprivileged daemon unit.
pub const SERVICE_UNIT_PATH: &str = "/etc/systemd/system/slipforkd.service";
/// `/etc/apparmor.d/slipfork` — the `userns,` profile (`ADR-0010` §7).
pub const APPARMOR_PROFILE_PATH: &str = "/etc/apparmor.d/slipfork";

/// Names the non-root remedy includes the resolved account (the `--user` value that will work
/// after `sudo`). The bare command is not enough on a host whose login account differs.
const NON_ROOT_REMEDY: &str = "daemon install writes /etc and enables system units; run it as root (sudo slipfork daemon install --user ";

/// One daemon-socket budget shared by the handover and the CLI's auto-spawn wait.
pub const DAEMON_GRACE: Duration = Duration::from_secs(15);

/// Poll interval while waiting for `<runtime_dir>/daemon.sock` to answer or disappear.
pub const DAEMON_POLL: Duration = Duration::from_millis(50);

/// The handover's bounded-wait port: the engine's single clock plus the poll pause.
///
/// The production adapter is `slipfork_engine::SystemClock` itself, so `daemon install` keeps time
/// with the same clock the daemon uses. Tests inject a fake whose
/// [`pause`](HandoverClock::pause) advances [`now`](MonotonicClock::now), so a grace window is
/// reached deterministically without sleeping.
pub trait HandoverClock: MonotonicClock {
    /// Wait one poll interval before re-reading the socket.
    fn pause(&self);
}

impl HandoverClock for slipfork_engine::SystemClock {
    fn pause(&self) {
        std::thread::sleep(DAEMON_POLL);
    }
}

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

/// The systemd units `daemon install` enables together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedUnit {
    /// `slipforkd.socket` (owns the privileged binds).
    Socket,
    /// `slipforkd.service` (the unprivileged daemon).
    Service,
}

impl ManagedUnit {
    /// The unit file name `systemctl enable --now` receives.
    pub fn unit_name(self) -> &'static str {
        match self {
            Self::Socket => "slipforkd.socket",
            Self::Service => "slipforkd.service",
        }
    }
}

/// Narrow filesystem + unit-management port (`ADR-0011` §1).
///
/// Each verb names one real effect; the install sequence never composes shell command lines.
pub trait UnitInstaller: Send + Sync {
    /// Read one file, or `Ok(None)` when it does not exist.
    fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, String>;
    /// Write one file through a same-directory temporary file and `rename`.
    fn write_file_atomic(&self, path: &Path, contents: &[u8]) -> Result<(), String>;
    /// `systemctl daemon-reload`.
    fn daemon_reload(&self) -> Result<(), String>;
    /// `apparmor_parser -r <profile_path>`.
    fn apparmor_replace(&self, profile_path: &Path) -> Result<(), String>;
    /// `systemctl stop` every unit that exists in `units` (a missing unit is skipped).
    ///
    /// Runs before `enable --now`, so `systemctl start slipforkd.service` never races a
    /// unit-managed daemon of an older build for the control socket.
    fn stop_units(&self, units: &[ManagedUnit]) -> Result<(), String>;
    /// `true` when a process answers `<runtime_dir>/daemon.sock` (one connect, dropped at once).
    ///
    /// The bounded wait's only question: a leftover socket file nobody bound answers `false`.
    fn is_served(&self, runtime_dir: &Path) -> Result<bool, String>;
    /// One `DaemonRequest::Shutdown` round trip against `<runtime_dir>/daemon.sock`.
    ///
    /// Fails closed when the holder does not speak this build's typed contract (a foreign or older
    /// daemon) — the handover then proves ownership before it signals anything.
    fn shutdown(&self, runtime_dir: &Path) -> Result<(), String>;
    /// The one probe that proves (or refuses) who answers the socket.
    fn socket_holder(&self, runtime_dir: &Path) -> Result<SocketHolder, String>;
    /// `pidfd_send_signal(2)` the process pinned by a [`SocketHolder::ProvenSlipforkd`] proof.
    ///
    /// Signalling through the pidfd — never a pid — is what keeps the grace-window `SIGKILL` away
    /// from a process that merely reused the number.
    fn signal_proven(&self, pidfd: &OwnedFd, signal: ProcessSignal) -> Result<(), String>;
    /// `systemctl enable --now <units>`.
    fn enable_now(&self, units: &[ManagedUnit]) -> Result<(), String>;
    /// `systemctl restart slipforkd.service`.
    fn restart_service(&self) -> Result<(), String>;
}

/// Every already-resolved input of one `slipfork daemon install` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallRequest {
    /// Whether the invoking process is `uid 0` (`current_uid_gid` at the composition root).
    pub is_root: bool,
    /// `--user <ACCOUNT>`, or `None` to fall back to `SUDO_USER` / fail closed as direct root.
    pub explicit_user: Option<String>,
    /// `slipforkd` binary rendered into `ExecStart=`.
    pub executable: PathBuf,
    /// Repeatable `--bin-dir` values for the AppArmor profile (default: this executable's dir).
    pub bin_dirs: Vec<PathBuf>,
    /// Planned static forward port rendered as the trailing `ListenStream=`.
    pub forward_port: u16,
    /// Explicit `--state-dir`, or `None` to derive `<account-home>/.cache/slipfork`.
    pub state_dir: Option<PathBuf>,
    /// The invoking client's `$XDG_CACHE_HOME`, so the installed unit and the client derive one
    /// state root; `None` falls back to `<account-home>/.cache/slipfork` on both sides.
    pub xdg_cache_home: Option<PathBuf>,
    /// The invoking client's `$XDG_DATA_HOME` (vault/data home derivation).
    pub xdg_data_home: Option<PathBuf>,
}

/// Per-file outcome of an install run (`wrote` or `unchanged`; never `skipped`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    /// The file was created or its bytes changed.
    Wrote,
    /// The file already contained the exact rendered bytes.
    Unchanged,
}

impl FileStatus {
    /// The single word the human and `--json` documents use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wrote => "wrote",
            Self::Unchanged => "unchanged",
        }
    }
}

/// One completed install: the account the daemon runs as plus each file's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// The unprivileged developer account rendered into `User=`.
    pub user: String,
    /// `slipforkd.socket` outcome.
    pub socket: FileStatus,
    /// `slipforkd.service` outcome.
    pub service: FileStatus,
    /// `/etc/apparmor.d/slipfork` outcome.
    pub apparmor: FileStatus,
}

impl InstallReport {
    /// The three `(path, status)` rows, declared once for the human transcript.
    fn rows(&self) -> [(&'static str, FileStatus); 3] {
        [
            (SOCKET_UNIT_PATH, self.socket),
            (SERVICE_UNIT_PATH, self.service),
            (APPARMOR_PROFILE_PATH, self.apparmor),
        ]
    }

    /// `true` when every file already carried the rendered bytes.
    fn all_unchanged(&self) -> bool {
        self.rows()
            .iter()
            .all(|(_, status)| *status == FileStatus::Unchanged)
    }

    /// The first-run transcript (`docs/design/dx-0.1.0-ux.md` §2).
    pub fn render_text(&self) -> String {
        if self.all_unchanged() {
            return format!(
                "unchanged slipforkd.socket, slipforkd.service, {APPARMOR_PROFILE_PATH}; enabled\n"
            );
        }
        let mut text = String::new();
        for (path, status) in self.rows() {
            text.push_str(&format!("{:<9} {path}\n", status.as_str()));
        }
        text.push_str(&format!(
            "enabled   slipforkd.socket + slipforkd.service as {}, ingress 127.0.0.1:80 + [::1]:80\n",
            self.user
        ));
        text.push_str("next      slipfork up\n");
        text
    }
}

/// The one ingress `UnitSpec` derivation shared by `daemon unit` and `daemon install`.
///
/// Both commands resolve the same [`ResolvedAccount`] and [`Layout`] first, so the preview and the
/// installed bytes can never disagree.
pub fn ingress_unit_spec(
    account: &ResolvedAccount,
    layout: &Layout,
    executable: PathBuf,
    forward_port: u16,
) -> Result<UnitSpec, String> {
    let listen_addrs = INGRESS_ADDRS
        .iter()
        .map(|addr| {
            addr.parse()
                .map_err(|err| format!("invalid loopback ingress '{addr}': {err}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(UnitSpec {
        user: account.name.clone(),
        listen_addrs,
        forward_port,
        executable,
        state_dir: layout.state_dir().to_path_buf(),
        runtime_dir: layout.runtime_dir().to_path_buf(),
    })
}

/// Render and install the three files, then reload/enable the units (`ADR-0011` §1).
///
/// The function is pure over `identity` + `installer` + `clock` + `request`: it reads no
/// environment, opens no file, and spawns no process. Its one process-facing effect is the socket
/// handover, which is entirely delegated to the injected [`UnitInstaller`] (stop, shutdown, prove,
/// signal) so this flow only decides the order. It fails closed **before** touching the installer
/// when it is not root or when the target account is unusable.
///
/// A failure halfway through leaves already-written files in place; because every write is
/// idempotent, re-running the same command converges (and `enable --now` is safe to repeat).
pub fn install(
    identity: &dyn UserIdentity,
    installer: &dyn UnitInstaller,
    clock: &dyn HandoverClock,
    request: &InstallRequest,
) -> Result<InstallReport, String> {
    if !request.is_root {
        let account = identity.username()?;
        return Err(format!("{NON_ROOT_REMEDY}{account})"));
    }
    let account = target_account(identity, true, request.explicit_user.as_deref())?;
    let layout = Layout::for_account(
        &account,
        request.state_dir.clone(),
        request.xdg_cache_home.as_deref(),
        request.xdg_data_home.as_deref(),
    );
    let spec = ingress_unit_spec(
        &account,
        &layout,
        request.executable.clone(),
        request.forward_port,
    )?;
    let apparmor = render_userns_profile(&request.bin_dirs)?;

    let files = [
        (
            Path::new(SOCKET_UNIT_PATH),
            render_socket_unit(&spec).map_err(|err| err.to_string())?,
        ),
        (
            Path::new(SERVICE_UNIT_PATH),
            render_service_unit(&spec).map_err(|err| err.to_string())?,
        ),
        (Path::new(APPARMOR_PROFILE_PATH), apparmor),
    ];

    let mut statuses = Vec::with_capacity(files.len());
    let mut service_existed = false;
    for (index, (path, contents)) in files.iter().enumerate() {
        let existing = installer.read_file(path).map_err(converge_after_failure)?;
        let unchanged = existing.as_deref() == Some(contents.as_bytes());
        if index == 1 {
            service_existed = existing.is_some();
        }
        if unchanged {
            statuses.push(FileStatus::Unchanged);
        } else {
            installer
                .write_file_atomic(path, contents.as_bytes())
                .map_err(converge_after_failure)?;
            statuses.push(FileStatus::Wrote);
        }
    }
    let socket = statuses[0];
    let service = statuses[1];
    let apparmor = statuses[2];

    installer.daemon_reload().map_err(converge_after_failure)?;
    installer
        .apparmor_replace(Path::new(APPARMOR_PROFILE_PATH))
        .map_err(converge_after_failure)?;
    // A zero-setup daemon (of any build) may hold `<runtime_dir>/daemon.sock`; hand it over before
    // `enable --now`.
    release_daemon(installer, clock, layout.runtime_dir()).map_err(converge_after_failure)?;
    installer
        .enable_now(&[ManagedUnit::Socket, ManagedUnit::Service])
        .map_err(converge_after_failure)?;
    // An already-installed service keeps running the old `ExecStart=` after `daemon-reload`;
    // restart it only when its bytes actually changed so a no-op re-run never bounces the daemon.
    if service == FileStatus::Wrote && service_existed {
        installer
            .restart_service()
            .map_err(converge_after_failure)?;
    }

    Ok(InstallReport {
        user: account.name,
        socket,
        service,
        apparmor,
    })
}

/// Every mid-sequence effect failure tells the operator that a re-run converges.
fn converge_after_failure(message: String) -> String {
    format!("{message} — re-run to converge")
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
    clock: &dyn HandoverClock,
    runtime_dir: &Path,
) -> Result<(), String> {
    let socket = DaemonControlServer::socket_path_in(runtime_dir);

    installer.stop_units(&[ManagedUnit::Socket, ManagedUnit::Service])?;

    // A leftover socket file nobody bound is not a holder: only a successful connect counts.
    if !installer.is_served(runtime_dir)? {
        return Ok(());
    }

    // (b) The typed contract first: ask the daemon to stop itself, then await its disappearance.
    if installer.shutdown(runtime_dir).is_ok() && await_release(installer, clock, runtime_dir)? {
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
                    if await_release(installer, clock, runtime_dir)? {
                        return Ok(());
                    }
                    return Err(held_socket(
                        &socket,
                        pid,
                        &format!("could not be signalled ({err})"),
                    ));
                }
                if await_release(installer, clock, runtime_dir)? {
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
            clock,
            runtime_dir,
            &socket,
            Some(pid),
            "is not a slipforkd this install can prove and stop",
        ),
        SocketHolder::Unprovable => refuse_unless_released(
            installer,
            clock,
            runtime_dir,
            &socket,
            None,
            "answers the socket, but this install cannot prove which process holds it",
        ),
        // The probe could not name a holder; if the socket still answers, a different one took it.
        SocketHolder::Absent => refuse_unless_released(
            installer,
            clock,
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

/// Await the control socket's disappearance, bounded by one grace window from the injected clock.
fn await_release(
    installer: &dyn UnitInstaller,
    clock: &dyn HandoverClock,
    runtime_dir: &Path,
) -> Result<bool, String> {
    let deadline = clock.now() + DAEMON_GRACE;
    loop {
        if !installer.is_served(runtime_dir)? {
            return Ok(true);
        }
        if clock.now() >= deadline {
            return Ok(false);
        }
        clock.pause();
    }
}

/// Refuse in one line unless the socket was really released (invariant `1d`).
///
/// `pid` is `None` when the probe itself could not name the holder (a peer outside this PID
/// namespace, or an unreadable executable); naming `sudo kill 0` would be worse than naming none.
fn refuse_unless_released(
    installer: &dyn UnitInstaller,
    clock: &dyn HandoverClock,
    runtime_dir: &Path,
    socket: &Path,
    pid: Option<u32>,
    reason: &str,
) -> Result<(), String> {
    if await_release(installer, clock, runtime_dir)? {
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

/// Resolve the account the unit runs as, or fail closed in one line.
///
/// Shared by `daemon unit --user` and `daemon install`: `--user` wins, otherwise root uses
/// `SUDO_USER` and a non-root preview uses its own `id -un`. Direct root has no target account.
pub fn target_account(
    identity: &dyn UserIdentity,
    is_root: bool,
    explicit_user: Option<&str>,
) -> Result<ResolvedAccount, String> {
    if let Some(account) = explicit_user {
        return identity.lookup_account(account);
    }
    if !is_root {
        return identity.lookup_account(&identity.username()?);
    }
    match identity.invoking_account()? {
        Some(account) => identity.lookup_account(&account),
        None => Err(UNPRIVILEGED_ACCOUNT_REMEDY.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::os::unix::net::UnixListener;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant, SystemTime};

    use slipfork_engine::SystemClock;

    use super::*;

    const TEST_USER: &str = "slipforktest";
    const TEST_HOME: &str = "/home/slipforktest";

    #[derive(Debug, Clone)]
    struct FakeIdentity {
        username: String,
        invoking: Option<String>,
    }

    impl Default for FakeIdentity {
        fn default() -> Self {
            Self {
                username: TEST_USER.to_owned(),
                invoking: None,
            }
        }
    }

    fn sudo_identity() -> FakeIdentity {
        FakeIdentity {
            username: "root".to_owned(),
            invoking: Some(TEST_USER.to_owned()),
        }
    }

    impl UserIdentity for FakeIdentity {
        fn username(&self) -> Result<String, String> {
            Ok(self.username.clone())
        }

        fn lookup_account(&self, account: &str) -> Result<ResolvedAccount, String> {
            if account == "root" {
                return Err(UNPRIVILEGED_ACCOUNT_REMEDY.to_owned());
            }
            if account != TEST_USER && account != "dev" {
                return Err(format!("no OS account '{account}' exists on this host"));
            }
            Ok(ResolvedAccount {
                name: account.to_owned(),
                uid: 1000,
                gid: 1000,
                home: PathBuf::from(TEST_HOME),
            })
        }

        fn invoking_account(&self) -> Result<Option<String>, String> {
            Ok(self.invoking.clone())
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Effect {
        Write(PathBuf, Vec<u8>),
        DaemonReload,
        ApparmorReplace(PathBuf),
        StopUnits(Vec<ManagedUnit>),
        Signal(ProcessSignal),
        EnableNow(Vec<ManagedUnit>),
        RestartService,
    }

    /// When the fake daemon behind the scripted socket verbs releases `daemon.sock`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeDaemon {
        /// Nothing answers the socket.
        Absent,
        /// This build's daemon: accepts `shutdown`, then the socket is gone.
        Typed,
        /// A foreign build: `shutdown` fails, and only `SIGKILL` releases it.
        ForeignUntilKill,
        /// A foreign build that survives every signal.
        Immortal,
        /// The real [`probe_socket_holder`] over a real `UnixListener` (nothing scripted).
        Real,
    }

    /// Recording [`UnitInstaller`]: seeds `read_file` bytes and records every effect verb.
    ///
    /// The socket verbs are scripted (`daemon`, `holder`), so no test opens a real daemon or
    /// signals a real process; the one exception is [`FakeDaemon::Real`], which runs the real
    /// ownership proof against a real listener.
    struct RecordingInstaller {
        files: Mutex<BTreeMap<PathBuf, Vec<u8>>>,
        effects: Mutex<Vec<Effect>>,
        reads: AtomicUsize,
        daemon: Mutex<FakeDaemon>,
        shutdown_called: AtomicBool,
        killed: AtomicBool,
        holder: Mutex<Option<SocketHolder>>,
    }

    impl Default for RecordingInstaller {
        fn default() -> Self {
            Self {
                files: Mutex::new(BTreeMap::new()),
                effects: Mutex::new(Vec::new()),
                reads: AtomicUsize::new(0),
                daemon: Mutex::new(FakeDaemon::Absent),
                shutdown_called: AtomicBool::new(false),
                killed: AtomicBool::new(false),
                holder: Mutex::new(None),
            }
        }
    }

    impl RecordingInstaller {
        fn seeded(files: &[(PathBuf, String)]) -> Self {
            let installer = Self::default();
            {
                let mut map = installer.files.lock().expect("files");
                for (path, contents) in files {
                    map.insert(path.clone(), contents.as_bytes().to_vec());
                }
            }
            installer
        }

        /// Script the fake daemon behind the socket verbs.
        fn daemon(self, daemon: FakeDaemon) -> Self {
            *self.daemon.lock().expect("daemon") = daemon;
            self
        }

        /// Script the peer the ownership probe reports.
        fn holding(self, holder: SocketHolder) -> Self {
            *self.holder.lock().expect("holder") = Some(holder);
            self
        }

        fn effects(&self) -> Vec<Effect> {
            self.effects.lock().expect("effects").clone()
        }

        fn writes(&self) -> Vec<Effect> {
            self.effects()
                .into_iter()
                .filter(|effect| matches!(effect, Effect::Write(..)))
                .collect()
        }

        fn signals(&self) -> Vec<ProcessSignal> {
            self.effects()
                .into_iter()
                .filter_map(|effect| match effect {
                    Effect::Signal(signal) => Some(signal),
                    _ => None,
                })
                .collect()
        }

        fn stop_units_calls(&self) -> Vec<Vec<ManagedUnit>> {
            self.effects()
                .into_iter()
                .filter_map(|effect| match effect {
                    Effect::StopUnits(units) => Some(units),
                    _ => None,
                })
                .collect()
        }
    }

    impl UnitInstaller for RecordingInstaller {
        fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, String> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(self.files.lock().expect("files").get(path).cloned())
        }

        fn write_file_atomic(&self, path: &Path, contents: &[u8]) -> Result<(), String> {
            self.effects
                .lock()
                .expect("effects")
                .push(Effect::Write(path.to_path_buf(), contents.to_vec()));
            self.files
                .lock()
                .expect("files")
                .insert(path.to_path_buf(), contents.to_vec());
            Ok(())
        }

        fn daemon_reload(&self) -> Result<(), String> {
            self.effects
                .lock()
                .expect("effects")
                .push(Effect::DaemonReload);
            Ok(())
        }

        fn apparmor_replace(&self, profile_path: &Path) -> Result<(), String> {
            self.effects
                .lock()
                .expect("effects")
                .push(Effect::ApparmorReplace(profile_path.to_path_buf()));
            Ok(())
        }

        fn stop_units(&self, units: &[ManagedUnit]) -> Result<(), String> {
            self.effects
                .lock()
                .expect("effects")
                .push(Effect::StopUnits(units.to_vec()));
            Ok(())
        }

        fn is_served(&self, runtime_dir: &Path) -> Result<bool, String> {
            Ok(match *self.daemon.lock().expect("daemon") {
                FakeDaemon::Absent => false,
                FakeDaemon::Typed => !self.shutdown_called.load(Ordering::SeqCst),
                FakeDaemon::ForeignUntilKill => !self.killed.load(Ordering::SeqCst),
                FakeDaemon::Immortal => true,
                FakeDaemon::Real => {
                    UnixStream::connect(DaemonControlServer::socket_path_in(runtime_dir)).is_ok()
                }
            })
        }

        fn shutdown(&self, _runtime_dir: &Path) -> Result<(), String> {
            match *self.daemon.lock().expect("daemon") {
                FakeDaemon::Typed => {
                    self.shutdown_called.store(true, Ordering::SeqCst);
                    Ok(())
                }
                // A foreign build answers something this client cannot parse, and the real
                // listener in the `Real` scenario never speaks the contract at all.
                _ => {
                    Err("daemon control protocol error: this reply is not this build's".to_owned())
                }
            }
        }

        fn socket_holder(&self, runtime_dir: &Path) -> Result<SocketHolder, String> {
            if *self.daemon.lock().expect("daemon") == FakeDaemon::Real {
                return probe_socket_holder(runtime_dir);
            }
            Ok(self
                .holder
                .lock()
                .expect("holder")
                .take()
                .unwrap_or(SocketHolder::Absent))
        }

        fn signal_proven(&self, _pidfd: &OwnedFd, signal: ProcessSignal) -> Result<(), String> {
            self.effects
                .lock()
                .expect("effects")
                .push(Effect::Signal(signal));
            if signal == ProcessSignal::Kill {
                self.killed.store(true, Ordering::SeqCst);
            }
            Ok(())
        }

        fn enable_now(&self, units: &[ManagedUnit]) -> Result<(), String> {
            self.effects
                .lock()
                .expect("effects")
                .push(Effect::EnableNow(units.to_vec()));
            Ok(())
        }

        fn restart_service(&self) -> Result<(), String> {
            self.effects
                .lock()
                .expect("effects")
                .push(Effect::RestartService);
            Ok(())
        }
    }

    /// A deterministic [`HandoverClock`]: `pause` advances `now` by exactly one poll interval.
    struct FakeClock {
        now: Mutex<Instant>,
        pauses: AtomicUsize,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                now: Mutex::new(Instant::now()),
                pauses: AtomicUsize::new(0),
            }
        }

        /// How much fake time the handover waited in total.
        fn waited(&self) -> Duration {
            let pauses = u32::try_from(self.pauses.load(Ordering::SeqCst)).expect("pause count");
            DAEMON_POLL * pauses
        }
    }

    impl MonotonicClock for FakeClock {
        fn now(&self) -> Instant {
            *self.now.lock().expect("now")
        }

        fn now_wall(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH
        }
    }

    impl HandoverClock for FakeClock {
        fn pause(&self) {
            self.pauses.fetch_add(1, Ordering::SeqCst);
            *self.now.lock().expect("now") += DAEMON_POLL;
        }
    }

    fn request(is_root: bool) -> InstallRequest {
        InstallRequest {
            is_root,
            explicit_user: None,
            executable: PathBuf::from("/usr/local/bin/slipforkd"),
            bin_dirs: vec![PathBuf::from("/usr/local/bin")],
            forward_port: 7080,
            state_dir: None,
            xdg_cache_home: None,
            xdg_data_home: None,
        }
    }

    fn expected_spec() -> UnitSpec {
        let account = ResolvedAccount {
            name: TEST_USER.to_owned(),
            uid: 1000,
            gid: 1000,
            home: PathBuf::from(TEST_HOME),
        };
        let layout = Layout::for_account(&account, None, None, None);
        ingress_unit_spec(
            &account,
            &layout,
            PathBuf::from("/usr/local/bin/slipforkd"),
            7080,
        )
        .expect("shared ingress unit spec")
    }

    #[test]
    fn non_root_fails_closed_before_touching_the_installer() {
        let installer = RecordingInstaller::default();
        let error = install(
            &FakeIdentity::default(),
            &installer,
            &SystemClock,
            &request(false),
        )
        .expect_err("non-root must fail closed");
        assert_eq!(
            error,
            "daemon install writes /etc and enables system units; run it as root \
             (sudo slipfork daemon install --user slipforktest)"
        );
        assert_eq!(installer.reads.load(Ordering::SeqCst), 0, "zero reads");
        assert!(
            installer.effects().is_empty(),
            "zero writes and zero unit calls"
        );
    }

    #[test]
    fn root_without_a_valid_sudo_user_fails_closed() {
        let installer = RecordingInstaller::default();
        let error = install(
            &FakeIdentity::default(),
            &installer,
            &SystemClock,
            &request(true),
        )
        .expect_err("direct root must fail closed");
        assert_eq!(error, UNPRIVILEGED_ACCOUNT_REMEDY);
        assert!(installer.effects().is_empty());

        let mut root_user = request(true);
        root_user.explicit_user = Some("root".to_owned());
        let error = install(
            &FakeIdentity::default(),
            &installer,
            &SystemClock,
            &root_user,
        )
        .expect_err("--user root must fail closed");
        assert_eq!(error, UNPRIVILEGED_ACCOUNT_REMEDY);
        assert!(installer.effects().is_empty());

        let mut unknown = request(true);
        unknown.explicit_user = Some("nobody-here".to_owned());
        let error = install(&FakeIdentity::default(), &installer, &SystemClock, &unknown)
            .expect_err("an unknown --user must fail closed");
        assert!(error.contains("no OS account"), "{error}");
        assert!(installer.effects().is_empty());
    }

    #[test]
    fn sudo_user_becomes_the_installed_account() {
        let installer = RecordingInstaller::default();
        let identity = FakeIdentity {
            username: "root".to_owned(),
            invoking: Some("dev".to_owned()),
        };
        let report =
            install(&identity, &installer, &SystemClock, &request(true)).expect("install as dev");
        assert_eq!(report.user, "dev");
        let service = installer
            .writes()
            .into_iter()
            .find_map(|effect| match effect {
                Effect::Write(path, bytes) if path == Path::new(SERVICE_UNIT_PATH) => {
                    Some(String::from_utf8(bytes).expect("utf8 unit"))
                }
                _ => None,
            })
            .expect("service unit written");
        assert!(service.contains("\nUser=dev\n"), "{service}");
    }

    #[test]
    fn first_run_writes_the_exact_daemon_unit_bytes_and_enables_once() {
        let installer = RecordingInstaller::default();
        let report =
            install(&sudo_identity(), &installer, &SystemClock, &request(true)).expect("install");

        assert_eq!(report.socket, FileStatus::Wrote);
        assert_eq!(report.service, FileStatus::Wrote);
        assert_eq!(report.apparmor, FileStatus::Wrote);
        assert_eq!(report.user, TEST_USER);

        let writes = installer.writes();
        assert_eq!(writes.len(), 3, "one write per installed file: {writes:?}");
        let expected = expected_spec();
        let expected_socket = render_socket_unit(&expected).expect("socket");
        let expected_service = render_service_unit(&expected).expect("service");
        let expected_apparmor =
            render_userns_profile(&[PathBuf::from("/usr/local/bin")]).expect("profile");
        for (path, expected) in [
            (SOCKET_UNIT_PATH, expected_socket.as_bytes()),
            (SERVICE_UNIT_PATH, expected_service.as_bytes()),
            (APPARMOR_PROFILE_PATH, expected_apparmor.as_bytes()),
        ] {
            let written = writes.iter().find_map(|effect| match effect {
                Effect::Write(written_path, bytes) if written_path == Path::new(path) => {
                    Some(bytes.as_slice())
                }
                _ => None,
            });
            assert_eq!(
                written,
                Some(expected),
                "{path} must be byte-identical to `slipfork daemon unit`"
            );
        }

        let effects = installer.effects();
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(effect, Effect::EnableNow(_)))
                .count(),
            1,
            "enable --now must run exactly once per invocation: {effects:?}"
        );
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(effect, Effect::DaemonReload))
                .count(),
            1
        );
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(effect, Effect::ApparmorReplace(_)))
                .count(),
            1
        );
        assert!(
            !effects.contains(&Effect::RestartService),
            "a fresh install starts the service; it never restarts it: {effects:?}"
        );
        let stop = effects
            .iter()
            .position(|effect| matches!(effect, Effect::StopUnits(_)))
            .expect("the handover stops the units it owns");
        let enable_position = effects
            .iter()
            .position(|effect| matches!(effect, Effect::EnableNow(_)))
            .expect("enable runs");
        assert!(
            stop < enable_position,
            "the units (and any daemon they run) are stopped before enable --now: {effects:?}"
        );
        assert_eq!(
            installer.stop_units_calls(),
            vec![vec![ManagedUnit::Socket, ManagedUnit::Service]],
            "the handover stops exactly the units this installer writes, by their own names"
        );
        assert!(
            installer.signals().is_empty(),
            "no holder answers the fresh state dir, so nothing is signalled: {effects:?}"
        );
        let enable = effects
            .iter()
            .find_map(|effect| match effect {
                Effect::EnableNow(units) => Some(units.clone()),
                _ => None,
            })
            .expect("enable effect");
        assert_eq!(enable, vec![ManagedUnit::Socket, ManagedUnit::Service]);
    }

    #[test]
    fn identical_bytes_are_not_rewritten_and_enable_still_runs() {
        let first = RecordingInstaller::default();
        install(&sudo_identity(), &first, &SystemClock, &request(true)).expect("first install");
        let seeded: Vec<(PathBuf, String)> = first
            .writes()
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::Write(path, bytes) => Some((path, String::from_utf8(bytes).expect("utf8"))),
                _ => None,
            })
            .collect();
        assert_eq!(seeded.len(), 3);

        let installer = RecordingInstaller::seeded(&seeded);
        let report = install(&sudo_identity(), &installer, &SystemClock, &request(true))
            .expect("idempotent re-run");
        assert_eq!(report.socket, FileStatus::Unchanged);
        assert_eq!(report.service, FileStatus::Unchanged);
        assert_eq!(report.apparmor, FileStatus::Unchanged);
        assert!(
            installer.writes().is_empty(),
            "a re-run with identical bytes performs zero writes"
        );
        let effects = installer.effects();
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(effect, Effect::EnableNow(_)))
                .count(),
            1,
            "enable --now still runs once: {effects:?}"
        );
        assert!(!effects.contains(&Effect::RestartService));
        assert_eq!(
            report.render_text(),
            format!(
                "unchanged slipforkd.socket, slipforkd.service, {APPARMOR_PROFILE_PATH}; enabled\n"
            )
        );
    }

    #[test]
    fn a_changed_service_unit_restarts_the_already_installed_service() {
        let expected_service = render_service_unit(&expected_spec()).expect("service");
        let stale_service = expected_service.replace("LimitNOFILE=65536", "LimitNOFILE=1024");
        let installer =
            RecordingInstaller::seeded(&[(PathBuf::from(SERVICE_UNIT_PATH), stale_service)]);
        let report = install(&sudo_identity(), &installer, &SystemClock, &request(true))
            .expect("upgrade install");
        assert_eq!(report.service, FileStatus::Wrote);
        assert!(
            installer.effects().contains(&Effect::RestartService),
            "an already-installed service must restart to pick up new bytes"
        );
    }

    #[test]
    fn the_invoking_clients_xdg_cache_home_pins_the_installed_state_dir() {
        let installer = RecordingInstaller::default();
        let mut request = request(true);
        request.xdg_cache_home = Some(PathBuf::from("/xdg/cache"));
        install(&sudo_identity(), &installer, &SystemClock, &request).expect("install");
        let service = installer
            .writes()
            .into_iter()
            .find_map(|effect| match effect {
                Effect::Write(path, bytes) if path == Path::new(SERVICE_UNIT_PATH) => {
                    Some(String::from_utf8(bytes).expect("utf8 unit"))
                }
                _ => None,
            })
            .expect("service unit written");
        assert!(
            service
                .contains("--state-dir /xdg/cache/slipfork --runtime-dir /xdg/cache/slipfork/run"),
            "the unit must derive the same state/runtime roots as the client: {service}"
        );
    }

    #[test]
    fn every_effect_failure_says_a_re_run_converges() {
        assert_eq!(
            converge_after_failure("cannot write /etc/x: EPERM".to_owned()),
            "cannot write /etc/x: EPERM — re-run to converge"
        );
    }

    #[test]
    fn the_first_run_transcript_matches_the_design() {
        assert_eq!(FileStatus::Wrote.as_str(), "wrote");
        assert_eq!(FileStatus::Unchanged.as_str(), "unchanged");
        let report = InstallReport {
            user: "dev".to_owned(),
            socket: FileStatus::Wrote,
            service: FileStatus::Wrote,
            apparmor: FileStatus::Wrote,
        };
        assert_eq!(
            report.render_text(),
            "wrote     /etc/systemd/system/slipforkd.socket\n\
             wrote     /etc/systemd/system/slipforkd.service\n\
             wrote     /etc/apparmor.d/slipfork\n\
             enabled   slipforkd.socket + slipforkd.service as dev, ingress 127.0.0.1:80 + [::1]:80\n\
             next      slipfork up\n"
        );
    }

    fn temp_runtime() -> tempfile::TempDir {
        tempfile::tempdir().expect("private runtime dir")
    }

    /// One scripted proven holder: a real pidfd pinned to `pid` (never signalled by the fake).
    fn proven_holder(pid: u32) -> SocketHolder {
        SocketHolder::ProvenSlipforkd {
            pid,
            pidfd: open_pidfd(pid).expect("pin the scripted pid"),
        }
    }

    #[test]
    fn a_runtime_dir_without_a_holder_performs_no_signal() {
        let dir = temp_runtime();
        let installer = RecordingInstaller::default();
        let clock = FakeClock::new();

        release_daemon(&installer, &clock, dir.path()).expect("no holder releases nothing");

        assert_eq!(
            installer.stop_units_calls(),
            vec![vec![ManagedUnit::Socket, ManagedUnit::Service]]
        );
        assert!(
            installer.signals().is_empty(),
            "no holder means no signal: {:?}",
            installer.effects()
        );
        assert_eq!(clock.waited(), Duration::ZERO, "nothing is awaited");
    }

    #[test]
    fn a_holder_that_answers_shutdown_is_released_without_a_signal() {
        let dir = temp_runtime();
        let installer = RecordingInstaller::default().daemon(FakeDaemon::Typed);
        let clock = FakeClock::new();

        release_daemon(&installer, &clock, dir.path())
            .expect("a daemon that speaks the typed contract shuts itself down");

        assert!(
            installer.signals().is_empty(),
            "a daemon that answers `shutdown` is never signalled: {:?}",
            installer.signals()
        );
        assert!(
            clock.waited() < DAEMON_GRACE,
            "the socket is gone as soon as the daemon accepts `shutdown`, waited {:?}",
            clock.waited()
        );
    }

    #[test]
    fn a_foreign_holder_owned_by_slipforkd_is_terminated_after_the_grace() {
        let dir = temp_runtime();
        let installer = RecordingInstaller::default()
            .daemon(FakeDaemon::ForeignUntilKill)
            .holding(proven_holder(std::process::id()));
        let clock = FakeClock::new();

        release_daemon(&installer, &clock, dir.path())
            .expect("a proven slipforkd holder converges");

        assert_eq!(
            installer.signals(),
            vec![ProcessSignal::Terminate, ProcessSignal::Kill],
            "SIGTERM first, SIGKILL only after the grace elapsed"
        );
        assert!(
            clock.waited() >= DAEMON_GRACE,
            "SIGKILL must wait a full grace window, waited {:?}",
            clock.waited()
        );
    }

    #[test]
    fn an_unowned_holder_is_refused_in_one_line_without_a_signal() {
        let dir = temp_runtime();
        let installer = RecordingInstaller::default()
            .daemon(FakeDaemon::Immortal)
            .holding(SocketHolder::Foreign { pid: 777 });
        let clock = FakeClock::new();

        let error = release_daemon(&installer, &clock, dir.path())
            .expect_err("an unproven holder must be refused");

        assert_eq!(error.lines().count(), 1, "exactly one line: {error}");
        for expected in [
            DaemonControlServer::socket_path_in(dir.path())
                .display()
                .to_string(),
            "pid 777".to_owned(),
            "sudo kill 777".to_owned(),
        ] {
            assert!(
                error.contains(&expected),
                "the refusal must name {expected}: {error}"
            );
        }
        assert!(
            installer.signals().is_empty(),
            "an unowned process is never signalled: {error}"
        );
    }

    #[test]
    fn a_probe_that_cannot_name_the_holder_still_refuses_a_live_socket() {
        // Regression: `Absent`/`Unprovable` are not release decisions. The socket still answers,
        // so the install must fail closed with a manual command and no signal.
        for holder in [SocketHolder::Absent, SocketHolder::Unprovable] {
            let dir = temp_runtime();
            let installer = RecordingInstaller::default()
                .daemon(FakeDaemon::Immortal)
                .holding(holder);
            let clock = FakeClock::new();

            let error = release_daemon(&installer, &clock, dir.path())
                .expect_err("a held socket must be refused");

            assert_eq!(error.lines().count(), 1, "exactly one line: {error}");
            assert!(
                error.contains("sudo systemctl stop slipforkd.socket slipforkd.service"),
                "the refusal names the manual command: {error}"
            );
            assert!(
                installer.signals().is_empty(),
                "a holder that cannot be proven is never signalled: {error}"
            );
        }
    }

    #[test]
    fn a_real_listener_that_is_not_slipforkd_is_never_proven() {
        let dir = temp_runtime();
        let _listener = UnixListener::bind(DaemonControlServer::socket_path_in(dir.path()))
            .expect("bind the real listener");
        let pid = std::process::id();

        // The proof reads the kernel's `SO_PEERCRED` and the real `/proc/<pid>/exe` of this test
        // process, whose basename is the test binary — never `slipforkd`.
        assert!(
            matches!(
                probe_socket_holder(dir.path()).expect("probe the real listener"),
                SocketHolder::Foreign { pid: observed } if observed == pid
            ),
            "a real non-slipforkd peer is reported as foreign"
        );

        let installer = RecordingInstaller::default().daemon(FakeDaemon::Real);
        let clock = FakeClock::new();
        let error = release_daemon(&installer, &clock, dir.path())
            .expect_err("a real foreign peer must be refused");

        assert_eq!(error.lines().count(), 1, "exactly one line: {error}");
        assert!(
            error.contains(&format!("pid {pid}")) && error.contains(&format!("sudo kill {pid}")),
            "the refusal names the real peer and its manual command: {error}"
        );
        assert!(
            installer.signals().is_empty(),
            "a real non-slipforkd process is never signalled: {error}"
        );
    }
}
