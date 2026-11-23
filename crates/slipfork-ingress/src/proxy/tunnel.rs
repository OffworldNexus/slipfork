//! Raw bidirectional tunnel for an upgraded connection.

use std::io;
use std::net::{Shutdown, TcpStream};
use std::thread;

use super::body::pump_bytes;

/// Relay bytes in both directions until either side closes, then shut both sockets down and join
/// the uplink thread. The tunnel is entered only after a `101 Switching Protocols` head, so no
/// idle timeout is armed here.
pub(crate) fn tunnel(mut client: TcpStream, upstream: TcpStream) -> io::Result<()> {
    client.set_read_timeout(None)?;
    client.set_write_timeout(None)?;
    upstream.set_read_timeout(None)?;
    upstream.set_write_timeout(None)?;
    let mut client_read = client.try_clone()?;
    let mut upstream_write = upstream.try_clone()?;
    let mut upstream_read = upstream.try_clone()?;
    let uplink = thread::spawn(move || pump_bytes(&mut client_read, &mut upstream_write));
    let _ = pump_bytes(&mut upstream_read, &mut client);
    let _ = client.shutdown(Shutdown::Both);
    let _ = upstream.shutdown(Shutdown::Both);
    let _ = uplink.join();
    Ok(())
}
