//! The backing-service `ServiceSpec` table `snow.yml` `resources:` requests (`ADR-0009` §2–§3).

use super::env::container_env;
use super::runtime_error;
use slipfork_edge::EdgeError;
use slipfork_snow::SnowFile;
use slipfork_world::StateSubdirMount;
use slipfork_world::rootfs::{BackingServiceKey, WorldRootfs};
use slipfork_world::{ServiceKind, ServiceRuntime, ServiceSpec};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// The backing-service OCI images `snow.yml` `resources:` requests (`ADR-0009` §2.2).
pub(super) fn requested_service_images(snow: &SnowFile) -> Vec<BackingServiceKey> {
    let mut images = BTreeSet::new();
    for cluster in &snow.resources.pg_clusters {
        images.insert(BackingServiceKey::Postgres {
            version: cluster.version.clone(),
        });
    }
    for cluster in &snow.resources.redis_clusters {
        images.insert(BackingServiceKey::Redis {
            version: cluster.version.clone(),
        });
    }
    if !snow.resources.s3_buckets.is_empty() {
        images.insert(BackingServiceKey::SeaweedFs);
    }
    images.into_iter().collect()
}

/// One `ServiceSpec` per declared backing service, all on the single `WorldEngine` supervision
/// surface (`ADR-0009` §2–§3).
///
/// `dump_mounts` maps a `pg_clusters.<id>` service id to the verified `<sha256>.pgcustom` files the
/// base seed world must expose read-only at `/dumps/...`.
pub(super) fn service_specs(
    snow: &SnowFile,
    rootfs: &WorldRootfs,
) -> Result<Vec<ServiceSpec>, EdgeError> {
    let mut specs = Vec::new();
    for cluster in &snow.resources.pg_clusters {
        let image = BackingServiceKey::Postgres {
            version: cluster.version.clone(),
        };
        let id = format!("pg_clusters.{}", cluster.id);
        require_image(rootfs, &image, &id)?;
        ensure_container_dirs(rootfs, &image, &["/var/lib/postgresql/data"])?;
        specs.push(ServiceSpec {
            kind: ServiceKind::BackingService,
            id: id.clone(),
            runtime: ServiceRuntime::OciContainer {
                image,
                argv: vec!["docker-entrypoint.sh".to_owned(), "postgres".to_owned()],
                state_mounts: vec![StateSubdirMount {
                    host_subdir: PathBuf::from("pgdata").join(&cluster.id),
                    container_path: PathBuf::from("/var/lib/postgresql/data"),
                }],
                readonly_mounts: Vec::new(),
            },
            env: container_env(
                "/tmp",
                &[
                    ("LC_ALL", "C.UTF-8"),
                    ("PGDATA", "/var/lib/postgresql/data"),
                    ("POSTGRES_HOST_AUTH_METHOD", "trust"),
                ],
                &format!(
                    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/lib/postgresql/{}/bin",
                    cluster.version
                ),
            )?,
            readiness_port: Some(5432),
        });
    }
    for cluster in &snow.resources.redis_clusters {
        let image = BackingServiceKey::Redis {
            version: cluster.version.clone(),
        };
        require_image(rootfs, &image, &format!("redis_clusters.{}", cluster.id))?;
        ensure_container_dirs(rootfs, &image, &["/data"])?;
        specs.push(ServiceSpec {
            kind: ServiceKind::BackingService,
            id: format!("redis_clusters.{}", cluster.id),
            runtime: ServiceRuntime::OciContainer {
                image,
                argv: vec![
                    "docker-entrypoint.sh".to_owned(),
                    "redis-server".to_owned(),
                    "--dir".to_owned(),
                    "/data".to_owned(),
                ],
                state_mounts: vec![StateSubdirMount {
                    host_subdir: PathBuf::from("redis").join(&cluster.id),
                    container_path: PathBuf::from("/data"),
                }],
                readonly_mounts: Vec::new(),
            },
            env: container_env(
                "/data",
                &[("LC_ALL", "C")],
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )?,
            readiness_port: Some(6379),
        });
    }
    if !snow.resources.s3_buckets.is_empty() {
        let image = BackingServiceKey::SeaweedFs;
        require_image(rootfs, &image, "s3_buckets")?;
        ensure_container_dirs(rootfs, &image, &["/data"])?;
        specs.push(ServiceSpec {
            kind: ServiceKind::BackingService,
            id: "s3_buckets".to_owned(),
            runtime: ServiceRuntime::OciContainer {
                image,
                argv: vec![
                    "weed".to_owned(),
                    "server".to_owned(),
                    "-dir=/data".to_owned(),
                    // The world netns carries no DNS resolver, so `weed` must never resolve
                    // `localhost` for its master/filer/S3 bind addresses.
                    "-ip=127.0.0.1".to_owned(),
                    "-ip.bind=127.0.0.1".to_owned(),
                    "-s3".to_owned(),
                    "-s3.port=9000".to_owned(),
                    "-s3.ip.bind=127.0.0.1".to_owned(),
                ],
                state_mounts: vec![StateSubdirMount {
                    host_subdir: PathBuf::from("s3"),
                    container_path: PathBuf::from("/data"),
                }],
                readonly_mounts: Vec::new(),
            },
            env: container_env(
                "/data",
                &[],
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )?,
            readiness_port: Some(9000),
        });
    }
    Ok(specs)
}

/// Create the bind-mount destinations a backing-service image declares as volumes.
///
/// `docker export` omits `VOLUME` contents, so the extracted read-only rootfs may lack the
/// directory itself. The engine refuses to invent a destination; the Composition Root prepares it
/// once in the content-addressed cache before the container starts.
pub(super) fn ensure_container_dirs(
    rootfs: &WorldRootfs,
    image: &BackingServiceKey,
    paths: &[&str],
) -> Result<(), EdgeError> {
    let image = rootfs.service_images.get(image).ok_or_else(|| {
        runtime_error(format!(
            "backing-service image '{image}' is not materialized; re-run `slipfork up`"
        ))
    })?;
    for path in paths {
        let host = image.rootfs.join(path.trim_start_matches('/'));
        std::fs::create_dir_all(&host)
            .map_err(|err| runtime_error(format!("cannot create {}: {err}", host.display())))?;
    }
    Ok(())
}

pub(super) fn require_image(
    rootfs: &WorldRootfs,
    image: &BackingServiceKey,
    resource: &str,
) -> Result<(), EdgeError> {
    if rootfs.service_images.contains_key(image) {
        return Ok(());
    }
    Err(runtime_error(format!(
        "resource '{resource}' needs backing-service image '{image}', but the materialized rootfs \
         carries no OCI image for it; re-run `slipfork up` so the L0 builder extracts it"
    )))
}
