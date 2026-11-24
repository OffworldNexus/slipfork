//! Per-host registry credentials: one plain value loaded once by the composition root.
//!
//! The store is `<config>/slipfork/registries.json` (mode `0600`), shape
//! `{"<host>": {"username": "...", "secret": "..."}}`. Docker's `config.json` is a per-host
//! read-only fallback for hosts the Slipfork file does not name; it is never written. Secrets live
//! in memory and on disk only: `Debug` always prints `[REDACTED]`.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::reference::canonical_host;
use crate::registry::Credentials;

/// One credentials entry as stored on disk.
#[derive(Deserialize, Serialize)]
struct StoredCredential {
    username: String,
    secret: String,
}

/// The `auths` slice of Docker's `config.json` that the fallback reads.
#[derive(Deserialize)]
struct DockerConfig {
    #[serde(default)]
    auths: BTreeMap<String, DockerAuth>,
}

/// One Docker `auths[host]` entry; only the base64 `user:pass` field is consulted.
#[derive(Deserialize)]
struct DockerAuth {
    #[serde(default)]
    auth: Option<String>,
}

/// Per-host registry credentials keyed by canonical registry host.
#[derive(Clone, Default)]
pub struct RegistryCredentials {
    /// Entries the Slipfork store owns and rewrites.
    entries: BTreeMap<String, Credentials>,
    /// Docker's `config.json` fallback, consulted but never written back.
    inherited: BTreeMap<String, Credentials>,
}

impl RegistryCredentials {
    /// Loads `<config>/slipfork/registries.json`, then fills unnamed hosts from Docker.
    ///
    /// A missing file is empty. A malformed Slipfork file is an error (pulls would silently
    /// become anonymous); a malformed Docker file is ignored because it is only a fallback.
    pub fn load(registries: &Path, docker_config: Option<&Path>) -> Result<Self> {
        let mut credentials = Self::default();
        match fs::read(registries) {
            Ok(bytes) => {
                let document: BTreeMap<String, StoredCredential> =
                    serde_json::from_slice(&bytes).map_err(|source| Error::Json {
                        url: registries.display().to_string(),
                        source,
                    })?;
                for (host, stored) in document {
                    credentials.set(&host, stored.username, stored.secret)?;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(Error::io("read", registries, source)),
        }
        if let Some(path) = docker_config
            && let Ok(bytes) = fs::read(path)
            && let Ok(document) = serde_json::from_slice::<DockerConfig>(&bytes)
        {
            for (host, auth) in document.auths {
                let host = canonical_host(&host);
                if host.is_empty() || credentials.entries.contains_key(&host) {
                    continue;
                }
                let Some((username, secret)) = auth.auth.as_deref().and_then(decode_auth) else {
                    continue;
                };
                credentials
                    .inherited
                    .insert(host, Credentials::basic(username, secret));
            }
        }
        Ok(credentials)
    }

    /// Reads one host's credentials, falling back to Docker, then anonymous.
    #[must_use]
    pub fn credentials_for(&self, host: &str) -> Credentials {
        let host = canonical_host(host);
        self.entries
            .get(&host)
            .or_else(|| self.inherited.get(&host))
            .cloned()
            .unwrap_or(Credentials::Anonymous)
    }

    /// Records `host`'s username and secret under its canonical host name.
    ///
    /// # Errors
    ///
    /// Refuses an empty host instead of writing an entry no reference can ever match.
    pub fn set(
        &mut self,
        host: &str,
        username: impl Into<String>,
        secret: impl Into<String>,
    ) -> Result<&mut Self> {
        let host = canonical_host(host);
        if host.is_empty() {
            return Err(Error::InvalidRegistry {
                registry: String::new(),
            });
        }
        self.inherited.remove(&host);
        self.entries
            .insert(host, Credentials::basic(username, secret));
        Ok(self)
    }

    /// Adds one host to an existing value, for callers that already hold a builder.
    #[must_use]
    pub fn with_host(
        mut self,
        host: &str,
        username: impl Into<String>,
        secret: impl Into<String>,
    ) -> Self {
        let host = canonical_host(host);
        if !host.is_empty() {
            self.entries
                .insert(host, Credentials::basic(username, secret));
        }
        self
    }

    /// Forgets one host in this process; returns whether any entry was present.
    pub fn remove(&mut self, host: &str) -> bool {
        let host = canonical_host(host);
        self.entries.remove(&host).is_some() | self.inherited.remove(&host).is_some()
    }

    /// Canonical host and username of every Slipfork-stored entry; Docker fallbacks are implicit.
    pub fn hosts(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.entries
            .iter()
            .filter_map(|(host, credentials)| match credentials {
                Credentials::Basic { username, .. } => Some((host.as_str(), username.as_str())),
                Credentials::Anonymous => None,
            })
    }

    /// Writes the store atomically with mode `0600`; the previous file survives a failed rename.
    ///
    /// # Errors
    ///
    /// Propagates directory creation, serialization, write, chmod, and rename failures.
    pub fn save(&self, registries: &Path) -> Result<()> {
        if let Some(parent) = registries
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|source| Error::io("create", parent, source))?;
        }
        let mut document = BTreeMap::new();
        for (host, credentials) in &self.entries {
            if let Credentials::Basic { username, password } = credentials {
                document.insert(
                    host.clone(),
                    StoredCredential {
                        username: username.clone(),
                        secret: password.clone(),
                    },
                );
            }
        }
        let bytes = serde_json::to_vec_pretty(&document).map_err(|source| Error::Json {
            url: registries.display().to_string(),
            source,
        })?;
        let temporary = registries.with_extension("json.tmp");
        // A crashed save may leave a wider-mode temp file; remove it so `mode(0o600)` applies.
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(Error::io("remove", &temporary, source)),
        }
        // Create with 0600 *before* any secret byte reaches the file: no world-readable window.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|source| Error::io("create", &temporary, source))?;
        file.write_all(&bytes)
            .map_err(|source| Error::io("write", &temporary, source))?;
        drop(file);
        fs::rename(&temporary, registries)
            .map_err(|source| Error::io("install", registries, source))
    }
}

impl fmt::Debug for RegistryCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegistryCredentials")
            .field("entries", &self.entries)
            .field("inherited", &self.inherited)
            .finish()
    }
}

/// A loopback registry (the lab's `127.0.0.1[:port]` / `localhost[:port]`) speaks plain HTTP.
#[must_use]
pub(crate) fn is_loopback(registry: &str) -> bool {
    let host = canonical_host(registry);
    let host = host_only(&host);
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// Strips one `:port` or `[v6]:port` suffix from a canonical host.
fn host_only(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match host.rsplit_once(':') {
        Some((head, port))
            if !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            head
        }
        _ => host,
    }
}

/// Decodes Docker's base64 `user:pass` auth blob; anything malformed is skipped.
fn decode_auth(auth: &str) -> Option<(String, String)> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(auth.trim())
        .ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let (username, secret) = text.split_once(':')?;
    (!username.is_empty()).then(|| (username.to_owned(), secret.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use base64::Engine as _;

    use crate::reference::canonical_host;
    use crate::registry::Credentials;

    use super::{RegistryCredentials, is_loopback};

    /// A real disk tree under `/var/tmp` (never RAM-backed `/tmp`), removed on drop.
    struct Lab {
        root: PathBuf,
    }

    impl Lab {
        fn new(prefix: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos());
            let root = PathBuf::from("/var/tmp").join(format!(
                "slipfork-credentials-{prefix}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&root).expect("create the credentials lab under /var/tmp");
            Self { root }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.join(relative)
        }
    }

    impl Drop for Lab {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn docker_auth(username: &str, secret: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(format!("{username}:{secret}"))
    }

    #[test]
    fn store_roundtrip_is_mode_0600_and_debug_redacts_the_secret() {
        let lab = Lab::new("roundtrip");
        let path = lab.path("slipfork/registries.json");
        let mut store = RegistryCredentials::default();
        store
            .set("https://ghcr.io/", "octo", "s3cret")
            .expect("record ghcr");
        assert!(matches!(
            store.credentials_for("ghcr.io"),
            Credentials::Basic { .. }
        ));
        store.save(&path).expect("write the 0600 store");

        let mode = fs::metadata(&path)
            .expect("stat the store")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the credentials file must be owner-only");

        let loaded = RegistryCredentials::load(&path, None).expect("read the store back");
        assert!(matches!(
            loaded.credentials_for("https://ghcr.io"),
            Credentials::Basic { .. }
        ));
        assert_eq!(
            loaded.hosts().collect::<Vec<_>>(),
            vec![("ghcr.io", "octo")]
        );
        let debug = format!("{loaded:?}");
        assert!(debug.contains("octo"), "{debug}");
        assert!(!debug.contains("s3cret"), "Debug must redact: {debug}");

        let mut loaded = loaded;
        assert!(loaded.remove("ghcr.io"));
        loaded.save(&path).expect("rewrite the empty store");
        let empty = RegistryCredentials::load(&path, None).expect("reload the empty store");
        assert_eq!(empty.hosts().count(), 0);
    }

    #[test]
    fn docker_fallback_fills_only_hosts_the_slipfork_store_does_not_name() {
        let lab = Lab::new("docker");
        let registries = lab.path("slipfork/registries.json");
        fs::create_dir_all(registries.parent().expect("store parent")).expect("create config dir");
        fs::write(
            &registries,
            r#"{"ghcr.io":{"username":"from-file","secret":"file-secret"}}"#,
        )
        .expect("write the slipfork store");
        let docker = lab.path("docker/config.json");
        fs::create_dir_all(docker.parent().expect("docker parent")).expect("create docker dir");
        fs::write(
            &docker,
            format!(
                r#"{{"auths":{{"https://index.docker.io/v1/":{{"auth":"{}"}},"ghcr.io":{{"auth":"{}"}},"quay.io":{{"auth":"not-base64"}}}}}}"#,
                docker_auth("hub-user", "hub-secret"),
                docker_auth("docker-user", "docker-secret")
            ),
        )
        .expect("write the docker config");

        let credentials =
            RegistryCredentials::load(&registries, Some(&docker)).expect("load both stores");
        assert_eq!(
            credentials.hosts().collect::<Vec<_>>(),
            vec![("ghcr.io", "from-file")],
            "only the Slipfork-owned entry is listed; the Docker fallback stays implicit"
        );
        assert!(
            matches!(
                credentials.credentials_for("https://index.docker.io/v1/"),
                Credentials::Basic { .. }
            ),
            "the Hub alias resolves Docker's base64 user:pass fallback"
        );
        assert!(
            matches!(
                credentials.credentials_for("ghcr.io"),
                Credentials::Basic { .. }
            ),
            "the Slipfork file wins over Docker for a named host"
        );
        credentials.save(&registries).expect("rewrite the store");
        let written = fs::read_to_string(&registries).expect("read the rewritten store");
        assert!(
            written.contains("file-secret"),
            "the owned entry is persisted: {written}"
        );
        assert!(
            !written.contains("hub-secret"),
            "Docker's fallback secret is never copied into the Slipfork store: {written}"
        );
        assert!(matches!(
            credentials.credentials_for("quay.io"),
            Credentials::Anonymous
        ));
    }

    #[test]
    fn host_normalization_collapses_hub_aliases_schemes_and_trailing_slashes() {
        for raw in [
            "docker.io",
            "index.docker.io",
            "registry.hub.docker.com",
            "registry-1.docker.io",
            "https://index.docker.io/v1/",
            "HTTP://DOCKER.IO/",
        ] {
            assert_eq!(canonical_host(raw), "registry-1.docker.io", "{raw}");
        }
        assert_eq!(canonical_host("ghcr.io/"), "ghcr.io");
        assert_eq!(canonical_host(" HTTPS://GHCR.IO/v2/ "), "ghcr.io");
        assert_eq!(canonical_host("localhost:5000/"), "localhost:5000");
    }

    #[test]
    fn only_loopback_hosts_select_plain_http() {
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("127.0.0.1:5000"));
        assert!(is_loopback("localhost"));
        assert!(is_loopback("LOCALHOST:8080"));
        assert!(is_loopback("[::1]:5000"));
        assert!(!is_loopback("ghcr.io"));
        assert!(!is_loopback("127.0.0.1.example"));
    }

    #[test]
    fn a_missing_store_loads_empty_and_an_empty_host_is_refused() {
        let lab = Lab::new("missing");
        let credentials =
            RegistryCredentials::load(&lab.path("absent/registries.json"), None).expect("empty");
        assert_eq!(credentials.hosts().count(), 0);
        let mut credentials = credentials;
        let error = credentials
            .set("  ", "user", "secret")
            .expect_err("an empty host has no reference to match");
        assert!(format!("{error}").contains("next: "), "{error}");
    }
}
