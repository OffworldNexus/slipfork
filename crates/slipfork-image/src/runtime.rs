//! `RUN` execution through `crun`, one container per Dockerfile instruction.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread;

use crate::dockerfile::{CacheMount, Command as StepCommand, ImageConfig, Owner, RunSpec};
use crate::error::{Error, Result};
use crate::identity::{Identity, PROCESS_UMASK};
pub(crate) use crate::step_log::StepLog;
use crate::step_log::exit_detail;

const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The `RUN` child's environment: image `ENV`, then our colour hints, then the `apt` config.
fn step_env(config: &ImageConfig) -> Vec<String> {
    let mut env = vec![format!("PATH={DEFAULT_PATH}")];
    for (key, value) in &config.env {
        env.push(format!("{key}={value}"));
    }
    // glibc's `getenv` returns the first match, so drop any image `ENV APT_CONFIG=...` before
    // appending the per-step config; the last entry alone would never win.
    env.retain(|entry| !entry.starts_with("APT_CONFIG="));
    env.push(format!("APT_CONFIG={APT_CONF_TARGET}"));
    if !config.env.contains_key("NO_COLOR") {
        for (key, value) in slipfork_world::COLOUR_HINTS {
            if !config.env.contains_key(key) {
                env.push(format!("{key}={value}"));
            }
        }
    }
    env
}

/// `apt` refuses to drop privileges it never had inside a single-mapping user namespace.
const APT_CONF: &str = "APT::Sandbox::User \"root\";\n";

/// Where the per-step `apt.conf` is bound: a tmpfs-only path, never the image's own `/dev`.
const APT_CONF_TARGET: &str = "/dev/slipfork-apt.conf";

/// Device nodes a rootless `mknod` cannot create; the host's own are bound read-only instead.
const DEVICES: [&str; 6] = [
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
];

/// Which child stream one step-log line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// The `RUN` command's standard output.
    Stdout,
    /// The `RUN` command's standard error.
    Stderr,
}

/// Plain host values `crun` needs; the composition root injects all of them.
#[derive(Debug, Clone)]
pub struct StepHost {
    /// `crun` executable path.
    pub crun: PathBuf,
    /// Host `/etc/resolv.conf` bound read-only into the build container.
    pub resolv_conf: PathBuf,
    /// Disk-backed scratch root for OCI bundles.
    pub scratch_root: PathBuf,
    /// Invoking uid, mapped to container root (and to every `USER` the step names).
    pub uid: u32,
    /// Invoking gid, mapped to container root (and to every `USER` the step names).
    pub gid: u32,
}

impl StepHost {
    /// Bundles the injected host facts.
    #[must_use]
    pub fn new(
        crun: impl Into<PathBuf>,
        resolv_conf: impl Into<PathBuf>,
        scratch_root: impl Into<PathBuf>,
        uid: u32,
        gid: u32,
    ) -> Self {
        Self {
            crun: crun.into(),
            resolv_conf: resolv_conf.into(),
            scratch_root: scratch_root.into(),
            uid,
            gid,
        }
    }

    /// Numeric owner for files written by `COPY`/`ADD`; the only id a single mapping represents.
    #[must_use]
    pub const fn owner(&self) -> Owner {
        Owner {
            uid: self.uid,
            gid: self.gid,
        }
    }
}

/// One `RUN` instruction on a writable step snapshot.
#[derive(Debug, Clone)]
pub struct CrunStepRuntime {
    host: StepHost,
}

impl CrunStepRuntime {
    /// Wraps the injected host.
    #[must_use]
    pub fn new(host: StepHost) -> Self {
        Self { host }
    }

    /// The host facts backing this runtime.
    #[must_use]
    pub fn host(&self) -> &StepHost {
        &self.host
    }

    /// Fails closed before a build touches disk when `RUN` cannot execute.
    pub fn ensure_crun(&self) -> Result<()> {
        if self.host.crun.is_file() {
            Ok(())
        } else {
            Err(Error::CrunMissing {
                path: self.host.crun.clone(),
            })
        }
    }

    /// Executes `run` inside `rootfs`, streaming every child line to `log`.
    ///
    /// The step's identity is resolved from the rootfs it was handed: `USER app` runs as the
    /// account the image itself defines, mapped one-to-one onto the invoking host uid/gid.
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &self,
        id: &str,
        rootfs: &Path,
        config: &ImageConfig,
        run: &RunSpec,
        cache: &[(CacheMount, PathBuf)],
        line: usize,
        log: &mut StepLog<'_>,
    ) -> Result<()> {
        self.ensure_crun()?;
        // The one resolver: the service-boot path resolves the sealer-recorded `ImageConfig.user`
        // for the same rootfs through this identical `Identity::resolve` call.
        let identity = Identity::resolve(rootfs, config.user.as_deref())?;
        fs::create_dir_all(&self.host.scratch_root)
            .map_err(|source| Error::io("create", &self.host.scratch_root, source))?;
        let bundle = self.host.scratch_root.join(id);
        let _ = fs::remove_dir_all(&bundle);
        fs::create_dir_all(&bundle).map_err(|source| Error::io("create", &bundle, source))?;

        let apt_conf = bundle.join("apt.conf");
        fs::write(&apt_conf, APT_CONF).map_err(|source| Error::io("write", &apt_conf, source))?;

        for mount in &run.mounts {
            let target = rooted(rootfs, &mount.target);
            fs::create_dir_all(&target).map_err(|source| Error::io("create", &target, source))?;
        }
        for directory in ["proc", "dev", "etc"] {
            let path = rootfs.join(directory);
            fs::create_dir_all(&path).map_err(|source| Error::io("create", &path, source))?;
        }

        let spec = StepBundle {
            host: &self.host,
            rootfs,
            apt_conf: &apt_conf,
            identity,
            config,
            run,
            cache,
        };
        let config_json = spec.oci_spec();
        let config_path = bundle.join("config.json");
        let bytes = serde_json::to_vec_pretty(&config_json).map_err(|source| Error::Json {
            url: config_path.display().to_string(),
            source,
        })?;
        fs::write(&config_path, bytes)
            .map_err(|source| Error::io("write", &config_path, source))?;

        // Reclaim any stale container state left behind by this process on an earlier failed
        // step before `crun run` creates `<crun_root>/sf-<pid>-<hash>`; other processes' live
        // builds carry their own pid prefix and are never touched.
        self.delete_container(id);
        let outcome = self.spawn_streaming(id, &bundle, log);
        self.cleanup(id, &bundle);
        let status = outcome?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::StepFailed {
                line,
                phase: "run",
                instruction: run.command.as_written().into(),
                message: "the command exited with a non-zero status".to_owned(),
                exit_code: status.code(),
                runtime: Some(exit_detail(status.code()).into()),
                next: "fix the command above; its output was streamed while it ran".to_owned(),
            })
        }
    }

    /// Spawns `crun` with piped output and forwards every line to `log` as it arrives.
    fn spawn_streaming(
        &self,
        id: &str,
        bundle: &Path,
        log: &mut StepLog<'_>,
    ) -> Result<std::process::ExitStatus> {
        let mut child = StdCommand::new(&self.host.crun)
            // `--cgroup-manager` is a crun global option: it must precede the subcommand.
            .args(["--cgroup-manager=disabled", "run", "--bundle"])
            .arg(bundle)
            .arg(id)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|source| {
                if source.kind() == std::io::ErrorKind::NotFound {
                    Error::CrunMissing {
                        path: self.host.crun.clone(),
                    }
                } else {
                    Error::io("run", &self.host.crun, source)
                }
            })?;
        let (sender, receiver) = mpsc::channel::<(Stream, String)>();
        let readers: Vec<thread::JoinHandle<()>> = [
            child.stdout.take().map(Reader::stdout),
            child.stderr.take().map(Reader::stderr),
        ]
        .into_iter()
        .flatten()
        .map(|reader| {
            let sender = sender.clone();
            thread::spawn(move || stream_lines(reader, &sender))
        })
        .collect();
        drop(sender);
        for (stream, line) in receiver {
            log.line(stream, &line);
        }
        for reader in readers {
            let _ = reader.join();
        }
        child
            .wait()
            .map_err(|source| Error::io("wait", &self.host.crun, source))
    }

    fn delete_container(&self, id: &str) {
        for args in [
            ["--cgroup-manager=disabled", "kill", "--all", id, "SIGKILL"].as_slice(),
            ["--cgroup-manager=disabled", "delete", "--force", id].as_slice(),
        ] {
            let _ = StdCommand::new(&self.host.crun)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    fn cleanup(&self, id: &str, bundle: &Path) {
        self.delete_container(id);
        let _ = fs::remove_dir_all(bundle);
    }
}

/// Everything one `RUN` step needs to render its OCI spec, resolved once in
/// [`CrunStepRuntime::run`].
struct StepBundle<'a> {
    /// Injected host facts (resolv.conf and the single uid/gid mapping).
    host: &'a StepHost,
    /// The writable step rootfs that becomes the container root.
    rootfs: &'a Path,
    /// The per-step `apt.conf` bound read-only at [`APT_CONF_TARGET`].
    apt_conf: &'a Path,
    /// Container identity the step runs as.
    identity: Identity,
    /// Image metadata (`env`, `workdir`, `user`) applied to the process.
    config: &'a ImageConfig,
    /// The command to execute and any declared cache mounts.
    run: &'a RunSpec,
    /// Resolved host directories for `RUN --mount=type=cache`.
    cache: &'a [(CacheMount, PathBuf)],
}

impl StepBundle<'_> {
    /// Builds the OCI runtime spec for this step: single-mapping identity, Docker default
    /// capabilities for root, no new privileges, a tmpfs `/dev` with host devices, and a seccomp
    /// profile that turns the `chown` family into a success no-op.
    fn oci_spec(&self) -> serde_json::Value {
        let args = match &self.run.command {
            StepCommand::Shell(script) => {
                vec!["/bin/sh".to_owned(), "-c".to_owned(), script.clone()]
            }
            StepCommand::Exec(argv) => argv.clone(),
        };
        let env = step_env(self.config);

        let mut mounts = vec![
            serde_json::json!({"destination": "/proc", "type": "proc", "source": "proc"}),
            serde_json::json!({
                "destination": "/dev",
                "type": "tmpfs",
                "source": "tmpfs",
                "options": ["nosuid", "strictatime", "mode=755", "size=65536k"]
            }),
        ];
        // Bound after the `/dev` tmpfs, so these mountpoints exist only in the tmpfs and never in
        // the image rootfs; a rootless build cannot `mknod` them itself.
        for device in DEVICES {
            mounts.push(serde_json::json!({
                "destination": device,
                "type": "bind",
                "source": device,
                "options": ["ro", "rbind"]
            }));
        }
        mounts.push(serde_json::json!({
            "destination": APT_CONF_TARGET,
            "type": "bind",
            "source": self.apt_conf,
            "options": ["ro", "rbind"]
        }));
        mounts.push(serde_json::json!({
            "destination": "/etc/resolv.conf",
            "type": "bind",
            "source": self.host.resolv_conf,
            "options": ["ro", "rbind"]
        }));
        for (mount, host_dir) in self.cache {
            let mut options = vec!["rbind"];
            if mount.readonly {
                options.push("ro");
            }
            mounts.push(serde_json::json!({
                "destination": mount.target,
                "type": "bind",
                "source": host_dir,
                "options": options
            }));
        }

        let identity = self.identity;
        let capabilities = identity.capabilities();
        serde_json::json!({
            "ociVersion": "1.0.2",
            "process": {
                "terminal": false,
                "user": {"uid": identity.uid, "gid": identity.gid},
                "args": args,
                "env": env,
                // Docker parity: a `RUN` must not inherit the daemon's umask, or `uv sync` makes a
                // group-writable `.venv` that the app's safety check refuses to serve.
                "umask": PROCESS_UMASK,
                "cwd": self.config.workdir,
                "capabilities": {
                    "bounding": capabilities,
                    "effective": capabilities,
                    "permitted": capabilities,
                    "inheritable": [],
                    "ambient": []
                },
                "noNewPrivileges": true
            },
            "root": {"path": self.rootfs, "readonly": false},
            "hostname": "slipfork-build",
            "mounts": mounts,
            "linux": {
                "namespaces": [
                    {"type": "user"},
                    {"type": "pid"},
                    {"type": "mount"},
                    {"type": "ipc"},
                    {"type": "uts"}
                ],
                "uidMappings": [{"containerID": identity.uid, "hostID": self.host.uid, "size": 1}],
                "gidMappings": [{"containerID": identity.gid, "hostID": self.host.gid, "size": 1}],
                "maskedPaths": [],
                "readonlyPaths": [],
                // Charliecloud precedent: dpkg chowns to unmapped ids (`root:shadow`), so the
                // whole `chown` family reports success instead of failing the package install.
                "seccomp": {
                    "defaultAction": "SCMP_ACT_ALLOW",
                    "syscalls": [{
                        "names": ["chown", "fchown", "lchown", "fchownat"],
                        "action": "SCMP_ACT_ERRNO",
                        "errnoRet": 0
                    }]
                }
            },
            "annotations": {"slipfork.builder": "native"}
        })
    }
}

/// One piped child stream, tagged so both readers feed the same line sink.
enum Reader {
    Stdout(std::process::ChildStdout),
    Stderr(std::process::ChildStderr),
}

impl Reader {
    fn stdout(pipe: std::process::ChildStdout) -> Self {
        Self::Stdout(pipe)
    }

    fn stderr(pipe: std::process::ChildStderr) -> Self {
        Self::Stderr(pipe)
    }

    /// The stream this pipe carries.
    fn stream(&self) -> Stream {
        match self {
            Self::Stdout(_) => Stream::Stdout,
            Self::Stderr(_) => Stream::Stderr,
        }
    }
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Stdout(pipe) => pipe.read(buf),
            Self::Stderr(pipe) => pipe.read(buf),
        }
    }
}

fn stream_lines(reader: Reader, sender: &Sender<(Stream, String)>) {
    let stream = reader.stream();
    for line in BufReader::new(reader).lines() {
        let Ok(line) = line else { break };
        if sender.send((stream, line)).is_err() {
            break;
        }
    }
}

/// Resolves a container-absolute path inside `rootfs`, refusing escapes.
pub(crate) fn rooted(rootfs: &Path, destination: &str) -> PathBuf {
    let trimmed = destination.trim_start_matches('/');
    rootfs.join(trimmed)
}

#[cfg(test)]
mod tests;
