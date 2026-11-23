//! `shell` and `exec` relays: a real PTY master, or typed duplex frames for a pipe.
//!
//! Both tty verbs converge on [`Daemon::relay_world_pty`], so descriptor passing, resize pumping,
//! and the final `{"exit":n}` exist once. `exec` with `tty: true` reuses `shell`'s machinery but
//! keeps the caller's argv, while `shell` still builds bash/`psql`/`redis-cli` argv in [`super::pty`].

use std::io::Read;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use slipfork_world::control::{FrameLines, InitFrame, StreamFrame};

use super::super::frames::{FrameReader, write_frame};
use super::super::{Daemon, Failure, wire};
use super::pty::PtyLink;
use super::stream::finish;

impl Daemon {
    /// Relays one `shell`: a real PTY master for a terminal, or the `exec` pipe path otherwise.
    pub(crate) fn handle_shell(
        &self,
        request: &wire::Request,
        reader: &mut FrameReader,
        writer: &mut UnixStream,
    ) -> Result<Value, Failure> {
        let tty = request
            .params
            .get("tty")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !tty {
            let params = self.core()?.shell_exec_params(&request.params)?;
            let exec = wire::Request {
                v: request.v,
                id: request.id.clone(),
                method: "exec".to_owned(),
                params,
            };
            return self.handle_exec(&exec, reader, writer);
        }
        let link = self.core()?.open_pty(&request.params)?;
        self.relay_world_pty(reader, writer, link)
    }

    /// Relays one `exec`: a PTY master under `tty: true`, else world frames plus client input.
    pub(crate) fn handle_exec(
        &self,
        request: &wire::Request,
        reader: &mut FrameReader,
        writer: &mut UnixStream,
    ) -> Result<Value, Failure> {
        let tty = request
            .params
            .get("tty")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if tty {
            let link = self.core()?.open_exec_pty(&request.params)?;
            return self.relay_world_pty(reader, writer, link);
        }
        let (mut client, stdin) = self.core()?.open_exec(&request.params)?;
        let id = request.id.clone();
        let pump = if stdin {
            Some(
                client
                    .input_writer()
                    .map_err(|error| Failure::new("world-unavailable", error.to_string()))?,
            )
        } else {
            None
        };
        std::thread::scope(|scope| {
            let handle = pump
                .map(|input| scope.spawn(move || super::super::frames::pump_input(reader, input)));
            let result = (|| -> Result<Value, Failure> {
                loop {
                    match client.poll() {
                        Ok(Some(StreamFrame::Output(event))) => {
                            let frame = wire::event(
                                &id,
                                json!({
                                    "kind": event.kind.as_str(),
                                    "pod": event.pod,
                                    "stream": event.stream.as_str(),
                                    "line": event.line,
                                }),
                            );
                            if write_frame(writer, &frame).is_err() {
                                return Err(Failure::new(
                                    "client-gone",
                                    "the client closed during `exec`",
                                ));
                            }
                        }
                        Ok(Some(StreamFrame::Reply { ok, error })) => {
                            let value = finish(ok, error)?;
                            let code = value.get("exit_code").and_then(Value::as_i64).unwrap_or(-1);
                            return Ok(json!({ "exit": code }));
                        }
                        Ok(None) => thread::sleep(Duration::from_millis(40)),
                        Err(error) => {
                            return Err(Failure::new("world-stream", error.to_string()));
                        }
                    }
                }
            })();
            let _ = writer.shutdown(std::net::Shutdown::Read);
            if let Some(handle) = handle {
                let _ = handle.join();
            }
            result
        })
    }

    /// The one PTY relay: hand the master to the client, pump resizes, read the final `exit`.
    fn relay_world_pty(
        &self,
        reader: &mut FrameReader,
        writer: &mut UnixStream,
        PtyLink { mut socket, master }: PtyLink,
    ) -> Result<Value, Failure> {
        slipfork_world::fdpass::send_fd(writer, b"pty", Some(master.as_fd())).map_err(|error| {
            Failure::new("client-gone", format!("send the PTY descriptor: {error}"))
        })?;
        drop(master);
        let resizes = socket.try_clone().map_err(|error| {
            Failure::new(
                "world-unavailable",
                format!("clone the PTY socket: {error}"),
            )
        })?;
        std::thread::scope(|scope| {
            let handle = scope.spawn(move || super::super::frames::pump_resizes(reader, resizes));
            let result = (|| -> Result<Value, Failure> {
                let mut lines = FrameLines::new();
                loop {
                    if super::super::frames::peer_closed(writer) {
                        // Dropping `socket` below hangs up on the world; its supervisor sees HUP
                        // and reaps the whole pod process group.
                        return Err(Failure::new(
                            "client-gone",
                            "the client closed during the PTY relay",
                        ));
                    }
                    let mut pollfd = libc::pollfd {
                        fd: socket.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: one valid `pollfd` lives across the call; 100ms bounds the wait so
                    // a vanished client is noticed promptly.
                    let ready = unsafe { libc::poll(&mut pollfd, 1, 100) };
                    if ready < 0 {
                        let error = std::io::Error::last_os_error();
                        if error.kind() == std::io::ErrorKind::Interrupted {
                            continue;
                        }
                        return Err(Failure::new(
                            "world-stream",
                            format!("poll the PTY reply: {error}"),
                        ));
                    }
                    if ready == 0 {
                        continue;
                    }
                    let mut chunk = [0_u8; 4096];
                    let read = socket.read(&mut chunk).map_err(|error| {
                        Failure::new("world-stream", format!("read the PTY reply: {error}"))
                    })?;
                    if read == 0 {
                        return Err(Failure::new(
                            "world-stream",
                            "the world closed the PTY control socket",
                        ));
                    }
                    lines.extend(&chunk[..read]);
                    let Some(raw) = lines.next_line() else {
                        continue;
                    };
                    let text = std::str::from_utf8(&raw).map_err(|error| {
                        Failure::new("world-stream", format!("non-UTF-8 PTY reply: {error}"))
                    })?;
                    match slipfork_world::control::decode_init_frame(text.trim()) {
                        Ok(InitFrame::Reply { ok, error, .. }) => {
                            let value = finish(ok, error)?;
                            let code = value.get("exit_code").and_then(Value::as_i64).unwrap_or(-1);
                            return Ok(json!({ "exit": code }));
                        }
                        Ok(_) => {}
                        Err(error) => {
                            return Err(Failure::new("world-stream", error.to_string()));
                        }
                    }
                }
            })();
            // The terminal is over; end the resize reader so the scope can join.
            let _ = writer.shutdown(std::net::Shutdown::Read);
            let _ = handle.join();
            result
        })
    }
}
