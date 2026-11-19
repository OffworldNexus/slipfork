//! Phase-reporting unit test for the streamed install (`M3.30.3.2`), outside the 500-line module.

use super::*;

fn executable(path: &Path, body: &str) -> PathBuf {
    std::fs::write(path, body).expect("write the real tool script");
    std::fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .expect("mark the tool script executable");
    path.to_path_buf()
}

#[test]
fn every_streamed_install_phase_is_reported() {
    let root = tempfile::tempdir().expect("disk scratch");
    let unit_dir = root.path().join("systemd");
    let apparmor_dir = root.path().join("apparmor.d");
    fs::create_dir_all(&unit_dir).expect("unit dir");
    fs::create_dir_all(&apparmor_dir).expect("apparmor dir");
    let systemctl = executable(&root.path().join("systemctl"), "#!/bin/sh\nexit 0\n");
    let parser = executable(&root.path().join("apparmor_parser"), "#!/bin/sh\nexit 0\n");
    let installer = UnitInstaller::new(unit_dir, apparmor_dir, systemctl, parser);
    let identity = UserIdentity::resolve().expect("the runner's real OS identity");
    let request = InstallRequest {
        is_root: true,
        explicit_user: Some("nobody".to_owned()),
        invoking_user: None,
        executable: PathBuf::from("/bin/true"),
        bin_dirs: Vec::new(),
        forward_port: 7080,
        state_dir: Some(root.path().join("state")),
        xdg_cache_home: None,
        xdg_data_home: None,
    };
    let mut phases: Vec<String> = Vec::new();
    install_streamed(
        &identity,
        &installer,
        DAEMON_HANDOVER,
        &request,
        &mut |step| {
            if let InstallProgress::Phase(label) = step {
                phases.push(label.to_owned());
            }
        },
    )
    .expect("a streamed install writes real files and runs the real tool scripts");
    for needle in [
        "reloading systemd units",
        "loading the AppArmor userns profile",
        "enabling slipforkd.socket + slipforkd.service",
    ] {
        assert!(
            phases.iter().any(|phase| phase == needle),
            "missing {needle}: {phases:?}"
        );
    }
}
