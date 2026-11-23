//! On-disk object store: atomic writes, sidecar metadata, and reads under one bucket.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use super::listing::{self, ListQuery, Listing};
use super::policy::S3Bucket;
use crate::error::{Error, Result};

/// Metadata sidecar for one object (`meta/<key>.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ObjectMeta {
    /// Stored `Content-Type`.
    pub content_type: String,
    /// Quoted MD5 digest (`"<hex>"`).
    pub etag: String,
    /// Byte length of the object payload.
    pub content_length: u64,
    /// RFC 3339 UTC timestamp of the last write.
    pub last_modified: String,
}

impl ObjectMeta {
    /// Reads one sidecar; `None` when it does not exist yet.
    pub(crate) fn read(path: &Path) -> Result<Option<Self>> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(Error::io("read", path, source)),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|source| Error::Json {
                path: path.to_path_buf(),
                source,
            })
    }
}

/// One bucket's objects and metadata inside `<world>/s3/<bucket>/`.
#[derive(Debug, Clone)]
pub(crate) struct S3BucketStore {
    root: PathBuf,
    bucket: String,
}

impl S3BucketStore {
    /// Builds the store for `bucket` under the world's `s3_root`.
    pub(crate) fn new(s3_root: &Path, bucket: &S3Bucket) -> Self {
        Self {
            root: s3_root.join(bucket.name()),
            bucket: bucket.name().to_owned(),
        }
    }

    /// Creates `objects/` and `meta/`; idempotent.
    pub(crate) fn create(&self) -> Result<()> {
        for dir in [self.objects(), self.meta()] {
            fs::create_dir_all(&dir).map_err(|source| Error::io("create", &dir, source))?;
        }
        Ok(())
    }

    fn objects(&self) -> PathBuf {
        self.root.join("objects")
    }

    fn meta(&self) -> PathBuf {
        self.root.join("meta")
    }

    fn object_path(&self, key: &str) -> Result<PathBuf> {
        Ok(self.objects().join(validate_key(key)?))
    }

    fn meta_path(&self, key: &str) -> Result<PathBuf> {
        let name = validate_key(key)?;
        Ok(self.meta().join(format!("{}.json", name.to_string_lossy())))
    }

    /// Writes `body` atomically and returns its fresh metadata.
    pub(crate) fn put(&self, key: &str, content_type: &str, body: &[u8]) -> Result<ObjectMeta> {
        let path = self.object_path(key)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| Error::io("create", parent, source))?;
        }
        let meta_path = self.meta_path(key)?;
        if let Some(parent) = meta_path.parent() {
            fs::create_dir_all(parent).map_err(|source| Error::io("create", parent, source))?;
        }
        let etag = format!("\"{:x}\"", Md5::digest(body));
        let meta = ObjectMeta {
            content_type: if content_type.is_empty() {
                "application/octet-stream".to_owned()
            } else {
                content_type.to_owned()
            },
            etag,
            content_length: body.len() as u64,
            last_modified: last_modified(SystemTime::now()),
        };
        write_atomic(&path, body)?;
        let bytes = serde_json::to_vec(&meta).map_err(|source| Error::Json {
            path: meta_path.clone(),
            source,
        })?;
        write_atomic(&meta_path, &bytes)?;
        Ok(meta)
    }

    /// Reads one object plus its metadata; `None` when either file is absent.
    pub(crate) fn get(&self, key: &str) -> Result<Option<(ObjectMeta, Vec<u8>)>> {
        let Some(meta) = self.head(key)? else {
            return Ok(None);
        };
        let path = self.object_path(key)?;
        let body = match fs::read(&path) {
            Ok(body) => body,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(Error::io("read", &path, source)),
        };
        Ok(Some((meta, body)))
    }

    /// Reads one object's metadata without its payload.
    pub(crate) fn head(&self, key: &str) -> Result<Option<ObjectMeta>> {
        ObjectMeta::read(&self.meta_path(key)?)
    }

    /// Deletes an object and its sidecar; returns whether an object existed.
    pub(crate) fn delete(&self, key: &str) -> Result<bool> {
        let path = self.object_path(key)?;
        let existed = match fs::remove_file(&path) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(source) => return Err(Error::io("remove", &path, source)),
        };
        let _ = fs::remove_file(self.meta_path(key)?);
        Ok(existed)
    }

    /// Lists keys under `objects/` with `ListObjectsV2` filtering.
    pub(crate) fn list(&self, query: &ListQuery) -> Result<Listing> {
        let objects = self.objects();
        match fs::read_dir(&objects) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Listing::empty(&self.bucket, query));
            }
            Err(source) => return Err(Error::io("read", &objects, source)),
        }
        listing::list(&objects, &self.meta(), &self.bucket, query)
            .map_err(|source| Error::io("list", &objects, source))
    }
}

/// Rejects absolute, empty, `.`/`..`, and trailing-slash keys.
pub(crate) fn validate_key(key: &str) -> Result<PathBuf> {
    let invalid = key.is_empty()
        || key.starts_with('/')
        || key.ends_with('/')
        || key
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."));
    if invalid {
        return Err(Error::Protocol {
            message: format!("invalid object key `{key}`"),
        });
    }
    Ok(PathBuf::from(key))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let token = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let tmp = path.with_file_name(format!(
        ".{}.tmp-{}-{token}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("object"),
        std::process::id()
    ));
    fs::write(&tmp, bytes).map_err(|source| Error::io("write", &tmp, source))?;
    fs::rename(&tmp, path).map_err(|source| {
        let _ = fs::remove_file(&tmp);
        Error::io("rename", path, source)
    })
}

/// RFC 3339 UTC (`2026-11-21T20:17:00Z`), derived without a clock dependency.
fn last_modified(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let rem = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if month <= 2 { y + 1 } else { y }, month, day)
}
