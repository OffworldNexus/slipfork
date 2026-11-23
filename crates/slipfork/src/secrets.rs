//! Generate-once-and-persist runtime resolvers for `secret(KEY, <generator>)` and
//! `var(KEY, <generator>)`.
//!
//! The `Forkfile` compiler stays pure: it names the generator, while this composition-root
//! adapter draws from the OS CSPRNG and stores the first value in the injected `0600` vaults so
//! `slipfork env`, `slipfork secret list`, later `up`s, and `fork`s all agree. Every failure is
//! returned to the caller: a vault that cannot be written is never silently skipped.

use std::collections::BTreeMap;
use std::fmt;

use slipfork_forkfile::{GeneratorSpec, RandomKind};
use slipfork_ospp::{FileSecretVault, Secret, SecretVault};

const VAR_COMPONENT: &str = "vars";

/// One resolver failure rendered as a single message plus one remedy line.
#[derive(Debug, Clone)]
pub(crate) struct ResolverError {
    /// The one-line `error:` text.
    pub message: String,
    /// The one-line `next:` remedy.
    pub next: &'static str,
}

impl fmt::Display for ResolverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl ResolverError {
    /// A vault read/write failure names the exact file; the remedy stays one line.
    fn vault(vault: &FileSecretVault, error: impl fmt::Display) -> Self {
        Self {
            message: format!("cannot persist `{}`: {error}", vault.path().display()),
            next: "make the state directory writable",
        }
    }

    fn entropy() -> Self {
        Self {
            message: "cannot read the host CSPRNG for a generated value".to_owned(),
            next: "check that /dev/urandom is available",
        }
    }
}

/// Injected resolver over `<state_dir>/secrets.json`, `<state_dir>/vars.json`, and process env.
pub(crate) struct EnvironmentResolver<'a> {
    secrets: &'a FileSecretVault,
    vars: &'a FileSecretVault,
    process_env: &'a BTreeMap<String, String>,
}

impl<'a> EnvironmentResolver<'a> {
    /// Binds the resolver to caller-owned vaults and the ambient process environment.
    pub(crate) fn new(
        secrets: &'a FileSecretVault,
        vars: &'a FileSecretVault,
        process_env: &'a BTreeMap<String, String>,
    ) -> Self {
        Self {
            secrets,
            vars,
            process_env,
        }
    }

    /// Resolves `secret(KEY, generator)`: stored value first, then a persisted CSPRNG value.
    pub(crate) fn secret(
        &self,
        component: &str,
        key: &str,
        generator: Option<&GeneratorSpec>,
    ) -> Result<Option<String>, ResolverError> {
        if let Some(secret) = self
            .secrets
            .load(component, key)
            .map_err(|error| ResolverError::vault(self.secrets, error))?
        {
            return Ok(Some(secret.expose().to_owned()));
        }
        let Some(generator) = generator else {
            return Ok(None);
        };
        let value = generate(generator).ok_or_else(ResolverError::entropy)?;
        self.secrets
            .store(component, key, &Secret::new(value.clone()))
            .map_err(|error| ResolverError::vault(self.secrets, error))?;
        Ok(Some(value))
    }

    /// Resolves `var(KEY, generator)`: process env wins, then `vars.json`, then a persisted value.
    pub(crate) fn var(
        &self,
        key: &str,
        generator: Option<&GeneratorSpec>,
    ) -> Result<Option<String>, ResolverError> {
        if let Some(value) = self.process_env.get(key) {
            return Ok(Some(value.clone()));
        }
        if let Some(stored) = self
            .vars
            .load(VAR_COMPONENT, key)
            .map_err(|error| ResolverError::vault(self.vars, error))?
        {
            return Ok(Some(stored.expose().to_owned()));
        }
        let Some(generator) = generator else {
            return Ok(None);
        };
        let value = generate(generator).ok_or_else(ResolverError::entropy)?;
        self.vars
            .store(VAR_COMPONENT, key, &Secret::new(value.clone()))
            .map_err(|error| ResolverError::vault(self.vars, error))?;
        Ok(Some(value))
    }
}

/// Draws one generator value from the OS CSPRNG.
fn generate(spec: &GeneratorSpec) -> Option<String> {
    let length = spec.n.and_then(|n| usize::try_from(n).ok()).unwrap_or(0);
    match spec.kind {
        RandomKind::Uuid => {
            let mut bytes = [0_u8; 16];
            getrandom::fill(&mut bytes).ok()?;
            Some(uuid(&bytes))
        }
        RandomKind::String => {
            let mut bytes = vec![0_u8; length.max(1)];
            getrandom::fill(&mut bytes).ok()?;
            const ALPHABET: &[u8] =
                b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
            Some(
                bytes
                    .iter()
                    .take(length)
                    .map(|byte| ALPHABET[*byte as usize % ALPHABET.len()] as char)
                    .collect(),
            )
        }
        RandomKind::Hex => {
            let mut bytes = vec![0_u8; length.div_ceil(2).max(1)];
            getrandom::fill(&mut bytes).ok()?;
            let mut text = hex::encode(bytes);
            text.truncate(length);
            Some(text)
        }
        RandomKind::Base64 => {
            use base64::Engine;
            let mut bytes = vec![0_u8; length.max(1)];
            getrandom::fill(&mut bytes).ok()?;
            let mut text = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
            text.truncate(length);
            Some(text)
        }
    }
}

fn uuid(bytes: &[u8; 16]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-4{:01x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6] & 0x0f,
        bytes[7],
        (bytes[8] & 0x3f) | 0x80,
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

#[cfg(test)]
mod tests {
    use super::{EnvironmentResolver, generate};
    use slipfork_forkfile::{GeneratorSpec, RandomKind};

    #[test]
    fn generated_values_persist_once_and_keep_0600() {
        use slipfork_ospp::FileSecretVault;
        use std::collections::BTreeMap;
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::TempDir::new_in("/var/tmp").expect("disk temp dir");
        let secrets = FileSecretVault::new(directory.path().join("secrets.json"));
        let vars = FileSecretVault::new(directory.path().join("vars.json"));
        let empty = BTreeMap::new();
        let resolver = EnvironmentResolver::new(&secrets, &vars, &empty);
        let spec = GeneratorSpec {
            kind: RandomKind::String,
            n: Some(16),
        };

        let first = resolver
            .secret("api", "TOKEN", Some(&spec))
            .expect("entropy")
            .expect("generated");
        assert_eq!(first.len(), 16);
        assert_eq!(
            resolver
                .secret("api", "TOKEN", None)
                .expect("load")
                .as_deref(),
            Some(first.as_str())
        );

        let generated = resolver
            .var("VAR", Some(&spec))
            .expect("entropy")
            .expect("generated");
        assert_eq!(
            resolver.var("VAR", Some(&spec)).expect("load").as_deref(),
            Some(generated.as_str())
        );
        for path in [secrets.path(), vars.path()] {
            let mode = std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{} must be 0600", path.display());
        }
    }

    #[test]
    fn every_generator_honours_its_length_and_alphabet() {
        let spec = |kind, n| GeneratorSpec { kind, n };
        let text = generate(&spec(RandomKind::String, Some(50))).expect("entropy");
        assert_eq!(text.len(), 50);
        assert!(text.chars().all(|c| c.is_ascii_alphanumeric()));

        let hex = generate(&spec(RandomKind::Hex, Some(32))).expect("entropy");
        assert_eq!(hex.len(), 32);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));

        let base64 = generate(&spec(RandomKind::Base64, Some(32))).expect("entropy");
        assert_eq!(base64.len(), 32);
        assert!(!base64.contains(['+', '/', '=']));

        let uuid = generate(&spec(RandomKind::Uuid, None)).expect("entropy");
        assert_eq!(uuid.len(), 36);
        assert_eq!(uuid.as_bytes()[14], b'4');
    }
}
