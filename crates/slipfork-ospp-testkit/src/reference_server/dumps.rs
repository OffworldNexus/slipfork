//! The reference server's out-of-band sealed dump endpoint (plaintext HTTP on loopback only).

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use super::frames::seal_dump;
use super::{IO_TIMEOUT, MAX_HTTP_HEADERS, POLL_SLICE, Shared, lock};

pub(super) fn http_loop(listener: TcpListener, shared: Arc<Shared>) {
    while !shared.stop.load(std::sync::atomic::Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _peer)) => handle_http(stream, &shared),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_SLICE);
            }
            Err(_) => break,
        }
    }
}

fn handle_http(mut stream: TcpStream, shared: &Arc<Shared>) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    });
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut headers = 0usize;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(read) => {
                headers += read;
                if headers > MAX_HTTP_HEADERS || line == "\r\n" || line == "\n" {
                    break;
                }
            }
            Err(_) => return,
        }
    }
    let path = request_line.split_whitespace().nth(1).unwrap_or_default();
    let Some(token) = path.strip_prefix("/dumps/") else {
        let _ = respond(&mut stream, 404, b"not found");
        return;
    };
    let entry = lock(&shared.dumps).remove(token);
    let Some(entry) = entry else {
        let _ = respond(&mut stream, 404, b"unknown dump token");
        return;
    };
    match seal_dump(
        &entry.public_key,
        &entry.subscription_id,
        &entry.resource,
        &entry.bytes,
        &shared.entropy,
    ) {
        Ok(body) => {
            let _ = respond(&mut stream, 200, &body);
        }
        Err(_) => {
            let _ = respond(&mut stream, 500, b"sealing failure");
        }
    }
}

fn respond(stream: &mut TcpStream, status: u16, body: &[u8]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\ncontent-length: {}\r\ncontent-type: application/octet-stream\r\nconnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}
