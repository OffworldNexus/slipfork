//! Lab E2E for `slipfork daemon install` (`ADR-0011` §1, §3).
//!
//! Every install target here is the real `nobody` passwd row, never the runner's own account, so a
//! regression that ignored `--user`/`invoking_user` would render `User=<runner>` and fail. The
//! installer under test is the one concrete [`UnitInstaller`]: it writes real `0644` files into a
//! real disk directory under `~/.cache/slipfork`, and it drives real `systemctl`/`apparmor_parser`
//! subprocesses. On a host without a system manager those legs fail closed in one clear line and
//! the written files are still observable.
//!
//! The handover decision (`hold_outcome`) is pure over [`HolderFacts`]: a foreign PID namespace
//! peer, a typed daemon, and a `SIGTERM`-surviving holder are pinned with literal facts, while the
//! unit stop, the probe, and the pidfd signal path run against real files and a real listener.

/// The shared real `systemctl`/`apparmor_parser` scripts (one module, two suites).
#[path = "support/install_tool.rs"]
mod install_tool;

use install_tool::recording_tool;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use slipfork::daemon_unit_install::{
    DAEMON_HANDOVER, FileStatus, HandoverBudget, HoldOutcome, HolderFacts, InstallPaths,
    InstallReport, InstallRequest, PeerOwner, SocketHolder, UnitInstaller, hold_outcome,
    ingress_unit_spec, install_streamed, probe_socket_holder, release_daemon,
};
use slipfork::layout::Layout;
use slipfork::user::{ResolvedAccount, UNPRIVILEGED_ACCOUNT_REMEDY, UserIdentity, lookup_account};
use slipfork_edge::{DaemonControlServer, UnitSpec, render_service_unit};
use slipfork_engine::linux_cow::ProcessSignal;

/// The quiet install these assertions drive: the streamed installer with a no-op sink.
fn install(
    identity: &UserIdentity,
    installer: &UnitInstaller,
    budget: HandoverBudget,
    request: &InstallRequest,
) -> Result<InstallReport, String> {
    install_streamed(identity, installer, budget, request, &mut |_| {})
}

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

/// One real disk install root: `systemd/system`, `apparmor.d`, and two recording tool programs.
struct Fixture {
    root: PathBuf,
    installer: UnitInstaller,
    log: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let home = std::env::var("HOME").expect("HOME is required for the install test root");
        let root = PathBuf::from(home)
            .join(".cache")
            .join("slipfork")
            .join(format!("install-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let unit_dir = root.join("systemd/system");
        let apparmor_dir = root.join("apparmor.d");
        fs::create_dir_all(&unit_dir).expect("create the system unit dir");
        fs::create_dir_all(&apparmor_dir).expect("create the apparmor dir");
        let log = root.join("tools.log");
        let systemctl = recording_tool(&root, "systemctl", &log);
        let apparmor_parser = recording_tool(&root, "apparmor_parser", &log);
        Self {
            installer: UnitInstaller::new(unit_dir, apparmor_dir, systemctl, apparmor_parser),
            root,
            log,
        }
    }

    fn paths(&self) -> InstallPaths {
        self.installer.paths()
    }

    /// Every argv line the real tool programs recorded, in call order.
    fn tool_log(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn install(
        &self,
        request: &InstallRequest,
    ) -> Result<slipfork::daemon_unit_install::InstallReport, String> {
        install(&host_identity(), &self.installer, DAEMON_HANDOVER, request)
    }

    fn runtime(&self) -> PathBuf {
        let runtime = self.root.join("run");
        fs::create_dir_all(&runtime).expect("runtime dir");
        runtime
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A real executable that answers every verb and, for `show`, prints `loaded` (a unit the host
/// knows), so `systemctl stop <unit>` is actually driven.
fn loaded_tool(dir: &Path, name: &str, log: &Path) -> PathBuf {
    write_tool(
        dir,
        name,
        &format!(
            "printf '%s' \"$*\" >> {log}\necho >> {log}\nif [ \"$1\" = show ]; then echo loaded; fi\nexit 0\n",
            log = log.display()
        ),
    )
}

/// A real executable that logs its argv and fails only on `fail_arg`.
fn tool_failing_on(dir: &Path, name: &str, log: &Path, fail_arg: &str) -> PathBuf {
    write_tool(
        dir,
        name,
        &format!(
            "printf '%s' \"$*\" >> {log}\necho >> {log}\nif [ \"$1\" = show ]; then echo not-found; fi\nif [ \"$1\" = '{fail_arg}' ]; then printf 'boom\\nsecond\\n' >&2; exit 1; fi\nexit 0\n",
            log = log.display()
        ),
    )
}

fn write_tool(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}")).expect("write the tool script");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
        .expect("make the tool executable");
    path
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

fn read(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("{} must be written: {err}", path.display()))
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path)
        .expect("written file metadata")
        .permissions()
        .mode()
        & 0o777
}

fn zero_budget() -> HandoverBudget {
    HandoverBudget {
        grace: Duration::ZERO,
        poll: Duration::ZERO,
    }
}

// ---------------------------------------------------------------------------------------------
// Passwd identity, root gate, and real file bytes
// ---------------------------------------------------------------------------------------------

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
fn non_root_fails_closed_before_writing_anything() {
    let fixture = Fixture::new("non-root");
    let error = fixture
        .install(&request(false))
        .expect_err("non-root must fail closed");
    assert_eq!(
        error,
        format!(
            "daemon install writes /etc and enables system units; run it as root \
             (sudo slipfork daemon install --user {})",
            host_identity().account
        )
    );
    assert_eq!(fixture.tool_log(), Vec::<String>::new(), "zero tool calls");
    assert!(
        fs::read_dir(fixture.root.join("systemd/system"))
            .expect("unit dir")
            .next()
            .is_none(),
        "zero unit files"
    );
}

#[test]
fn root_without_a_valid_sudo_user_fails_closed() {
    let fixture = Fixture::new("root-gate");
    let error = fixture
        .install(&request(true))
        .expect_err("direct root must fail closed");
    assert_eq!(error, UNPRIVILEGED_ACCOUNT_REMEDY);

    let mut root_user = request(true);
    root_user.explicit_user = Some("root".to_owned());
    let error = fixture
        .install(&root_user)
        .expect_err("--user root must fail closed");
    assert_eq!(error, UNPRIVILEGED_ACCOUNT_REMEDY);

    let mut unknown = request(true);
    unknown.explicit_user = Some("slipfork-no-such-account".to_owned());
    let error = fixture
        .install(&unknown)
        .expect_err("an unknown --user must fail closed");
    assert!(error.contains("no OS account"), "{error}");
    assert_eq!(fixture.tool_log(), Vec::<String>::new(), "zero tool calls");
}

#[test]
fn the_sudo_invoking_account_becomes_the_installed_account() {
    let fixture = Fixture::new("account");
    let target = target_account_row();
    let report = fixture
        .install(&nobody_request())
        .expect("install as the invoking account");
    assert_eq!(report.user, "nobody");

    let service = read(&fixture.paths().service);
    assert!(service.contains("\nUser=nobody\n"), "{service}");
    let state_dir = target.home.join(".cache/slipfork");
    assert!(
        service.contains(&format!("--state-dir {}", state_dir.display())),
        "the unit must derive the target account's state root: {service}"
    );
}

#[test]
fn first_run_writes_three_real_files_and_enables_once() {
    let fixture = Fixture::new("first-run");
    let report = fixture.install(&nobody_request()).expect("install");
    let paths = fixture.paths();

    assert_eq!(report.socket, FileStatus::Wrote);
    assert_eq!(report.service, FileStatus::Wrote);
    assert_eq!(report.apparmor, FileStatus::Wrote);
    assert_eq!(report.user, "nobody");
    assert_eq!(report.paths, paths);

    for path in [&paths.socket, &paths.service, &paths.apparmor] {
        assert!(path.exists(), "{} must exist on disk", path.display());
        assert_eq!(
            mode(path),
            0o644,
            "{} must be systemd-readable",
            path.display()
        );
    }
    let socket = read(&paths.socket);
    assert!(socket.contains("ListenStream=127.0.0.1:80\n"), "{socket}");
    assert!(socket.contains("ListenStream=127.0.0.1:7080\n"), "{socket}");
    let service = read(&paths.service);
    assert!(
        service.contains("ExecStart=/usr/local/bin/slipforkd"),
        "{service}"
    );
    assert!(service.contains("\nUser=nobody\n"), "{service}");
    assert!(service.contains("WantedBy=multi-user.target"), "{service}");
    assert!(read(&paths.apparmor).contains("slipfork"));

    let log = fixture.tool_log();
    let enables = log
        .iter()
        .filter(|line| line.starts_with("enable --now"))
        .count();
    assert_eq!(enables, 1, "enable --now runs exactly once: {log:?}");
    assert!(
        log.iter()
            .any(|line| line == "enable --now slipforkd.socket slipforkd.service"),
        "the handover enables exactly the units this installer writes: {log:?}"
    );
    assert!(
        log.iter().any(|line| line == "daemon-reload"),
        "daemon-reload runs: {log:?}"
    );
    assert!(
        log.iter().any(|line| line.starts_with("-r ")),
        "the AppArmor profile is loaded through apparmor_parser: {log:?}"
    );
    let shows = log
        .iter()
        .position(|line| line.starts_with("show --property=LoadState"))
        .expect("the handover stops the units it owns");
    let enable = log
        .iter()
        .position(|line| line.starts_with("enable --now"))
        .expect("enable runs");
    assert!(
        shows < enable,
        "the units are stopped before enable --now: {log:?}"
    );
    assert!(
        log.iter()
            .all(|line| !line.starts_with("stop ") && line != "restart slipforkd.service"),
        "a fresh install stops nothing and restarts nothing: {log:?}"
    );
}

#[test]
fn loaded_units_are_stopped_before_enable_now() {
    let fixture = Fixture::new("loaded-units");
    let installer = UnitInstaller::new(
        fixture
            .paths()
            .socket
            .parent()
            .expect("unit dir")
            .to_path_buf(),
        fixture
            .paths()
            .apparmor
            .parent()
            .expect("apparmor dir")
            .to_path_buf(),
        loaded_tool(&fixture.root, "systemctl", &fixture.log),
        recording_tool(&fixture.root, "apparmor_parser", &fixture.log),
    );
    install(
        &host_identity(),
        &installer,
        DAEMON_HANDOVER,
        &nobody_request(),
    )
    .expect("install over loaded units");

    let log = fixture.tool_log();
    let stop_socket = log
        .iter()
        .position(|line| line == "stop slipforkd.socket")
        .expect("a loaded socket unit is stopped");
    let stop_service = log
        .iter()
        .position(|line| line == "stop slipforkd.service")
        .expect("a loaded service unit is stopped");
    let enable = log
        .iter()
        .position(|line| line.starts_with("enable --now"))
        .expect("enable runs");
    assert!(
        stop_socket < enable && stop_service < enable,
        "both units are stopped before enable --now: {log:?}"
    );
}

#[test]
fn identical_bytes_are_not_rewritten_and_enable_still_runs() {
    let fixture = Fixture::new("idempotent");
    fixture.install(&nobody_request()).expect("first install");
    let paths = fixture.paths();
    let inodes: Vec<u64> = [&paths.socket, &paths.service, &paths.apparmor]
        .iter()
        .map(|path| fs::metadata(path).expect("written").ino())
        .collect();

    let report = fixture
        .install(&nobody_request())
        .expect("idempotent re-run");
    assert_eq!(report.socket, FileStatus::Unchanged);
    assert_eq!(report.service, FileStatus::Unchanged);
    assert_eq!(report.apparmor, FileStatus::Unchanged);
    let unchanged_inodes: Vec<u64> = [&paths.socket, &paths.service, &paths.apparmor]
        .iter()
        .map(|path| fs::metadata(path).expect("written").ino())
        .collect();
    assert_eq!(inodes, unchanged_inodes, "a re-run performs zero rewrites");

    let log = fixture.tool_log();
    assert_eq!(
        log.iter()
            .filter(|line| line.starts_with("enable --now"))
            .count(),
        2,
        "enable --now still runs once per invocation: {log:?}"
    );
    assert!(
        log.iter().all(|line| line != "restart slipforkd.service"),
        "unchanged bytes never bounce the daemon: {log:?}"
    );
    assert_eq!(
        report.render_text(),
        format!(
            "unchanged slipforkd.socket, slipforkd.service, {}; enabled\n",
            paths.apparmor.display()
        )
    );
}

#[test]
fn a_changed_service_unit_restarts_the_already_installed_service() {
    let fixture = Fixture::new("upgrade");
    let stale_service = render_service_unit(&stale_spec()).expect("a real stale service unit");
    fs::write(fixture.paths().service, stale_service).expect("seed the stale service unit");

    let report = fixture.install(&nobody_request()).expect("upgrade install");
    assert_eq!(report.service, FileStatus::Wrote);
    assert!(
        fixture
            .tool_log()
            .iter()
            .any(|line| line == "restart slipforkd.service"),
        "an already-installed service must restart to pick up new bytes: {:?}",
        fixture.tool_log()
    );
}

#[test]
fn the_invoking_clients_xdg_cache_home_pins_the_installed_state_dir() {
    let fixture = Fixture::new("xdg-cache");
    let mut request = nobody_request();
    request.xdg_cache_home = Some(PathBuf::from("/xdg/cache"));
    fixture.install(&request).expect("install");
    let service = read(&fixture.paths().service);
    assert!(
        service.contains("--state-dir /xdg/cache/slipfork --runtime-dir /xdg/cache/slipfork/run"),
        "the unit must derive the same state/runtime roots as the client: {service}"
    );
}

#[test]
fn every_effect_failure_says_a_re_run_converges() {
    // One failing verb at a time: the tool programs are real and the failure is a real exit.
    for (label, verb, stale_service) in [
        ("reload", "daemon-reload", false),
        ("apparmor", "-r", false),
        ("enable", "enable", false),
        ("restart", "restart", true),
    ] {
        let fixture = Fixture::new(&format!("converge-{label}"));
        if stale_service {
            fs::write(
                fixture.paths().service,
                render_service_unit(&stale_spec()).expect("stale unit"),
            )
            .expect("seed the stale service unit");
        }
        let systemctl = tool_failing_on(&fixture.root, "systemctl", &fixture.log, verb);
        let apparmor_parser = if verb == "-r" {
            tool_failing_on(&fixture.root, "apparmor_parser", &fixture.log, "-r")
        } else {
            recording_tool(&fixture.root, "apparmor_parser", &fixture.log)
        };
        let installer = UnitInstaller::new(
            fixture
                .paths()
                .socket
                .parent()
                .expect("unit dir")
                .to_path_buf(),
            fixture
                .paths()
                .apparmor
                .parent()
                .expect("apparmor dir")
                .to_path_buf(),
            systemctl,
            apparmor_parser,
        );
        let error = install(
            &host_identity(),
            &installer,
            DAEMON_HANDOVER,
            &nobody_request(),
        )
        .expect_err(&format!("a failed `{verb}` fails the install"));
        assert_eq!(
            error.lines().count(),
            1,
            "exactly one line for {verb}: {error}"
        );
        assert!(
            error.contains("boom") && error.ends_with("— re-run to converge"),
            "the {verb} failure names its first stderr line and the converging re-run: {error}"
        );
    }
}

#[test]
fn a_real_systemctl_that_cannot_serve_fails_closed_in_one_line() {
    let fixture = Fixture::new("live-manager");
    let paths = fixture.paths();
    let installer = UnitInstaller::new(
        paths.socket.parent().expect("unit dir").to_path_buf(),
        paths.apparmor.parent().expect("apparmor dir").to_path_buf(),
        PathBuf::from("systemctl"),
        PathBuf::from("apparmor_parser"),
    );
    let outcome = install(
        &host_identity(),
        &installer,
        DAEMON_HANDOVER,
        &nobody_request(),
    );
    assert!(
        fs::metadata(&paths.socket).is_ok(),
        "the rendered unit is written before the manager is asked to load it"
    );
    match outcome {
        Ok(_) => {}
        Err(error) => {
            eprintln!("live systemctl leg: {error}");
            assert_eq!(error.lines().count(), 1, "exactly one line: {error}");
            assert!(
                error.contains("cannot run `") || error.contains("exited with "),
                "the refusal says whether the manager is missing or failed: {error}"
            );
        }
    }
}

#[test]
fn the_first_run_transcript_matches_the_design() {
    assert_eq!(FileStatus::Wrote.as_str(), "wrote");
    assert_eq!(FileStatus::Unchanged.as_str(), "unchanged");
    let fixture = Fixture::new("transcript");
    let report = fixture.install(&nobody_request()).expect("install");
    let paths = fixture.paths();
    assert_eq!(
        report.render_text(),
        format!(
            "wrote     {}\n\
             wrote     {}\n\
             wrote     {}\n\
             enabled   slipforkd.socket + slipforkd.service as nobody, ingress 127.0.0.1:80 + [::1]:80\n\
             next      slipfork up\n",
            paths.socket.display(),
            paths.service.display(),
            paths.apparmor.display()
        )
    );
}

// ---------------------------------------------------------------------------------------------
// Handover: real files/units and the one pure refusal/terminate decision
// ---------------------------------------------------------------------------------------------

fn facts(socket: &Path, pid: Option<u32>, owner: PeerOwner) -> HolderFacts {
    HolderFacts {
        socket: socket.to_path_buf(),
        pid,
        owner,
        answered_shutdown: false,
        released: false,
        signals_sent: 0,
    }
}

#[test]
fn a_runtime_dir_without_a_holder_releases_without_a_signal() {
    let fixture = Fixture::new("handover-empty");
    let runtime = fixture.runtime();

    release_daemon(&fixture.installer, zero_budget(), &runtime)
        .expect("no holder releases nothing");

    let log = fixture.tool_log();
    assert_eq!(
        log.iter()
            .filter(|line| line.starts_with("show --property=LoadState"))
            .count(),
        2,
        "the handover probes both units it owns: {log:?}"
    );
    assert!(
        log.iter().all(|line| !line.starts_with("stop ")),
        "unwritten units are skipped, never stopped: {log:?}"
    );
}

#[test]
fn an_unowned_foreign_holder_is_refused_in_one_line_without_a_signal() {
    let fixture = Fixture::new("handover-unowned");
    let socket = DaemonControlServer::socket_path_in(&fixture.root.join("run"));
    let HoldOutcome::Refuse(line) =
        hold_outcome(&facts(&socket, Some(777), PeerOwner::Foreign { pid: 777 }))
    else {
        panic!("an unowned holder must be refused");
    };
    assert_eq!(line.lines().count(), 1, "exactly one line: {line}");
    for expected in [
        socket.display().to_string(),
        "pid 777".to_owned(),
        "sudo kill 777".to_owned(),
    ] {
        assert!(
            line.contains(&expected),
            "the refusal must name {expected}: {line}"
        );
    }
}

#[test]
fn a_probe_that_cannot_name_the_holder_still_refuses_a_live_socket() {
    // Regression: `Absent`/`Unprovable` are not release decisions. The socket still answers, so the
    // install fails closed with a manual command and no signal.
    let fixture = Fixture::new("handover-unprovable");
    let socket = DaemonControlServer::socket_path_in(&fixture.root.join("run"));
    let mut unresolved = facts(&socket, None, PeerOwner::Unprovable);
    let HoldOutcome::Refuse(line) = hold_outcome(&unresolved) else {
        panic!("a held socket must be refused");
    };
    assert_eq!(line.lines().count(), 1, "exactly one line: {line}");
    assert!(
        line.contains("sudo systemctl stop slipforkd.socket slipforkd.service"),
        "the refusal names the manual command: {line}"
    );

    // A probe that came back `Absent` while the typed daemon still answered names the same command.
    unresolved.answered_shutdown = true;
    let HoldOutcome::Refuse(line) = hold_outcome(&unresolved) else {
        panic!("an answering socket must be refused");
    };
    assert!(
        line.contains("could not name its holder")
            && line.contains("sudo systemctl stop slipforkd.socket slipforkd.service"),
        "the refusal names why it could not prove the holder: {line}"
    );
}

#[test]
fn a_holder_that_answers_shutdown_is_released_without_a_signal() {
    let fixture = Fixture::new("handover-typed");
    let socket = DaemonControlServer::socket_path_in(&fixture.root.join("run"));
    let mut answered = facts(&socket, Some(4242), PeerOwner::Daemon { pid: 4242 });
    answered.answered_shutdown = true;
    answered.released = true;
    assert_eq!(
        hold_outcome(&answered),
        HoldOutcome::Release,
        "a daemon that answers `shutdown` is never signalled"
    );
}

#[test]
fn a_foreign_holder_owned_by_slipforkd_is_terminated_after_the_grace() {
    let fixture = Fixture::new("handover-proven");
    let socket = DaemonControlServer::socket_path_in(&fixture.root.join("run"));
    let mut proven = facts(&socket, Some(1), PeerOwner::Daemon { pid: 1 });
    assert_eq!(
        hold_outcome(&proven),
        HoldOutcome::Terminate(ProcessSignal::Terminate),
        "SIGTERM first"
    );
    proven.signals_sent = 1;
    assert_eq!(
        hold_outcome(&proven),
        HoldOutcome::Terminate(ProcessSignal::Kill),
        "SIGKILL only after the grace elapsed"
    );
    proven.signals_sent = 2;
    assert!(
        matches!(hold_outcome(&proven), HoldOutcome::Refuse(line) if line.contains("after `SIGTERM` and `SIGKILL`")),
        "a holder that survives both signals is refused in one line"
    );
}

#[test]
fn a_real_listener_that_is_not_slipforkd_is_never_proven() {
    let fixture = Fixture::new("handover-foreign");
    let runtime = fixture.runtime();
    let _listener = UnixListener::bind(DaemonControlServer::socket_path_in(&runtime))
        .expect("bind the real listener");
    let pid = std::process::id();

    // The proof reads the kernel's `SO_PEERCRED` and the real `/proc/<pid>/exe` of this test
    // process, whose basename is the test binary — never `slipforkd`.
    assert!(
        matches!(
            probe_socket_holder(&runtime).expect("probe the real listener"),
            SocketHolder::Foreign { pid: observed } if observed == pid
        ),
        "a real non-slipforkd peer is reported as foreign"
    );

    // The same real peer decided by the pure handover: one line naming the pid and its manual
    // command, and zero signals (a `Refuse`, never a `Terminate`).
    let socket = DaemonControlServer::socket_path_in(&runtime);
    let HoldOutcome::Refuse(line) =
        hold_outcome(&facts(&socket, Some(pid), PeerOwner::Foreign { pid }))
    else {
        panic!("a real foreign peer must be refused");
    };
    assert_eq!(line.lines().count(), 1, "exactly one line: {line}");
    assert!(
        line.contains(&format!("pid {pid}")) && line.contains(&format!("sudo kill {pid}")),
        "the refusal names the real peer and its manual command: {line}"
    );
}
