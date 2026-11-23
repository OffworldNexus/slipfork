//! `slipfork daemon install [--ingress]`: the systemd units, the OpenRC conf, and AppArmor.
//!
//! The plan is pure data ([`UnitWriter::plan`]) so tests can write every unit under `/var/tmp`
//! without root; the CLI validates the privilege boundary before a single byte is written.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::ui::paint;

/// The host facts `daemon install` needs, injected by the composition root.
#[derive(Debug, Clone)]
pub struct InstallHost {
    /// The running binary.
    pub exe: PathBuf,
    /// `PATH` entries, in order.
    pub path: Vec<PathBuf>,
    /// `$XDG_CONFIG_HOME` (or the invoking user's `~/.config`).
    pub config_dir: PathBuf,
    /// Prefix for system paths (`/` by default).
    pub install_root: PathBuf,
    /// Invoking effective uid.
    pub uid: u32,
    /// UID the system ingress runs as.
    pub ingress_uid: u32,
    /// Passwd name of [`Self::ingress_uid`], resolved in the composition root.
    pub ingress_user: String,
    /// This install root runs OpenRC, resolved in the composition root.
    pub openrc: bool,
}

/// The per-user daemon unit.
pub const USER_UNIT: &str = "\
[Unit]
Description=Slipfork per-user daemon

[Service]
Type=simple
ExecStart={exe} __daemon
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
";

/// The system `127.0.0.1:80` socket unit.
pub const INGRESS_SOCKET: &str = "\
[Unit]
Description=Slipfork .localhost :80 ingress

[Socket]
ListenStream=127.0.0.1:80
Accept=yes
NoDelay=true

[Install]
WantedBy=sockets.target
";

/// The stateless per-connection handoff instance started by the socket unit.
pub const INGRESS_SERVICE: &str = "\
[Unit]
Description=Slipfork ingress connection handoff
After=slipfork-ingress.socket

[Service]
Type=oneshot
ExecStart={exe} __ingress
StandardInput=socket
StandardOutput=socket
User={uid}
Environment=XDG_RUNTIME_DIR=/run/user/{uid}
NoNewPrivileges=true
";

/// The AppArmor profile that permits unprivileged user namespaces for the binary.
pub const APPARMOR_PROFILE: &str = "\
abi <abi/4.0>,
include <tunables/global>

profile slipfork /**/slipfork flags=(unconfined) {
  userns,
  include if exists <local/slipfork>
}
";

/// Socket unit file name.
pub const SOCKET_UNIT: &str = "slipfork-ingress.socket";
/// Template service file name.
pub const SERVICE_UNIT: &str = "slipfork-ingress@.service";
/// AppArmor profile file name.
pub const APPARMOR_FILE: &str = "slipfork";
/// OpenRC conf file name (the packaged init script sources it).
pub const OPENRC_CONF: &str = "slipfork-ingress";

/// Builds the exact write plan; no I/O happens here.
#[derive(Debug, Clone)]
pub struct UnitWriter {
    /// `<install_root>/etc/systemd/system`.
    pub system_root: PathBuf,
    /// `<config_root>/systemd/user`.
    pub user_root: PathBuf,
    /// The injected config root (`$XDG_CONFIG_HOME` or `~/.config`).
    pub config_root: PathBuf,
    /// `<install_root>/etc/apparmor.d`.
    pub apparmor_root: PathBuf,
    /// `<install_root>/etc/conf.d`.
    pub conf_d_root: PathBuf,
    /// The running binary.
    pub exe: PathBuf,
    /// UID the system ingress runs as.
    pub ingress_uid: u32,
    /// Passwd name of [`Self::ingress_uid`], resolved in the composition root.
    pub ingress_user: String,
    /// This install root runs OpenRC, resolved in the composition root.
    pub openrc: bool,
}

impl UnitWriter {
    /// Resolves every root from the injected host state; no filesystem probing happens here.
    #[must_use]
    pub fn from_host(host: &InstallHost) -> Self {
        Self {
            system_root: host.install_root.join("etc/systemd/system"),
            user_root: host.config_dir.join("systemd/user"),
            config_root: host.config_dir.clone(),
            apparmor_root: host.install_root.join("etc/apparmor.d"),
            conf_d_root: host.install_root.join("etc/conf.d"),
            exe: host.exe.clone(),
            ingress_uid: host.ingress_uid,
            ingress_user: host.ingress_user.clone(),
            openrc: host.openrc,
        }
    }

    /// Every `(path, contents)` pair, user unit first.
    #[must_use]
    pub fn plan(&self, ingress: bool) -> Vec<(PathBuf, String)> {
        let exe = self.exe.display().to_string();
        let user_unit = self.user_root.join("slipfork.service");
        debug_assert!(user_unit.starts_with(&self.config_root));
        let mut files = vec![(user_unit, USER_UNIT.replace("{exe}", &exe))];
        if ingress {
            files.push((
                self.system_root.join(SOCKET_UNIT),
                INGRESS_SOCKET.to_owned(),
            ));
            files.push((
                self.system_root.join(SERVICE_UNIT),
                INGRESS_SERVICE
                    .replace("{exe}", &exe)
                    .replace("{uid}", &self.ingress_uid.to_string()),
            ));
            files.push((
                self.apparmor_root.join(APPARMOR_FILE),
                APPARMOR_PROFILE.to_owned(),
            ));
            if let Some((path, contents)) = self.openrc_conf() {
                files.push((path, contents));
            }
        }
        files
    }

    /// The OpenRC conf that makes the packaged init script run as the invoking user.
    fn openrc_conf(&self) -> Option<(PathBuf, String)> {
        if !self.openrc {
            return None;
        }
        Some((
            self.conf_d_root.join(OPENRC_CONF),
            format!(
                "\
# Slipfork :80 ingress (OpenRC). The packaged /etc/init.d/slipfork-ingress
# sources this file and runs `slipfork __ingress --listen 127.0.0.1:80` as
# this user; the ingress refuses to run as root.
SLIPFORK_INGRESS_USER={}
",
                self.ingress_user
            ),
        ))
    }

    /// The conf that a user already pinned to someone other than `root`, so it is left alone.
    #[must_use]
    pub fn preserved_conf(&self, ingress: bool) -> Option<PathBuf> {
        if !ingress {
            return None;
        }
        let (path, _) = self.openrc_conf()?;
        let existing = fs::read_to_string(&path).ok()?;
        let edited = existing
            .lines()
            .find_map(|line| line.trim().strip_prefix("SLIPFORK_INGRESS_USER="))
            .is_some_and(|value| value.trim() != "root");
        edited.then_some(path)
    }

    /// Writes every planned file atomically with mode `0644`.
    ///
    /// A user-edited OpenRC conf is kept as-is: only the packaged `root` value is replaced.
    ///
    /// # Errors
    ///
    /// Propagates directory creation, write, chmod, and rename failures.
    pub fn write_all(&self, ingress: bool) -> io::Result<Vec<PathBuf>> {
        let preserved = self.preserved_conf(ingress);
        let mut written = Vec::new();
        for (path, contents) in self.plan(ingress) {
            if preserved.as_deref() == Some(path.as_path()) {
                continue;
            }
            write_unit(&path, &contents)?;
            written.push(path);
        }
        Ok(written)
    }
}

/// `system_socket_path` for `doctor`, derived from the same plan roots.
#[must_use]
pub fn system_socket_path(install_root: &Path) -> PathBuf {
    install_root.join("etc/systemd/system").join(SOCKET_UNIT)
}

/// Installs the requested units, applies AppArmor, and prints the outcome.
pub fn install(host: &InstallHost, ingress: bool, json: bool, color: bool) -> ExitCode {
    if ingress && host.uid != 0 {
        return fail(
            json,
            "installing the :80 socket and AppArmor profile needs root",
            "sudo slipfork daemon install --ingress",
        );
    }
    let writer = UnitWriter::from_host(host);
    let preserved = writer.preserved_conf(ingress);
    let installed = match writer.write_all(ingress) {
        Ok(paths) => paths,
        Err(error) => {
            return fail(
                json,
                &format!("cannot write the daemon units: {error}"),
                "check that the config and system directories are writable",
            );
        }
    };
    let profile = (ingress && host.uid == 0).then(|| apply_apparmor(host, &writer));
    if json {
        let paths: Vec<String> = installed
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        let status = emit(&format!(
            "{}\n",
            serde_json::json!({ "installed": paths, "ingress": ingress })
        ));
        return status;
    }
    let mut out = String::new();
    for path in &installed {
        out.push_str(&format!(
            "{} {}\n",
            paint("✓ wrote", "32", color),
            paint(&path.display().to_string(), "2", color)
        ));
    }
    if let Some(path) = &preserved {
        out.push_str(&format!(
            "{} {}\n",
            paint("✓ kept", "32", color),
            paint(
                &format!(
                    "{} (kept: your SLIPFORK_INGRESS_USER=… setting)",
                    path.display()
                ),
                "2",
                color
            )
        ));
    }
    if let Some(profile) = profile {
        out.push_str(&profile.render(color));
    }
    let status = emit(&out);
    if writer.openrc && ingress {
        eprintln!(
            "{}",
            paint("! OpenRC has not started the :80 ingress yet", "33", color)
        );
        eprintln!(
            "next: rc-update add slipfork-ingress default && rc-service slipfork-ingress start"
        );
    } else {
        eprintln!(
            "{}",
            paint("! systemd has not reloaded the new units yet", "33", color)
        );
        eprintln!("next: systemctl --user daemon-reload && systemctl --user enable --now slipfork");
    }
    if !ingress {
        eprintln!(
            "{}",
            paint(
                "hint: sudo slipfork daemon install --ingress  # adds the :80 socket and AppArmor profile",
                "2",
                color,
            )
        );
    }
    status
}

/// The AppArmor load attempt.
#[derive(Debug, Clone)]
pub struct Profile {
    applied: bool,
    detail: String,
    path: PathBuf,
}

impl Profile {
    fn render(&self, color: bool) -> String {
        if self.applied {
            format!("{} {}\n", paint("✓ AppArmor", "32", color), self.detail)
        } else {
            format!(
                "{} {}\nnext: sudo apparmor_parser -r {}\n",
                paint("!", "33", color),
                self.detail,
                self.path.display()
            )
        }
    }
}

fn apply_apparmor(host: &InstallHost, writer: &UnitWriter) -> Profile {
    let path = writer.apparmor_root.join(APPARMOR_FILE);
    let parser = host
        .path
        .iter()
        .map(|directory| directory.join("apparmor_parser"))
        .find(|candidate| candidate.is_file());
    let Some(parser) = parser else {
        return Profile {
            applied: false,
            detail: "profile written but apparmor_parser is not on PATH".to_owned(),
            path,
        };
    };
    match std::process::Command::new(parser)
        .arg("-r")
        .arg(&path)
        .output()
    {
        Ok(output) if output.status.success() => Profile {
            applied: true,
            detail: "profile loaded".to_owned(),
            path,
        },
        Ok(output) => Profile {
            applied: false,
            detail: format!(
                "profile written but not loaded: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            path,
        },
        Err(error) => Profile {
            applied: false,
            detail: format!("profile written but not loaded: {error}"),
            path,
        },
    }
}

fn write_unit(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, contents)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o644))?;
    fs::rename(&temporary, path)
}

/// Writes one rendered chunk without panicking on a closed pipe.
fn emit(text: &str) -> ExitCode {
    let mut stdout = std::io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: failed to write output: {error}");
            ExitCode::FAILURE
        }
    }
}

fn fail(json: bool, message: &str, next: &str) -> ExitCode {
    eprintln!("error: {message}");
    eprintln!("next: {next}");
    if json {
        let _ = emit(&format!(
            "{}\n",
            serde_json::json!({ "error": message, "next": next })
        ));
    }
    ExitCode::FAILURE
}
