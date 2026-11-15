//! Host payload provisioning for the `L0` rootfs (`ADR-0009` §1–§3).
//!
//! `slipforkd` is the only place allowed to resolve these ambient paths: the pinned static `crun`
//! release asset lives under `<state_dir>/cache/runtime/<pinned file>` (or an explicit `--crun`),
//! and `slipfork-world-init` defaults to the sibling executable. Nothing is downloaded behind the
//! developer's back: a missing pin fails closed with the exact `curl` command that installs it.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use slipfork_engine::rootfs::{RuntimeAssets, pin::crun_pin};

/// Linux architecture of this binary (`x86_64`, `aarch64`); never guessed at runtime.
pub fn target_arch() -> Result<&'static str, String> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x86_64"),
        "aarch64" => Ok("aarch64"),
        other => Err(format!(
            "unsupported host architecture '{other}'; pinned static crun assets exist for x86_64 only"
        )),
    }
}

/// Resolve and verify the two static host payloads staged into every world.
pub fn runtime_assets(
    state_dir: &Path,
    crun: Option<PathBuf>,
    world_init: Option<PathBuf>,
) -> Result<RuntimeAssets, String> {
    let arch = target_arch()?;
    let pin = crun_pin(arch).map_err(|err| err.to_string())?;
    let crun = match crun {
        Some(path) => path,
        None => state_dir.join("cache").join("runtime").join(pin.file_name),
    };
    if !crun.is_file() {
        // First-run provisioning of the pinned asset. The bytes never enter a world until they
        // match the compile-time SHA-256, and every later boot is offline.
        provision_crun(&crun, pin.sha256, pin.release_url)?;
    }
    let world_init = match world_init {
        Some(path) => path,
        None => default_world_init()?,
    };
    if !world_init.is_file() {
        return Err(format!(
            "slipfork-world-init helper is missing at {}; pass --world-init <PATH>",
            world_init.display()
        ));
    }
    let absolute = |path: PathBuf| -> Result<PathBuf, String> {
        std::fs::canonicalize(&path)
            .map_err(|err| format!("cannot resolve {}: {err}", path.display()))
    };
    Ok(RuntimeAssets {
        crun: absolute(crun)?,
        world_init: absolute(world_init)?,
    })
}

/// Download the pinned static `crun` and verify its compile-time SHA-256.
fn provision_crun(path: &Path, sha256: &str, url: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    let temporary = path.with_extension("download");
    let status = std::process::Command::new("curl")
        .args(["-fsSL", "--retry", "2", "-o"])
        .arg(&temporary)
        .arg(url)
        .status()
        .map_err(|err| {
            format!(
                "cannot run curl to fetch the pinned static crun ({err}); install curl or place \
                 the asset (sha256 {sha256}) at {}",
                path.display()
            )
        })?;
    if !status.success() {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!(
            "failed to download the pinned static crun from {url}; place the asset \
             (sha256 {sha256}) at {}",
            path.display()
        ));
    }
    match verify_sha256(&temporary, sha256) {
        Ok(()) => {}
        Err(reason) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(reason);
        }
    }
    std::fs::rename(&temporary, path)
        .map_err(|err| format!("cannot install {}: {err}", path.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|err| format!("cannot make {} executable: {err}", path.display()))
}

fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let bytes =
        std::fs::read(path).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    let actual = hex::encode(Sha256::digest(bytes));
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "downloaded crun at {} has sha256 {actual}, expected {expected}",
            path.display()
        ))
    }
}

/// The `slipfork-world-init` binary installed next to `slipforkd`.
fn default_world_init() -> Result<PathBuf, String> {
    let daemon = std::env::current_exe()
        .map_err(|err| format!("cannot resolve the slipforkd executable: {err}"))?;
    let directory = daemon.parent().ok_or_else(|| {
        format!(
            "cannot resolve the directory of {}; pass --world-init <PATH>",
            daemon.display()
        )
    })?;
    Ok(directory.join("slipfork-world-init"))
}
