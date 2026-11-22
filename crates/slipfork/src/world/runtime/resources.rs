//! Resource step driver: golden postgres template, per-world `PGDATA`/AOF clones, readiness.
//!
//! Each resource is *staged* here with narrow collaborator scopes (lock-free image cache, lock-free
//! Btrfs rootfs clone, lock-free `control.sock` for S3 and `initdb`, brief supervisor lock strictly
//! for `crun run --detach`), and its readiness wait runs outside all locks.

use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use slipfork_forkfile::SecretValue;
use slipfork_world::{
    BootState, ImageFacts, PostgresLauncher, PostgresTemplate, RedisLauncher, ResourceKind,
    ResourceNode, S3BucketSpec, S3Credentials, S3WorldConfig, control,
};

use super::WorldRuntime;
use super::ready::{Ready, Staged};
use crate::world::supervise::BootSink;

/// Starts one backing resource: native S3, or a launched postgres/redis pod.
pub(super) fn start_resource(
    runtime: &WorldRuntime,
    id: &str,
    log: &mut BootSink,
) -> Result<Staged, String> {
    let node = runtime
        .resources
        .iter()
        .find(|candidate| candidate.id == id)
        .cloned()
        .ok_or_else(|| format!("unknown resource `{id}`"))?;
    let world_dir = runtime.layout.world(&runtime.world);
    if node.resource == ResourceKind::S3 {
        // Native S3: no image, no pod. The world init binds the listener inside the world
        // netns and the daemon never sees the signing secret again. A second `ConfigureS3`
        // replaces the whole server, so only the first bucket node configures every bucket.
        if runtime
            .resources
            .iter()
            .find(|candidate| candidate.resource == ResourceKind::S3)
            .is_some_and(|candidate| candidate.id != node.id)
        {
            return Ok(Staged {
                state: BootState::Ready,
                detail: "shared native S3".to_owned(),
                ready: Ready::None,
                progress: None,
            });
        }
        let credentials = S3Credentials::local(runtime.world.as_str());
        let buckets = runtime
            .resources
            .iter()
            .filter(|candidate| candidate.resource == ResourceKind::S3)
            .flat_map(|candidate| candidate.buckets.iter())
            .map(|bucket| S3BucketSpec {
                name: bucket.name().to_owned(),
                policy: bucket.policy(),
                access_key: credentials.access_key().to_owned(),
                secret_key: SecretValue::new(credentials.secret_key()),
            })
            .collect();
        let config = S3WorldConfig {
            listen_addr: SocketAddr::from(([127, 0, 0, 1], node.resource.port())),
            s3_root: world_dir.join(node.resource.data_dir()),
            buckets,
        };
        control::configure_s3(runtime.pods_dir(), config).map_err(|error| error.to_string())?;
        return Ok(Staged {
            state: BootState::Ready,
            detail: "started".to_owned(),
            ready: Ready::None,
            progress: None,
        });
    }
    let reference = node
        .resource
        .image()
        .ok_or_else(|| format!("resource `{id}` has no OCI image"))?;
    let image = runtime.images.pull(reference, log)?;
    let rootfs = runtime.clone_rootfs(&image.rootfs, &format!("res-{id}"), true)?;
    let ready = match node.resource {
        ResourceKind::Postgres => stage_postgres(runtime, &node, &rootfs, &image.facts, log)?,
        ResourceKind::Redis => stage_redis(runtime, &node, &rootfs, &image.facts)?,
        ResourceKind::S3 => unreachable!("the native S3 server has no pod"),
    };
    Ok(Staged {
        state: BootState::Ready,
        detail: "started".to_owned(),
        ready,
        progress: None,
    })
}

fn stage_postgres(
    runtime: &WorldRuntime,
    node: &ResourceNode,
    rootfs: &Path,
    facts: &ImageFacts,
    log: &mut BootSink,
) -> Result<Ready, String> {
    let template = PostgresTemplate::new(runtime.layout.clone());
    let launcher = PostgresLauncher::new(runtime.layout.clone(), runtime.world.clone());
    // `dynamic_shared_memory_type=posix` needs a tmpfs; the world pod root already is one.
    let shm = runtime.pods_dir().join(format!("shm-{}", node.id));
    fs::create_dir_all(&shm).map_err(|error| format!("create `{}`: {error}", shm.display()))?;
    // A forked world already owns its snapshotted `<fork>--<world>.pg.<id>` clone; only a fresh
    // world without an existing PGDATA clone needs to ensure and clone the golden template.
    if !launcher.clone_path(&node.id).exists() {
        let mut last_report: Option<Instant> = None;
        let mut init_log = log.clone();
        let mut wait_log = log.clone();
        template
            .ensure(
                &runtime.engine,
                rootfs,
                &mut |spec| {
                    let mut tail: VecDeque<String> = VecDeque::new();
                    let code = runtime.run_pod_once(spec, &mut |_, line| {
                        if tail.len() == 10 {
                            tail.pop_front();
                        }
                        tail.push_back(line.to_owned());
                        let _ = writeln!(init_log, "{line}");
                        let _ = init_log.flush();
                    })?;
                    if code == 0 {
                        Ok(())
                    } else {
                        Err(slipfork_world::Error::PodLaunch {
                            message: format!(
                                "`initdb` exited with {code}: {}",
                                tail.into_iter().collect::<Vec<_>>().join(" | ")
                            ),
                        })
                    }
                },
                &mut |waited| {
                    if last_report.is_some_and(|at| at.elapsed() < Duration::from_millis(250)) {
                        return;
                    }
                    last_report = Some(Instant::now());
                    let _ = writeln!(
                        wait_log,
                        "waiting {:.1}s for another `slipfork up` to finish the golden postgres template",
                        waited.as_secs_f64()
                    );
                    let _ = wait_log.flush();
                },
            )
            .map_err(|error| error.to_string())?;
    }
    launcher
        .clone(&runtime.engine, &template.path(), &node.id)
        .map_err(|error| error.to_string())?;
    let spec = launcher
        .spec(&node.id, rootfs, &shm, facts)
        .map_err(|error| error.to_string())?;
    runtime.launch_pod(&spec)?;
    Ok(Ready::Unix {
        socket: launcher.socket_path(&node.id),
        label: format!("postgres {}", node.id),
        service: node.id.clone(),
    })
}

fn stage_redis(
    runtime: &WorldRuntime,
    node: &ResourceNode,
    rootfs: &Path,
    facts: &ImageFacts,
) -> Result<Ready, String> {
    let launcher = RedisLauncher::new(runtime.layout.clone(), runtime.world.clone());
    launcher
        .prepare(&node.id)
        .map_err(|error| error.to_string())?;
    let spec = launcher
        .spec(&node.id, rootfs, facts)
        .map_err(|error| error.to_string())?;
    runtime.launch_pod(&spec)?;
    Ok(Ready::Service {
        pods_dir: runtime.pods_dir().to_path_buf(),
        port: 6379,
        label: format!("redis {}", node.id),
        service: node.id.clone(),
    })
}
