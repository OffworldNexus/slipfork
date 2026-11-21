//! Out-of-band dump download: a ticket plus the bounded HTTP reader.

use std::fmt;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use url::Url;

use crate::dump::DumpReader;
use crate::endpoint::is_loopback_host;
use crate::error::OsppError;
use crate::keys::SubscriptionKey;

/// Hard cap on the response header block a dump endpoint may return.
pub const MAX_DUMP_HEADER_BYTES: usize = 16 * 1024;

/// Everything needed to open one sealed dump stream, off the session thread.
pub struct DumpTicket {
    /// Short-lived out-of-band URL (a bearer token: never log it).
    pub url: String,
    /// The subscription key the dump was sealed to.
    pub key: Arc<SubscriptionKey>,
    /// Subscription id binding the stream AAD.
    pub subscription_id: String,
    /// Resource path binding the stream AAD.
    pub resource: String,
}

impl DumpTicket {
    /// Open the HTTPS body and the RFC 9180 stateful decoder for this ticket.
    pub fn open(&self, io: Duration) -> Result<DumpReader<Box<dyn Read + Send>>, OsppError> {
        let source = open_dump_stream(&self.url, io)?;
        DumpReader::open(source, &self.key, &self.subscription_id, &self.resource)
    }
}

impl fmt::Debug for DumpTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DumpTicket")
            .field("url", &"<redacted>")
            .field("subscription_id", &self.subscription_id)
            .field("resource", &self.resource)
            .finish_non_exhaustive()
    }
}

/// Open the out-of-band OSPP v1 dump body: `https://` anywhere, `http://` on loopback only.
///
/// One `ureq` GET per dump with the injected timeout and zero redirects: no pooled
/// connection outlives the sealed stream. The response is bounded at
/// [`MAX_DUMP_HEADER_BYTES`] of headers with a mandatory numeric `content-length`,
/// and the body is read through `take(content-length)`.
pub fn open_dump_stream(url: &str, io: Duration) -> Result<Box<dyn Read + Send>, OsppError> {
    let url = dump_url(url)?;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(io)
        .timeout_read(io)
        .timeout_write(io)
        .redirects(0)
        .build();
    let response = agent
        .get(url.as_str())
        .set("accept-encoding", "identity")
        .call()
        .map_err(dump_request_error)?;
    let header_bytes: usize = response
        .headers_names()
        .iter()
        .map(|name| name.len() + 4 + response.header(name).map_or(0, str::len))
        .sum();
    if header_bytes > MAX_DUMP_HEADER_BYTES {
        return Err(OsppError::Transport {
            kind: "dump-http",
            message: "dump response headers exceed the 16 KiB cap".to_owned(),
        });
    }
    let length = response
        .header("content-length")
        .ok_or_else(|| OsppError::Transport {
            kind: "dump-http",
            message: "dump response has no content-length; the body cannot be bounded".to_owned(),
        })?
        .parse::<u64>()
        .map_err(|_| OsppError::Transport {
            kind: "dump-http",
            message: "dump response content-length is not a byte count".to_owned(),
        })?;
    Ok(Box::new(response.into_reader().take(length)))
}

fn dump_request_error(error: ureq::Error) -> OsppError {
    match error {
        ureq::Error::Status(status, _) => OsppError::DumpHttp { status },
        ureq::Error::Transport(failure) => OsppError::Transport {
            // `failure.kind()` never contains the bearer-token URL.
            kind: "dump-http",
            message: format!("{:?}", failure.kind()),
        },
    }
}

/// Validate a dump URL: `https://` anywhere, `http://` strictly on a loopback host.
fn dump_url(url: &str) -> Result<Url, OsppError> {
    let parsed = Url::parse(url).map_err(|_| OsppError::Transport {
        kind: "dump-url",
        message: "dump URL is not a valid URL".to_owned(),
    })?;
    match parsed.scheme() {
        "https" => Ok(parsed),
        "http" => match parsed.host_str() {
            Some(host) if is_loopback_host(host) => Ok(parsed),
            _ => Err(OsppError::Transport {
                kind: "dump-url",
                message: "plaintext dump URLs are only allowed on a loopback host".to_owned(),
            }),
        },
        _ => Err(OsppError::Transport {
            kind: "dump-url",
            message: "dump URL scheme is unsupported".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_dump_urls_are_loopback_only() {
        assert!(dump_url("http://127.0.0.1:1234/dumps/x").is_ok());
        assert!(dump_url("http://ci.example.com/dumps/x").is_err());
        assert!(dump_url("https://ci.example.com/dumps/x").is_ok());
    }
}
