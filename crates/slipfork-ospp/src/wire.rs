//! Closed `ospp-v1` JSON frame set (`docs/design/ospp.md` §Transport & Framing).
//!
//! One Rust type per frame, discriminated by `"type"`; an unknown type, an
//! unknown field, or a binary WebSocket frame is a protocol violation that fails
//! closed. There is no flattening catch-all and no opaque extras bag.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use slipfork_forkfile::EvaluatedFork;

use crate::error::OsppError;
use crate::model::OsppClaims;

/// The only `hello.version` this client and reference server speak.
///
/// Bumped from `1` when `subscribe` started carrying the evaluated `Forkfile`; any
/// other value is refused with `unsupported_version` (fail closed, never negotiated
/// down). The `Sec-WebSocket-Protocol` name is stable across version bumps.
pub const PROTOCOL_VERSION: u32 = 2;
/// `auth_methods` / `key_methods` / `method` name for ECDSA P-256.
pub const AUTH_METHOD_P256: &str = "p256";
/// `encryption_methods` / `encryption.method` name for RFC 9180 HPKE P-256.
pub const ENCRYPTION_METHOD_HPKE_P256: &str = "hpke-p256";
/// The single WebSocket subprotocol name.
pub const OSPP_SUBPROTOCOL: &str = "ospp-v1";

/// `challenge.nonce`: a 32-byte hex server nonce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge {
    /// 32-byte hex nonce.
    pub nonce: String,
}

/// The `response` object of `auth_response` and `pair_begin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthResponse {
    /// Compressed SEC1 P-256 public key, 66 lowercase hex digits.
    pub public_key: String,
    /// 32-byte hex client nonce.
    pub client_nonce: String,
    /// Raw `r || s` ECDSA P-256 signature, 128 lowercase hex digits.
    pub signature: String,
}

/// The `encryption` object of `subscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Encryption {
    /// Encryption method name (`hpke-p256`).
    pub method: String,
    /// Recipient public key hex.
    pub public_key: String,
}

/// One client-to-server frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    /// Open an authenticated session.
    Hello {
        /// Protocol version.
        version: u32,
        /// Supported authentication method names.
        auth_methods: Vec<String>,
    },
    /// Answer an `auth_challenge`.
    AuthResponse {
        /// Proof of possession.
        response: AuthResponse,
    },
    /// Open a device-pairing session.
    PairHello {
        /// Protocol version.
        version: u32,
        /// Supported key-proof method names.
        key_methods: Vec<String>,
    },
    /// Answer a `pair_challenge` with ALTCHA + proof of possession.
    PairBegin {
        /// Opaque ALTCHA solution payload.
        altcha: String,
        /// Hostname proposed for the enrolling machine.
        hostname: String,
        /// Proof of possession.
        response: AuthResponse,
    },
    /// Subscribe (or re-subscribe with the same id) to a project environment.
    Subscribe {
        /// Stable subscription id.
        id: String,
        /// Verbatim Git remote string.
        remote: String,
        /// Ephemeral recipient key.
        encryption: Encryption,
        /// The feature-evaluated manifest (boxed: it dwarfs every other frame).
        forkfile: Box<EvaluatedFork>,
        /// ID-only claims.
        claims: OsppClaims,
    },
    /// Stop an active subscription.
    Unsubscribe {
        /// Subscription id to release.
        id: String,
    },
    /// Request an out-of-band encrypted resource dump.
    DumpRequest {
        /// Correlation id.
        id: String,
        /// Active subscription id.
        subscription_id: String,
        /// Resource path, for example `pg_databases.main`.
        resource: String,
    },
}

/// One server-to-client frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    /// Server-selected authentication method and nonce.
    AuthChallenge {
        /// Selected method (`p256`).
        method: String,
        /// Server nonce.
        challenge: Challenge,
    },
    /// Session accepted with the server's supported encryption methods.
    AuthOk {
        /// Supported secret-encryption method names.
        encryption_methods: Vec<String>,
    },
    /// Server-selected key-proof method, ALTCHA challenge, and nonce.
    PairChallenge {
        /// Selected method (`p256`).
        method: String,
        /// ALTCHA proof-of-work challenge object.
        altcha: serde_json::Value,
        /// Server nonce.
        challenge: Challenge,
    },
    /// Server-supplied browser pairing URL and single-use handle.
    PairHandle {
        /// Full browser pairing URL without the fragment.
        url: String,
        /// 32-byte hex handle.
        handle: String,
    },
    /// Browser-approved pairing verdict.
    PairApproved {
        /// Authenticated account name.
        account: String,
        /// Approved machine name.
        machine: String,
    },
    /// Initial environment for a subscription.
    EnvResolved {
        /// Subscription id.
        id: String,
        /// Component id → variable name → value.
        components: BTreeMap<String, BTreeMap<String, WireValue>>,
    },
    /// Live environment update for an active subscription.
    EnvUpdated {
        /// Subscription id.
        id: String,
        /// Component id → variable name → value.
        components: BTreeMap<String, BTreeMap<String, WireValue>>,
    },
    /// Short-lived out-of-band download URL.
    DumpReady {
        /// Correlation id echoed from `dump_request`.
        id: String,
        /// HTTPS URL serving the sealed stream.
        url: String,
    },
    /// Typed refusal.
    Error {
        /// Correlation id when the error answers a request.
        #[serde(default)]
        id: Option<String>,
        /// Typed error code.
        code: String,
        /// Human-readable explanation.
        message: String,
    },
}

impl ServerFrame {
    /// Parse one UTF-8 JSON text frame, failing closed on anything outside the
    /// closed frame set.
    pub fn parse(text: &str) -> Result<Self, OsppError> {
        let value = validate_fields(text, SERVER_FIELDS)?;
        serde_json::from_value(value).map_err(|error| OsppError::Protocol(error.to_string()))
    }

    /// Encode one frame as a JSON text message.
    pub fn to_json(&self) -> Result<String, OsppError> {
        serde_json::to_string(self).map_err(|error| OsppError::Protocol(error.to_string()))
    }
}

impl ClientFrame {
    /// Parse one UTF-8 JSON text frame, failing closed on anything outside the
    /// closed frame set.
    pub fn parse(text: &str) -> Result<Self, OsppError> {
        let value = validate_fields(text, CLIENT_FIELDS)?;
        serde_json::from_value(value).map_err(|error| OsppError::Protocol(error.to_string()))
    }

    /// Encode one frame as a JSON text message.
    pub fn to_json(&self) -> Result<String, OsppError> {
        serde_json::to_string(self).map_err(|error| OsppError::Protocol(error.to_string()))
    }
}

/// Exact field set per frame type (`type` included). Anything else is a
/// protocol violation: there is no catch-all `extra` bag.
const CLIENT_FIELDS: &[(&str, &[&str])] = &[
    ("hello", &["type", "version", "auth_methods"]),
    ("auth_response", &["type", "response"]),
    ("pair_hello", &["type", "version", "key_methods"]),
    ("pair_begin", &["type", "altcha", "hostname", "response"]),
    (
        "subscribe",
        &["type", "id", "remote", "encryption", "forkfile", "claims"],
    ),
    ("unsubscribe", &["type", "id"]),
    (
        "dump_request",
        &["type", "id", "subscription_id", "resource"],
    ),
];

const SERVER_FIELDS: &[(&str, &[&str])] = &[
    ("auth_challenge", &["type", "method", "challenge"]),
    ("auth_ok", &["type", "encryption_methods"]),
    ("pair_challenge", &["type", "method", "altcha", "challenge"]),
    ("pair_handle", &["type", "url", "handle"]),
    ("pair_approved", &["type", "account", "machine"]),
    ("env_resolved", &["type", "id", "components"]),
    ("env_updated", &["type", "id", "components"]),
    ("dump_ready", &["type", "id", "url"]),
    ("error", &["type", "id", "code", "message"]),
];

fn validate_fields(
    text: &str,
    fields: &'static [(&'static str, &'static [&'static str])],
) -> Result<serde_json::Value, OsppError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| OsppError::Protocol(error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| OsppError::Protocol("frame must be a JSON object".to_owned()))?;
    let kind = object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| OsppError::Protocol("frame has no string type".to_owned()))?;
    let allowed = fields
        .iter()
        .find(|(name, _)| *name == kind)
        .map(|(_, fields)| *fields)
        .ok_or_else(|| OsppError::Protocol(format!("unknown frame type {kind}")))?;
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(OsppError::Protocol(format!(
                "{kind} frame has unknown field {key}"
            )));
        }
    }
    Ok(value)
}

/// One component environment value: plain text or a sealed `{ "encrypted": … }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireValue {
    /// Server-resolved template-free plaintext or a claimed `${{ ... }}`.
    Plain(String),
    /// Standard-base64 `enc || ciphertext` sealed to the subscription key.
    Encrypted(String),
}

impl WireValue {
    /// The sealed standard-base64 payload when this value is encrypted.
    pub fn ciphertext(&self) -> Option<&str> {
        match self {
            Self::Plain(_) => None,
            Self::Encrypted(ciphertext) => Some(ciphertext),
        }
    }
}

impl Serialize for WireValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Plain(value) => serializer.serialize_str(value),
            Self::Encrypted(ciphertext) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("encrypted", ciphertext)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for WireValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct WireValueVisitor;

        impl<'de> Visitor<'de> for WireValueVisitor {
            type Value = WireValue;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a plain string or a single-key { \"encrypted\": … } object")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(WireValue::Plain(value.to_owned()))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let key: String = map
                    .next_key()?
                    .ok_or_else(|| de::Error::custom("empty environment value object"))?;
                if key != "encrypted" {
                    return Err(de::Error::unknown_field(&key, &["encrypted"]));
                }
                let ciphertext: String = map.next_value()?;
                if map.next_key::<String>()?.is_some() {
                    return Err(de::Error::custom(
                        "environment value object must have exactly one key",
                    ));
                }
                Ok(WireValue::Encrypted(ciphertext))
            }
        }

        deserializer.deserialize_any(WireValueVisitor)
    }
}

/// The transcript for `auth_response`: `b"ospp-v1" || server_nonce || client_nonce`.
pub fn auth_transcript(server_nonce: &[u8], client_nonce: &[u8]) -> Vec<u8> {
    let mut transcript = Vec::with_capacity(7 + server_nonce.len() + client_nonce.len());
    transcript.extend_from_slice(b"ospp-v1");
    transcript.extend_from_slice(server_nonce);
    transcript.extend_from_slice(client_nonce);
    transcript
}

/// The transcript for `pair_begin`: `b"ospp-v1-pair" || server_nonce || client_nonce`.
pub fn pairing_transcript(server_nonce: &[u8], client_nonce: &[u8]) -> Vec<u8> {
    let mut transcript = Vec::with_capacity(12 + server_nonce.len() + client_nonce.len());
    transcript.extend_from_slice(b"ospp-v1-pair");
    transcript.extend_from_slice(server_nonce);
    transcript.extend_from_slice(client_nonce);
    transcript
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hello_round_trips_to_the_spec_shape() {
        let frame = ClientFrame::Hello {
            version: PROTOCOL_VERSION,
            auth_methods: vec![AUTH_METHOD_P256.to_owned()],
        };
        assert_eq!(
            serde_json::to_value(&frame).expect("json"),
            json!({ "type": "hello", "version": 2, "auth_methods": ["p256"] })
        );
    }

    #[test]
    fn subscribe_carries_the_evaluated_manifest_single_shaped() {
        let forkfile = slipfork_forkfile::parse_forkfile(
            r#"
postgres pg { database app_db }
domain main
service api ./api {
    port 8000
    env.DATABASE_URL = app_db.url
    run /bin/true
}
"#,
        )
        .expect("valid Forkfile")
        .evaluate(&slipfork_forkfile::FeatureSet::new())
        .expect("evaluates");
        let frame = ClientFrame::Subscribe {
            id: "sub-1".to_owned(),
            remote: "git@github.com:acme/web.git".to_owned(),
            encryption: Encryption {
                method: ENCRYPTION_METHOD_HPKE_P256.to_owned(),
                public_key: "04".to_owned(),
            },
            forkfile: Box::new(forkfile),
            claims: OsppClaims::default(),
        };
        let json = frame.to_json().expect("json");
        assert!(json.contains("\"forkfile\""), "{json}");
        assert!(!json.contains("\"snow\""), "{json}");
        assert_eq!(ClientFrame::parse(&json).expect("parse"), frame);
    }

    #[test]
    fn unknown_frames_and_fields_fail_closed() {
        assert!(ServerFrame::parse(r#"{"type":"legacy_env_bundle"}"#).is_err());
        assert!(
            ServerFrame::parse(
                r#"{"type":"auth_ok","encryption_methods":["hpke-p256"],"extra":1}"#
            )
            .is_err()
        );
    }

    #[test]
    fn environment_values_reject_extra_keys() {
        let ok: ServerFrame = ServerFrame::parse(
            r#"{"type":"env_resolved","id":"s","components":{"api":{"A":"x","B":{"encrypted":"y"}}}}"#,
        )
        .expect("valid frame");
        let ServerFrame::EnvResolved { components, .. } = ok else {
            panic!("wrong frame");
        };
        assert_eq!(components["api"]["A"], WireValue::Plain("x".to_owned()));
        assert_eq!(components["api"]["B"], WireValue::Encrypted("y".to_owned()));
        assert!(
            ServerFrame::parse(
                r#"{"type":"env_resolved","id":"s","components":{"api":{"B":{"encrypted":"y","z":1}}}}"#
            )
            .is_err()
        );
        assert!(
            ServerFrame::parse(
                r#"{"type":"env_resolved","id":"s","components":{"api":{"B":{"other":"y"}}}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn transcripts_are_golden_bytes() {
        let server_nonce = [0x11u8; 32];
        let client_nonce = [0x22u8; 32];
        let mut golden = b"ospp-v1".to_vec();
        golden.extend_from_slice(&server_nonce);
        golden.extend_from_slice(&client_nonce);
        assert_eq!(auth_transcript(&server_nonce, &client_nonce), golden);

        let mut paired = b"ospp-v1-pair".to_vec();
        paired.extend_from_slice(&server_nonce);
        paired.extend_from_slice(&client_nonce);
        assert_eq!(pairing_transcript(&server_nonce, &client_nonce), paired);
    }
}
