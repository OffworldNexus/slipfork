//! Bounded byte relay shared by every stream splice in the workspace.
//!
//! One chunk is at most [`RELAY_CAP`] bytes and is flushed as soon as it is read, so a request or
//! response body larger than the buffer streams through with constant memory and half-close
//! propagation instead of being buffered to completion.
//!
//! [`pipe`] is the single bidirectional relay: it owns the stream clones, the uplink thread, the
//! half-close handshake, and the join, so callers only supply the two endpoints. The edge ingress
//! relays TCP/AF_UNIX client streams through it, and `slipfork-world-init` uses the same code to
//! bridge in-world loopback traffic to `slipforkd`.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::Duration;

/// Chunk size for relayed bytes in either direction.
pub const RELAY_CAP: usize = 16 * 1024;

/// One whole-duplex stream `pipe` can relay in both directions.
pub trait Duplex: Read + Write + Send + 'static {
    /// Duplicate the underlying descriptor (each direction and the teardown need their own).
    fn try_clone_duplex(&self) -> io::Result<Self>
    where
        Self: Sized;

    /// Half-close the write side so the peer observes EOF without losing the read side.
    fn shutdown_write_half(&self);

    /// Close both directions, unblocking anything still reading or writing.
    fn shutdown_both(&self);

    /// Arm (`Some`) or clear (`None`) the read and write idle bound.
    fn set_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
}

impl Duplex for TcpStream {
    fn try_clone_duplex(&self) -> io::Result<Self> {
        self.try_clone()
    }

    fn shutdown_write_half(&self) {
        let _ = self.shutdown(Shutdown::Write);
    }

    fn shutdown_both(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }

    fn set_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(timeout)?;
        self.set_write_timeout(timeout)
    }
}

impl Duplex for UnixStream {
    fn try_clone_duplex(&self) -> io::Result<Self> {
        self.try_clone()
    }

    fn shutdown_write_half(&self) {
        let _ = self.shutdown(Shutdown::Write);
    }

    fn shutdown_both(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }

    fn set_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(timeout)?;
        self.set_write_timeout(timeout)
    }
}

/// Copy until EOF or an error, flushing after every chunk.
///
/// A read error or a write error ends the copy instead of panicking: the peer is gone, and the
/// caller's subsequent half-close/shutdown reports the outcome to the other direction.
fn copy<R: Read, W: Write>(reader: &mut R, writer: &mut W) {
    let mut buffer = [0_u8; RELAY_CAP];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                if writer.write_all(&buffer[..read]).is_err() || writer.flush().is_err() {
                    return;
                }
            }
        }
    }
}

/// Relay bytes in both directions until either side closes.
///
/// One direction runs on a worker thread while the other runs on the caller's thread; both
/// half-close their peer's write side on EOF, and both sockets are fully shut down afterwards so
/// the worker's pending read/write returns and is joined. The idle bound armed on the endpoints is
/// preserved for the whole exchange — callers that legitimately idle (an upgraded WebSocket) clear
/// it first via [`Duplex::set_timeout`].
pub fn pipe<A: Duplex, B: Duplex>(a: &mut A, b: &mut B) -> io::Result<()> {
    let mut a_read = a.try_clone_duplex()?;
    let a_control = a.try_clone_duplex()?;
    let mut b_read = b.try_clone_duplex()?;
    let b_control = b.try_clone_duplex()?;
    let mut b_writer = b.try_clone_duplex()?;

    let uplink = thread::spawn(move || {
        copy(&mut a_read, &mut b_writer);
        b_writer.shutdown_write_half();
    });
    copy(&mut b_read, a);
    a.shutdown_write_half();
    a_control.shutdown_both();
    b_control.shutdown_both();
    let _ = uplink.join();
    Ok(())
}
