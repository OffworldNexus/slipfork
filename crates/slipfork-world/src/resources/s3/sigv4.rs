//! AWS Signature V4 (header auth) verification for the native S3 endpoint.

use std::fmt;

use hmac::{Hmac, Mac};
use hyper::header::HeaderMap;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

const AUTHORIZATION: &str = "authorization";
const AMZ_DATE: &str = "x-amz-date";
const AMZ_CONTENT_SHA256: &str = "x-amz-content-sha256";
const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

/// One SigV4 identity served by this world; `Debug` never renders the secret.
#[derive(Clone)]
pub struct S3Credentials {
    access_key: String,
    secret_key: String,
}

impl S3Credentials {
    /// Builds one identity from a caller-resolved access/secret pair.
    #[must_use]
    pub fn new(access_key: &str, secret_key: &str) -> Self {
        Self {
            access_key: access_key.to_owned(),
            secret_key: secret_key.to_owned(),
        }
    }

    /// The local identity for `world` (`slipfork` / `slipfork-<world>-s3`).
    #[must_use]
    pub fn local(world: &str) -> Self {
        Self::new("slipfork", &format!("slipfork-{world}-s3"))
    }

    /// The access key id.
    #[must_use]
    pub fn access_key(&self) -> &str {
        &self.access_key
    }

    /// The signing secret, exposed only to the SigV4 key derivation.
    #[must_use]
    pub fn secret_key(&self) -> &str {
        &self.secret_key
    }
}

impl fmt::Debug for S3Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("S3Credentials")
            .field("access_key", &"[REDACTED]")
            .field("secret_key", &"[REDACTED]")
            .finish()
    }
}

/// The outcome of inspecting one request's `Authorization` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Auth {
    /// No signature was presented; the bucket policy decides.
    Anonymous,
    /// A signature matched the world's credentials.
    Valid,
    /// A signature was presented but is missing, malformed, or wrong.
    Invalid,
}

/// Verifies header-based SigV4 requests against the world's identities.
pub(crate) struct SigV4Verifier<'a> {
    credentials: &'a [S3Credentials],
}

impl<'a> SigV4Verifier<'a> {
    pub(crate) const fn new(credentials: &'a [S3Credentials]) -> Self {
        Self { credentials }
    }

    /// Verifies `Authorization`, the signed headers, and the canonical request.
    pub(crate) fn verify(
        &self,
        method: &str,
        path: &str,
        query: &str,
        headers: &HeaderMap,
    ) -> Auth {
        let Some(value) = headers.get(AUTHORIZATION) else {
            return Auth::Anonymous;
        };
        let Ok(value) = value.to_str() else {
            return Auth::Invalid;
        };
        let Some(rest) = value.strip_prefix("AWS4-HMAC-SHA256 ") else {
            return Auth::Invalid;
        };
        let (Some(credential), Some(signed_headers), Some(signature)) = (
            field(rest, "Credential"),
            field(rest, "SignedHeaders"),
            field(rest, "Signature"),
        ) else {
            return Auth::Invalid;
        };
        let mut scope = credential.split('/');
        let (Some(access_key), Some(date), Some(region), Some("s3"), Some("aws4_request")) = (
            scope.next(),
            scope.next(),
            scope.next(),
            scope.next(),
            scope.next(),
        ) else {
            return Auth::Invalid;
        };
        let Some(identity) = self
            .credentials
            .iter()
            .find(|identity| identity.access_key == access_key)
        else {
            return Auth::Invalid;
        };
        let Some(amz_date) = headers.get(AMZ_DATE).and_then(|value| value.to_str().ok()) else {
            return Auth::Invalid;
        };
        let payload = headers
            .get(AMZ_CONTENT_SHA256)
            .and_then(|value| value.to_str().ok())
            .unwrap_or(UNSIGNED_PAYLOAD);
        let canonical_headers = signed_headers
            .split(';')
            .map(|name| {
                let value = headers
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .trim();
                format!("{name}:{value}\n")
            })
            .collect::<String>();
        let canonical_request = format!(
            "{method}\n{}\n{}\n{canonical_headers}\n{signed_headers}\n{payload}",
            canonical_uri(path),
            canonical_query(query),
        );
        let scope = format!("{date}/{region}/s3/aws4_request");
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex(&Sha256::digest(canonical_request.as_bytes()))
        );
        let key = signing_key(&identity.secret_key, date, region);
        let expected = hex(&hmac_sha256(&key, string_to_sign.as_bytes()));
        if expected.as_bytes().ct_eq(signature.as_bytes()).into() {
            Auth::Valid
        } else {
            Auth::Invalid
        }
    }
}

fn field<'a>(authorization: &'a str, name: &str) -> Option<&'a str> {
    authorization.split(',').find_map(|part| {
        let part = part.trim();
        part.strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('='))
    })
}

fn signing_key(secret: &str, date: &str, region: &str) -> Vec<u8> {
    let date_key = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let region_key = hmac_sha256(&date_key, region.as_bytes());
    let service_key = hmac_sha256(&region_key, b"s3");
    hmac_sha256(&service_key, b"aws4_request")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn canonical_uri(path: &str) -> String {
    let decoded = percent_decode(path);
    let mut out = String::with_capacity(decoded.len());
    for byte in decoded.bytes() {
        if byte == b'/' || byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
        {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn canonical_query(query: &str) -> String {
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (
                encode(&percent_decode(name)),
                encode(&percent_decode(value)),
            )
        })
        .collect();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Decodes `%XX` escapes; malformed escapes are kept verbatim.
pub(crate) fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &bytes[index + 1..index + 3];
            if let Ok(text) = std::str::from_utf8(hex)
                && let Ok(byte) = u8::from_str_radix(text, 16)
            {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
