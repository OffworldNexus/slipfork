//! Shared loopback `Distribution v2` HTTP primitives for the docker test labs.
//!
//! Only byte-level framing lives here: each suite owns its listener loop, registry state,
//! challenge headers, and token policy. The hex helper is shared by the live suite's real OCI
//! manifests, so the module silences `dead_code` for the non-live build.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use sha2::{Digest, Sha256};

/// Reads one HTTP request head: the request line plus lowercased header names.
pub fn read_head(stream: &mut TcpStream) -> Option<(String, BTreeMap<String, String>)> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    if buffer.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&buffer);
    let mut lines = text.split("\r\n");
    let line = lines.next()?.to_owned();
    let mut headers = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    Some((line, headers))
}

/// Writes one `Connection: close` HTTP response with an explicit `Content-Length`.
pub fn respond(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(&str, String)],
    body: &[u8],
) {
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

/// One scoped `WWW-Authenticate: Bearer` challenge header.
pub fn challenge(realm: &str, service: &str, scope: &str) -> String {
    format!("Bearer realm=\"{realm}\",service=\"{service}\",scope=\"{scope}\"")
}

/// `sha256:<hex>` of a manifest, config, or layer body.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2 + 7);
    out.push_str("sha256:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
