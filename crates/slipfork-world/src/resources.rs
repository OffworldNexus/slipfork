//! Real backing-resource pods (`postgres:18`, `redis:7`) plus the native S3 registry.

mod cache;
mod pg;
mod s3;

use std::path::Path;

pub use cache::RedisLauncher;
pub use pg::{
    DatabaseBootstrap, DatabaseFault, DatabaseName, PgSeed, PostgresClone, PostgresLauncher,
    PostgresTemplate, SeedSpec, TemplateState,
};
pub use s3::{S3Bucket, S3Credentials, S3Server};

/// A backing service backed by `worlds/<world>/<data_dir>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    /// `postgres:18` on `:5432`.
    Postgres,
    /// `redis:7` on `:6379`.
    Redis,
    /// Native in-process S3 server on `:8333` (no container, no image).
    S3,
}

impl ResourceKind {
    /// Directory name under the world data subvolume.
    #[must_use]
    pub const fn data_dir(self) -> &'static str {
        match self {
            Self::Postgres => "pg",
            Self::Redis => "redis",
            Self::S3 => "s3",
        }
    }

    /// The kind owning `directory` (`pg`, `redis`, `s3`), so clone-name parsing has one source.
    #[must_use]
    pub fn from_data_dir(directory: &str) -> Option<Self> {
        [Self::Postgres, Self::Redis, Self::S3]
            .into_iter()
            .find(|kind| kind.data_dir() == directory)
    }

    /// Loopback port inside the world namespace.
    #[must_use]
    pub const fn port(self) -> u16 {
        match self {
            Self::Postgres => 5432,
            Self::Redis => 6379,
            Self::S3 => 8333,
        }
    }

    /// The upstream OCI image a container-backed resource needs; `None` for the native S3 server.
    #[must_use]
    pub const fn image(self) -> Option<&'static str> {
        match self {
            Self::Postgres => Some("postgres:18"),
            Self::Redis => Some("redis:7"),
            Self::S3 => None,
        }
    }
}

/// Facts a pulled image config contributes to a resource pod (env + entrypoint/cmd).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageFacts {
    /// Image `config.Env` entries, inherited verbatim.
    pub env: Vec<String>,
    /// Image `config.Entrypoint`.
    pub entrypoint: Vec<String>,
    /// Image `config.Cmd`.
    pub cmd: Vec<String>,
}

/// The first rootfs-relative binary among `candidates`, as an in-container absolute path.
///
/// The launchers exec one binary each, so they never depend on the image's `PATH` and a missing
/// `initdb`/`redis-server` fails closed before a pod bundle is drawn.
pub(crate) fn binary_in(rootfs: &Path, candidates: &[&'static str]) -> Option<String> {
    candidates
        .iter()
        .find(|relative| rootfs.join(relative).is_file())
        .map(|relative| format!("/{relative}"))
}
