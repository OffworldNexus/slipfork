//! OCI Distribution v2 registry client: auth challenges, tokens, manifests and blobs.

use std::io::Read;
use std::time::Duration;

use base64::Engine as _;

use crate::Architecture;
use crate::credentials::{RegistryCredentials, is_loopback};
use crate::digest;
use crate::error::{Error, Result};
use crate::manifest::{Document, ImageManifest};
use crate::reference::{ImageReference, canonical_host};

mod auth;

use auth::{Bearer, Challenge, fetch_token_body, http_error, login_probe, may_retry, token_url};

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

/// The registry's shared timeout and TLS configuration: 15 s connect and 300 s read timeouts,
/// workspace TLS, redirects followed. A fresh agent is built per call — there is no singleton.
/// Registry pulls and `ADD <http(s)://…>` downloads both configure `ureq` this way.
pub(crate) fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(300))
        .build()
}

/// HTTPS client for one injected registry scheme.
pub struct RegistryClient {
    agent: ureq::Agent,
    credentials: RegistryCredentials,
    user_agent: String,
    scheme: RegistryScheme,
}

impl RegistryClient {
    /// Builds the client; no socket opens until a session runs.
    #[must_use]
    pub fn new(credentials: RegistryCredentials, user_agent: impl Into<String>) -> Self {
        Self {
            agent: agent(),
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

    /// The credentials that apply to `registry`'s host, anonymous when none are stored.
    #[must_use]
    pub fn credentials_for(&self, registry: &str) -> Credentials {
        self.credentials.credentials_for(registry)
    }

    /// Verifies `registry` accepts the injected credentials without pulling an image.
    ///
    /// Discovery is unauthenticated: `200` means the registry serves anonymous reads; a Bearer
    /// challenge mints a token for its `realm` and `service` with **no** scope (ghcr's placeholder
    /// `repository:user/image:pull` is never requested). A refusal is one typed error whose
    /// `next:` names the remedy.
    pub fn login(&self, registry: &str) -> Result<()> {
        let registry = canonical_host(registry);
        if registry.is_empty() {
            return Err(Error::InvalidRegistry { registry });
        }
        let authorization = self.credentials_for(&registry).header();
        let url = format!("{}/v2/", self.base(&registry));
        login_probe(
            &self.agent,
            &self.user_agent,
            &url,
            authorization.as_deref(),
            &registry,
        )
    }

    fn base(&self, registry: &str) -> String {
        let scheme = if self.scheme == RegistryScheme::Http || is_loopback(registry) {
            "http"
        } else {
            "https"
        };
        format!("{scheme}://{registry}")
    }
}

/// One pull against one reference: a bearer token is cached until a challenge asks for another.
pub struct RegistrySession<'a> {
    client: &'a RegistryClient,
    reference: &'a ImageReference,
    token: Option<Bearer>,
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

    /// The `/v2/` ping is discovery only: it never mints or caches a token.
    ///
    /// ghcr.io advertises a placeholder scope (`repository:user/image:pull`) that does not name
    /// the requested repository, and its token endpoint answers `403` for it. So the ping is sent
    /// unauthenticated and a `401` means "the endpoint exists and is the registry"; the scoped
    /// manifest challenge mints the pull token on the first real request.
    fn probe(&mut self) -> Result<()> {
        if self.probed {
            return Ok(());
        }
        let url = format!("{}/v2/", self.client.base(self.reference.registry()));
        match self.send(&url, None) {
            Ok(response) => drop(response),
            Err(err) => match *err {
                ureq::Error::Status(401, _) => {}
                err => return Err(self.map_error(&url, err)),
            },
        }
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

    /// `GET` with the bearer token, re-minting it once when a `401` names other auth terms.
    fn get(&mut self, url: &str, accept: Option<&str>) -> Result<ureq::Response> {
        self.get_once(url, accept, false)
    }

    fn get_once(
        &mut self,
        url: &str,
        accept: Option<&str>,
        retried: bool,
    ) -> Result<ureq::Response> {
        match self.send(url, accept) {
            Ok(response) => Ok(response),
            Err(err) => match *err {
                ureq::Error::Status(401, response) => {
                    let header = response
                        .header("WWW-Authenticate")
                        .ok_or_else(|| Error::Challenge {
                            header: String::new(),
                        })?
                        .to_owned();
                    let challenge = Challenge::parse(&header).ok_or(Error::Challenge { header })?;
                    // ghcr advertises a placeholder scope on the `/v2/` ping, so the ping is
                    // never authorized; the scoped manifest challenge mints the pull token.
                    if !may_retry(retried, self.token.as_ref(), &challenge) {
                        return Err(self.unauthorized());
                    }
                    let value = self.fetch_token(&challenge)?;
                    self.token = Some(Bearer::minted_for(&challenge, value));
                    self.get_once(url, accept, true)
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
            request = request.set("Authorization", &format!("Bearer {}", token.value));
        }
        request.call().map_err(Box::new)
    }

    fn fetch_token(&self, challenge: &Challenge) -> Result<String> {
        let credentials = self.client.credentials_for(self.reference.registry());
        let authorization = credentials.header();
        fetch_token_body(
            &self.client.agent,
            &token_url(challenge),
            &self.client.user_agent,
            authorization.as_deref(),
        )
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
            err => http_error(url, err),
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

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    use crate::Architecture;
    use crate::credentials::RegistryCredentials;
    use crate::reference::ImageReference;

    use super::{RegistryClient, RegistryScheme};

    const MANIFEST: &str = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a","size":2},"layers":[]}"#;

    /// Reads one request head and returns its first line (the request line).
    fn read_request_line(stream: &mut TcpStream) -> (String, String) {
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).expect("read the request head");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        let head = String::from_utf8_lossy(&request).into_owned();
        let line = head.lines().next().unwrap_or_default().to_owned();
        (line, head)
    }

    fn respond(stream: &mut TcpStream, status: &str, headers: &str, body: &[u8]) {
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
            body.len()
        );
        stream
            .write_all(head.as_bytes())
            .expect("write the response head");
        stream.write_all(body).expect("write the response body");
        stream.flush().expect("flush the response");
    }

    /// Serves the exact ghcr shape: a placeholder-scoped `/v2/` ping, the scoped manifest
    /// challenge, the token endpoint, then the retried manifest. Returns every request line.
    fn serve_registry() -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the loopback registry");
        let port = listener
            .local_addr()
            .expect("read the loopback address")
            .port();
        let addr = format!("127.0.0.1:{port}");
        let realm = format!("http://{addr}/token");
        let handle = thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().expect("accept a registry request");
                let (line, head) = read_request_line(&mut stream);
                seen.push(line.clone());
                if line.starts_with("GET /v2/ HTTP") {
                    respond(
                        &mut stream,
                        "401 Unauthorized",
                        &format!(
                            "WWW-Authenticate: Bearer realm=\"{realm}\",service=\"ghcr.io\",scope=\"repository:user/image:pull\"\r\n"
                        ),
                        b"",
                    );
                } else if line.starts_with("GET /token") {
                    respond(
                        &mut stream,
                        "200 OK",
                        "Content-Type: application/json\r\n",
                        br#"{"token":"t"}"#,
                    );
                } else if head.contains("Authorization: Bearer t") {
                    respond(
                        &mut stream,
                        "200 OK",
                        "Content-Type: application/vnd.oci.image.manifest.v1+json\r\n",
                        MANIFEST.as_bytes(),
                    );
                } else {
                    respond(
                        &mut stream,
                        "401 Unauthorized",
                        &format!(
                            "WWW-Authenticate: Bearer realm=\"{realm}\",service=\"ghcr.io\",scope=\"repository:owner/app:pull\"\r\n"
                        ),
                        b"",
                    );
                }
            }
            seen
        });
        (addr, handle)
    }

    /// A ghcr-style ping advertises `repository:user/image:pull`; the ping must stay
    /// unauthenticated and only the real manifest scope may ever be minted.
    #[test]
    fn the_v2_ping_never_mints_a_placeholder_token_and_the_manifest_scope_wins() {
        let (addr, server) = serve_registry();
        let reference =
            ImageReference::parse(&format!("{addr}/owner/app:latest")).expect("parse a reference");
        let client =
            RegistryClient::new(RegistryCredentials::default(), "slipfork/test".to_owned())
                .with_scheme(RegistryScheme::Http);
        let mut session = client.session(&reference);

        let resolved = session
            .manifest(Architecture::X86_64)
            .expect("the scoped manifest resolves after the placeholder ping");
        let seen = server.join().expect("the loopback registry exits");

        assert_eq!(resolved.manifest.layers.len(), 0);
        let token_requests: Vec<&String> = seen
            .iter()
            .filter(|line| line.starts_with("GET /token"))
            .collect();
        assert_eq!(token_requests.len(), 1, "exactly one token mint: {seen:?}");
        assert!(
            token_requests[0].contains("scope=repository:owner/app:pull"),
            "the token names the real repository: {:?}",
            token_requests[0]
        );
        assert!(
            !token_requests[0].contains("user/image"),
            "the placeholder scope is never minted: {:?}",
            token_requests[0]
        );
        assert!(
            seen.iter().any(|line| line.starts_with("GET /v2/ HTTP")),
            "the ping still runs: {seen:?}"
        );
    }

    /// Serves a scoped `/v2/` Bearer challenge then the token endpoint, capturing every head.
    fn serve_login(
        token_status: &'static str,
    ) -> (String, thread::JoinHandle<Vec<(String, String)>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the loopback registry");
        let port = listener
            .local_addr()
            .expect("read the loopback address")
            .port();
        let addr = format!("127.0.0.1:{port}");
        let realm = format!("http://{addr}/token");
        let handle = thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept a login request");
                let (line, head) = read_request_line(&mut stream);
                seen.push((line.clone(), head.clone()));
                if line.starts_with("GET /v2/ HTTP") {
                    respond(
                        &mut stream,
                        "401 Unauthorized",
                        &format!(
                            "WWW-Authenticate: Bearer realm=\"{realm}\",service=\"ghcr.io\",scope=\"repository:user/image:pull\"\r\n"
                        ),
                        b"",
                    );
                } else if line.starts_with("GET /token") {
                    if token_status == "200 OK" {
                        respond(
                            &mut stream,
                            token_status,
                            "Content-Type: application/json\r\n",
                            br#"{"token":"t"}"#,
                        );
                    } else {
                        respond(&mut stream, token_status, "", b"");
                    }
                } else {
                    respond(&mut stream, "404 Not Found", "", b"");
                }
            }
            seen
        });
        (addr, handle)
    }

    /// A loopback host selects HTTP by itself, and login mints a scope-less token with Basic.
    #[test]
    fn login_probe_mints_a_scopeless_token_over_auto_http() {
        let (addr, server) = serve_login("200 OK");
        let credentials = RegistryCredentials::default().with_host(&addr, "octo", "s3cret");
        let client = RegistryClient::new(credentials, "slipfork/test".to_owned());

        client
            .login(&addr)
            .expect("a loopback login probes over plain HTTP");

        let seen = server.join().expect("the loopback registry exits");
        let (line, head) = seen
            .iter()
            .find(|(line, _)| line.starts_with("GET /token"))
            .expect("exactly one token mint");
        assert!(line.contains("service=ghcr.io"), "{line}");
        assert!(
            !line.contains("scope="),
            "login never mints ghcr's placeholder scope: {line}"
        );
        assert!(
            head.contains("Authorization: Basic "),
            "login sends the injected Basic header: {head}"
        );
    }

    /// A refused login probe is one typed error whose `next:` carries the remedy, never a secret.
    #[test]
    fn login_probe_reports_bad_credentials_as_one_typed_error() {
        let (addr, server) = serve_login("401 Unauthorized");
        let credentials = RegistryCredentials::default().with_host(&addr, "octo", "wrong-secret");
        let client = RegistryClient::new(credentials, "slipfork/test".to_owned());

        let error = client
            .login(&addr)
            .expect_err("bad credentials must fail the login");

        assert!(
            matches!(error, crate::Error::LoginRejected { .. }),
            "{error:?}"
        );
        let text = error.to_string();
        assert!(text.contains("next: "), "{text}");
        assert!(
            !text.contains("wrong-secret"),
            "the secret stays out: {text}"
        );
        let _ = server.join();
    }

    /// A `401` with no `WWW-Authenticate` header is terminal: no identical retry is sent.
    #[test]
    fn login_probe_reports_a_challengeless_401_without_retrying() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the loopback registry");
        let port = listener
            .local_addr()
            .expect("read the loopback address")
            .port();
        let addr = format!("127.0.0.1:{port}");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept one login request");
            let (line, _) = read_request_line(&mut stream);
            respond(&mut stream, "401 Unauthorized", "", b"");
            line
        });

        let credentials = RegistryCredentials::default().with_host(&addr, "octo", "s3cret");
        let client = RegistryClient::new(credentials, "slipfork/test".to_owned());
        let error = client
            .login(&addr)
            .expect_err("a headerless 401 cannot authenticate");

        assert!(
            matches!(error, crate::Error::LoginRejected { .. }),
            "{error:?}"
        );
        let text = error.to_string();
        assert!(text.contains("next: "), "{text}");
        assert!(text.contains("HTTP 401"), "{text}");
        let line = server.join().expect("the loopback registry exits");
        assert!(
            line.starts_with("GET /v2/ HTTP"),
            "only one probe is sent: {line}"
        );
    }
}
