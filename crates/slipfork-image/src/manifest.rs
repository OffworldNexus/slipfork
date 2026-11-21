//! OCI manifest and manifest-list documents.

use serde::Deserialize;

use crate::Architecture;
use crate::error::{Error, Result};

/// One OCI content descriptor.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    /// Media type of the referenced document or blob.
    #[serde(default)]
    pub media_type: Option<String>,
    /// `sha256:<hex>` content digest.
    pub digest: String,
    /// Size in bytes.
    #[serde(default)]
    pub size: u64,
    /// Platform of a manifest-list entry.
    #[serde(default)]
    pub platform: Option<Platform>,
}

/// Platform block of a manifest-list entry.
#[derive(Debug, Clone, Deserialize)]
pub struct Platform {
    /// OCI architecture (`amd64`, `arm64`, …).
    pub architecture: String,
    /// Operating system (`linux`).
    pub os: String,
    /// Optional architecture variant.
    #[serde(default)]
    pub variant: Option<String>,
}

/// A per-architecture image manifest.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageManifest {
    /// Image config descriptor (not downloaded: unpacking needs only the layers).
    pub config: Descriptor,
    /// Ordered layer descriptors, applied bottom-up.
    pub layers: Vec<Descriptor>,
    /// Declared media type.
    #[serde(default)]
    pub media_type: Option<String>,
}

/// A multi-arch manifest list / OCI image index.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageIndex {
    /// Per-platform manifests.
    pub manifests: Vec<Descriptor>,
    /// Declared media type.
    #[serde(default)]
    pub media_type: Option<String>,
}

impl ImageIndex {
    /// The first `linux/<architecture>` manifest.
    #[must_use]
    pub fn select(&self, architecture: Architecture) -> Option<&Descriptor> {
        self.manifests.iter().find(|descriptor| {
            descriptor.platform.as_ref().is_some_and(|platform| {
                platform.os == "linux" && platform.architecture == architecture.oci()
            })
        })
    }

    /// Comma-separated `os/arch` list for a typed error message.
    #[must_use]
    pub fn available(&self) -> String {
        self.manifests
            .iter()
            .filter_map(|descriptor| descriptor.platform.as_ref())
            .map(|platform| format!("{}/{}", platform.os, platform.architecture))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Either document a registry can answer a manifest request with.
pub(crate) enum Document {
    Index(ImageIndex),
    Manifest(ImageManifest),
}

impl Document {
    /// Parses whichever document the registry returned.
    pub(crate) fn parse(body: &[u8], url: &str) -> Result<Self> {
        let value: serde_json::Value =
            serde_json::from_slice(body).map_err(|source| Error::Json {
                url: url.to_owned(),
                source,
            })?;
        if value.get("manifests").is_some() {
            serde_json::from_value(value)
                .map(Self::Index)
                .map_err(|source| Error::Json {
                    url: url.to_owned(),
                    source,
                })
        } else {
            serde_json::from_value(value)
                .map(Self::Manifest)
                .map_err(|source| Error::Json {
                    url: url.to_owned(),
                    source,
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> ImageIndex {
        serde_json::from_value(serde_json::json!({
            "schemaVersion": 2,
            "manifests": [
                {
                    "mediaType": "application/vnd.oci.image.manifest.v1+json",
                    "digest": "sha256:amd64",
                    "size": 10,
                    "platform": { "architecture": "amd64", "os": "linux" }
                },
                {
                    "mediaType": "application/vnd.oci.image.manifest.v1+json",
                    "digest": "sha256:arm64",
                    "size": 10,
                    "platform": { "architecture": "arm64", "os": "linux", "variant": "v8" }
                },
                {
                    "mediaType": "application/vnd.oci.image.manifest.v1+json",
                    "digest": "sha256:windows",
                    "size": 10,
                    "platform": { "architecture": "amd64", "os": "windows" }
                }
            ]
        }))
        .expect("valid index")
    }

    #[test]
    fn manifest_lists_resolve_the_injected_architecture() {
        let index = index();
        assert_eq!(
            index
                .select(Architecture::X86_64)
                .expect("amd64 entry")
                .digest,
            "sha256:amd64"
        );
        assert_eq!(
            index
                .select(Architecture::Aarch64)
                .expect("arm64 entry")
                .digest,
            "sha256:arm64"
        );
        assert_eq!(index.available(), "linux/amd64, linux/arm64, windows/amd64");
    }
}
