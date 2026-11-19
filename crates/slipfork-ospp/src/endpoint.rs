//! The single injected OSPP v1 server identity.
//!
//! `OsppEndpoint` validates exactly one URL shape at the Composition Root and is
//! then the sole authority across `connect`, `subscribe`, `dump`, and `auth_pair`:
//! `wss://` anywhere, and plaintext `ws://` strictly on loopback so the in-tree
//! reference server can run without a trusted certificate.

use std::fmt;

use crate::error::OsppError;

/// Validated `ospp-v1` WebSocket endpoint.
#[derive(Clone, PartialEq, Eq)]
pub struct OsppEndpoint {
    raw: String,
    host: String,
    port: u16,
    path: String,
    secure: bool,
}

impl OsppEndpoint {
    /// Parse and validate an endpoint URL.
    pub fn parse(raw: &str) -> Result<Self, OsppError> {
        let (scheme, rest) = raw
            .split_once("://")
            .ok_or_else(|| invalid(raw, "missing scheme"))?;
        let secure = match scheme {
            "wss" => true,
            "ws" => false,
            _ => return Err(invalid(raw, "scheme must be wss:// or ws://")),
        };
        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            return Err(invalid(raw, "missing host"));
        }
        let (host, port) =
            split_authority(authority).ok_or_else(|| invalid(raw, "malformed authority"))?;
        if host.is_empty() {
            return Err(invalid(raw, "missing host"));
        }
        if !secure && !is_loopback_host(&host) {
            return Err(invalid(
                raw,
                "plaintext ws:// is only allowed on a loopback host",
            ));
        }
        Ok(Self {
            raw: raw.to_owned(),
            host,
            port: port.unwrap_or(if secure { 443 } else { 80 }),
            path: path.to_owned(),
            secure,
        })
    }

    /// The original validated URL.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Host name without brackets.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Effective port.
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Absolute request path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Whether the transport is TLS.
    pub const fn is_secure(&self) -> bool {
        self.secure
    }
}

impl fmt::Debug for OsppEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OsppEndpoint").field(&self.raw).finish()
    }
}

impl fmt::Display for OsppEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

fn split_authority(authority: &str) -> Option<(String, Option<u16>)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = match tail.strip_prefix(':') {
            Some(port) => Some(port.parse().ok()?),
            None if tail.is_empty() => None,
            None => return None,
        };
        return Some((host.to_owned(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => {
            Some((host.to_owned(), Some(port.parse().ok()?)))
        }
        Some(_) => None,
        None => Some((authority.to_owned(), None)),
    }
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn invalid(raw: &str, reason: &str) -> OsppError {
    OsppError::Transport(format!("invalid OSPP endpoint {raw:?}: {reason}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_wss_and_loopback_ws() {
        let secure = OsppEndpoint::parse("wss://ci.example.com/ospp/v1").expect("wss");
        assert!(secure.is_secure());
        assert_eq!(secure.host(), "ci.example.com");
        assert_eq!(secure.port(), 443);
        assert_eq!(secure.path(), "/ospp/v1");

        let local = OsppEndpoint::parse("ws://127.0.0.1:9090/_ospp/v1").expect("ws loopback");
        assert!(!local.is_secure());
        assert_eq!(local.port(), 9090);
    }

    #[test]
    fn rejects_plaintext_off_loopback() {
        assert!(OsppEndpoint::parse("ws://ci.example.com/ospp").is_err());
        assert!(OsppEndpoint::parse("https://ci.example.com/ospp").is_err());
    }
}
