//! The single injected OSPP v1 server identity.
//!
//! `OsppEndpoint` validates exactly one URL shape at the Composition Root and is
//! then the sole authority across `connect`, `subscribe`, `dump`, and `pair`:
//! `wss://` anywhere, and plaintext `ws://` strictly on loopback so the in-tree
//! reference server can run without a trusted certificate.

use std::fmt;

use url::Url;

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
    /// Parse and validate an endpoint URL with the `url` crate.
    pub fn parse(raw: &str) -> Result<Self, OsppError> {
        let parsed = Url::parse(raw)
            .map_err(|error| invalid("<unparseable>", format!("not a URL: {error}")))?;
        let host = parsed
            .host_str()
            .filter(|host| !host.is_empty())
            .ok_or_else(|| invalid("unknown", "missing host".to_owned()))?
            .to_owned();
        let secure = match parsed.scheme() {
            "wss" => true,
            "ws" => false,
            _ => {
                return Err(invalid(&host, "scheme must be wss:// or ws://".to_owned()));
            }
        };
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(invalid(&host, "credentials are not allowed".to_owned()));
        }
        if parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(invalid(
                &host,
                "endpoint must not carry a query or fragment".to_owned(),
            ));
        }
        if !secure && !is_loopback_host(&host) {
            return Err(invalid(
                &host,
                "plaintext ws:// is only allowed on a loopback host".to_owned(),
            ));
        }
        Ok(Self {
            raw: raw.to_owned(),
            host,
            port: parsed.port().unwrap_or(if secure { 443 } else { 80 }),
            path: if parsed.path().is_empty() {
                "/".to_owned()
            } else {
                parsed.path().to_owned()
            },
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

/// The one loopback policy shared by `ws://` endpoints and `http://` dump URLs.
pub fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn invalid(url_host: &str, reason: String) -> OsppError {
    OsppError::InvalidEndpoint {
        url_host: url_host.to_owned(),
        reason,
    }
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
    fn rejects_plaintext_off_loopback_without_echoing_the_url() {
        for raw in [
            "ws://ci.example.com/ospp",
            "https://ci.example.com/ospp",
            "wss://user:pass@ci.example.com/ospp",
            "wss://ci.example.com/ospp?token=secret",
        ] {
            let error = OsppEndpoint::parse(raw).expect_err("refused");
            assert!(!error.to_string().contains("secret"), "{error}");
        }
    }
}
