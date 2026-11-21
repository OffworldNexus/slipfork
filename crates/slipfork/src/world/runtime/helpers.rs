//! Pure path/env/resource helpers plus bounded pod sweeping before a world boots.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use slipfork_world::{
    Bind, BindSource, BtrfsEngine, Dirtiness, ImageFacts, PodEnv, PodSpec, ResourceKind,
    ResourceNode, WorldEngine, postgres, redis,
};

/// Clears leftover pod rootfs clones or `ino == 2` snapshot stubs before a world boots.
pub(super) fn sweep_stale_pods(engine: &BtrfsEngine, pods_dir: &Path) -> Result<(), String> {
    if !pods_dir.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(pods_dir)
        .map_err(|error| format!("read `{}`: {error}", pods_dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("read `{}`: {error}", pods_dir.display()))?;
        let path = entry.path();
        if slipfork_world::is_subvolume_path(&path).unwrap_or(false) {
            engine
                .destroy(&path, Dirtiness::Clean, true)
                .map_err(|error| error.to_string())?;
        } else if path.is_dir() {
            fs::remove_dir_all(&path)
                .map_err(|error| format!("remove `{}`: {error}", path.display()))?;
        } else {
            fs::remove_file(&path)
                .map_err(|error| format!("remove `{}`: {error}", path.display()))?;
        }
    }
    let _ = fs::remove_dir(pods_dir);
    Ok(())
}

pub(super) fn project_bind(project: &Path) -> Bind {
    Bind {
        source: BindSource::Host(project.to_path_buf()),
        destination: "/project".to_owned(),
        read_only: false,
    }
}

pub(super) fn merge_env(
    resolved: &BTreeMap<String, BTreeMap<String, (String, bool)>>,
    subject: &str,
    image: Option<&str>,
    port: Option<u16>,
) -> Vec<PodEnv> {
    let mut merged = BTreeMap::new();
    for component in image.into_iter().chain(std::iter::once(subject)) {
        if let Some(vars) = resolved.get(component) {
            for (key, (value, _)) in vars {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    if let Some(port) = port {
        merged.insert("PORT".to_owned(), port.to_string());
    }
    merged
        .into_iter()
        .map(|(key, value)| PodEnv::new(&key, &value))
        .collect()
}

pub(super) fn shell_command(command: &str) -> String {
    command.strip_prefix('@').unwrap_or(command).to_owned()
}

pub(super) fn build_resource_spec(
    node: &ResourceNode,
    world_dir: &Path,
    rootfs: &Path,
    facts: &ImageFacts,
) -> Result<PodSpec, String> {
    // Resource dirs stay plain directories inside the `worlds/<world>` subvolume so one
    // `BTRFS_IOC_SNAP_CREATE_V2` clones PostgreSQL and Redis data atomically on `fork`.
    let data = world_dir.join(node.id.as_str());
    fs::create_dir_all(&data).map_err(|error| format!("create `{}`: {error}", data.display()))?;
    match node.resource {
        ResourceKind::Postgres => {
            let mut spec = postgres(&node.id, rootfs, &data, facts);
            if let Some(database) = node.databases.first() {
                spec.env.push(PodEnv::new("POSTGRES_DB", database));
            }
            Ok(spec)
        }
        ResourceKind::Redis => Ok(redis(&node.id, rootfs, &data, facts)),
        ResourceKind::S3 => Err("the native S3 server has no pod".to_owned()),
    }
}
