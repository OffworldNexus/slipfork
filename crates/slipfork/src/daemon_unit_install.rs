//! `slipfork daemon install`: the single installer for `:80` system socket activation.
//!
//! `systemd` (PID 1) is the only component allowed to bind `127.0.0.1:80` and `[::1]:80` on a host
//! whose `net.ipv4.ip_unprivileged_port_start` is `1024`; neither an unprivileged process nor
//! `systemd --user` may do it (`docs/research/tunnel-weaver-and-systemd-facts.md` §5). This module
//! owns the pure install orchestration (`ADR-0011` §1): it renders the **exact bytes**
//! `slipfork daemon unit` prints through the `slipfork-edge` renderers, resolves the target
//! developer account, and drives a narrow [`UnitInstaller`] port. Every filesystem and
//! unit-management effect happens in the adapter (`OsUnitInstaller` in `main.rs`); there is no
//! stringly `run(program, args)` bag, no `--force`, and no user-unit fallback.
//!
//! Idempotency is byte equality: a file is rewritten only when its content differs, so a second
//! `daemon install` performs zero writes while still running `enable --now`.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use crate::apparmor::render_userns_profile;
use crate::layout::Layout;
use crate::user::{ResolvedAccount, UNPRIVILEGED_ACCOUNT_REMEDY, UserIdentity};
use slipfork_edge::{INGRESS_ADDRS, UnitSpec, render_service_unit, render_socket_unit};

/// `/etc/systemd/system/slipforkd.socket` — PID 1 binds the ingress/forward ports here.
pub const SOCKET_UNIT_PATH: &str = "/etc/systemd/system/slipforkd.socket";
/// `/etc/systemd/system/slipforkd.service` — the unprivileged daemon unit.
pub const SERVICE_UNIT_PATH: &str = "/etc/systemd/system/slipforkd.service";
/// `/etc/apparmor.d/slipfork` — the `userns,` profile (`ADR-0010` §7).
pub const APPARMOR_PROFILE_PATH: &str = "/etc/apparmor.d/slipfork";

/// Names the non-root remedy includes the resolved account (the `--user` value that will work
/// after `sudo`). The bare command is not enough on a host whose login account differs.
const NON_ROOT_REMEDY: &str = "daemon install writes /etc and enables system units; run it as root (sudo slipfork daemon install --user ";

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
    /// Shut down a live non-`:80` auto-spawned daemon and await `daemon.sock` release.
    ///
    /// Runs after the files are written and before `enable --now`, so
    /// `systemctl start slipforkd.service` never races the zero-setup daemon for the socket.
    fn release_running_daemon(&self, runtime_dir: &Path) -> Result<(), String>;
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
/// The function is pure over `identity` + `installer` + `request`: it reads no environment, opens
/// no file, and spawns no process itself. It fails closed **before** touching the installer when
/// it is not root or when the target account is unusable.
///
/// A failure halfway through leaves already-written files in place; because every write is
/// idempotent, re-running the same command converges (and `enable --now` is safe to repeat).
pub fn install(
    identity: &dyn UserIdentity,
    installer: &dyn UnitInstaller,
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
    // A zero-setup daemon holds `<state_dir>/run/daemon.sock`; release it before `enable --now`.
    installer
        .release_running_daemon(layout.runtime_dir())
        .map_err(converge_after_failure)?;
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
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        EnableNow(Vec<ManagedUnit>),
        ReleaseDaemon,
        RestartService,
    }

    /// Recording [`UnitInstaller`]: seeds `read_file` bytes and records every effect verb.
    #[derive(Default)]
    struct RecordingInstaller {
        files: Mutex<BTreeMap<PathBuf, Vec<u8>>>,
        effects: Mutex<Vec<Effect>>,
        reads: AtomicUsize,
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

        fn effects(&self) -> Vec<Effect> {
            self.effects.lock().expect("effects").clone()
        }

        fn writes(&self) -> Vec<Effect> {
            self.effects()
                .into_iter()
                .filter(|effect| matches!(effect, Effect::Write(..)))
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

        fn release_running_daemon(&self, _runtime_dir: &Path) -> Result<(), String> {
            self.effects
                .lock()
                .expect("effects")
                .push(Effect::ReleaseDaemon);
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
        let error = install(&FakeIdentity::default(), &installer, &request(false))
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
        let error = install(&FakeIdentity::default(), &installer, &request(true))
            .expect_err("direct root must fail closed");
        assert_eq!(error, UNPRIVILEGED_ACCOUNT_REMEDY);
        assert!(installer.effects().is_empty());

        let mut root_user = request(true);
        root_user.explicit_user = Some("root".to_owned());
        let error = install(&FakeIdentity::default(), &installer, &root_user)
            .expect_err("--user root must fail closed");
        assert_eq!(error, UNPRIVILEGED_ACCOUNT_REMEDY);
        assert!(installer.effects().is_empty());

        let mut unknown = request(true);
        unknown.explicit_user = Some("nobody-here".to_owned());
        let error = install(&FakeIdentity::default(), &installer, &unknown)
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
        let report = install(&identity, &installer, &request(true)).expect("install as dev");
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
        let report = install(&sudo_identity(), &installer, &request(true)).expect("install");

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
        let release = effects
            .iter()
            .position(|effect| matches!(effect, Effect::ReleaseDaemon))
            .expect("the handover runs once");
        let enable_position = effects
            .iter()
            .position(|effect| matches!(effect, Effect::EnableNow(_)))
            .expect("enable runs");
        assert!(
            release < enable_position,
            "the auto-spawned daemon is released before enable --now: {effects:?}"
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
        install(&sudo_identity(), &first, &request(true)).expect("first install");
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
        let report =
            install(&sudo_identity(), &installer, &request(true)).expect("idempotent re-run");
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
        let report =
            install(&sudo_identity(), &installer, &request(true)).expect("upgrade install");
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
        install(&sudo_identity(), &installer, &request).expect("install");
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
}
