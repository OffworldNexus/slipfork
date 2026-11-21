//! `RUN` execution through `crun`, one container per Dockerfile instruction.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread;

use crate::dockerfile::{CacheMount, Command as StepCommand, ImageConfig, Owner, RunSpec};
use crate::error::{Error, Result};

const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Plain host values `crun` needs; the composition root injects all of them.
#[derive(Debug, Clone)]
pub struct StepHost {
    /// `crun` executable path.
    pub crun: PathBuf,
    /// Host `/etc/resolv.conf` bound read-only into the build container.
    pub resolv_conf: PathBuf,
    /// Disk-backed scratch root for OCI bundles.
    pub scratch_root: PathBuf,
    /// Invoking uid, mapped identically inside the user namespace.
    pub uid: u32,
    /// Invoking gid, mapped identically inside the user namespace.
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

    /// Default numeric owner for files written by `COPY`/`ADD`.
    #[must_use]
    pub const fn owner(&self) -> Owner {
        Owner {
            uid: self.uid,
            gid: self.gid,
        }
    }

    /// A `/etc/passwd` that contains the invoking uid, so images never need a real one.
    fn passwd(&self) -> String {
        format!(
            "root:x:0:0:root:/root:/sbin/nologin\nslipfork:x:{}:{}:slipfork:/:/bin/sh\n",
            self.uid, self.gid
        )
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
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &self,
        id: &str,
        rootfs: &Path,
        config: &ImageConfig,
        run: &RunSpec,
        cache: &[(CacheMount, PathBuf)],
        line: usize,
        log: &mut dyn Write,
    ) -> Result<()> {
        self.ensure_crun()?;
        fs::create_dir_all(&self.host.scratch_root)
            .map_err(|source| Error::io("create", &self.host.scratch_root, source))?;
        let bundle = self.host.scratch_root.join(id);
        let _ = fs::remove_dir_all(&bundle);
        fs::create_dir_all(&bundle).map_err(|source| Error::io("create", &bundle, source))?;

        let passwd = bundle.join("passwd");
        fs::write(&passwd, self.host.passwd())
            .map_err(|source| Error::io("write", &passwd, source))?;

        for mount in &run.mounts {
            let target = rooted(rootfs, &mount.target);
            fs::create_dir_all(&target).map_err(|source| Error::io("create", &target, source))?;
        }
        for directory in ["proc", "dev", "etc"] {
            let path = rootfs.join(directory);
            fs::create_dir_all(&path).map_err(|source| Error::io("create", &path, source))?;
        }

        let config_json = self.oci_config(rootfs, config, run, cache, &passwd);
        let config_path = bundle.join("config.json");
        let bytes = serde_json::to_vec_pretty(&config_json).map_err(|source| Error::Json {
            url: config_path.display().to_string(),
            source,
        })?;
        fs::write(&config_path, bytes)
            .map_err(|source| Error::io("write", &config_path, source))?;

        let outcome = self.spawn_streaming(id, &bundle, log);
        self.cleanup(id, &bundle);
        let status = outcome?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::StepFailed {
                line,
                instruction: run.command.render(),
                message: format!(
                    "`crun` exited with {}",
                    status
                        .code()
                        .map_or_else(|| "a signal".to_owned(), |code| format!("code {code}"))
                ),
                next: "fix the command above; its output was streamed while it ran".to_owned(),
            })
        }
    }

    /// Spawns `crun` with piped output and forwards every line to `log` as it arrives.
    fn spawn_streaming(
        &self,
        id: &str,
        bundle: &Path,
        log: &mut dyn Write,
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
        let (sender, receiver) = mpsc::channel::<String>();
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
        for line in receiver {
            let _ = writeln!(log, "{line}");
            let _ = log.flush();
        }
        for reader in readers {
            let _ = reader.join();
        }
        child
            .wait()
            .map_err(|source| Error::io("wait", &self.host.crun, source))
    }

    fn cleanup(&self, id: &str, bundle: &Path) {
        let _ = StdCommand::new(&self.host.crun)
            .args(["--cgroup-manager=disabled", "delete", "--force", id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = fs::remove_dir_all(bundle);
    }

    fn oci_config(
        &self,
        rootfs: &Path,
        config: &ImageConfig,
        run: &RunSpec,
        cache: &[(CacheMount, PathBuf)],
        passwd: &Path,
    ) -> serde_json::Value {
        let args = match &run.command {
            StepCommand::Shell(script) => {
                vec!["/bin/sh".to_owned(), "-c".to_owned(), script.clone()]
            }
            StepCommand::Exec(argv) => argv.clone(),
        };
        let mut env = vec![format!("PATH={DEFAULT_PATH}")];
        for (key, value) in &config.env {
            env.push(format!("{key}={value}"));
        }
        let mut mounts = vec![
            serde_json::json!({"destination": "/proc", "type": "proc", "source": "proc"}),
            serde_json::json!({
                "destination": "/dev",
                "type": "tmpfs",
                "source": "tmpfs",
                "options": ["nosuid", "strictatime", "mode=755", "size=65536k"]
            }),
            serde_json::json!({
                "destination": "/etc/resolv.conf",
                "type": "bind",
                "source": self.host.resolv_conf,
                "options": ["ro", "rbind"]
            }),
            serde_json::json!({
                "destination": "/etc/passwd",
                "type": "bind",
                "source": passwd,
                "options": ["ro", "rbind"]
            }),
        ];
        for (mount, host_dir) in cache {
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
        serde_json::json!({
            "ociVersion": "1.0.2",
            "process": {
                "terminal": false,
                "user": {"uid": self.host.uid, "gid": self.host.gid},
                "args": args,
                "env": env,
                "cwd": config.workdir,
                "noNewPrivileges": false
            },
            "root": {"path": rootfs, "readonly": false},
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
                "uidMappings": [{"containerID": self.host.uid, "hostID": self.host.uid, "size": 1}],
                "gidMappings": [{"containerID": self.host.gid, "hostID": self.host.gid, "size": 1}],
                "maskedPaths": [],
                "readonlyPaths": []
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
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Stdout(pipe) => pipe.read(buf),
            Self::Stderr(pipe) => pipe.read(buf),
        }
    }
}

fn stream_lines(reader: Reader, sender: &Sender<String>) {
    for line in BufReader::new(reader).lines() {
        let Ok(line) = line else { break };
        if sender.send(line).is_err() {
            break;
        }
    }
}

/// Resolves a container-absolute path inside `rootfs`, refusing escapes.
pub(crate) fn rooted(rootfs: &Path, destination: &str) -> PathBuf {
    let trimmed = destination.trim_start_matches('/');
    rootfs.join(trimmed)
}
