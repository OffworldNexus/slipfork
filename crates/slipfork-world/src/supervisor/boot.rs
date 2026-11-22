//! Launching the world init and polling its NDJSON channel with hard timeouts.

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::control::{self, InitFrame};
use crate::error::{Error, Result};
use crate::layout::WorldName;
use crate::namespaces::{NamespaceIdentity, NamespaceTable};
use crate::runtime_dir::RuntimePods;

use super::{LiveWorld, WorldHost, WorldSupervisor};

/// Spawns `unshare` around the world init and proves the ready handshake.
pub(super) fn boot(host: WorldHost, world: &WorldName) -> Result<WorldSupervisor> {
    for (tool, path) in [
        ("crun", &host.crun),
        ("unshare", &host.unshare),
        ("slipfork-world-init", &host.world_init),
    ] {
        if !path.is_file() {
            return Err(Error::MissingTool {
                tool,
                path: path.clone(),
            });
        }
    }
    let pods = RuntimePods::prepare(&host.runtime_dir, world)?;
    let inside_file = pods.path().join("namespaces.inside.json");
    let mut command = Command::new(&host.unshare);
    command
        .args([
            "--user",
            "--map-current-user",
            // Identity mapping alone clears the capability sets on `setresuid`, so `lo` setup
            // needs the explicit keep; this is verified live on the lab VM.
            "--keep-caps",
            "--net",
            "--ipc",
            "--pid",
            "--fork",
            "--mount-proc",
            "--kill-child=SIGKILL",
            "--",
        ])
        .arg(&host.world_init)
        .arg("--namespaces")
        .arg(&inside_file)
        .arg("--pods")
        .arg(pods.path())
        .arg("--crun")
        .arg(&host.crun)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .process_group(0);
    let mut launcher = command.spawn().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            Error::MissingTool {
                tool: "unshare",
                path: host.unshare.clone(),
            }
        } else {
            Error::io("spawn", &host.unshare, source)
        }
    })?;
    let pgid = i32::try_from(launcher.id()).unwrap_or(i32::MAX);
    let stdin = launcher.stdin.take().ok_or_else(|| Error::Init {
        message: "unshare gave no stdin pipe".to_owned(),
    })?;
    let stdout = BufReader::new(launcher.stdout.take().ok_or_else(|| Error::Init {
        message: "unshare gave no stdout pipe".to_owned(),
    })?);
    let mut live = LiveWorld {
        launcher,
        stdin,
        stdout,
        pgid,
        init_pid: 0,
        // Placeholder until the handshake resolves the real init pid.
        namespaces: NamespaceTable::from_proc(std::process::id())?,
        next_request: 0,
        pods: Vec::new(),
    };
    let ready = await_ready(&mut live, host.boot_timeout);
    let (init_pid, identity) = match ready {
        Ok(ready) => ready,
        Err(err) => {
            let _ = live.launcher.kill();
            let _ = live.launcher.wait();
            return Err(err);
        }
    };
    let namespaces = NamespaceTable::from_proc(init_pid)?;
    namespaces.verify(&identity)?;
    namespaces.write_json(&pods.path().join("namespaces.json"))?;
    live.init_pid = init_pid;
    live.namespaces = namespaces;
    Ok(WorldSupervisor {
        host,
        world: world.clone(),
        pods,
        live: Some(live),
    })
}

fn await_ready(live: &mut LiveWorld, timeout: Duration) -> Result<(u32, NamespaceIdentity)> {
    let deadline = Instant::now() + timeout;
    loop {
        let line = read_line(&mut live.stdout, deadline)?.ok_or_else(|| Error::Init {
            message: "slipfork-world-init exited before its ready handshake".to_owned(),
        })?;
        match control::decode_init_frame(line.trim())? {
            InitFrame::Ready { namespaces, .. } => {
                let host_pid = resolve_init_pid(live.launcher.id(), timeout)?;
                return Ok((host_pid, namespaces));
            }
            InitFrame::Stopped { .. } => {
                return Err(Error::Init {
                    message: "world init stopped during boot".to_owned(),
                });
            }
            InitFrame::Reply { .. } | InitFrame::Output { .. } => {}
        }
    }
}

/// Finds the launcher's child whose `pid` namespace inode matches the reported identity.
fn resolve_init_pid(launcher_pid: u32, timeout: Duration) -> Result<u32> {
    let deadline = Instant::now() + timeout;
    let children = format!("/proc/{launcher_pid}/task/{launcher_pid}/children");
    loop {
        if let Ok(text) = fs::read_to_string(&children) {
            for pid in text
                .split_whitespace()
                .filter_map(|value| value.parse::<u32>().ok())
            {
                if NamespaceTable::from_proc(pid).is_ok() {
                    return Ok(pid);
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::Init {
                message: format!("no init process below launcher {launcher_pid}"),
            });
        }
        thread::sleep(Duration::from_millis(1));
    }
}

pub(super) fn await_reply(
    live: &mut LiveWorld,
    request_id: &str,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let line = read_line(&mut live.stdout, deadline)?.ok_or_else(|| Error::Init {
            message: "world init closed its control channel".to_owned(),
        })?;
        match control::decode_init_frame(line.trim())? {
            InitFrame::Reply { id, ok, error, .. } if id == request_id => {
                if let Some(error) = error {
                    // Only pod launches answer with an error frame; the remedy is the runtime's.
                    let message = if error.message.is_empty() {
                        error.code
                    } else {
                        error.message
                    };
                    return Err(Error::PodLaunch { message });
                }
                return Ok(ok.unwrap_or(serde_json::Value::Null));
            }
            InitFrame::Reply { id, .. } => {
                return Err(Error::Protocol {
                    message: format!("unexpected reply id `{id}`"),
                });
            }
            InitFrame::Stopped { .. } => {
                return Err(Error::Init {
                    message: "world init stopped while a request was pending".to_owned(),
                });
            }
            InitFrame::Ready { .. } | InitFrame::Output { .. } => {}
        }
    }
}

fn read_line(
    reader: &mut BufReader<std::process::ChildStdout>,
    deadline: Instant,
) -> Result<Option<String>> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::Timeout {
                action: "wait for slipfork-world-init",
            });
        }
        if !poll_readable(reader.get_ref().as_raw_fd(), remaining)? {
            continue;
        }
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|source| Error::io("read", Path::new("slipfork-world-init stdout"), source))?;
        if read == 0 {
            return Ok(None);
        }
        return Ok(Some(line));
    }
}

fn poll_readable(fd: RawFd, timeout: Duration) -> Result<bool> {
    let millis = i32::try_from(timeout.as_millis().max(1)).unwrap_or(i32::MAX);
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid `pollfd` lives across the call.
    let ret = unsafe { libc::poll(&mut pollfd, 1, millis) };
    if ret < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(Error::io(
            "poll",
            Path::new("slipfork-world-init stdout"),
            error,
        ));
    }
    Ok(ret > 0)
}

/// Non-zombie processes still in `pgid`; the reap proof, never a `pgrep` guess.
pub(super) fn group_members(pgid: i32) -> Result<Vec<u32>> {
    let mut members = Vec::new();
    let entries =
        fs::read_dir("/proc").map_err(|source| Error::io("read", Path::new("/proc"), source))?;
    for entry in entries {
        let entry = entry.map_err(|source| Error::io("read", Path::new("/proc"), source))?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let state = fields.next().unwrap_or_default();
        let _ppid = fields.next();
        let Some(process_group) = fields.next().and_then(|value| value.parse::<i32>().ok()) else {
            continue;
        };
        if process_group == pgid && state != "Z" {
            members.push(pid);
        }
    }
    Ok(members)
}

pub(super) fn wait_for_empty_group(pgid: i32, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let members = group_members(pgid)?;
        if members.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::LeftoverProcesses {
                count: members.len(),
                pgid,
            });
        }
        thread::sleep(Duration::from_millis(20));
    }
}
