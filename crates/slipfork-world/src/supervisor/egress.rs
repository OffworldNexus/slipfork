//! World egress: one supervised `slirp4netns` user-mode NAT behind the world's `tap0`.
//!
//! `unshare --net` hands the world a private network namespace with only `lo`, so every setup,
//! hook, and service pod would fail closed with `ENETUNREACH`/`EAI_AGAIN`. One
//! `slirp4netns --configure` forwarder attached to that netns adds `tap0` on `10.0.2.100/24`
//! plus a default route; `--disable-host-loopback` keeps the host's `127.0.0.1` unreachable from
//! pods. The forwarder lives exactly as long as the world: the supervisor reaps its process
//! group on `down`, on drop, and while unwinding a panic.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::layout::WorldName;

/// Interface `slirp4netns --configure` adds inside the world network namespace.
const DEVICE: &str = "tap0";

/// Point-to-point MTU of the slirp user-mode link.
const MTU: &str = "65520";

/// File the forwarder's stdout/stderr land in, inside the world's durable log directory.
pub(super) const LOG_FILE: &str = "slirp4netns.log";

/// Upper bound for `tap0` to appear; `--configure` finishes in single-digit milliseconds.
pub(super) const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll interval for the netns-relative interface table.
const POLL: Duration = Duration::from_millis(20);

/// Trailing bytes of the forwarder log quoted in an early-exit error.
const TAIL_BYTES: u64 = 2048;

/// `/proc/<init-pid>/net/dev` is netns-relative and readable unprivileged, so it is the readiness
/// oracle: a `tap0:` row means the default route is already installed.
pub(super) fn net_dev_path(init_pid: u32) -> PathBuf {
    PathBuf::from(format!("/proc/{init_pid}/net/dev"))
}

/// `<logs_root>/<world>/slirp4netns.log`: the forwarder capture beside the world's pod logs.
///
/// It must never live in the tmpfs pod root (`<runtime_dir>/slipfork/pods/<world>`), because the
/// fork path unlinks every entry there right after boot and would orphan the live forwarder's log.
#[must_use]
pub(super) fn log_path(logs_root: &Path, world: &WorldName) -> PathBuf {
    logs_root.join(world.as_str()).join(LOG_FILE)
}

/// Creates the private `<logs_root>/<world>` chain mode `0700` like every other capture, then
/// opens the forwarder log append-only mode `0600`.
fn open_log(log: &Path) -> Result<File> {
    if let Some(dir) = log.parent() {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|source| Error::io("create", dir, source))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|source| Error::io("chmod", dir, source))?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(log)
        .map_err(|source| Error::io("open", log, source))
}

/// The exact `slirp4netns` argument vector; only the binary path is injected.
pub(super) fn egress_argv(init_pid: u32) -> Vec<String> {
    vec![
        "--configure".to_owned(),
        format!("--mtu={MTU}"),
        "--disable-host-loopback".to_owned(),
        init_pid.to_string(),
        DEVICE.to_owned(),
    ]
}

/// One exact `device:` row in a `/proc/<pid>/net/dev` body.
fn lists_device(body: &str, device: &str) -> bool {
    body.lines().any(|line| {
        line.trim_start()
            .strip_prefix(device)
            .is_some_and(|rest| rest.starts_with(':'))
    })
}

/// Whether the netns behind `net_dev` already carries the interface.
pub(super) fn interface_present(net_dev: &Path, device: &str) -> bool {
    fs::read_to_string(net_dev).is_ok_and(|body| lists_device(&body, device))
}

/// Polls `net_dev` until `device` appears or the deadline passes; `checkpoint` runs once per
/// turn so a forwarder that exited early fails fast instead of burning the whole timeout.
pub(super) fn await_device(
    net_dev: &Path,
    device: &str,
    timeout: Duration,
    mut checkpoint: impl FnMut() -> Result<()>,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        checkpoint()?;
        if interface_present(net_dev, device) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Egress {
                status: None,
                message: format!(
                    "`{device}` did not appear within {}s",
                    timeout.as_secs_f32()
                ),
            });
        }
        thread::sleep(POLL);
    }
}

/// Last few hundred bytes of the forwarder log, flattened onto one line.
fn log_tail(path: &Path) -> String {
    let Ok(mut file) = File::open(path) else {
        return "no output".to_owned();
    };
    let end = file.metadata().map_or(0, |meta| meta.len());
    if file
        .seek(SeekFrom::Start(end.saturating_sub(TAIL_BYTES)))
        .is_err()
    {
        return "no output".to_owned();
    }
    let mut text = String::new();
    if file.read_to_string(&mut text).is_err() {
        return "no output".to_owned();
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        "no output".to_owned()
    } else {
        words.join(" ")
    }
}

/// One forwarder process bound to the world's lifetime; [`Self::reap`] is idempotent.
#[derive(Debug)]
pub(super) struct WorldEgress {
    program: PathBuf,
    child: Child,
    pgid: i32,
}

impl WorldEgress {
    /// Spawns `slirp4netns --configure … tap0` in its own process group, captures its output in
    /// the injected durable log path (creating that private chain), and blocks until the device is
    /// visible in the world netns.
    pub(super) fn attach(
        program: &Path,
        init_pid: u32,
        net_dev: &Path,
        log: &Path,
        timeout: Duration,
    ) -> Result<Self> {
        let stdout = open_log(log)?;
        let stderr = stdout
            .try_clone()
            .map_err(|source| Error::io("clone", log, source))?;
        let child = Command::new(program)
            .args(egress_argv(init_pid))
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .process_group(0)
            .spawn()
            .map_err(|source| match source.kind() {
                ErrorKind::NotFound => Error::MissingTool {
                    tool: "slirp4netns",
                    path: program.to_path_buf(),
                },
                _ => Error::io("spawn", program, source),
            })?;
        let mut egress = Self {
            program: program.to_path_buf(),
            pgid: i32::try_from(child.id()).unwrap_or(i32::MAX),
            child,
        };
        egress.await_attached(net_dev, log, timeout)?;
        Ok(egress)
    }

    /// Waits for `tap0` while watching the child, so an early exit is named with its log tail.
    fn await_attached(&mut self, net_dev: &Path, log: &Path, timeout: Duration) -> Result<()> {
        let Self { program, child, .. } = self;
        await_device(net_dev, DEVICE, timeout, || {
            match child
                .try_wait()
                .map_err(|source| Error::io("wait", program, source))?
            {
                Some(status) => Err(Error::Egress {
                    status: status.code(),
                    message: format!(
                        "`{DEVICE}` never attached; forwarder output: {}",
                        log_tail(log)
                    ),
                }),
                None => Ok(()),
            }
        })
    }

    /// Kills the forwarder's own process group and reaps it; safe to call twice.
    pub(super) fn reap(&mut self) -> Result<()> {
        if self
            .child
            .try_wait()
            .map_err(|source| Error::io("wait", &self.program, source))?
            .is_some()
        {
            return Ok(());
        }
        // SAFETY: a negative pid addresses the forwarder's own group and never dereferences memory.
        unsafe {
            libc::kill(-self.pgid, libc::SIGKILL);
        }
        self.child
            .wait()
            .map_err(|source| Error::io("wait", &self.program, source))?;
        Ok(())
    }
}

impl Drop for WorldEgress {
    fn drop(&mut self) {
        if let Err(error) = self.reap() {
            eprintln!("slipfork-world: egress teardown failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disk-backed scratch root; the poller only ever reads real files.
    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::TempDir::new_in("/var/tmp").expect("disk-backed scratch root");
        let net_dev = root.path().join("4242/net/dev");
        std::fs::create_dir_all(net_dev.parent().expect("net dir")).expect("create net dir");
        (root, net_dev)
    }

    /// The durable capture path boot composes: `<store>/logs/<world>/slirp4netns.log`.
    fn durable_log(root: &Path) -> PathBuf {
        let world = WorldName::parse("demo").expect("valid world name");
        log_path(&root.join("logs"), &world)
    }

    /// Permission bits of one path.
    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path)
            .expect("path metadata")
            .permissions()
            .mode()
            & 0o777
    }

    const LO_ONLY: &str = "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes\n    lo: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n";
    const WITH_TAP0: &str = "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes\n    lo: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n  tap0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n";

    /// The forwarder argv is pinned: configure, slirp MTU, host loopback dark, pid, device.
    #[test]
    fn argv_attaches_tap0_with_host_loopback_disabled() {
        assert_eq!(
            egress_argv(4242),
            [
                "--configure",
                "--mtu=65520",
                "--disable-host-loopback",
                "4242",
                "tap0",
            ]
            .map(str::to_owned)
        );
        assert_eq!(net_dev_path(4242), PathBuf::from("/proc/4242/net/dev"));
    }

    /// The forwarder log lands beside the world's durable captures, never in the tmpfs pod root
    /// the fork path unlinks per entry right after boot, and both dir and file are private.
    #[test]
    fn forwarder_log_lives_beside_durable_captures() {
        let (root, net_dev) = scratch();
        std::fs::write(&net_dev, LO_ONLY).expect("seed a lo-only interface table");
        let log = durable_log(root.path());
        assert_eq!(log, root.path().join("logs/demo/slirp4netns.log"));
        assert!(
            !log.starts_with(root.path().join("pods")),
            "the fork path unlinks the pod root: {}",
            log.display()
        );
        let _ = WorldEgress::attach(
            Path::new("/nonexistent/slirp4netns"),
            4242,
            &net_dev,
            &log,
            Duration::from_millis(50),
        );
        assert_eq!(mode(log.parent().expect("log parent")), 0o700);
        assert_eq!(mode(&log), 0o600);
    }

    /// The readiness poller reads a `/proc`-shaped tree and observes `tap0:` appearing.
    #[test]
    fn readiness_polls_a_proc_shaped_tree() {
        let (_root, net_dev) = scratch();
        std::fs::write(&net_dev, LO_ONLY).expect("seed a lo-only interface table");
        assert!(!interface_present(&net_dev, DEVICE));
        let writer = net_dev.clone();
        let forwarder = thread::spawn(move || {
            thread::sleep(Duration::from_millis(80));
            std::fs::write(&writer, WITH_TAP0).expect("attach tap0");
        });
        await_device(&net_dev, DEVICE, Duration::from_secs(5), || Ok(())).expect("tap0 attaches");
        forwarder.join().expect("forwarder thread joins");
        assert!(interface_present(&net_dev, DEVICE));
    }

    /// Only the exact `tap0:` row counts; a `tap00:` sibling must not satisfy readiness.
    #[test]
    fn poller_requires_the_exact_interface_name() {
        let (_root, net_dev) = scratch();
        std::fs::write(
            &net_dev,
            "    lo: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n  tap00: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        )
        .expect("seed a look-alike interface");
        assert!(!interface_present(&net_dev, DEVICE));
        let error = await_device(&net_dev, DEVICE, Duration::from_millis(80), || Ok(()))
            .expect_err("a look-alike device never boots the world");
        assert!(matches!(error, Error::Egress { status: None, .. }));
    }

    /// A missing `slirp4netns` is a tool error with an install remedy, never a silent no-route
    /// boot. The child is reaped by `Drop` because it never spawned.
    #[test]
    fn missing_forwarder_reports_the_tool_and_remedy() {
        let (root, net_dev) = scratch();
        std::fs::write(&net_dev, LO_ONLY).expect("seed a lo-only interface table");
        let error = WorldEgress::attach(
            Path::new("/nonexistent/slirp4netns"),
            4242,
            &net_dev,
            &durable_log(root.path()),
            Duration::from_millis(200),
        )
        .expect_err("a missing forwarder must fail the boot");
        match &error {
            Error::MissingTool { tool, .. } => assert_eq!(*tool, "slirp4netns"),
            other => panic!("unexpected error: {other}"),
        }
        let text = error.to_string();
        assert!(text.contains("install slirp4netns"), "{text}");
    }

    /// A forwarder that exits before `tap0` exists names its exit code and log tail.
    #[test]
    fn early_exit_names_the_status_and_log_tail() {
        let (root, net_dev) = scratch();
        std::fs::write(&net_dev, LO_ONLY).expect("seed a lo-only interface table");
        let log = durable_log(root.path());
        let error = WorldEgress::attach(
            Path::new("/bin/false"),
            4242,
            &net_dev,
            &log,
            Duration::from_secs(5),
        )
        .expect_err("an instantly exiting forwarder must fail the boot");
        match &error {
            Error::Egress {
                status: Some(1), ..
            } => {}
            other => panic!("unexpected error: {other}"),
        }
        let text = error.to_string();
        assert!(text.contains("exit 1"), "{text}");
        assert!(text.contains("next: install slirp4netns"), "{text}");
    }
}
