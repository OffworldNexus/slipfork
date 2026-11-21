//! Resource step driver: golden postgres template, per-world `PGDATA`/AOF clones, readiness.
//!
//! Each resource is *staged* here (clone + launch) and its long readiness wait runs later through
//! [`super::Staged::await_ready`], with no runtime lock held, so PG, Redis, and app pods boot
//! concurrently instead of serializing behind one mutex.

use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use slipfork_forkfile::SecretValue;
use slipfork_image::ImageReference;
use slipfork_world::{
    BootState, ImageFacts, PostgresLauncher, PostgresTemplate, RedisLauncher, ResourceKind,
    ResourceNode, S3BucketSpec, S3Credentials, S3WorldConfig, control,
};

use super::WorldRuntime;
use super::ready::{Ready, Staged};

/// Starts one backing resource: native S3, or a launched postgres/redis pod.
pub(super) fn start_resource(runtime: &mut WorldRuntime, id: &str) -> Result<Staged, String> {
    let node = runtime
        .resources
        .iter()
        .find(|candidate| candidate.id == id)
        .cloned()
        .ok_or_else(|| format!("unknown resource `{id}`"))?;
    let world_dir = runtime.layout.world(&runtime.world);
    if node.resource == ResourceKind::S3 {
        // Native S3: no image, no pod. The world init binds the listener inside the world
        // netns and the daemon never sees the signing secret again.
        let credentials = S3Credentials::local(runtime.world.as_str());
        let buckets = node
            .buckets
            .iter()
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
        control::configure_s3(runtime.supervisor.pods_dir(), config)
            .map_err(|error| error.to_string())?;
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
    let image = if let Some(image) = runtime.pulled.get(reference) {
        image.clone()
    } else {
        let parsed = ImageReference::parse(reference).map_err(|error| error.to_string())?;
        runtime.note(format!("pull {reference}"));
        let image = runtime
            .puller
            .pull_into(&parsed, &mut *runtime.log.get_mut())
            .map_err(|error| error.to_string())?;
        runtime.pulled.insert(reference.to_owned(), image.clone());
        image
    };
    let rootfs = runtime.clone_rootfs(&image.rootfs, &format!("res-{id}"))?;
    let ready = match node.resource {
        ResourceKind::Postgres => stage_postgres(runtime, &node, &rootfs, &image.facts)?,
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
    runtime: &mut WorldRuntime,
    node: &ResourceNode,
    rootfs: &Path,
    facts: &ImageFacts,
) -> Result<Ready, String> {
    let template = PostgresTemplate::new(runtime.layout.clone());
    let launcher = PostgresLauncher::new(runtime.layout.clone(), runtime.world.clone());
    // `dynamic_shared_memory_type=posix` needs a tmpfs; the world pod root already is one.
    let shm = runtime
        .supervisor
        .pods_dir()
        .join(format!("shm-{}", node.id));
    fs::create_dir_all(&shm).map_err(|error| format!("create `{}`: {error}", shm.display()))?;
    let mut last_report: Option<Instant> = None;
    {
        let engine = &runtime.engine;
        let supervisor = &mut runtime.supervisor;
        let log = &runtime.log;
        template
            .ensure(
                engine,
                rootfs,
                &mut |spec| {
                    let mut tail: VecDeque<String> = VecDeque::new();
                    let code = supervisor.run_once(spec, &mut |_, line| {
                        if tail.len() == 10 {
                            tail.pop_front();
                        }
                        tail.push_back(line.to_owned());
                        let mut sink = log.borrow_mut();
                        let _ = writeln!(sink, "{line}");
                        let _ = sink.flush();
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
                    let mut sink = log.borrow_mut();
                    let _ = writeln!(
                        sink,
                        "waiting {:.1}s for another `slipfork up` to finish the golden postgres template",
                        waited.as_secs_f64()
                    );
                    let _ = sink.flush();
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
    runtime
        .supervisor
        .launch(&spec)
        .map_err(|error| error.to_string())?;
    Ok(Ready::Unix {
        socket: launcher.socket_path(&node.id),
        label: format!("postgres {}", node.id),
        service: node.id.clone(),
    })
}

fn stage_redis(
    runtime: &mut WorldRuntime,
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
    runtime
        .supervisor
        .launch(&spec)
        .map_err(|error| error.to_string())?;
    Ok(Ready::Service {
        pods_dir: runtime.supervisor.pods_dir().to_path_buf(),
        port: 6379,
        label: format!("redis {}", node.id),
        service: node.id.clone(),
    })
}
