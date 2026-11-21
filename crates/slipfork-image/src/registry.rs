//! OCI Distribution v2 registry client: auth challenges, tokens, manifests and blobs.

use std::io::Read;
use std::time::Duration;

use base64::Engine as _;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::Architecture;
use crate::digest;
use crate::error::{Error, Result};
use crate::manifest::{Document, ImageManifest};
use crate::reference::ImageReference;

const MANIFEST_ACCEPT: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";

/// Scheme of the base URLs the composition root injects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RegistryScheme {
    /// Production registries.
    #[default]
    Https,
    /// Loopback reference registries in the lab.
    Http,
}

impl RegistryScheme {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Https => "https",
            Self::Http => "http",
        }
    }
}

/// Registry credentials; `Debug` never prints the password.
#[derive(Clone)]
pub enum Credentials {
    /// Public pulls.
    Anonymous,
    /// HTTP basic auth against the token endpoint.
    Basic {
        /// Username.
        username: String,
        /// Password or access token.
        password: String,
    },
}

impl Credentials {
    /// Builds basic-auth credentials.
    #[must_use]
    pub fn basic(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self::Basic {
            username: username.into(),
            password: password.into(),
        }
    }
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anonymous => f.write_str("Anonymous"),
            Self::Basic { username, .. } => f
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &"[REDACTED]")
                .finish(),
        }
    }
}

impl Credentials {
    fn header(&self) -> Option<String> {
        match self {
            Self::Anonymous => None,
            Self::Basic { username, password } => Some(format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
            )),
        }
    }
}

/// HTTPS client for one injected registry scheme.
pub struct RegistryClient {
    agent: ureq::Agent,
    credentials: Credentials,
    user_agent: String,
    scheme: RegistryScheme,
}

impl RegistryClient {
    /// Builds the client; no socket opens until a session runs.
    #[must_use]
    pub fn new(credentials: Credentials, user_agent: impl Into<String>) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout_read(Duration::from_secs(300))
            .build();
        Self {
            agent,
            credentials,
            user_agent: user_agent.into(),
            scheme: RegistryScheme::Https,
        }
    }

    /// Uses `scheme` for every base URL (loopback lab registries speak plain HTTP).
    #[must_use]
    pub fn with_scheme(mut self, scheme: RegistryScheme) -> Self {
        self.scheme = scheme;
        self
    }

    /// Opens a per-reference session that caches the bearer token.
    pub fn session<'a>(&'a self, reference: &'a ImageReference) -> RegistrySession<'a> {
        RegistrySession {
            client: self,
            reference,
            token: None,
            probed: false,
        }
    }

    fn base(&self, registry: &str) -> String {
        format!("{}://{registry}", self.scheme.as_str())
    }
}

/// One pull against one reference: the token is fetched once and reused.
pub struct RegistrySession<'a> {
    client: &'a RegistryClient,
    reference: &'a ImageReference,
    token: Option<String>,
    probed: bool,
}

impl RegistrySession<'_> {
    /// Resolves `reference` to the manifest for the injected architecture.
    pub fn manifest(&mut self, architecture: Architecture) -> Result<ResolvedManifest> {
        self.probe()?;
        let url = self.manifest_url(self.reference.reference());
        let body = self.document(&url)?;
        let computed = digest::sha256(&body);
        if let Some(expected) = self.reference.digest() {
            if expected != computed {
                return Err(mismatch("manifest", expected, &computed));
            }
        }
        match Document::parse(&body, &url)? {
            Document::Manifest(manifest) => Ok(ResolvedManifest {
                digest: computed,
                manifest,
            }),
            Document::Index(index) => {
                let selected =
                    index
                        .select(architecture)
                        .ok_or_else(|| Error::NoMatchingPlatform {
                            wanted: format!("linux/{}", architecture.oci()),
                            available: index.available(),
                        })?;
                self.resolve_child(&selected.digest)
            }
        }
    }

    /// Streams one layer blob; the caller verifies the digest as it unpacks.
    pub fn blob(&mut self, digest: &str) -> Result<Box<dyn Read + Send + Sync + 'static>> {
        self.probe()?;
        let url = format!(
            "{}/v2/{}/blobs/{digest}",
            self.client.base(self.reference.registry()),
            self.reference.repository()
        );
        let response = self.get(&url, None)?;
        Ok(response.into_reader())
    }

    fn resolve_child(&mut self, digest: &str) -> Result<ResolvedManifest> {
        let url = self.manifest_url(digest);
        let body = self.document(&url)?;
        let computed = digest::sha256(&body);
        if computed != digest {
            return Err(mismatch("manifest", digest, &computed));
        }
        match Document::parse(&body, &url)? {
            Document::Manifest(manifest) => Ok(ResolvedManifest {
                digest: computed,
                manifest,
            }),
            Document::Index(_) => Err(Error::NestedIndex { url }),
        }
    }

    fn probe(&mut self) -> Result<()> {
        if self.probed {
            return Ok(());
        }
        let url = format!("{}/v2/", self.client.base(self.reference.registry()));
        let response = self.get(&url, None)?;
        drop(response);
        self.probed = true;
        Ok(())
    }

    fn manifest_url(&self, reference: &str) -> String {
        format!(
            "{}/v2/{}/manifests/{reference}",
            self.client.base(self.reference.registry()),
            self.reference.repository()
        )
    }

    /// Reads a document body and checks `Docker-Content-Digest` when the registry sends one.
    fn document(&mut self, url: &str) -> Result<Vec<u8>> {
        let response = self.get(url, Some(MANIFEST_ACCEPT))?;
        let declared = response.header("Docker-Content-Digest").map(str::to_owned);
        let body = read_body(response, url)?;
        if let Some(expected) = declared {
            let computed = digest::sha256(&body);
            if expected != computed {
                return Err(mismatch("manifest", &expected, &computed));
            }
        }
        Ok(body)
    }

    /// `GET` with the bearer token, fetching one token on the first `401`.
    fn get(&mut self, url: &str, accept: Option<&str>) -> Result<ureq::Response> {
        match self.send(url, accept) {
            Ok(response) => Ok(response),
            Err(err) => match *err {
                ureq::Error::Status(401, response) => {
                    if self.token.is_some() {
                        return Err(self.unauthorized());
                    }
                    let header = response
                        .header("WWW-Authenticate")
                        .ok_or_else(|| Error::Challenge {
                            header: String::new(),
                        })?
                        .to_owned();
                    let challenge = Challenge::parse(&header).ok_or(Error::Challenge { header })?;
                    self.token = Some(self.fetch_token(&challenge)?);
                    self.send(url, accept)
                        .map_err(|retry| self.map_error(url, *retry))
                }
                err => Err(self.map_error(url, err)),
            },
        }
    }

    fn send(
        &self,
        url: &str,
        accept: Option<&str>,
    ) -> std::result::Result<ureq::Response, Box<ureq::Error>> {
        let mut request = self
            .client
            .agent
            .get(url)
            .set("User-Agent", &self.client.user_agent);
        if let Some(accept) = accept {
            request = request.set("Accept", accept);
        }
        if let Some(token) = &self.token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        request.call().map_err(Box::new)
    }

    fn fetch_token(&self, challenge: &Challenge) -> Result<String> {
        let mut query: Vec<String> = Vec::new();
        if let Some(service) = &challenge.service {
            query.push(format!("service={}", encode_query(service)));
        }
        if let Some(scope) = &challenge.scope {
            query.push(format!("scope={}", encode_query(scope)));
        }
        let url = if query.is_empty() {
            challenge.realm.clone()
        } else {
            format!("{}?{}", challenge.realm, query.join("&"))
        };
        let mut request = self
            .client
            .agent
            .get(&url)
            .set("User-Agent", &self.client.user_agent);
        if let Some(header) = self.client.credentials.header() {
            request = request.set("Authorization", &header);
        }
        let response = request.call().map_err(|err| self.map_error(&url, err))?;
        let body = read_body(response, &url)?;
        #[derive(serde::Deserialize)]
        struct TokenBody {
            token: Option<String>,
            access_token: Option<String>,
        }
        let body: TokenBody = serde_json::from_slice(&body).map_err(|source| Error::Json {
            url: url.clone(),
            source,
        })?;
        body.token
            .or(body.access_token)
            .ok_or(Error::Token { realm: url })
    }

    fn unauthorized(&self) -> Error {
        Error::Unauthorized {
            registry: self.reference.registry().to_owned(),
            repository: self.reference.repository().to_owned(),
        }
    }

    fn map_error(&self, url: &str, err: ureq::Error) -> Error {
        match err {
            ureq::Error::Status(401, _) => self.unauthorized(),
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
}

/// The per-architecture manifest plus the digest that names its cache subvolume.
#[derive(Debug)]
pub struct ResolvedManifest {
    /// `sha256:<hex>` of the manifest body.
    pub digest: String,
    /// Parsed manifest with the ordered layers.
    pub manifest: ImageManifest,
}

fn mismatch(subject: &str, expected: &str, actual: &str) -> Error {
    Error::DigestMismatch {
        subject: subject.to_owned(),
        expected: expected.to_owned(),
        actual: actual.to_owned(),
    }
}

fn read_body(response: ureq::Response, url: &str) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut body)
        .map_err(|source| Error::Transport {
            url: url.to_owned(),
            message: source.to_string(),
        })?;
    Ok(body)
}

/// One parsed `WWW-Authenticate: Bearer` challenge.
struct Challenge {
    realm: String,
    service: Option<String>,
    scope: Option<String>,
}

impl Challenge {
    fn parse(header: &str) -> Option<Self> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_auth_is_base64_and_debug_redacts_the_password() {
        let credentials = Credentials::basic("user", "secret");
        assert_eq!(
            credentials.header().as_deref(),
            Some("Basic dXNlcjpzZWNyZXQ=")
        );
        assert_eq!(
            Credentials::basic("ab", "cd").header().as_deref(),
            Some("Basic YWI6Y2Q="),
            "the encoder must pad to a 4-character boundary"
        );
        assert!(Credentials::Anonymous.header().is_none());
        let rendered = format!("{credentials:?}");
        assert!(rendered.contains("[REDACTED]"));
        assert!(!rendered.contains("secret"));
    }

    #[test]
    fn bearer_challenges_keep_realm_service_and_scope() {
        let challenge = Challenge::parse(
            "Bearer realm=\"https://auth.docker.io/token\",service=\"registry.docker.io\",scope=\"repository:library/postgres:pull\"",
        )
        .expect("bearer challenge");
        assert_eq!(challenge.realm, "https://auth.docker.io/token");
        assert_eq!(challenge.service.as_deref(), Some("registry.docker.io"));
        assert_eq!(
            challenge.scope.as_deref(),
            Some("repository:library/postgres:pull")
        );
        assert!(Challenge::parse("Basic realm=\"x\"").is_none());
    }

    #[test]
    fn query_values_keep_digest_characters_readable() {
        assert_eq!(
            encode_query("repository:library/postgres:pull"),
            "repository:library/postgres:pull"
        );
        assert_eq!(encode_query("a b&c"), "a%20b%26c");
        assert_eq!(encode_query("a+b=c"), "a%2Bb%3Dc");
    }
}
