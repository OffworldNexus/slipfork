//! Lab E2E for `slipfork daemon install` (`ADR-0011` §1, §3).
//!
//! Every install target here is the real `nobody` passwd row, never the runner's own account, so a
//! regression that ignored `--user`/`invoking_user` would render `User=<runner>` and fail. The
//! socket handover runs against a real private runtime dir (and one real `UnixListener`); only the
//! adapter's answers are scripted, which is exactly what the [`UnitInstaller`] port exists for.

use std::collections::BTreeMap;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use slipfork::daemon_unit_install::{
    APPARMOR_PROFILE_PATH, DAEMON_HANDOVER, FileStatus, HandoverBudget, InstallReport,
    InstallRequest, ManagedUnit, SERVICE_UNIT_PATH, SOCKET_UNIT_PATH, SocketHolder, UnitInstaller,
    ingress_unit_spec, install, probe_socket_holder, release_daemon,
};
use slipfork::layout::Layout;
use slipfork::user::{ResolvedAccount, UNPRIVILEGED_ACCOUNT_REMEDY, UserIdentity, lookup_account};
use slipfork_edge::{DaemonControlServer, UnitSpec, render_service_unit};
use slipfork_engine::linux_cow::{ProcessSignal, open_pidfd};

/// The real, non-runner passwd row every install path must resolve.
fn target_account_row() -> ResolvedAccount {
    lookup_account("nobody").unwrap_or_else(|err| {
        panic!(
            "this suite needs the real `nobody` passwd row to prove the install honours \
             --user/invoking_user; add it to the host account database and rerun ({err})"
        )
    })
}

/// The test runner's real OS identity (real uid, gid, account, and home).
fn host_identity() -> UserIdentity {
    UserIdentity::resolve().expect("the test runner's real OS identity")
}

/// The kernel's own `id` answer, so an assertion never restates the value it checks.
fn real_id(flag: &str) -> u32 {
    let output = Command::new("id")
        .args(flag.split(' '))
        .output()
        .expect("run `id`");
    String::from_utf8(output.stdout)
        .expect("utf8 id output")
        .trim()
        .parse()
        .expect("a numeric id")
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
/// The socket verbs are scripted (`daemon`, `holder`), so no test opens a real daemon or signals a
/// real process; the one exception is [`FakeDaemon::Real`], which runs the real ownership proof
/// against a real listener.
struct RecordingInstaller {
    files: Mutex<BTreeMap<PathBuf, Vec<u8>>>,
    effects: Mutex<Vec<Effect>>,
    reads: AtomicUsize,
    daemon: Mutex<FakeDaemon>,
    shutdown_called: AtomicBool,
    killed: AtomicBool,
    reload_fails: AtomicBool,
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
            reload_fails: AtomicBool::new(false),
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

    /// Make `systemctl daemon-reload` fail with one line, to prove every effect failure converges.
    fn failing_daemon_reload(self) -> Self {
        self.reload_fails.store(true, Ordering::SeqCst);
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
        if self.reload_fails.load(Ordering::SeqCst) {
            return Err("boom".to_owned());
        }
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
            // A foreign build answers something this client cannot parse, and the real listener in
            // the `Real` scenario never speaks the contract at all.
            _ => Err("daemon control protocol error: this reply is not this build's".to_owned()),
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

    fn signal_proven(
        &self,
        _pidfd: &std::os::fd::OwnedFd,
        signal: ProcessSignal,
    ) -> Result<(), String> {
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

fn request(is_root: bool) -> InstallRequest {
    InstallRequest {
        is_root,
        explicit_user: None,
        invoking_user: None,
        executable: PathBuf::from("/usr/local/bin/slipforkd"),
        bin_dirs: vec![PathBuf::from("/usr/local/bin")],
        forward_port: 7080,
        state_dir: None,
        xdg_cache_home: None,
        xdg_data_home: None,
    }
}

/// A request whose resolved target is the real `nobody` row, never the runner's own account.
fn nobody_request() -> InstallRequest {
    let mut request = request(true);
    request.invoking_user = Some("nobody".to_owned());
    request
}

/// One written file's UTF-8 contents (a path that was never written panics with its name).
fn written(installer: &RecordingInstaller, path: &str) -> String {
    installer
        .writes()
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Write(written, bytes) if written == Path::new(path) => {
                Some(String::from_utf8(bytes).expect("utf8 unit"))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("{path} must be written"))
}

/// A real rendered service spec for the runner's own account: always different bytes from a
/// `nobody` install, so it seeds a stale unit without restating the install's expected output.
fn stale_spec() -> UnitSpec {
    let identity = host_identity();
    let account = ResolvedAccount {
        name: identity.account,
        uid: identity.uid,
        gid: identity.gid,
        home: identity.home,
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
fn the_install_target_is_a_real_distinct_passwd_account() {
    let identity = host_identity();
    let target = target_account_row();
    assert_eq!(target.name, "nobody");
    assert_eq!(target.uid, real_id("-u nobody"));
    assert_eq!(target.gid, real_id("-g nobody"));
    assert_ne!(target.uid, identity.uid, "the target is never the runner");
    assert!(target.home.is_absolute());
}

#[test]
fn non_root_fails_closed_before_touching_the_installer() {
    let identity = host_identity();
    let installer = RecordingInstaller::default();
    let error = install(&identity, &installer, DAEMON_HANDOVER, &request(false))
        .expect_err("non-root must fail closed");
    assert_eq!(
        error,
        format!(
            "daemon install writes /etc and enables system units; run it as root \
             (sudo slipfork daemon install --user {})",
            identity.account
        )
    );
    assert_eq!(installer.reads.load(Ordering::SeqCst), 0, "zero reads");
    assert!(
        installer.effects().is_empty(),
        "zero writes and zero unit calls"
    );
}

#[test]
fn root_without_a_valid_sudo_user_fails_closed() {
    let identity = host_identity();
    let installer = RecordingInstaller::default();
    let error = install(&identity, &installer, DAEMON_HANDOVER, &request(true))
        .expect_err("direct root must fail closed");
    assert_eq!(error, UNPRIVILEGED_ACCOUNT_REMEDY);
    assert!(installer.effects().is_empty());

    let mut root_user = request(true);
    root_user.explicit_user = Some("root".to_owned());
    let error = install(&identity, &installer, DAEMON_HANDOVER, &root_user)
        .expect_err("--user root must fail closed");
    assert_eq!(error, UNPRIVILEGED_ACCOUNT_REMEDY);
    assert!(installer.effects().is_empty());

    let mut unknown = request(true);
    unknown.explicit_user = Some("slipfork-no-such-account".to_owned());
    let error = install(&identity, &installer, DAEMON_HANDOVER, &unknown)
        .expect_err("an unknown --user must fail closed");
    assert!(error.contains("no OS account"), "{error}");
    assert!(installer.effects().is_empty());
}

#[test]
fn the_sudo_invoking_account_becomes_the_installed_account() {
    let identity = host_identity();
    let target = target_account_row();
    let installer = RecordingInstaller::default();
    let report = install(&identity, &installer, DAEMON_HANDOVER, &nobody_request())
        .expect("install as the invoking account");
    assert_eq!(report.user, "nobody");

    let service = written(&installer, SERVICE_UNIT_PATH);
    assert!(service.contains("\nUser=nobody\n"), "{service}");
    // The unit's roots derive from the target row's home, not the runner's.
    let state_dir = target.home.join(".cache/slipfork");
    assert!(
        service.contains(&format!("--state-dir {}", state_dir.display())),
        "the unit must derive the target account's state root: {service}"
    );
}

#[test]
fn first_run_writes_the_three_files_and_enables_once() {
    let installer = RecordingInstaller::default();
    let report = install(
        &host_identity(),
        &installer,
        DAEMON_HANDOVER,
        &nobody_request(),
    )
    .expect("install");

    assert_eq!(report.socket, FileStatus::Wrote);
    assert_eq!(report.service, FileStatus::Wrote);
    assert_eq!(report.apparmor, FileStatus::Wrote);
    assert_eq!(report.user, "nobody");

    let writes = installer.writes();
    assert_eq!(writes.len(), 3, "one write per installed file: {writes:?}");
    // Literal anchors; the byte-for-byte `slipfork daemon unit` cross-check lives in
    // `tests/critical_cli.rs`.
    let socket = written(&installer, SOCKET_UNIT_PATH);
    assert!(socket.contains("ListenStream=127.0.0.1:80\n"), "{socket}");
    assert!(socket.contains("ListenStream=127.0.0.1:7080\n"), "{socket}");
    let service = written(&installer, SERVICE_UNIT_PATH);
    assert!(
        service.contains("ExecStart=/usr/local/bin/slipforkd"),
        "{service}"
    );
    assert!(service.contains("\nUser=nobody\n"), "{service}");
    assert!(service.contains("WantedBy=multi-user.target"), "{service}");
    let apparmor = written(&installer, APPARMOR_PROFILE_PATH);
    assert!(apparmor.contains("slipfork"), "{apparmor}");

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
    install(&host_identity(), &first, DAEMON_HANDOVER, &nobody_request()).expect("first install");
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
    let report = install(
        &host_identity(),
        &installer,
        DAEMON_HANDOVER,
        &nobody_request(),
    )
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
    let stale_service = render_service_unit(&stale_spec()).expect("a real stale service unit");
    let installer =
        RecordingInstaller::seeded(&[(PathBuf::from(SERVICE_UNIT_PATH), stale_service)]);
    let report = install(
        &host_identity(),
        &installer,
        DAEMON_HANDOVER,
        &nobody_request(),
    )
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
    let mut request = nobody_request();
    request.xdg_cache_home = Some(PathBuf::from("/xdg/cache"));
    install(&host_identity(), &installer, DAEMON_HANDOVER, &request).expect("install");
    let service = written(&installer, SERVICE_UNIT_PATH);
    assert!(
        service.contains("--state-dir /xdg/cache/slipfork --runtime-dir /xdg/cache/slipfork/run"),
        "the unit must derive the same state/runtime roots as the client: {service}"
    );
}

#[test]
fn every_effect_failure_says_a_re_run_converges() {
    let installer = RecordingInstaller::default().failing_daemon_reload();
    let error = install(
        &host_identity(),
        &installer,
        DAEMON_HANDOVER,
        &nobody_request(),
    )
    .expect_err("a failed daemon-reload fails the install");
    assert_eq!(error, "boom — re-run to converge");
}

#[test]
fn the_first_run_transcript_matches_the_design() {
    assert_eq!(FileStatus::Wrote.as_str(), "wrote");
    assert_eq!(FileStatus::Unchanged.as_str(), "unchanged");
    let report = InstallReport {
        user: "nobody".to_owned(),
        socket: FileStatus::Wrote,
        service: FileStatus::Wrote,
        apparmor: FileStatus::Wrote,
    };
    assert_eq!(
        report.render_text(),
        "wrote     /etc/systemd/system/slipforkd.socket\n\
         wrote     /etc/systemd/system/slipforkd.service\n\
         wrote     /etc/apparmor.d/slipfork\n\
         enabled   slipforkd.socket + slipforkd.service as nobody, ingress 127.0.0.1:80 + [::1]:80\n\
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
    let budget = HandoverBudget {
        grace: Duration::ZERO,
        poll: Duration::ZERO,
    };

    release_daemon(&installer, budget, dir.path()).expect("no holder releases nothing");

    assert_eq!(
        installer.stop_units_calls(),
        vec![vec![ManagedUnit::Socket, ManagedUnit::Service]]
    );
    assert!(
        installer.signals().is_empty(),
        "no holder means no signal: {:?}",
        installer.effects()
    );
}

#[test]
fn a_holder_that_answers_shutdown_is_released_without_a_signal() {
    let dir = temp_runtime();
    let installer = RecordingInstaller::default().daemon(FakeDaemon::Typed);
    let budget = HandoverBudget {
        grace: Duration::ZERO,
        poll: Duration::ZERO,
    };

    release_daemon(&installer, budget, dir.path())
        .expect("a daemon that speaks the typed contract shuts itself down");

    assert!(
        installer.signals().is_empty(),
        "a daemon that answers `shutdown` is never signalled: {:?}",
        installer.signals()
    );
}

#[test]
fn a_foreign_holder_owned_by_slipforkd_is_terminated_after_the_grace() {
    let dir = temp_runtime();
    let installer = RecordingInstaller::default()
        .daemon(FakeDaemon::ForeignUntilKill)
        .holding(proven_holder(std::process::id()));
    let budget = HandoverBudget {
        grace: Duration::ZERO,
        poll: Duration::ZERO,
    };

    release_daemon(&installer, budget, dir.path()).expect("a proven slipforkd holder converges");

    assert_eq!(
        installer.signals(),
        vec![ProcessSignal::Terminate, ProcessSignal::Kill],
        "SIGTERM first, SIGKILL only after the grace elapsed"
    );
}

#[test]
fn an_unowned_holder_is_refused_in_one_line_without_a_signal() {
    let dir = temp_runtime();
    let installer = RecordingInstaller::default()
        .daemon(FakeDaemon::Immortal)
        .holding(SocketHolder::Foreign { pid: 777 });
    let budget = HandoverBudget {
        grace: Duration::ZERO,
        poll: Duration::ZERO,
    };

    let error = release_daemon(&installer, budget, dir.path())
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
    // Regression: `Absent`/`Unprovable` are not release decisions. The socket still answers, so the
    // install must fail closed with a manual command and no signal.
    for holder in [SocketHolder::Absent, SocketHolder::Unprovable] {
        let dir = temp_runtime();
        let installer = RecordingInstaller::default()
            .daemon(FakeDaemon::Immortal)
            .holding(holder);
        let budget = HandoverBudget {
            grace: Duration::ZERO,
            poll: Duration::ZERO,
        };

        let error = release_daemon(&installer, budget, dir.path())
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
    let budget = HandoverBudget {
        grace: Duration::ZERO,
        poll: Duration::ZERO,
    };
    let error = release_daemon(&installer, budget, dir.path())
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
