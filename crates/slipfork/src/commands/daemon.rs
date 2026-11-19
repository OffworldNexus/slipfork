//! `slipfork daemon unit` / `slipfork daemon install`: render and write the systemd units.

use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::json;
use slipfork::apparmor::render_userns_profile;
use slipfork::cli::UnitKind;
use slipfork::daemon_unit_install::{
    self, APPARMOR_DIR, APPARMOR_PARSER, DAEMON_HANDOVER, InstallProgress, InstallRequest,
    SYSTEM_UNIT_DIR, SYSTEMCTL, UnitInstaller, ingress_unit_spec,
};
use slipfork_edge::{render_service_unit, render_socket_unit};

use crate::CliApp;

/// Render the non-template `slipforkd.socket` / `slipforkd.service` units and AppArmor profile.
pub(crate) fn unit(
    app: &CliApp,
    only: Option<UnitKind>,
    forward_port: u16,
    user: Option<String>,
    bin_dir: Vec<PathBuf>,
) -> Result<ExitCode, String> {
    let identity = app.ui.identity()?;
    let (uid, _) = app.ui.uid_gid();
    let invoking = app.ui.invoking_account();
    // One account derivation shared with `daemon install`, so `--user` reproduces an install.
    let account = identity.target_account(uid == 0, user.as_deref(), invoking.as_deref())?;
    let layout = app.ui.layout_for(&account);
    let spec = ingress_unit_spec(&account, &layout, app.ui.daemon_executable()?, forward_port)?;
    let bin_dirs = bin_dirs_or_executable_dir(app, bin_dir)?;
    let socket = || render_socket_unit(&spec).map_err(|err| err.to_string());
    let service = || render_service_unit(&spec).map_err(|err| err.to_string());
    let apparmor = || render_userns_profile(&bin_dirs);
    let (text, wants_socket, wants_service, wants_apparmor) = match only {
        Some(UnitKind::Socket) => (socket()?, true, false, false),
        Some(UnitKind::Service) => (service()?, false, true, false),
        Some(UnitKind::Apparmor) => (apparmor()?, false, false, true),
        None => (
            format!("{}\n{}\n{}", socket()?, service()?, apparmor()?),
            true,
            true,
            true,
        ),
    };
    if app.output_mode.json {
        println!(
            "{}",
            json!({
                "ok": true,
                "socket": if wants_socket { serde_json::Value::String(socket()?) } else { serde_json::Value::Null },
                "service": if wants_service { serde_json::Value::String(service()?) } else { serde_json::Value::Null },
                "apparmor": if wants_apparmor { serde_json::Value::String(apparmor()?) } else { serde_json::Value::Null },
            })
        );
    } else {
        print!("{text}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Write `/etc/systemd/system/slipforkd.{socket,service}` plus the AppArmor profile, then
/// `daemon-reload`, `apparmor_parser -r`, and `enable --now`.
///
/// The composition root constructs the concrete [`UnitInstaller`] over the real system unit
/// directory; the orchestration itself fails closed before any effect when the process is not root.
pub(crate) fn install(
    app: &CliApp,
    user: Option<String>,
    bin_dir: Vec<PathBuf>,
    forward_port: u16,
) -> Result<ExitCode, String> {
    let (uid, _) = app.ui.uid_gid();
    // The root gate runs before any account-database read, so a non-root host always hears the
    // sudo remedy instead of an identity-probe failure.
    if uid != 0 {
        return Err(daemon_unit_install::non_root_remedy(
            &app.ui.account_name()?,
        ));
    }
    let identity = app.ui.identity()?;
    let request = InstallRequest {
        is_root: true,
        explicit_user: user,
        invoking_user: app.ui.invoking_account(),
        executable: app.ui.daemon_executable()?,
        bin_dirs: bin_dirs_or_executable_dir(app, bin_dir)?,
        forward_port,
        state_dir: app.ui.state_dir.clone(),
        xdg_cache_home: app.ui.xdg_cache_home.clone(),
        xdg_data_home: app.ui.xdg_data_home.clone(),
    };
    let installer = UnitInstaller::new(
        PathBuf::from(SYSTEM_UNIT_DIR),
        PathBuf::from(APPARMOR_DIR),
        PathBuf::from(SYSTEMCTL),
        PathBuf::from(APPARMOR_PARSER),
    );
    let mut phase = String::new();
    let report = daemon_unit_install::install_streamed(
        &identity,
        &installer,
        DAEMON_HANDOVER,
        &request,
        &mut |step| match step {
            InstallProgress::Phase(label) => {
                phase = label.to_owned();
                app.terminal.start_progress(label);
            }
            InstallProgress::Line(line) => {
                app.terminal.update_progress(&format!("{phase} — {line}"));
            }
        },
    )?;
    app.terminal.finish_progress();
    if app.output_mode.json {
        println!(
            "{}",
            json!({
                "ok": true,
                "user": report.user,
                "socket": report.socket.as_str(),
                "service": report.service.as_str(),
                "apparmor": report.apparmor.as_str(),
                "paths": {
                    "socket": report.paths.socket,
                    "service": report.paths.service,
                    "apparmor": report.paths.apparmor,
                },
            })
        );
    } else {
        print!("{}", report.render_text());
    }
    Ok(ExitCode::SUCCESS)
}

/// The repeatable `--bin-dir` list, defaulting to this executable's directory.
fn bin_dirs_or_executable_dir(app: &CliApp, bin_dir: Vec<PathBuf>) -> Result<Vec<PathBuf>, String> {
    if bin_dir.is_empty() {
        Ok(vec![app.ui.exe_dir()?])
    } else {
        Ok(bin_dir)
    }
}
