//! Real kernel PTY `exec`: `crun exec --tty --console-socket` plus `SCM_RIGHTS` handoff.
//!
//! `crun` allocates the `/dev/pts/N` pair on the host side, keeps the slave for the process it
//! starts, and dials the caller's mode-`0600` `--console-socket` to deliver the master. The init
//! forwards that master to the caller with [`slipfork_world::fdpass::send_fd`]; raw terminal bytes
//! then flow between the caller and the child over the handed-off descriptor, untouched by this
//! module. While the child runs, the init multiplexes [`Request::ExecResize`] frames on the control
//! socket into `ioctl(TIOCSWINSZ)` on its retained descriptor, so a caller resize reaches the same
//! terminal. `TempProcessSpec::write_pty` keeps the pod's identity uid/gid and config env (the
//! `crun exec` default would `setresgid(0): EINVAL` in the identity-mapped world userns).

use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use slipfork_world::control::{self, FrameLines, InitFrame, PROTOCOL_VERSION, Request};
use slipfork_world::fdpass::{recv_fd, send_fd};
use slipfork_world::kill_group;
use slipfork_world::pod_log::CHUNK;

use crate::pod::{WorldShared, reap_zombies};
use crate::shared::wait_child;
use crate::{Fault, REMEDY_CRUN, REMEDY_PROTOCOL, REMEDY_RUNTIME};

use super::lines::{TempProcessSpec, refuse, reply};

/// How long `crun` may take to dial the console socket before the exec is refused.
const CONSOLE_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the supervisor checks the child and the control socket.
const TICK_MS: i32 = 80;

/// Everything one `ExecPty` request carries into the PTY thread.
pub(crate) struct PtyRequest {
    pub(crate) id: String,
    pub(crate) pod: String,
    pub(crate) argv: Vec<String>,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) buffered: FrameLines,
}

/// Runs one command inside a live pod on a real PTY and hands the master to the caller.
pub(crate) fn serve_exec_pty(
    shared: Arc<WorldShared>,
    request: PtyRequest,
    mut stream: UnixStream,
) -> Result<(), Fault> {
    let PtyRequest {
        id: request_id,
        pod,
        argv,
        cols,
        rows,
        buffered,
    } = request;
    let Some(info) = shared.info(&pod) else {
        return refuse(
            &mut stream,
            &request_id,
            "no-pod",
            format!("pod `{pod}` is not running"),
        );
    };
    if argv.is_empty() {
        return refuse(
            &mut stream,
            &request_id,
            "empty-argv",
            "no command given".into(),
        );
    }
    let seq = shared.next_exec_seq();
    let spec = match TempProcessSpec::write_pty(&info, seq, &argv) {
        Ok(file) => file,
        Err(fault) => {
            eprintln!("world init: {fault}");
            return refuse(&mut stream, &request_id, "exec-failed", fault.summary());
        }
    };
    let console = match ConsoleSocket::bind(shared.pods_root(), seq) {
        Ok(socket) => socket,
        Err(fault) => {
            eprintln!("world init: {fault}");
            return refuse(&mut stream, &request_id, "exec-failed", fault.summary());
        }
    };
    let mut command = shared.crun_command();
    command
        .args(["exec", "--tty", "--console-socket"])
        .arg(console.path())
        .arg("--process")
        .arg(spec.path())
        .arg(&pod)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(source) => {
            let fault = Fault::cause(
                format!("PTY exec in pod `{pod}` with crun"),
                REMEDY_CRUN,
                source,
            );
            eprintln!("world init: {fault}");
            return refuse(&mut stream, &request_id, "exec-failed", fault.summary());
        }
    };
    let child_pid = i32::try_from(child.id()).unwrap_or_default();
    shared.register_exec(child_pid);
    let master = match console.accept(&mut child) {
        Ok((connection, master)) => {
            drop(connection);
            master
        }
        Err(fault) => {
            eprintln!("world init: {fault}");
            abort(&shared, &mut child, child_pid);
            return refuse(&mut stream, &request_id, "exec-failed", fault.summary());
        }
    };
    // The initial size must be set before the caller reads `stty size` through its descriptor.
    if let Err(source) = set_size(master.as_raw_fd(), cols, rows) {
        let fault = Fault::cause("PTY initial window size", REMEDY_RUNTIME, source);
        eprintln!("world init: {fault}");
        abort(&shared, &mut child, child_pid);
        return refuse(&mut stream, &request_id, "exec-failed", fault.summary());
    }
    if send_fd(&stream, b"pty", Some(master.as_fd())).is_err() {
        // The caller vanished before it could receive the terminal; tear the child down.
        abort(&shared, &mut child, child_pid);
        return Ok(());
    }
    let outcome = match supervise(
        &stream,
        &request_id,
        master.as_raw_fd(),
        &shared,
        child_pid,
        &mut child,
        buffered,
    ) {
        Ok(outcome) => outcome,
        Err(fault) => {
            // The control socket failed mid-session: never leave the exec child orphaned.
            eprintln!("world init: {fault}");
            abort(&shared, &mut child, child_pid);
            return Err(fault);
        }
    };
    shared.unregister_exec(child_pid);
    reap_zombies(&shared);
    drop(spec);
    drop(console);
    let Some(code) = outcome else {
        // The caller closed its descriptor: the child is gone and nobody is listening.
        return Ok(());
    };
    reply(
        &mut stream,
        &InitFrame::Reply {
            v: PROTOCOL_VERSION,
            id: request_id,
            ok: Some(serde_json::json!({ "exit_code": code, "pod": pod })),
            error: None,
        },
    )
}

/// Applies `ExecResize` frames until the child exits.
///
/// Returns `Some(exit_code)` once the child is reaped, or `None` when the caller closed its end
/// (the child is then killed and reaped before returning).
fn supervise(
    mut stream: &UnixStream,
    request_id: &str,
    master: RawFd,
    shared: &WorldShared,
    child_pid: i32,
    child: &mut Child,
    mut buffered: FrameLines,
) -> Result<Option<i32>, Fault> {
    loop {
        for raw in buffered.feed(&[], CHUNK) {
            apply_resize(&raw, request_id, master);
        }
        if let Some(code) = exit_code(shared, child_pid, child) {
            return Ok(Some(code));
        }
        match poll(stream.as_raw_fd(), TICK_MS) {
            Ok(0) => continue,
            Ok(revents) => {
                if revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                    kill_group(child_pid);
                    let _ = wait_child(child, shared, child_pid);
                    return Ok(None);
                }
                if revents & (libc::POLLIN | libc::POLLRDHUP) == 0 {
                    continue;
                }
            }
            Err(source) => {
                return Err(Fault::cause(
                    "PTY control socket poll",
                    REMEDY_PROTOCOL,
                    source,
                ));
            }
        }
        let mut chunk = [0_u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => {
                kill_group(child_pid);
                let _ = wait_child(child, shared, child_pid);
                return Ok(None);
            }
            Ok(read) => {
                for raw in buffered.feed(&chunk[..read], CHUNK) {
                    apply_resize(&raw, request_id, master);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => {
                kill_group(child_pid);
                let _ = wait_child(child, shared, child_pid);
                return Ok(None);
            }
        }
    }
}

fn apply_resize(raw: &[u8], request_id: &str, master: RawFd) {
    let Ok(text) = std::str::from_utf8(raw) else {
        return;
    };
    let Ok(Request::ExecResize { id, cols, rows, .. }) = control::decode_request(text.trim())
    else {
        return;
    };
    if id == request_id {
        let _ = set_size(master, cols, rows);
    }
}

/// The child's exit code once it is done, recovering statuses reaped by the main loop.
fn exit_code(shared: &WorldShared, child_pid: i32, child: &mut Child) -> Option<i32> {
    if let Some(code) = shared.take_status(child_pid) {
        return Some(code);
    }
    match child.try_wait() {
        Ok(Some(status)) => Some(
            status
                .code()
                .or_else(|| status.signal().map(|sig| 128 + sig))
                .unwrap_or(128),
        ),
        Ok(None) => None,
        Err(_) => shared.take_status(child_pid),
    }
}

fn abort(shared: &WorldShared, child: &mut Child, child_pid: i32) {
    if child_pid != 0 {
        kill_group(child_pid);
    }
    let _ = wait_child(child, shared, child_pid);
    shared.unregister_exec(child_pid);
    reap_zombies(shared);
}

/// A mode-`0600` `--console-socket` listener under the world's tmpfs pod root.
struct ConsoleSocket {
    listener: UnixListener,
    path: PathBuf,
}

impl ConsoleSocket {
    fn bind(pods_root: &Path, seq: u64) -> Result<Self, Fault> {
        let path = control::console_socket_path(pods_root, seq)
            .map_err(|source| Fault::cause("PTY console socket path", REMEDY_RUNTIME, source))?;
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).map_err(|source| {
            Fault::cause(
                format!("PTY console socket bind at `{}`", path.display()),
                REMEDY_RUNTIME,
                source,
            )
        })?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(
            |source| {
                Fault::cause(
                    format!("PTY console socket chmod at `{}`", path.display()),
                    REMEDY_RUNTIME,
                    source,
                )
            },
        )?;
        listener.set_nonblocking(true).map_err(|source| {
            Fault::cause(
                "PTY console socket nonblocking mode",
                REMEDY_RUNTIME,
                source,
            )
        })?;
        Ok(Self { listener, path })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Waits for `crun` to dial the listener and deliver the `/dev/pts/N` master descriptor.
    fn accept(&self, child: &mut Child) -> Result<(UnixStream, OwnedFd), Fault> {
        let deadline = Instant::now() + CONSOLE_TIMEOUT;
        loop {
            match self.listener.accept() {
                Ok((connection, _)) => {
                    connection.set_nonblocking(false).map_err(|source| {
                        Fault::cause("PTY console connection mode", REMEDY_RUNTIME, source)
                    })?;
                    connection
                        .set_read_timeout(Some(CONSOLE_TIMEOUT))
                        .map_err(|source| {
                            Fault::cause(
                                "PTY console connection read timeout",
                                REMEDY_RUNTIME,
                                source,
                            )
                        })?;
                    let (_payload, master) = match recv_fd(&connection) {
                        Ok(received) => received,
                        Err(source)
                            if matches!(
                                source.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                            ) =>
                        {
                            return Err(Fault::timed_out(
                                "crun sent no PTY master descriptor",
                                REMEDY_CRUN,
                            ));
                        }
                        Err(source) => {
                            return Err(Fault::cause(
                                "PTY console descriptor receive",
                                REMEDY_PROTOCOL,
                                source,
                            ));
                        }
                    };
                    let master = master.ok_or_else(|| {
                        Fault::new(
                            "crun sent no PTY master descriptor",
                            "re-run `slipfork shell` against a live pod",
                        )
                    })?;
                    return Ok((connection, master));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(source) => {
                    return Err(Fault::cause(
                        "PTY console socket accept",
                        REMEDY_PROTOCOL,
                        source,
                    ));
                }
            }
            if let Ok(Some(status)) = child.try_wait() {
                return Err(Fault::new(
                    format!(
                        "crun PTY exec exited with {} before dialing the console socket{}",
                        status.code().unwrap_or(-1),
                        stderr_note(child)
                    ),
                    REMEDY_CRUN,
                ));
            }
            if Instant::now() >= deadline {
                kill_group(i32::try_from(child.id()).unwrap_or_default());
                return Err(Fault::timed_out(
                    "crun did not dial the PTY console socket",
                    REMEDY_CRUN,
                ));
            }
            let _ = poll(self.listener.as_raw_fd(), TICK_MS);
        }
    }
}

impl Drop for ConsoleSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn stderr_note(child: &mut Child) -> String {
    let Some(mut stderr) = child.stderr.take() else {
        return String::new();
    };
    let mut note = String::new();
    let _ = stderr.read_to_string(&mut note);
    let note = note.trim();
    if note.is_empty() {
        String::new()
    } else {
        format!(": {note}")
    }
}

fn set_size(fd: RawFd, cols: u16, rows: u16) -> io::Result<()> {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `fd` is a live terminal descriptor and `size` is a valid `winsize` for the call.
    let ret = unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &size) };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn poll(fd: RawFd, timeout_ms: i32) -> io::Result<libc::c_short> {
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN | libc::POLLRDHUP,
        revents: 0,
    };
    // SAFETY: one valid `pollfd` lives across the call and the kernel writes only `revents`.
    let ret = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
    if ret < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(0);
        }
        return Err(error);
    }
    Ok(if ret == 0 { 0 } else { pollfd.revents })
}
