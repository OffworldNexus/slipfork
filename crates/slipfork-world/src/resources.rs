//! Real backing-resource pods (`postgres:18`, `redis:7`, SeaweedFS) and the S3 policy plan.
//!
//! `s3 uploads` stays private: SeaweedFS is configured with a real admin identity, so anonymous
//! requests are rejected (`403`). `s3 assets { cdn static_cdn }` becomes `public-read` by scoping
//! an anonymous `Read:<bucket>` grant inside `s3.json` while one shared `weed server` per world
//! hosts every declared bucket on `127.0.0.1` with explicit non-colliding internal ports.

mod s3;

use std::path::Path;

pub use s3::{S3Bucket, S3Credentials, S3Policy, SeaweedfsPlan, SeaweedfsPorts};

use crate::pod::{Bind, BindSource, PodEnv, PodFile, PodSpec};

/// A real backing service backed by `worlds/<world>/<data_dir>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    /// `postgres:18` on `:5432`.
    Postgres,
    /// `redis:7` on `:6379`.
    Redis,
    /// Shared SeaweedFS S3 server on `:8333`.
    Seaweedfs,
}

impl ResourceKind {
    /// Directory name under the world data subvolume.
    #[must_use]
    pub const fn data_dir(self) -> &'static str {
        match self {
            Self::Postgres => "pg",
            Self::Redis => "redis",
            Self::Seaweedfs => "s3",
        }
    }

    /// Loopback port inside the world namespace.
    #[must_use]
    pub const fn port(self) -> u16 {
        match self {
            Self::Postgres => 5432,
            Self::Redis => 6379,
            Self::Seaweedfs => 8333,
        }
    }

    /// Upstream image reference the caller must have pulled.
    #[must_use]
    pub const fn image(self) -> &'static str {
        match self {
            Self::Postgres => "postgres:18",
            Self::Redis => "redis:7",
            // Upstream's real image is `chrislusf/seaweedfs`; there is no `library/seaweedfs`.
            Self::Seaweedfs => "chrislusf/seaweedfs:3.80",
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

/// Shared per-world SeaweedFS pod: explicit `127.0.0.1` ports, `copy_1 = 1`, and bucket bootstrap.
#[must_use]
pub fn seaweedfs(
    id: &str,
    rootfs: &Path,
    data: &Path,
    plan: &SeaweedfsPlan,
    facts: &ImageFacts,
) -> PodSpec {
    let ports = plan.ports();
    // Wait until S3 `:8333` (`0x208D`) is in `TCP_LISTEN` (`0A`) before dialing `weed shell`, so
    // master + filer are already connected and Go gRPC never enters exponential dial backoff.
    let script = format!(
        concat!(
            "parent=$$\n",
            "(\n",
            "  tries=0\n",
            "  until grep -qi ':208D 00000000:0000 0A' /proc/net/tcp 2>/dev/null; do\n",
            "    tries=$((tries + 1))\n",
            "    if [ \"$tries\" -ge 120 ]; then\n",
            "      echo 'slipfork: seaweedfs s3 port did not listen within 30s; next: read the pod log' >&2\n",
            "      kill -TERM \"$parent\"\n",
            "      exit 1\n",
            "    fi\n",
            "    sleep 0.25 2>/dev/null || sleep 1\n",
            "  done\n",
            "  tries=0\n",
            "  until printf 's3.bucket.list\\n' | /usr/bin/weed shell ",
            "-master=127.0.0.1:{m_http} -filer=127.0.0.1:{f_http} >/dev/null 2>&1; do\n",
            "    tries=$((tries + 1))\n",
            "    if [ \"$tries\" -ge 40 ]; then\n",
            "      echo 'slipfork: seaweedfs master/filer did not answer within 10s; next: read the pod log' >&2\n",
            "      kill -TERM \"$parent\"\n",
            "      exit 1\n",
            "    fi\n",
            "    sleep 0.25 2>/dev/null || sleep 1\n",
            "  done\n",
            "  /usr/bin/weed shell -master=127.0.0.1:{m_http} -filer=127.0.0.1:{f_http} ",
            "< /etc/seaweedfs/bootstrap.txt >/dev/null\n",
            ") &\n",
            "exec /usr/bin/weed server -dir=/data -ip=127.0.0.1 -ip.bind=127.0.0.1 ",
            "-master.port={m_http} -master.port.grpc={m_grpc} ",
            "-master.raftHashicorp -master.raftBootstrap -master.volumeSizeLimitMB=1024 ",
            "-volume.port={v_http} -volume.port.grpc={v_grpc} ",
            "-volume.max=256 -volume.minFreeSpace=128MiB -volume.preStopSeconds=0 ",
            "-filer -filer.port={f_http} -filer.port.grpc={f_grpc} ",
            "-s3 -s3.port=8333 -s3.port.grpc={s3_grpc} -s3.config=/etc/seaweedfs/s3.json\n"
        ),
        m_http = ports.master_http,
        m_grpc = ports.master_grpc,
        v_http = ports.volume_http,
        v_grpc = ports.volume_grpc,
        f_http = ports.filer_http,
        f_grpc = ports.filer_grpc,
        s3_grpc = ports.s3_grpc,
    );
    PodSpec {
        id: id.to_owned(),
        rootfs: rootfs.to_path_buf(),
        workdir: "/".to_owned(),
        argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), script],
        inherited_env: facts.env.clone(),
        env: Vec::new(),
        binds: vec![
            Bind {
                source: BindSource::Host(data.to_path_buf()),
                destination: "/data".to_owned(),
                read_only: false,
            },
            Bind {
                source: BindSource::Bundle("master.toml".to_owned()),
                destination: "/etc/seaweedfs/master.toml".to_owned(),
                read_only: true,
            },
            Bind {
                source: BindSource::Bundle("s3.json".to_owned()),
                destination: "/etc/seaweedfs/s3.json".to_owned(),
                read_only: true,
            },
            Bind {
                source: BindSource::Bundle("bootstrap.txt".to_owned()),
                destination: "/etc/seaweedfs/bootstrap.txt".to_owned(),
                read_only: true,
            },
        ],
        files: vec![
            PodFile {
                name: "master.toml".to_owned(),
                contents: plan.master_config.clone(),
            },
            PodFile {
                name: "s3.json".to_owned(),
                contents: plan.s3_config.clone(),
            },
            PodFile {
                name: "bootstrap.txt".to_owned(),
                contents: plan.bootstrap.clone(),
            },
        ],
    }
}
