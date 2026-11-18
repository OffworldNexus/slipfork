//! `slipfork daemon install`: the single installer for `:80` system socket activation.
//!
//! `systemd` (PID 1) is the only component allowed to bind `127.0.0.1:80` and `[::1]:80` on a host
//! whose `net.ipv4.ip_unprivileged_port_start` is `1024`; neither an unprivileged process nor
//! `systemd --user` may do it (`docs/research/tunnel-weaver-and-systemd-facts.md` §5). This module
//! owns the install orchestration (`ADR-0011` §1): it renders the **exact bytes**
//! `slipfork daemon unit` prints through the `slipfork-edge` renderers, resolves the target
//! developer account, and drives one concrete [`UnitInstaller`] over the real host filesystem and
//! `systemctl`/`apparmor_parser`. The composition root constructs it with the real unit directory
//! (`/etc/systemd/system`) and AppArmor directory (`/etc/apparmor.d`); tests construct the same
//! struct over a real disk directory. There is no installer trait, no scripted effect recorder, no
//! stringly `run(program, args)` bag, no `--force`, and no user-unit fallback.
//!
//! Idempotency is byte equality: a file is rewritten only when its content differs, so a second
//! `daemon install` performs zero writes while still running `enable --now`.
//!
//! The socket handover ([`release_daemon`]) is **version-agnostic** and runs one order: stop the
//! units this module writes, ask the daemon to shut itself down and await it, then — if anything
//! still answers — [`probe_socket_holder`] must prove the holder is a `slipforkd` from
//! `SO_PEERCRED` plus the `/proc/<pid>/exe` basename, **pin it with a pidfd**, and signal only
//! through that descriptor, so `SIGKILL` can never reach a recycled pid. The probe and the shutdown
//! both go through the installer, so there is one socket path, one adapter, and no second client
//! racing it. An unowned holder is refused in one line and never signalled.

#![forbid(unsafe_code)]

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::NamedTempFile;

use crate::apparmor::render_userns_profile;
use crate::layout::Layout;
use crate::user::{ResolvedAccount, UserIdentity};
use slipfork_edge::{INGRESS_ADDRS, UnitSpec, render_service_unit, render_socket_unit};

pub use crate::daemon_handover::{
    DAEMON_GRACE, DAEMON_HANDOVER, DAEMON_POLL, HandoverBudget, HoldOutcome, HolderFacts,
    ManagedUnit, PeerOwner, SocketHolder, hold_outcome, probe_socket_holder, release_daemon,
};

/// `/etc/systemd/system` — PID 1 reads the two unit files here (`ADR-0011` §1).
pub const SYSTEM_UNIT_DIR: &str = "/etc/systemd/system";
/// `/etc/apparmor.d` — the `userns,` profile directory (`ADR-0010` §7).
pub const APPARMOR_DIR: &str = "/etc/apparmor.d";
/// The system manager every install verb speaks to.
pub const SYSTEMCTL: &str = "systemctl";
/// The AppArmor profile loader.
pub const APPARMOR_PARSER: &str = "apparmor_parser";
/// Fixed executable search path for every install verb (nothing is inherited).
const INSTALL_TOOL_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// The AppArmor profile file name inside [`APPARMOR_DIR`].
const APPARMOR_PROFILE_NAME: &str = "slipfork";

/// Names the non-root remedy includes the resolved account (the `--user` value that will work
/// after `sudo`). The bare command is not enough on a host whose login account differs.
const NON_ROOT_REMEDY: &str = "daemon install writes /etc and enables system units; run it as root (sudo slipfork daemon install --user ";

/// The one non-root refusal line, produced before any account-database read.
pub fn non_root_remedy(account: &str) -> String {
    format!("{NON_ROOT_REMEDY}{account})")
}

/// The three absolute file paths one install writes, derived from the installer's directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPaths {
    pub socket: PathBuf,
    pub service: PathBuf,
    pub apparmor: PathBuf,
}

/// The one installer for `slipfork daemon install`: real files, real unit manager.
///
/// The composition root constructs it with [`SYSTEM_UNIT_DIR`] + [`APPARMOR_DIR`] + [`SYSTEMCTL`] +
/// [`APPARMOR_PARSER`]; a lab test constructs the same struct over a real disk directory and real
/// executable scripts named `systemctl`/`apparmor_parser` (a host without a system manager still
/// runs the real subprocesses and records their argv on disk). Nothing here reads the environment.
#[derive(Debug, Clone)]
pub struct UnitInstaller {
    unit_dir: PathBuf,
    apparmor_dir: PathBuf,
    systemctl: PathBuf,
    apparmor_parser: PathBuf,
}

impl UnitInstaller {
    /// Build the installer over explicit directories and tool programs.
    pub fn new(
        unit_dir: PathBuf,
        apparmor_dir: PathBuf,
        systemctl: PathBuf,
        apparmor_parser: PathBuf,
    ) -> Self {
        Self {
            unit_dir,
            apparmor_dir,
            systemctl,
            apparmor_parser,
        }
    }

    /// The three file paths every install target uses (the one derivation).
    pub fn paths(&self) -> InstallPaths {
        InstallPaths {
            socket: self.unit_dir.join(ManagedUnit::Socket.unit_name()),
            service: self.unit_dir.join(ManagedUnit::Service.unit_name()),
            apparmor: self.apparmor_dir.join(APPARMOR_PROFILE_NAME),
        }
    }

    /// Read one file, or `Ok(None)` when it does not exist.
    fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, String> {
        match fs::read(path) {
            Ok(contents) => Ok(Some(contents)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(format!("cannot read {}: {err}", path.display())),
        }
    }

    /// Write one `0644` file through a same-directory temporary file and `persist` (rename).
    fn persist_file(&self, path: &Path, contents: &[u8]) -> Result<(), String> {
        let parent = path
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
        let mut temp = NamedTempFile::new_in(parent).map_err(|err| {
            format!(
                "cannot create a temporary file beside {}: {err}",
                path.display()
            )
        })?;
        let written = (|| -> io::Result<()> {
            temp.write_all(contents)?;
            // The mode is part of the contract: a rewritten unit is always a systemd-readable `0644`.
            temp.as_file()
                .set_permissions(fs::Permissions::from_mode(0o644))?;
            temp.as_file().sync_all()
        })();
        if let Err(err) = written {
            return Err(format!("cannot write {}: {err}", path.display()));
        }
        temp.persist(path).map_err(|err| {
            format!(
                "cannot rename the temporary file over {}: {err}",
                path.display()
            )
        })?;
        // A crash after `persist` must never leave a zero-length unit: persist the directory entry.
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|err| format!("cannot fsync {}: {err}", parent.display()))
    }

    /// `systemctl daemon-reload`.
    fn daemon_reload(&self) -> Result<(), String> {
        self.run_tool(&[OsStr::new("daemon-reload")])
    }

    /// `apparmor_parser -r <profile_path>`.
    fn apparmor_replace(&self, profile_path: &Path) -> Result<(), String> {
        self.run_program(
            &self.apparmor_parser,
            &[OsStr::new("-r"), profile_path.as_os_str()],
        )
        .map(|_| ())
    }

    /// `systemctl enable --now <units>`.
    fn enable_now(&self, units: &[ManagedUnit]) -> Result<(), String> {
        let mut args = vec![OsStr::new("enable"), OsStr::new("--now")];
        args.extend(units.iter().map(|unit| OsStr::new(unit.unit_name())));
        self.run_tool(&args)
    }

    /// `systemctl restart slipforkd.service`.
    fn restart_service(&self) -> Result<(), String> {
        self.run_tool(&[
            OsStr::new("restart"),
            OsStr::new(ManagedUnit::Service.unit_name()),
        ])
    }

    /// Run one `systemctl` verb with a cleared environment and the fixed tool path.
    pub(crate) fn run_tool(&self, args: &[&OsStr]) -> Result<(), String> {
        self.run_capture(args).map(|_| ())
    }

    /// Run one `systemctl` verb the same way, returning its stdout.
    pub(crate) fn run_capture(&self, args: &[&OsStr]) -> Result<String, String> {
        self.run_program(&self.systemctl, args)
    }

    /// Run one install tool with a cleared environment and the fixed tool path.
    fn run_program(&self, program: &Path, args: &[&OsStr]) -> Result<String, String> {
        let output = Command::new(program)
            .args(args)
            .env_clear()
            .env("PATH", INSTALL_TOOL_PATH)
            .output()
            .map_err(|err| {
                format!(
                    "cannot run `{}`: {err}; next: install apparmor, or run `slipfork daemon unit` to preview the files",
                    program.display()
                )
            })?;
        if !output.status.success() {
            // Exactly one line: the tool's first stderr line is the whole context.
            let stderr = String::from_utf8_lossy(&output.stderr);
            let first = stderr.lines().next().unwrap_or("").trim();
            return Err(format!(
                "`{} {}` exited with {}: {first}",
                program.display(),
                args.iter()
                    .map(|arg| arg.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" "),
                output.status
            ));
        }
        String::from_utf8(output.stdout)
            .map_err(|err| format!("`{}` printed non-UTF-8 on stdout: {err}", program.display()))
    }
}

/// Every already-resolved input of one `slipfork daemon install` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallRequest {
    /// Whether the invoking process is `uid 0` (`current_uid_gid` at the composition root).
    pub is_root: bool,
    /// `--user <ACCOUNT>`, or `None` to fall back to `invoking_user` / fail closed as direct root.
    pub explicit_user: Option<String>,
    /// The `SUDO_USER` login account the composition root read, or `None` for direct root.
    pub invoking_user: Option<String>,
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

impl InstallRequest {
    /// The account the units run as: `--user`, else the sudo invoking account, else the process's.
    ///
    /// One cohesive derivation shared by the preview (`daemon unit`) and this install, so both
    /// render from the same resolved row.
    pub fn target_account(&self, identity: &UserIdentity) -> Result<ResolvedAccount, String> {
        identity.target_account(
            self.is_root,
            self.explicit_user.as_deref(),
            self.invoking_user.as_deref(),
        )
    }
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
    /// The exact paths this run wrote (the installer's own derivation).
    pub paths: InstallPaths,
}

impl InstallReport {
    /// The three `(path, status)` rows, declared once for the human transcript.
    fn rows(&self) -> [(&Path, FileStatus); 3] {
        [
            (self.paths.socket.as_path(), self.socket),
            (self.paths.service.as_path(), self.service),
            (self.paths.apparmor.as_path(), self.apparmor),
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
                "unchanged {}, {}, {}; enabled\n",
                ManagedUnit::Socket.unit_name(),
                ManagedUnit::Service.unit_name(),
                self.paths.apparmor.display()
            );
        }
        let mut text = String::new();
        for (path, status) in self.rows() {
            text.push_str(&format!("{:<9} {}\n", status.as_str(), path.display()));
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
/// The function reads no environment and resolves no account database; every effect runs through
/// the one concrete [`UnitInstaller`] (write, reload, AppArmor, handover, enable). It fails closed
/// **before** touching any file when it is not root or when the target account is unusable.
///
/// A failure halfway through leaves already-written files in place; because every write is
/// idempotent, re-running the same command converges (and `enable --now` is safe to repeat).
pub fn install(
    identity: &UserIdentity,
    installer: &UnitInstaller,
    budget: HandoverBudget,
    request: &InstallRequest,
) -> Result<InstallReport, String> {
    if !request.is_root {
        return Err(non_root_remedy(&identity.account));
    }
    let account = request.target_account(identity)?;
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
    let paths = installer.paths();

    let files = [
        (
            paths.socket.as_path(),
            render_socket_unit(&spec).map_err(|err| err.to_string())?,
        ),
        (
            paths.service.as_path(),
            render_service_unit(&spec).map_err(|err| err.to_string())?,
        ),
        (paths.apparmor.as_path(), apparmor),
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
                .persist_file(path, contents.as_bytes())
                .map_err(converge_after_failure)?;
            statuses.push(FileStatus::Wrote);
        }
    }
    let socket = statuses[0];
    let service = statuses[1];
    let apparmor = statuses[2];

    installer.daemon_reload().map_err(converge_after_failure)?;
    installer
        .apparmor_replace(&paths.apparmor)
        .map_err(converge_after_failure)?;
    // A zero-setup daemon (of any build) may hold `<runtime_dir>/daemon.sock`; hand it over before
    // `enable --now`.
    release_daemon(installer, budget, layout.runtime_dir()).map_err(converge_after_failure)?;
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
        paths,
    })
}

/// Every mid-sequence effect failure tells the operator that a re-run converges.
fn converge_after_failure(message: String) -> String {
    format!("{message} — re-run to converge")
}
