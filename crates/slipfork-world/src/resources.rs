//! Real backing-resource pods (`postgres:18`, `redis:7`) plus the native S3 registry.

mod s3;

use std::path::Path;

pub use s3::{S3Bucket, S3Credentials, S3Server};

use crate::pod::{Bind, BindSource, PodEnv, PodSpec};

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

/// Builds the resource argv: the image entrypoint/cmd unless the spec overrides it.
fn compose(facts: &ImageFacts, args: &[&str], fallback: &[&str]) -> Vec<String> {
    if facts.entrypoint.is_empty() {
        let chosen = if args.is_empty() { fallback } else { args };
        return chosen.iter().map(|arg| (*arg).to_owned()).collect();
    }
    let mut argv = facts.entrypoint.clone();
    if args.is_empty() {
        argv.extend(facts.cmd.iter().cloned());
        if argv.len() == facts.entrypoint.len() {
            argv.extend(fallback.iter().map(|arg| (*arg).to_owned()));
        }
    } else {
        argv.extend(args.iter().map(|arg| (*arg).to_owned()));
    }
    argv
}

/// `postgres:18` pod: trust auth locally, data on `/var/lib/postgresql/data`.
#[must_use]
pub fn postgres(id: &str, rootfs: &Path, data: &Path, facts: &ImageFacts) -> PodSpec {
    PodSpec {
        id: id.to_owned(),
        rootfs: rootfs.to_path_buf(),
        workdir: "/".to_owned(),
        argv: compose(facts, &[], &["docker-entrypoint.sh", "postgres"]),
        inherited_env: facts.env.clone(),
        env: vec![
            PodEnv::new("PGDATA", "/var/lib/postgresql/data"),
            PodEnv::new("POSTGRES_HOST_AUTH_METHOD", "trust"),
        ],
        binds: vec![Bind {
            source: BindSource::Host(data.to_path_buf()),
            destination: "/var/lib/postgresql/data".to_owned(),
            read_only: false,
        }],
        files: Vec::new(),
    }
}

/// `redis:7` pod: append-only persistence on `/data`, loopback only.
#[must_use]
pub fn redis(id: &str, rootfs: &Path, data: &Path, facts: &ImageFacts) -> PodSpec {
    let flags = [
        "redis-server",
        "--bind",
        "127.0.0.1",
        "--dir",
        "/data",
        "--appendonly",
        "yes",
    ];
    PodSpec {
        id: id.to_owned(),
        rootfs: rootfs.to_path_buf(),
        workdir: "/".to_owned(),
        argv: compose(facts, &flags, &flags),
        inherited_env: facts.env.clone(),
        env: Vec::new(),
        binds: vec![Bind {
            source: BindSource::Host(data.to_path_buf()),
            destination: "/data".to_owned(),
            read_only: false,
        }],
        files: Vec::new(),
    }
}
