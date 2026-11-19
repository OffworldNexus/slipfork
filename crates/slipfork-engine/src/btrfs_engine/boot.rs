//! World PID 1 boot: the one `unshare` argv, its `0600` config, the readiness contract, and the
//! `SCM_RIGHTS` dial socket (`ADR-0010` §6, `H-011`).

#![forbid(unsafe_code)]

use std::fs;
use std::io::Write;
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use slipfork_world::{
    EngineError, UNIX_SOCKET_PATH_MAX, WorldHandle, WorldSpec, unix_socket_path_fits,
};

use crate::child_io::drain_stderr;
use crate::linux_cow;
use crate::supervise::{ProcessIdentity, SupervisedProcess, host_child_pids};
use crate::userns_remediation_hint;
use crate::wait::poll_until;
use crate::world_init::{
    WORLD_INIT_CONFIG_FILE, WORLD_INIT_NAMESPACES_FILE, WORLD_INIT_SOCKET_FILE, WorldInitConfig,
    WorldInitIngress, synthesize_identity_files,
};

use super::teardown::WORLD_STOP_KILL_GRACE;
use super::{BtrfsEngine, WORLD_ROOTFS_DIR, io_error};

/// Installed file name of the per-world PID 1 helper (diagnostics only: the path is injected).
const WORLD_INIT_PROGRAM: &str = "slipfork-world-init";
/// Fixed search path for helper subprocesses (`unshare`, `nsenter`); nothing is inherited.
const HELPER_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
/// Bound on booting a world (mounts, loopback, listeners, `netns.sock`).
const WORLD_INIT_TIMEOUT: Duration = Duration::from_secs(20);
/// Bound on one `dial_world_tcp` round trip.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);

impl BtrfsEngine {
    /// The one `nsenter` argv entering every namespace of `init_pid` at `cwd`.
    ///
    /// The world owns no root filesystem: its mount namespace is a private copy of the host's, so
    /// `cwd` is a real path there and needs no guest remap. `--wdns` is opened after the namespace
    /// switch, so the process lands on that directory in the world rather than on a detached
    /// host dentry. `--ipc` joins the world's private System V IPC namespace, so `shmget` keys
    /// never collide across parallel worlds (`ADR-0010` §6).
    pub(crate) fn entry_prefix(&self, init_pid: u32, cwd: &Path) -> Vec<String> {
        vec![
            format!("--target={init_pid}"),
            "--user".to_owned(),
            "--preserve-credentials".to_owned(),
            "--mount".to_owned(),
            "--net".to_owned(),
            "--ipc".to_owned(),
            "--pid".to_owned(),
            format!("--wdns={}", cwd.display()),
            "--".to_owned(),
        ]
    }

    /// Full `unshare` argv for the world PID 1 (`ADR-0010` §6).
    ///
    /// Identity UID/GID mapping (`$UID -> $UID`) plus `--keep-caps` preserves exact `$UID:$GID`
    /// ownership on every file written to a shared mount and grants `CAP_SYS_ADMIN` /
    /// `CAP_NET_ADMIN` / `CAP_NET_BIND_SERVICE` inside the user namespace. `--ipc` gives the world
    /// a private System V shared memory and POSIX message-queue namespace: without it, parallel
    /// `postgres` containers in separate network namespaces still collide on `shmget` key `90000`
    /// and port-derived keys.
    fn world_init_argv(&self, config_path: &Path) -> Vec<String> {
        vec![
            "--user".to_owned(),
            format!("--map-user={}", self.host_uid),
            format!("--map-group={}", self.host_gid),
            "--keep-caps".to_owned(),
            "--mount".to_owned(),
            "--net".to_owned(),
            "--ipc".to_owned(),
            "--pid".to_owned(),
            "--fork".to_owned(),
            "--kill-child=SIGKILL".to_owned(),
            "--mount-proc".to_owned(),
            self.world_init_bin.display().to_string(),
            config_path.display().to_string(),
        ]
    }

    /// Materialize the world's control directory, boot the PID 1, and wait until its listeners are
    /// ready. Returns the dial socket path, the verified PID 1 identity, and its launcher.
    pub(crate) fn boot_world(
        &self,
        world_id: &str,
        world_dir: &Path,
        ctl_dir: &Path,
        spec: &WorldSpec,
    ) -> Result<(PathBuf, ProcessIdentity, Child), EngineError> {
        let label = world_id;
        let netns_sock = ctl_dir.join(WORLD_INIT_SOCKET_FILE);
        if !unix_socket_path_fits(&netns_sock) {
            return Err(EngineError::WorldInitFailed {
                world: label.to_owned(),
                reason: format!(
                    "world dial socket '{}' exceeds the {} byte AF_UNIX path limit; use a \
                     shorter --state-dir",
                    netns_sock.display(),
                    UNIX_SOCKET_PATH_MAX
                ),
            });
        }
        // A re-used world id must never inherit a previous boot's `init.ready` or `netns.sock`:
        // the control directory is ephemeral, so it is recreated from scratch.
        if ctl_dir.exists() {
            fs::remove_dir_all(ctl_dir).map_err(|err| io_error(ctl_dir, &err))?;
        }
        let home = spec
            .base_env
            .home()
            .ok_or_else(|| EngineError::InvalidWorldSpec {
                reason: "base_env must carry a world-scoped HOME".to_owned(),
            })?
            .to_path_buf();
        let world_rootfs = world_dir.join(WORLD_ROOTFS_DIR);
        let crun_guest = spec.rootfs.crun_bin.clone();
        let crun_host = world_rootfs.join(crun_guest.strip_prefix("/").unwrap_or(&crun_guest));
        if !crun_host.is_file() {
            return Err(EngineError::WorldInitFailed {
                world: label.to_owned(),
                reason: format!(
                    "the world rootfs '{}' does not contain the pinned static crun at \
                     '{}' ({})",
                    world_rootfs.display(),
                    crun_guest.display(),
                    crun_host.display()
                ),
            });
        }
        let oci_dir = ctl_dir.join("oci");
        let oci_run = ctl_dir.join("oci-run");
        for dir in [&oci_dir, &oci_run] {
            fs::create_dir_all(dir).map_err(|err| io_error(dir, &err))?;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                .map_err(|err| io_error(dir, &err))?;
        }
        // Identity files are bound into each backing-service bundle by host path; the world has no
        // root filesystem of its own, so nothing is mounted at `/.slipfork`.
        synthesize_identity_files(ctl_dir, &spec.rootfs, &home, self.host_uid, self.host_gid)?;

        let namespaces_file = ctl_dir.join(WORLD_INIT_NAMESPACES_FILE);
        let local_ingress = match &spec.local_ingress {
            Some(bridge) => {
                if !bridge.host_ingress_sock.exists() {
                    return Err(EngineError::WorldInitFailed {
                        world: label.to_owned(),
                        reason: format!(
                            "local ingress socket '{}' does not exist; bind it before booting a \
                             world",
                            bridge.host_ingress_sock.display()
                        ),
                    });
                }
                Some(WorldInitIngress {
                    port: bridge.port,
                    host_ingress_sock: bridge.host_ingress_sock.clone(),
                })
            }
            None => None,
        };
        let config = WorldInitConfig {
            netns_sock: netns_sock.clone(),
            namespaces_file: namespaces_file.clone(),
            tmpfs_dirs: vec![oci_dir.clone(), oci_run.clone()],
            local_ingress,
        };
        let config_path = ctl_dir.join(WORLD_INIT_CONFIG_FILE);
        fs::write(
            &config_path,
            serde_json::to_vec(&config).map_err(|err| EngineError::WorldInitFailed {
                world: label.to_owned(),
                reason: format!("cannot serialize world init config: {err}"),
            })?,
        )
        .map_err(|err| EngineError::WorldInitFailed {
            world: label.to_owned(),
            reason: format!("cannot write {}: {err}", config_path.display()),
        })?;
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).map_err(|err| {
            EngineError::WorldInitFailed {
                world: label.to_owned(),
                reason: format!("cannot chmod {} to 0600: {err}", config_path.display()),
            }
        })?;

        // `stdin` is a process-scoped lifeline pipe (`O_CLOEXEC` in the engine, inherited as fd 0
        // by `slipfork-world-init`): unlike `PR_SET_PDEATHSIG` (which Linux fires on creating
        // *thread* exit, breaking RPC thread-per-connection servers), the pipe write end closes
        // only on `destroy_world`, engine `Drop`, or daemon exit/crash.
        let mut launcher = Command::new("unshare")
            .args(self.world_init_argv(&config_path))
            .env_clear()
            .env("PATH", HELPER_PATH)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| EngineError::WorldInitFailed {
                world: label.to_owned(),
                reason: format!(
                    "cannot spawn `unshare` for `{WORLD_INIT_PROGRAM}` ({}): {err}",
                    self.world_init_bin.display()
                ),
            })?;

        let mut init: Option<ProcessIdentity> = None;
        // `true` once the `unshare` launcher itself exited before the world was ready: only that
        // failure is classified as a host user-namespace refusal (`ADR-0010` §7).
        let mut unshare_failed = false;
        let reason = poll_until(WORLD_INIT_TIMEOUT, || {
            if init.is_none() {
                match host_child_pids(launcher.id()) {
                    Ok(children) => {
                        init = children.first().copied().and_then(ProcessIdentity::capture);
                    }
                    Err(err) => {
                        return Some(format!(
                            "cannot read /proc/{}/task/{}/children to discover the world PID 1 \
                             (CONFIG_PROC_CHILDREN or /proc access required): {err}",
                            launcher.id(),
                            launcher.id()
                        ));
                    }
                }
            }
            if let Some(identity) = &init
                && identity.matches_live()
                && fs::read_to_string(config.ready_path()).is_ok_and(|ready| ready == "ready\n")
                && config.namespaces_file.exists()
                && netns_sock.exists()
                // The marker alone is not proof: a listener that cannot accept a connection is
                // not ready, so probe the dial socket before declaring the world up.
                && UnixStream::connect(&netns_sock).is_ok()
            {
                return Some(String::new());
            }
            match launcher.try_wait() {
                Ok(Some(status)) => {
                    unshare_failed = true;
                    Some(format!(
                        "the world init exited with {status} before becoming ready"
                    ))
                }
                Ok(None) => None,
                Err(err) => Some(format!("cannot observe the world init process: {err}")),
            }
        });
        let init_pid = init.as_ref().map(|identity| identity.pid);
        let outcome = match reason {
            None => Err(format!(
                "timed out after {}s waiting for {} (init pid {init_pid:?})",
                WORLD_INIT_TIMEOUT.as_secs(),
                config.ready_path().display()
            )),
            Some(reason) if reason.is_empty() => {
                Ok(init.clone().expect("readiness observed an init"))
            }
            Some(reason) => Err(reason),
        };

        match outcome {
            Ok(init) => Ok((netns_sock, init, launcher)),
            Err(reason) => {
                let stderr = abandon_unbooted_world(init.as_ref(), launcher).unwrap_or_default();
                let reason = format!("{reason}; helper stderr: {stderr}");
                // `ADR-0010` §7: an unprivileged `CLONE_NEWUSER` refusal (AppArmor
                // `kernel.apparmor_restrict_unprivileged_userns=1`) is a host-profile problem, not
                // a world bug, so it carries the one remediation formatter. Only `unshare`'s own
                // failure is classified; a world-init failure keeps its own diagnostic.
                if unshare_failed && userns_denied(&stderr) {
                    let bin_dir = self.world_init_bin.parent().unwrap_or(Path::new("/"));
                    return Err(EngineError::UsernsDenied {
                        detail: reason,
                        hint: userns_remediation_hint(bin_dir),
                    });
                }
                Err(EngineError::WorldInitFailed {
                    world: label.to_owned(),
                    reason,
                })
            }
        }
    }

    pub(crate) fn open_world_tcp(
        &self,
        world: &WorldHandle,
        port: u16,
    ) -> Result<TcpStream, EngineError> {
        let netns_sock = {
            let mut worlds = self.lock()?;
            let record = self.live_record(&mut worlds, world)?;
            record.netns_sock.clone()
        };
        let mut socket = UnixStream::connect(&netns_sock).map_err(|err| EngineError::Io {
            path: netns_sock.display().to_string(),
            message: err.to_string(),
        })?;
        socket
            .set_read_timeout(Some(DIAL_TIMEOUT))
            .and_then(|()| socket.set_write_timeout(Some(DIAL_TIMEOUT)))
            .map_err(|err| EngineError::Io {
                path: netns_sock.display().to_string(),
                message: err.to_string(),
            })?;
        socket
            .write_all(&port.to_be_bytes())
            .map_err(|err| EngineError::DialFailed {
                world: format!("{world:?}"),
                port,
                errno: err.raw_os_error().unwrap_or(libc::EIO),
            })?;
        let (status, descriptor) =
            linux_cow::recv_fd(&socket).map_err(|err| EngineError::DialFailed {
                world: format!("{world:?}"),
                port,
                errno: err.raw_os_error().unwrap_or(libc::EIO),
            })?;
        if status != linux_cow::STATUS_OK {
            return Err(EngineError::DialFailed {
                world: format!("{world:?}"),
                port,
                errno: i32::from(status),
            });
        }
        let descriptor = descriptor.ok_or_else(|| {
            EngineError::Execution(format!(
                "world init answered dial for port {port} without an SCM_RIGHTS descriptor"
            ))
        })?;
        Ok(TcpStream::from(descriptor))
    }
}

/// Abandon a world that will never become a record: drop the `stdin` lifeline, kill the `unshare`
/// launcher, collect its stderr tail, and reap it through the one [`SupervisedProcess`] stop path.
///
/// Two sites reach this before a `WorldRecord` exists — a `boot_world` that failed its readiness
/// contract and a `clone_world` whose record could not be inserted — so neither can reap through
/// [`BtrfsEngine::reap_world_init`] (it owns a record). Each caller keeps its own control
/// directory and diagnostic handling; the returned `stderr` tail is `None` when the launcher wrote
/// nothing.
pub(crate) fn abandon_unbooted_world(
    init: Option<&ProcessIdentity>,
    launcher: Child,
) -> Option<String> {
    let mut supervised = SupervisedProcess::from_parts(Some(launcher), init.cloned());
    supervised.close_stdin();
    // `drain_stderr` reads to EOF, so the launcher is killed before the tail is collected; `stop`
    // then owns the identity `SIGKILL`, the child reap, and the bounded `/proc` wait.
    if let Some(child) = supervised.child_mut() {
        let _ = child.kill();
    }
    let stderr = supervised.child_mut().map(drain_stderr).unwrap_or_default();
    let _ = supervised.stop(
        Duration::ZERO,
        WORLD_STOP_KILL_GRACE,
        Duration::ZERO,
        "unbooted world init",
    );
    (!stderr.is_empty()).then_some(stderr)
}

/// `true` when a failed `unshare` invocation reports a denied user namespace (`EPERM`).
///
/// The kernel and AppArmor both phrase this refusal as `Operation not permitted`; `unshare` prints
/// it to stderr and exits non-zero (`ADR-0010` §7).
fn userns_denied(stderr: &str) -> bool {
    stderr.contains("Operation not permitted") || stderr.contains("EPERM")
}
