//! The single owner of SIGKILL-based process-group teardown.
//!
//! Both the host supervisor (survivors that escaped the launcher group) and the in-namespace
//! init (containers `crun` left behind) must kill a process *and* the group it leads. One
//! implementation means one `getpgid(2)` policy and no verbatim forks.

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
