//! The single owner of SIGKILL-based process-group and world-tree teardown.
//!
//! Both the host supervisor (survivors that escaped the launcher group) and the in-namespace
//! init (containers `crun` left behind) must kill a process *and* the group it leads. On the host,
//! `__supervise` calls `setsid(2)` (`session == supervisor_pid`) while `WorldSupervisor::boot` and
//! `WorldEgress::attach` put `unshare`/`slipfork-world-init` and `slirp4netns` into dedicated
//! `.process_group(0)` groups (`pgid != supervisor_pid`), so killing `-supervisor_pid` alone
//! misses both helper groups and re-parents them to PID 1. [`reap_world_tree`] sweeps every
//! non-zombie survivor in `session == supervisor_pid` or carrying `--pods <pods_dir>` and waits
//! until `/proc` proves none remain; per `pid_namespaces(7)` (`zap_pid_ns_processes`), child
//! PID-namespace PID 1 (`slipfork-world-init`) exiting also guarantees every pod inside that
//! namespace has been `SIGKILL`ed and reaped by the kernel.

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// Kills `pid`, and its whole group when `pid` really leads that group.
///
/// A non-positive pid is ignored: `kill(-0, …)` would signal this process's own group.
pub fn kill_group(pid: i32) {
    if pid <= 0 {
        return;
    }
    // SAFETY: `kill(2)`/`getpgid(2)` never dereference memory.
    unsafe {
        if libc::getpgid(pid) == pid {
            libc::kill(-pid, libc::SIGKILL);
        } else {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

/// Kills `supervisor_pid`'s group plus any escaped world groups (`unshare`, `slipfork-world-init`,
/// `slirp4netns`) in `session == supervisor_pid` or matching `--pods <pods_dir>`, then polls `/proc`
/// up to `timeout` until zero non-zombie processes remain.
pub fn reap_world_tree(supervisor_pid: u32, pods_dir: Option<&Path>, timeout: Duration) {
    if let Ok(pid) = i32::try_from(supervisor_pid) {
        kill_group(pid);
    }
    let candidates = pods_candidates(pods_dir);
    if supervisor_pid <= 1 && candidates.is_empty() {
        return;
    }
    let deadline = Instant::now() + timeout;
    loop {
        let survivors = world_survivors(supervisor_pid, &candidates);
        if survivors.is_empty() {
            return;
        }
        for pid in survivors {
            kill_group(pid);
        }
        if Instant::now() >= deadline {
            return;
        }
        thread::sleep(Duration::from_millis(15));
    }
}

fn pods_candidates(pods_dir: Option<&Path>) -> Vec<Vec<u8>> {
    let Some(dir) = pods_dir else {
        return Vec::new();
    };
    let raw = dir.as_os_str().as_bytes().to_vec();
    if raw.is_empty() {
        return Vec::new();
    }
    let mut out = vec![raw];
    let canonical = fs::canonicalize(dir).ok().or_else(|| {
        let parent = fs::canonicalize(dir.parent()?).ok()?;
        Some(parent.join(dir.file_name()?))
    });
    if let Some(canonical) = canonical {
        let bytes = canonical.as_os_str().as_bytes().to_vec();
        if !out.contains(&bytes) {
            out.push(bytes);
        }
    }
    out
}

fn world_survivors(supervisor_pid: u32, pods_paths: &[Vec<u8>]) -> Vec<i32> {
    let self_pid = std::process::id();
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid <= 1 || pid == self_pid {
            continue;
        }
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some((state, session)) = parse_stat_state_and_session(&stat) else {
            continue;
        };
        if state == "Z" {
            continue;
        }
        let in_session = supervisor_pid > 1 && session == supervisor_pid;
        let in_pods = !pods_paths.is_empty()
            && fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|cmdline| cmdline_has_pods(&cmdline, pods_paths));
        if in_session || in_pods {
            if let Ok(pid) = i32::try_from(pid) {
                found.push(pid);
            }
        }
    }
    found
}

fn parse_stat_state_and_session(stat: &str) -> Option<(&str, u32)> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?;
    let _ppid = fields.next()?;
    let _pgrp = fields.next()?;
    let session = fields.next()?.parse::<u32>().ok()?;
    Some((state, session))
}

fn cmdline_has_pods(cmdline: &[u8], candidates: &[Vec<u8>]) -> bool {
    let args: Vec<&[u8]> = cmdline.split(|byte| *byte == 0).collect();
    args.windows(2).any(|pair| {
        pair[0] == b"--pods"
            && candidates
                .iter()
                .any(|candidate| pair[1] == candidate.as_slice())
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    use super::{cmdline_has_pods, parse_stat_state_and_session, reap_world_tree};

    #[test]
    fn stat_and_cmdline_matchers_handle_comm_parens_and_exact_pods_argv() {
        let stat = "4202 (slipfork (init) worker) S 1 4201 4200 0 -1 4194304 0 0 0 0";
        assert_eq!(parse_stat_state_and_session(stat), Some(("S", 4200)));
        let zombie = "4202 (slipfork-world-init) Z 1 4201 4200 0 -1 4194304 0 0 0 0";
        assert_eq!(parse_stat_state_and_session(zombie), Some(("Z", 4200)));

        let candidates = vec![b"/run/user/1000/slipfork/pods/good".to_vec()];
        let exact = b"/usr/bin/unshare\0--fork\0--\0slipfork-world-init\0--pods\0/run/user/1000/slipfork/pods/good\0";
        let sibling = b"slipfork-world-init\0--pods\0/run/user/1000/slipfork/pods/good-other\0";
        let joined = b"slipfork-world-init\0--pods=/run/user/1000/slipfork/pods/good\0";
        assert!(cmdline_has_pods(exact, &candidates));
        assert!(!cmdline_has_pods(sibling, &candidates));
        assert!(!cmdline_has_pods(joined, &candidates));
    }

    #[test]
    fn reap_world_tree_reaps_reparented_process_group_leaders() {
        let dir = tempfile::Builder::new()
            .prefix("slipfork-reap-tree-")
            .tempdir_in("/var/tmp")
            .expect("disk temp dir");
        let pods_dir = dir.path().join("pods/demo");
        fs::create_dir_all(&pods_dir).expect("create pods dir");
        let pid_file = dir.path().join("escaped.pid");

        // Mirrors `__supervise` (`setsid`) spawning `unshare` (`.process_group(0)` with `--pods`)
        // and then dying so the escaped group leader is re-parented to PID 1.
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "python3 -c 'import subprocess, sys; \
                 p = subprocess.Popen([\"/bin/sh\", \"-c\", \"sleep 30\", \"sh\", \"--pods\", sys.argv[1]], start_new_session=False, process_group=0); \
                 open(sys.argv[2], \"w\").write(str(p.pid))' \"$1\" \"$2\"",
                "sh",
            ])
            .arg(&pods_dir)
            .arg(&pid_file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: `setsid(2)` only moves the spawned shell into its own session, like `__supervise`.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut leader = command.spawn().expect("spawn session leader");
        let leader_pid = leader.id();
        let status = leader.wait().expect("reap session leader");
        assert!(status.success(), "leader helper must succeed: {status}");

        let escaped: u32 = fs::read_to_string(&pid_file)
            .expect("read escaped pid")
            .trim()
            .parse()
            .expect("parse escaped pid");
        assert!(
            Path::new(&format!("/proc/{escaped}")).exists(),
            "escaped child must outlive its exited session leader before reap_world_tree"
        );

        reap_world_tree(leader_pid, Some(&pods_dir), Duration::from_secs(5));
        assert!(
            !Path::new(&format!("/proc/{escaped}")).exists(),
            "escaped group leader {escaped} must be reaped"
        );
    }
}
