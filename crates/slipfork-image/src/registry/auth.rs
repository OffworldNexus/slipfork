//! OCI Distribution v2 auth: `WWW-Authenticate` challenges, bearer tokens, and the login probe.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::error::{Error, Result};

use super::read_body;

/// A cached bearer token plus the exact `service`/`scope` it was minted for.
#[derive(Debug, Clone)]
pub(super) struct Bearer {
    pub(super) value: String,
    service: Option<String>,
    scope: Option<String>,
}

impl Bearer {
    pub(super) fn minted_for(challenge: &Challenge, value: String) -> Self {
        Self {
            value,
            service: challenge.service.clone(),
            scope: challenge.scope.clone(),
        }
    }

    /// The token is only reusable for the challenge whose `service` and `scope` it encodes.
    fn matches(&self, challenge: &Challenge) -> bool {
        self.service == challenge.service && self.scope == challenge.scope
    }
}

/// A 401 challenge justifies a new token only when the cached one was minted for other terms.
fn needs_fresh_token(current: Option<&Bearer>, challenge: &Challenge) -> bool {
    current.is_none_or(|bearer| !bearer.matches(challenge))
}

/// One re-fetch and one retry per request; a second 401 with the same terms is terminal.
pub(super) fn may_retry(retried: bool, current: Option<&Bearer>, challenge: &Challenge) -> bool {
    !retried && needs_fresh_token(current, challenge)
}

/// One parsed `WWW-Authenticate: Bearer` challenge.
pub(super) struct Challenge {
    realm: String,
    service: Option<String>,
    scope: Option<String>,
}

impl Challenge {
    pub(super) fn parse(header: &str) -> Option<Self> {
        let (scheme, rest) = header.split_once(' ')?;
        if !scheme.eq_ignore_ascii_case("bearer") {
            return None;
        }
        let mut realm = None;
        let mut service = None;
        let mut scope = None;
        for part in rest.split(',') {
            let Some((key, value)) = part.trim().split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"').to_owned();
            match key.trim().to_ascii_lowercase().as_str() {
                "realm" => realm = Some(value),
                "service" => service = Some(value),
                "scope" => scope = Some(value),
                _ => {}
            }
        }
        Some(Self {
            realm: realm?,
            service,
            scope,
        })
    }
}

/// The challenge's `realm`, `service`, and `scope` verbatim: Docker Hub's realm is
/// `https://auth.docker.io/token` and its service is `registry.docker.io`.
pub(super) fn token_url(challenge: &Challenge) -> String {
    let mut query: Vec<String> = Vec::new();
    if let Some(service) = &challenge.service {
        query.push(format!("service={}", encode_query(service)));
    }
    if let Some(scope) = &challenge.scope {
        query.push(format!("scope={}", encode_query(scope)));
    }
    if query.is_empty() {
        challenge.realm.clone()
    } else {
        format!("{}?{}", challenge.realm, query.join("&"))
    }
}

/// Query values keep token scopes readable: only what breaks a query string is escaped.
const QUERY: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b':')
    .remove(b'/');

fn encode_query(value: &str) -> String {
    utf8_percent_encode(value, QUERY).to_string()
}

/// One token mint: the realm is called with the optional Basic header and its JSON token read.
pub(super) fn fetch_token_body(
    agent: &ureq::Agent,
    url: &str,
    user_agent: &str,
    authorization: Option<&str>,
) -> Result<String> {
    let mut request = agent.get(url).set("User-Agent", user_agent);
    if let Some(header) = authorization {
        request = request.set("Authorization", header);
    }
    let response = request.call().map_err(|err| http_error(url, err))?;
    let body = read_body(response, url)?;
    #[derive(serde::Deserialize)]
    struct TokenBody {
        token: Option<String>,
        access_token: Option<String>,
    }
    let body: TokenBody = serde_json::from_slice(&body).map_err(|source| Error::Json {
        url: url.to_owned(),
        source,
    })?;
    body.token
        .or(body.access_token)
        .ok_or_else(|| Error::Token {
            realm: url.to_owned(),
        })
}

/// Maps a transport or status failure onto the typed registry error set.
pub(super) fn http_error(url: &str, err: ureq::Error) -> Error {
    match err {
        ureq::Error::Status(status, _) => Error::Http {
            url: url.to_owned(),
            status,
        },
        ureq::Error::Transport(transport) => Error::Transport {
            url: url.to_owned(),
            message: transport.to_string(),
        },
    }
}

/// Any refusal during a login probe becomes one typed error with a single `next:` remedy.
fn login_failure(registry: &str, error: Error) -> Error {
    match error {
        Error::Transport { .. } => error,
        other => Error::LoginRejected {
            registry: registry.to_owned(),
            detail: other.cause(),
        },
    }
}

/// Probes `url` (`<scheme>://<registry>/v2/`) without pulling an image.
///
/// A `200` proves the registry serves anonymous reads; a Bearer challenge mints a token for its
/// `realm` and `service` with **no** scope (ghcr's placeholder `repository:user/image:pull` is
/// never requested). A `401` without a usable challenge is terminal: retrying the identical
/// request with the same header can only produce the same `401`.
pub(super) fn login_probe(
    agent: &ureq::Agent,
    user_agent: &str,
    url: &str,
    authorization: Option<&str>,
    registry: &str,
) -> Result<()> {
    let mut request = agent.get(url).set("User-Agent", user_agent);
    if let Some(header) = authorization {
        request = request.set("Authorization", header);
    }
    match request.call() {
        Ok(response) => {
            drop(response);
            Ok(())
        }
        Err(err) => match err {
            ureq::Error::Status(401, response) => {
                let header = response
                    .header("WWW-Authenticate")
                    .unwrap_or_default()
                    .to_owned();
                match Challenge::parse(&header) {
                    Some(mut challenge) => {
                        challenge.scope = None;
                        fetch_token_body(agent, &token_url(&challenge), user_agent, authorization)
                            .map(|_| ())
                            .map_err(|error| login_failure(registry, error))
                    }
                    None => Err(login_failure(
                        registry,
                        Error::Http {
                            url: url.to_owned(),
                            status: 401,
                        },
                    )),
                }
            }
            err => Err(login_failure(registry, http_error(url, err))),
        },
    }
}
