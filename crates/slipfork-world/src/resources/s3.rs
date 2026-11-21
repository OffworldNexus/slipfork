//! SeaweedFS multi-bucket policy, volume-growth config, and non-colliding loopback port plan.

use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::error::{Error, Result};

const MASTER_TOML: &str = "\
[master.volume_growth]
copy_1 = 1
copy_2 = 2
copy_3 = 3
copy_other = 1
threshold = 0.9
";

const FIXED_RESERVED_PORTS: &[u16] = &[80, 5432, 6379, 8080, 8333, 18080];

/// Bucket visibility matching DigitalOcean Spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S3Policy {
    /// Anonymous `GET` returns `403` (the cloud default).
    Private,
    /// Anonymous `GET` returns `200`; rendered behind `cdn <id>`.
    PublicRead,
}

/// One S3 bucket under a world's shared `s3` data directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Bucket {
    name: String,
    policy: S3Policy,
}

impl S3Bucket {
    /// Validates `name` as an S3 bucket name (`3..=63`, lowercase, digits, `.`, `-`).
    pub fn new(name: &str, policy: S3Policy) -> Result<Self> {
        let valid_len = (3..=63).contains(&name.len());
        let valid_chars = name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        });
        let edges = name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
            && name
                .chars()
                .last()
                .is_some_and(|c| c.is_ascii_alphanumeric());
        if !valid_len || !valid_chars || !edges {
            return Err(Error::InvalidBucket {
                name: name.to_owned(),
            });
        }
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

    /// The bucket visibility.
    #[must_use]
    pub const fn policy(&self) -> S3Policy {
        self.policy
    }
}

/// SeaweedFS admin credentials; `Debug` is fully redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct S3Credentials {
    pub(super) access_key: String,
    pub(super) secret_key: String,
}

impl S3Credentials {
    /// Builds the identity written to the tmpfs `s3.json`.
    #[must_use]
    pub fn new(access_key: &str, secret_key: &str) -> Self {
        Self {
            access_key: access_key.to_owned(),
            secret_key: secret_key.to_owned(),
        }
    }

    /// The local Space identity for `world` (`slipfork` / `slipfork-<world>-s3`).
    #[must_use]
    pub fn local(world: &str) -> Self {
        Self::new("slipfork", &format!("slipfork-{world}-s3"))
    }
}

impl fmt::Debug for S3Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Credentials")
            .field("access_key", &"[REDACTED]")
            .field("secret_key", &"[REDACTED]")
            .finish()
    }
}

/// Explicit internal loopback ports for a world's shared `weed server`.
///
/// Every internal HTTP + gRPC port is distinct, avoids `:8080`/`:18080` and backing-resource
/// defaults, and shifts clear of any declared user service ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeaweedfsPorts {
    /// Master HTTP port (`-master.port`).
    pub master_http: u16,
    /// Master gRPC port (`-master.port.grpc`).
    pub master_grpc: u16,
    /// Volume HTTP port (`-volume.port`).
    pub volume_http: u16,
    /// Volume gRPC port (`-volume.port.grpc`).
    pub volume_grpc: u16,
    /// Filer HTTP port (`-filer.port`).
    pub filer_http: u16,
    /// Filer gRPC port (`-filer.port.grpc`).
    pub filer_grpc: u16,
    /// S3 gRPC port (`-s3.port.grpc`; S3 HTTP is `:8333`).
    pub s3_grpc: u16,
}

impl SeaweedfsPorts {
    /// Allocates explicit, distinct internal ports that never collide with `reserved` or `:8080`.
    #[must_use]
    pub fn avoiding(reserved: &[u16]) -> Self {
        let mut taken: Vec<u16> = FIXED_RESERVED_PORTS
            .iter()
            .chain(reserved.iter())
            .copied()
            .collect();
        let (master_http, master_grpc) = pick_pair(29_333, &mut taken);
        let (volume_http, volume_grpc) = pick_pair(29_380, &mut taken);
        let (filer_http, filer_grpc) = pick_pair(29_888, &mut taken);
        let s3_grpc = pick_single(18_333, &mut taken);
        Self {
            master_http,
            master_grpc,
            volume_http,
            volume_grpc,
            filer_http,
            filer_grpc,
            s3_grpc,
        }
    }
}

fn pick_pair(start: u16, taken: &mut Vec<u16>) -> (u16, u16) {
    let mut http = start;
    loop {
        let grpc = http.saturating_add(10_000);
        if grpc > http && !taken.contains(&http) && !taken.contains(&grpc) {
            taken.push(http);
            taken.push(grpc);
            return (http, grpc);
        }
        http = http.saturating_add(1);
    }
}

fn pick_single(start: u16, taken: &mut Vec<u16>) -> u16 {
    let mut port = start;
    while taken.contains(&port) {
        port = port.saturating_add(1);
    }
    taken.push(port);
    port
}

/// Rendered SeaweedFS bootstrap: `s3.json`, `master.toml`, bucket script, and loopback ports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeaweedfsPlan {
    /// JSON for `-s3.config`, bound read-only from the tmpfs bundle.
    pub s3_config: String,
    /// TOML for `/etc/seaweedfs/master.toml` (`copy_1 = 1` single-node volume growth).
    pub master_config: String,
    /// `weed shell` stdin script creating every declared bucket.
    pub bootstrap: String,
    ports: SeaweedfsPorts,
    buckets: Vec<S3Bucket>,
}

impl SeaweedfsPlan {
    /// Renders the static identity file, single-node master config, and bucket bootstrap.
    pub fn render(credentials: &S3Credentials, buckets: &[S3Bucket]) -> Result<Self> {
        let mut anonymous_actions: Vec<String> = Vec::new();
        let mut bootstrap = String::new();
        for bucket in buckets {
            bootstrap.push_str(&format!("s3.bucket.create -name {}\n", bucket.name));
            if bucket.policy == S3Policy::PublicRead {
                anonymous_actions.push(format!("Read:{}", bucket.name));
            }
        }
        let config = json!({
            "identities": [
                {
                    "name": "slipfork",
                    "credentials": [{
                        "accessKey": credentials.access_key,
                        "secretKey": credentials.secret_key
                    }],
                    "actions": ["Admin", "Read", "Write", "List", "Tagging"]
                },
                {
                    "name": "anonymous",
                    "actions": anonymous_actions
                }
            ]
        });
        let s3_config = serde_json::to_string_pretty(&config).map_err(|source| Error::Json {
            path: PathBuf::from("-s3.config"),
            source,
        })?;
        Ok(Self {
            s3_config,
            master_config: MASTER_TOML.to_owned(),
            bootstrap,
            ports: SeaweedfsPorts::avoiding(&[]),
            buckets: buckets.to_vec(),
        })
    }

    /// Shifts internal SeaweedFS ports clear of any user service ports in the world.
    #[must_use]
    pub fn avoiding_ports(mut self, reserved: &[u16]) -> Self {
        self.ports = SeaweedfsPorts::avoiding(reserved);
        self
    }

    /// The resolved internal loopback ports for this server.
    #[must_use]
    pub const fn ports(&self) -> SeaweedfsPorts {
        self.ports
    }

    /// Blocks until every bucket answers its declared anonymous policy (`403`/`200`).
    ///
    /// # Errors
    ///
    /// Returns a one-line message plus a `next:` remedy when a bucket never matches in `timeout`.
    pub fn await_policies(
        &self,
        pods_dir: &Path,
        timeout: Duration,
    ) -> std::result::Result<(), String> {
        for bucket in &self.buckets {
            let expected = match bucket.policy {
                S3Policy::Private => "403",
                S3Policy::PublicRead => "200",
            };
            let request = format!(
                "HEAD /{} HTTP/1.1\r\nHost: 127.0.0.1:8333\r\nConnection: close\r\n\r\n",
                bucket.name
            );
            let deadline = Instant::now() + timeout;
            loop {
                if let Ok(mut stream) = crate::control::dial_tcp(pods_dir, 8333) {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    if stream.write_all(request.as_bytes()).is_ok() {
                        let mut head = [0_u8; 32];
                        if let Ok(read) = stream.read(&mut head)
                            && head[..read].starts_with(format!("HTTP/1.1 {expected}").as_bytes())
                        {
                            break;
                        }
                    }
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "S3 bucket `{}` never answered `{expected}` within {}s\nnext: inspect the SeaweedFS pod log with `slipfork logs {}`",
                        bucket.name,
                        timeout.as_secs(),
                        bucket.name
                    ));
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::SeaweedfsPorts;

    #[test]
    fn internal_ports_are_distinct_and_avoid_8080_and_user_services() {
        let ports = SeaweedfsPorts::avoiding(&[8080, 18_333, 29_333, 39_380, 29_888]);
        let all = [
            8333,
            ports.s3_grpc,
            ports.master_http,
            ports.master_grpc,
            ports.volume_http,
            ports.volume_grpc,
            ports.filer_http,
            ports.filer_grpc,
        ];
        let unique: BTreeSet<u16> = all.into_iter().collect();
        assert_eq!(unique.len(), all.len(), "all SeaweedFS ports are distinct");
        for forbidden in [8080, 18_080, 18_333, 29_333, 39_380, 29_888] {
            assert!(!unique.contains(&forbidden), "collided on :{forbidden}");
        }
        assert_eq!(ports.master_grpc, ports.master_http + 10_000);
        assert_eq!(ports.volume_grpc, ports.volume_http + 10_000);
        assert_eq!(ports.filer_grpc, ports.filer_http + 10_000);
    }
}
