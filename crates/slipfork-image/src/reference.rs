//! OCI image reference parsing (`registry/repository[:tag|@digest]`).

use std::fmt;

use crate::error::{Error, Result};

const DOCKER_HUB: &str = "docker.io";
/// Docker Hub's Distribution API host: `docker.io`/`index.docker.io` are web aliases
/// that do not serve `/v2/`, so every Hub reference is normalized to this host.
const DOCKER_HUB_API: &str = "registry-1.docker.io";
const MAX_TAG: usize = 128;

/// A parsed OCI image reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageReference {
    registry: String,
    repository: String,
    tag: Option<String>,
    digest: Option<String>,
}

impl ImageReference {
    /// Parses `raw`, applying Docker Hub defaults (`docker.io/library/<name>:latest`).
    pub fn parse(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(invalid(raw, "the reference is empty"));
        }
        let (name, digest) = match raw.split_once('@') {
            Some((name, digest)) => {
                validate_digest(digest).map_err(|reason| invalid(raw, reason))?;
                (name, Some(digest.to_owned()))
            }
            None => (raw, None),
        };
        let (name, tag) = if digest.is_some() {
            (name, None)
        } else {
            split_tag(name).map_err(|reason| invalid(raw, reason))?
        };
        let mut parts: Vec<&str> = name.split('/').collect();
        if parts.iter().any(|part| part.is_empty()) {
            return Err(invalid(raw, "the repository has an empty path segment"));
        }
        let registry = if is_registry(parts[0]) {
            canonical_registry(parts.remove(0))
        } else {
            DOCKER_HUB_API.to_owned()
        };
        if parts.is_empty() {
            return Err(invalid(raw, "the repository is empty"));
        }
        if registry == DOCKER_HUB_API && parts.len() == 1 {
            parts.insert(0, "library");
        }
        for part in &parts {
            validate_component(part).map_err(|reason| invalid(raw, reason))?;
        }
        Ok(Self {
            registry,
            repository: parts.join("/"),
            tag,
            digest,
        })
    }

    /// Registry host, including an explicit port when the caller supplied one.
    #[must_use]
    pub fn registry(&self) -> &str {
        &self.registry
    }

    /// Repository path inside the registry (`library/postgres`).
    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// Explicit tag, if the reference carried one.
    #[must_use]
    pub fn tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }

    /// Explicit digest, if the reference pinned one.
    #[must_use]
    pub fn digest(&self) -> Option<&str> {
        self.digest.as_deref()
    }

    /// The `manifests/<...>` reference: digest, else tag, else `latest`.
    #[must_use]
    pub fn reference(&self) -> &str {
        self.digest
            .as_deref()
            .or(self.tag.as_deref())
            .unwrap_or("latest")
    }
}

impl fmt::Display for ImageReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.registry, self.repository)?;
        if let Some(digest) = &self.digest {
            write!(f, "@{digest}")
        } else if let Some(tag) = &self.tag {
            write!(f, ":{tag}")
        } else {
            Ok(())
        }
    }
}

fn invalid(reference: &str, reason: &'static str) -> Error {
    Error::InvalidReference {
        reference: reference.to_owned(),
        reason,
    }
}

/// Maps Docker Hub's aliases onto the one host that serves the Distribution API.
fn canonical_registry(host: &str) -> String {
    match host {
        DOCKER_HUB | "index.docker.io" | "registry.hub.docker.com" => DOCKER_HUB_API.to_owned(),
        other => other.to_owned(),
    }
}

fn is_registry(component: &str) -> bool {
    component == "localhost" || component.contains('.') || component.contains(':')
}

fn split_tag(name: &str) -> std::result::Result<(&str, Option<String>), &'static str> {
    let Some((head, tag)) = name.rsplit_once(':') else {
        return Ok((name, None));
    };
    validate_tag(tag)?;
    Ok((head, Some(tag.to_owned())))
}

fn validate_tag(tag: &str) -> std::result::Result<(), &'static str> {
    let mut chars = tag.chars();
    let starts_well = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    let rest_well = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if starts_well && rest_well && tag.len() <= MAX_TAG {
        Ok(())
    } else {
        Err("the tag must be 1-128 chars of `[A-Za-z0-9._-]`")
    }
}

fn validate_component(component: &str) -> std::result::Result<(), &'static str> {
    let well_formed = !component.is_empty()
        && component.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        });
    let edge_ok = !component.starts_with(['.', '-'])
        && !component.ends_with(['.', '-'])
        && !component.contains("..");
    if well_formed && edge_ok {
        Ok(())
    } else {
        Err("repository components must be lowercase `[a-z0-9._-]`")
    }
}

fn validate_digest(digest: &str) -> std::result::Result<(), &'static str> {
    let Some((algorithm, encoded)) = digest.split_once(':') else {
        return Err("a digest needs the `<algorithm>:<hex>` form");
    };
    let algorithm_ok = !algorithm.is_empty()
        && algorithm
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    let encoded_ok = !encoded.is_empty()
        && encoded.len() <= 128
        && encoded
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    if algorithm_ok && encoded_ok {
        Ok(())
    } else {
        Err("a digest needs the `<algorithm>:<lowercase-hex>` form")
    }
}
