//! `RemoteResourceBundle` (S3 buckets with redacted `Debug`, PostgreSQL dump metadata) and the
//! single `OsppClient::resource_offer()` conversion into a remote resource offer.
//!
//! The wire types mirror `ADR-0008` §2's `ResourcesDescribe` fields exactly.
//! [`RemoteResourceBundle`] is a *transport* value: it validates the server's coordinates and, in
//! one crate-private step, becomes `slipfork_dag::RemoteResourceOffer`. No resolution policy
//! (`ResourceStrategy`, `DumpPolicy`, precedence) exists here; `slipfork_dag::ResourcePlan::resolve`
//! is the sole decision point and `OsppClient::resource_offer` is the sole bundle-to-offer mapper.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use slipfork_dag::{RemotePgDump, RemoteResourceOffer, RemoteS3Bucket};

use crate::error::OsppError;
use crate::hex::is_sha256_hex;
use crate::protocol::CAPABILITY_RESOURCES_DESCRIBE;

/// S3 coordinates offered by OSPP (`s3_buckets.<id>`).
///
/// `Debug` never prints the access key or secret key; consuming code builds
/// `slipfork_snow::S3Coordinates` from the accessors inside `slipforkd`.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3BucketCoordinates {
    endpoint: String,
    region: String,
    bucket: String,
    access_key: String,
    secret_key: String,
}

impl S3BucketCoordinates {
    /// Construct coordinates from typed `s3_buckets.<id>` fields.
    pub fn new(
        endpoint: impl Into<String>,
        region: impl Into<String>,
        bucket: impl Into<String>,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            region: region.into(),
            bucket: bucket.into(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
        }
    }

    /// S3-compatible endpoint URL.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// S3 region.
    pub fn region(&self) -> &str {
        &self.region
    }

    /// Bucket name.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Access key id (redacted by `Debug`).
    pub fn access_key(&self) -> &str {
        &self.access_key
    }

    /// Secret access key (redacted by `Debug`).
    pub fn secret_key(&self) -> &str {
        &self.secret_key
    }
}

impl fmt::Debug for S3BucketCoordinates {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3BucketCoordinates")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key", &"[REDACTED]")
            .field("secret_key", &"[REDACTED]")
            .finish()
    }
}

/// Compression format of an OSPP PostgreSQL seed dump.
///
/// `pg_dump --format=custom` archives use zstd; any other advertised format fails deserialization
/// (and therefore the capability) instead of silently falling back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PgDumpFormat {
    /// `pg_dump --format=custom` with zstd-compressed data blocks.
    PgCustomZstd,
}

/// Digest-pinned PostgreSQL seed-dump metadata offered by OSPP (`pg_databases.<id>`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PgDumpMetadata {
    db_id: String,
    sha256_compressed: String,
    compressed_bytes: u64,
    format: PgDumpFormat,
}

impl PgDumpMetadata {
    /// Construct metadata from typed `pg_databases.<id>` fields.
    pub fn new(
        db_id: impl Into<String>,
        sha256_compressed: impl Into<String>,
        compressed_bytes: u64,
        format: PgDumpFormat,
    ) -> Self {
        Self {
            db_id: db_id.into(),
            sha256_compressed: sha256_compressed.into(),
            compressed_bytes,
            format,
        }
    }

    /// Database id the archive was dumped from.
    pub fn db_id(&self) -> &str {
        &self.db_id
    }

    /// SHA-256 of the compressed archive (64 lowercase hex characters).
    pub fn sha256_compressed(&self) -> &str {
        &self.sha256_compressed
    }

    /// Compressed archive size in bytes.
    pub fn compressed_bytes(&self) -> u64 {
        self.compressed_bytes
    }

    /// Archive compression format.
    pub fn format(&self) -> PgDumpFormat {
        self.format
    }
}

/// Raw `ResourcesDescribe` body: typed `s3_buckets.<id>` and `pg_databases.<id>` maps.
///
/// This is the only type `OsppClient` deserializes coordinates into. Its `Debug` is redacted, and
/// only [`crate::OsppClient::resource_offer`] converts it into `slipfork_dag::RemoteResourceOffer`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RemoteResourceBundle {
    s3_buckets: BTreeMap<String, S3BucketCoordinates>,
    pg_databases: BTreeMap<String, PgDumpMetadata>,
}

impl RemoteResourceBundle {
    /// Build a bundle from the server's two coordinate maps.
    pub fn new(
        s3_buckets: BTreeMap<String, S3BucketCoordinates>,
        pg_databases: BTreeMap<String, PgDumpMetadata>,
    ) -> Self {
        Self {
            s3_buckets,
            pg_databases,
        }
    }

    /// Offered coordinates for `id`, if the server described that bucket.
    pub fn s3_bucket(&self, id: &str) -> Option<&S3BucketCoordinates> {
        self.s3_buckets.get(id)
    }

    /// Offered seed-dump metadata for `id`, if the server described that database.
    pub fn pg_dump(&self, id: &str) -> Option<&PgDumpMetadata> {
        self.pg_databases.get(id)
    }

    /// `true` when the server offered no resources at all.
    pub fn is_empty(&self) -> bool {
        self.s3_buckets.is_empty() && self.pg_databases.is_empty()
    }

    /// The single crate-private bundle-to-offer conversion used by `OsppClient::resource_offer`.
    ///
    /// Every field is validated here: an empty bucket field, a mismatched `db_id`, a non-lowercase
    /// digest, a zero byte count, or a non-zstd format fails closed instead of planning a restore
    /// from metadata the server cannot honour.
    pub(crate) fn into_offer(self, scope: &str) -> Result<RemoteResourceOffer, OsppError> {
        let mut s3_buckets = BTreeMap::new();
        for (id, bucket) in self.s3_buckets {
            for (field, value) in [
                ("endpoint", bucket.endpoint()),
                ("region", bucket.region()),
                ("bucket", bucket.bucket()),
                ("access_key", bucket.access_key()),
                ("secret_key", bucket.secret_key()),
            ] {
                if value.is_empty() {
                    return Err(invalid_bundle(
                        scope,
                        format!("s3_buckets.{id} is missing `{field}`"),
                    ));
                }
            }
            s3_buckets.insert(
                id,
                RemoteS3Bucket::new(
                    bucket.endpoint(),
                    bucket.region(),
                    bucket.bucket(),
                    bucket.access_key(),
                    bucket.secret_key(),
                ),
            );
        }

        let mut pg_databases = BTreeMap::new();
        for (id, metadata) in self.pg_databases {
            if metadata.db_id() != id {
                return Err(invalid_bundle(
                    scope,
                    format!("pg_databases.{id} advertises db_id `{}`", metadata.db_id()),
                ));
            }
            if !is_sha256_hex(metadata.sha256_compressed()) {
                return Err(invalid_bundle(
                    scope,
                    format!("pg_databases.{id} digest is not 64 lowercase hex characters"),
                ));
            }
            if metadata.compressed_bytes() == 0 {
                return Err(invalid_bundle(
                    scope,
                    format!("pg_databases.{id} advertises a zero-byte archive"),
                ));
            }
            pg_databases.insert(
                id,
                RemotePgDump::new(metadata.sha256_compressed(), metadata.compressed_bytes()),
            );
        }

        Ok(RemoteResourceOffer::new(s3_buckets, pg_databases))
    }
}

fn invalid_bundle(scope: &str, message: String) -> OsppError {
    OsppError::Capability {
        capability: CAPABILITY_RESOURCES_DESCRIBE,
        scope: scope.to_owned(),
        message,
    }
}
