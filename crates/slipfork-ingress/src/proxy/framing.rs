//! Request/response body framing: `Content-Length`, `Transfer-Encoding: chunked`, and headers.

use super::ProxyError;

/// How a request or response body is framed on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Framing {
    /// No body at all.
    None,
    /// Exactly this many bytes follow.
    Length(u64),
    /// `Transfer-Encoding: chunked` framing, forwarded verbatim.
    Chunked,
    /// The body ends when the sending side closes the connection.
    UntilEof,
}

/// Request body framing declared by `Transfer-Encoding` or `Content-Length`.
pub(crate) fn request_framing(request: &httparse::Request<'_, '_>) -> Result<Framing, ProxyError> {
    if let Some(value) = header(request.headers, "transfer-encoding") {
        if transfer_encoding_is_chunked(value) {
            return Ok(Framing::Chunked);
        }
    }
    match content_length(request.headers)? {
        None | Some(0) => Ok(Framing::None),
        Some(length) => Ok(Framing::Length(length)),
    }
}

/// Response body framing, which also depends on the request method and status.
pub(crate) fn response_framing(
    status: u16,
    method: &str,
    response: &httparse::Response<'_, '_>,
) -> Result<Framing, ProxyError> {
    if method.eq_ignore_ascii_case("HEAD")
        || status == 204
        || status == 304
        || (100..200).contains(&status)
    {
        return Ok(Framing::None);
    }
    if let Some(value) = header(response.headers, "transfer-encoding") {
        return Ok(if transfer_encoding_is_chunked(value) {
            Framing::Chunked
        } else {
            Framing::UntilEof
        });
    }
    match content_length(response.headers)? {
        None => Ok(Framing::UntilEof),
        Some(length) => Ok(Framing::Length(length)),
    }
}

/// The single `Content-Length`, rejecting conflicting duplicates with a `400`.
pub(crate) fn content_length(headers: &[httparse::Header<'_>]) -> Result<Option<u64>, ProxyError> {
    let mut lengths = headers.iter().filter_map(|header| {
        header
            .name
            .eq_ignore_ascii_case("content-length")
            .then_some(header.value)
    });
    let Some(first) = lengths.next() else {
        return Ok(None);
    };
    let length = parse_length(first)?;
    for value in lengths {
        if parse_length(value)? != length {
            return Err(ProxyError::ConflictingLength);
        }
    }
    Ok(Some(length))
}

fn parse_length(value: &[u8]) -> Result<u64, ProxyError> {
    let text = std::str::from_utf8(value).unwrap_or_default().trim();
    text.parse::<u64>()
        .map_err(|_| ProxyError::InvalidLength(text.to_owned()))
}

/// `true` when the final transfer coding is `chunked`.
fn transfer_encoding_is_chunked(value: &[u8]) -> bool {
    std::str::from_utf8(value).is_ok_and(|value| {
        value
            .split(',')
            .next_back()
            .is_some_and(|encoding| encoding.trim().eq_ignore_ascii_case("chunked"))
    })
}

/// Extract the single `Host` header.
pub(crate) fn host_header(request: &httparse::Request<'_, '_>) -> Result<String, ProxyError> {
    let mut host: Option<&str> = None;
    for header in request.headers.iter() {
        if !header.name.eq_ignore_ascii_case("host") {
            continue;
        }
        if host.is_some() {
            return Err(ProxyError::DuplicateHost);
        }
        let value = std::str::from_utf8(header.value).map_err(|_| ProxyError::MissingHost)?;
        if value.trim().is_empty() {
            return Err(ProxyError::MissingHost);
        }
        host = Some(value.trim());
    }
    host.map(str::to_owned).ok_or(ProxyError::MissingHost)
}

/// The first header value with this name.
pub(crate) fn header<'a>(headers: &[httparse::Header<'a>], name: &str) -> Option<&'a [u8]> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value)
}

/// `true` when a comma-separated header carries `token` (ASCII case-insensitive).
pub(crate) fn header_has_token(headers: &[httparse::Header<'_>], name: &str, token: &str) -> bool {
    header(headers, name).is_some_and(|value| {
        std::str::from_utf8(value).is_ok_and(|value| {
            value
                .split(',')
                .any(|candidate| candidate.trim().eq_ignore_ascii_case(token))
        })
    })
}

/// Hop-by-hop headers this hop replaces with its own framing.
pub(crate) fn is_hop_by_hop(name: &str) -> bool {
    name.eq_ignore_ascii_case("connection")
        || name.eq_ignore_ascii_case("keep-alive")
        || name.eq_ignore_ascii_case("proxy-connection")
}

/// `true` when a request may send another request on this connection afterwards.
pub(crate) fn request_keep_alive(request: &httparse::Request<'_, '_>) -> bool {
    if header_has_token(request.headers, "connection", "close") {
        return false;
    }
    match request.version {
        Some(1) => true,
        Some(0) => header_has_token(request.headers, "connection", "keep-alive"),
        _ => false,
    }
}
