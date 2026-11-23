//! `ListObjectsV2`: recursive key walk, prefix/delimiter filtering, pagination, and XML.

use std::fs;
use std::io;
use std::path::Path;

use super::object_ops::ObjectMeta;

/// Parsed `?list-type=2` query parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListQuery {
    pub prefix: String,
    pub delimiter: Option<String>,
    pub max_keys: usize,
    pub continuation_token: Option<String>,
}

impl ListQuery {
    /// S3's default page size.
    pub(crate) const DEFAULT_MAX_KEYS: usize = 1000;
}

/// One `Contents` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListEntry {
    pub key: String,
    pub size: u64,
    pub etag: String,
    pub last_modified: String,
}

/// One `ListBucketResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Listing {
    pub bucket: String,
    pub prefix: String,
    pub max_keys: usize,
    pub entries: Vec<ListEntry>,
    pub common_prefixes: Vec<String>,
    pub is_truncated: bool,
    pub next_token: Option<String>,
}

impl Listing {
    /// An empty page for a bucket with no `objects/` directory yet.
    pub(crate) fn empty(bucket: &str, query: &ListQuery) -> Self {
        Self {
            bucket: bucket.to_owned(),
            prefix: query.prefix.clone(),
            max_keys: query.max_keys,
            entries: Vec::new(),
            common_prefixes: Vec::new(),
            is_truncated: false,
            next_token: None,
        }
    }

    /// Renders the S3 XML body.
    pub(crate) fn xml(&self) -> String {
        let mut out = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
        );
        out.push_str(&format!("<Name>{}</Name>", escape(&self.bucket)));
        out.push_str(&format!("<Prefix>{}</Prefix>", escape(&self.prefix)));
        out.push_str(&format!(
            "<KeyCount>{}</KeyCount><MaxKeys>{}</MaxKeys>",
            self.entries.len() + self.common_prefixes.len(),
            self.max_keys,
        ));
        out.push_str(&format!("<IsTruncated>{}</IsTruncated>", self.is_truncated));
        if let Some(token) = &self.next_token {
            out.push_str(&format!(
                "<NextContinuationToken>{}</NextContinuationToken>",
                escape(token)
            ));
        }
        for entry in &self.entries {
            out.push_str(&format!(
                "<Contents><Key>{}</Key><LastModified>{}</LastModified><ETag>{}</ETag>\
                 <Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
                escape(&entry.key),
                escape(&entry.last_modified),
                escape(&entry.etag),
                entry.size,
            ));
        }
        for prefix in &self.common_prefixes {
            out.push_str(&format!(
                "<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>",
                escape(prefix)
            ));
        }
        out.push_str("</ListBucketResult>");
        out
    }
}

enum Item {
    Object(ListEntry),
    Prefix(String),
}

impl Item {
    fn sort_key(&self) -> &str {
        match self {
            Self::Object(entry) => &entry.key,
            Self::Prefix(prefix) => prefix,
        }
    }
}

/// Walks `objects/` and applies `ListObjectsV2` semantics.
pub(crate) fn list(
    objects: &Path,
    meta: &Path,
    bucket: &str,
    query: &ListQuery,
) -> io::Result<Listing> {
    let mut keys = Vec::new();
    walk(objects, objects, &mut keys)?;
    keys.sort();

    let mut items = Vec::new();
    for key in keys {
        if !key.starts_with(&query.prefix) {
            continue;
        }
        if let Some(delimiter) = &query.delimiter {
            let rest = &key[query.prefix.len()..];
            if let Some(index) = rest.find(delimiter.as_str()) {
                let prefix = format!("{}{}", query.prefix, &rest[..index + delimiter.len()]);
                if !items
                    .iter()
                    .any(|item: &Item| matches!(item, Item::Prefix(p) if *p == prefix))
                {
                    items.push(Item::Prefix(prefix));
                }
                continue;
            }
        }
        let path = objects.join(&key);
        let size = fs::metadata(&path).map_or(0, |metadata| metadata.len());
        let (etag, last_modified) = metadata(meta, &key)?;
        items.push(Item::Object(ListEntry {
            key,
            size,
            etag,
            last_modified,
        }));
    }
    items.sort_by(|left, right| left.sort_key().cmp(right.sort_key()));

    if let Some(token) = &query.continuation_token {
        items.retain(|item| item.sort_key() > token.as_str());
    }
    let max = query.max_keys.clamp(1, 1000);
    let is_truncated = items.len() > max;
    let next_token = is_truncated.then(|| {
        items
            .get(max - 1)
            .map(|item| item.sort_key().to_owned())
            .unwrap_or_default()
    });
    items.truncate(max);

    let mut listing = Listing {
        bucket: bucket.to_owned(),
        prefix: query.prefix.clone(),
        max_keys: query.max_keys,
        entries: Vec::new(),
        common_prefixes: Vec::new(),
        is_truncated,
        next_token,
    };
    for item in items {
        match item {
            Item::Object(entry) => listing.entries.push(entry),
            Item::Prefix(prefix) => listing.common_prefixes.push(prefix),
        }
    }
    Ok(listing)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            walk(root, &path, out)?;
        } else if file_type.is_file()
            && let Ok(relative) = path.strip_prefix(root)
            && let Some(key) = relative.to_str()
        {
            out.push(key.to_owned());
        }
    }
    Ok(())
}

/// Reads the single owner of object metadata; a dangling object is a real IO error.
fn metadata(meta: &Path, key: &str) -> io::Result<(String, String)> {
    let path = meta.join(format!("{key}.json"));
    let parsed = ObjectMeta::read(&path).map_err(io::Error::other)?;
    let parsed = parsed.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("missing metadata sidecar `{}`", path.display()),
        )
    })?;
    Ok((parsed.etag, parsed.last_modified))
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}
