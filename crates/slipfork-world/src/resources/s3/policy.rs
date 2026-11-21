//! S3 bucket identities and anonymous-read policy enforcement.

use slipfork_forkfile::{BucketPolicy, DnsLabel};

use crate::error::{Error, Result};

/// One validated bucket served by the native S3 server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Bucket {
    name: String,
    policy: BucketPolicy,
}

impl S3Bucket {
    /// Validates `name` through the canonical `Forkfile` bucket label rules.
    pub fn new(name: &str, policy: BucketPolicy) -> Result<Self> {
        DnsLabel::bucket(name).map_err(|_| Error::InvalidBucket {
            name: name.to_owned(),
        })?;
        Ok(Self {
            name: name.to_owned(),
            policy,
        })
    }

    /// The validated bucket name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The anonymous-read policy.
    #[must_use]
    pub const fn policy(&self) -> BucketPolicy {
        self.policy
    }

    /// Whether an unsigned request may read this bucket.
    #[must_use]
    pub const fn public_read(&self) -> bool {
        matches!(self.policy, BucketPolicy::PublicRead)
    }

    /// Anonymous authorization: only `GET`/`HEAD` on a `public-read` bucket is allowed.
    #[must_use]
    pub fn permits_anonymous(&self, method: &str) -> bool {
        self.public_read() && matches!(method, "GET" | "HEAD")
    }
}
