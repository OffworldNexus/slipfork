//! The durable per-world `Lease` (`specs/03-under-the-hood.md` §6.3).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{NodeKey, SnapshotError, SnapshotResult};

/// Filename inside `worlds/<world>/`.
pub const LEASE_FILE: &str = ".slipfork-lease.json";

/// One lease entry: the current effect/service key plus the image it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseEntry {
    /// Effect/service key.
    pub key: NodeKey,
    /// Image key the entry runs on.
    pub image: NodeKey,
}

/// What one world runs: keys written verbatim by `__supervise`, read by `gc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    /// Schema version, always `1`.
    pub v: u32,
    /// World id.
    pub world: String,
    /// Parent world id for a fork.
    pub origin: String,
    /// Boot epoch paired with every PG stamp.
    pub world_epoch: String,
    /// Compiled runtime spec digest.
    pub spec: String,
    /// The `world` key over the entries below.
    pub head: NodeKey,
    /// Sealed seed nodes this world clones.
    pub seeds: Vec<NodeKey>,
    /// Service slot -> entry.
    pub services: BTreeMap<String, LeaseEntry>,
    /// Effect slot -> entry.
    pub effects: BTreeMap<String, LeaseEntry>,
}

impl Lease {
    /// Reads and validates a lease; malformed input is an error (gc fails closed).
    pub fn read(path: &Path) -> SnapshotResult<Self> {
        let bytes = fs::read(path).map_err(|source| SnapshotError::Lease {
            path: path.to_path_buf(),
            message: source.to_string(),
        })?;
        let lease: Self =
            serde_json::from_slice(&bytes).map_err(|source| SnapshotError::Lease {
                path: path.to_path_buf(),
                message: source.to_string(),
            })?;
        if lease.v != 1 {
            return Err(SnapshotError::Lease {
                path: path.to_path_buf(),
                message: format!("unsupported lease version {}", lease.v),
            });
        }
        Ok(lease)
    }

    /// Writes the lease beside its world with tmp + rename.
    pub fn write_atomic(&self, path: &Path) -> SnapshotResult<()> {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(format!(".tmp-{}", std::process::id()));
        let tmp = PathBuf::from(tmp);
        let bytes = serde_json::to_vec_pretty(self).map_err(|source| SnapshotError::Lease {
            path: path.to_path_buf(),
            message: source.to_string(),
        })?;
        fs::write(&tmp, bytes).map_err(|source| SnapshotError::io("write", &tmp, source))?;
        fs::rename(&tmp, path).map_err(|source| SnapshotError::io("publish", path, source))
    }

    /// Recomputes the `world` key from this lease's own spec and entries (sorted parent order).
    #[must_use]
    pub fn derived_head(&self) -> NodeKey {
        let mut parents: Vec<NodeKey> = self.seeds.clone();
        parents.extend(self.services.values().map(|entry| entry.key));
        parents.extend(self.effects.values().map(|entry| entry.key));
        NodeKey::world(&parents, &self.spec)
    }

    /// Whether the stored head still equals the head its entries derive.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        self.head == self.derived_head()
    }

    /// Whether `slot`'s effect already ran for this exact key.
    #[must_use]
    pub fn effect_current(&self, slot: &str, key: &NodeKey) -> bool {
        self.effects
            .get(slot)
            .is_some_and(|entry| entry.key == *key)
    }

    /// The image and seed keys gc must never sweep.
    #[must_use]
    pub fn gc_mark(&self) -> BTreeSet<NodeKey> {
        let mut marked: BTreeSet<NodeKey> = self.seeds.iter().copied().collect();
        marked.extend(self.services.values().map(|entry| entry.image));
        marked.extend(self.effects.values().map(|entry| entry.image));
        marked
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{BuildGraph, Node, NodeKind};

    fn image() -> NodeKey {
        NodeKey::base("amd64", "sha256:postgres", "linux/amd64")
    }

    fn lease() -> Lease {
        let image = image();
        let seed = NodeKey::seed("amd64", &image, &["initdb".to_owned()], "v1");
        let service = NodeKey::service("amd64", &image, "api up", "spec-digest");
        let effect = NodeKey::effect("amd64", &image, "setup", "on_ready", "backend:setup#1", "m");
        let mut lease = Lease {
            v: 1,
            world: "demo".to_owned(),
            origin: "demo".to_owned(),
            world_epoch: "k3v9".to_owned(),
            spec: "spec-digest".to_owned(),
            head: image,
            seeds: vec![seed],
            services: BTreeMap::from([(
                "api".to_owned(),
                LeaseEntry {
                    key: service,
                    image,
                },
            )]),
            effects: BTreeMap::from([(
                "backend:setup#1".to_owned(),
                LeaseEntry { key: effect, image },
            )]),
        };
        lease.head = lease.derived_head();
        lease
    }

    #[test]
    fn derived_head_is_the_world_key_and_inconsistency_is_detected() {
        let lease = lease();
        assert!(lease.is_consistent());
        let parents: Vec<NodeKey> = lease
            .seeds
            .iter()
            .copied()
            .chain(lease.services.values().map(|entry| entry.key))
            .chain(lease.effects.values().map(|entry| entry.key))
            .collect();
        assert_eq!(lease.head, NodeKey::world(&parents, &lease.spec));
        let seed_key = lease.seeds[0];
        let service_key = lease.services["api"].key;
        let effect_key = lease.effects["backend:setup#1"].key;
        let graph = BuildGraph::new(
            lease.head,
            vec![
                Node {
                    key: seed_key,
                    kind: NodeKind::Seed,
                    parents: vec![],
                    canon: "seed".to_owned(),
                    line: 1,
                },
                Node {
                    key: service_key,
                    kind: NodeKind::Service,
                    parents: vec![],
                    canon: "service".to_owned(),
                    line: 2,
                },
                Node {
                    key: effect_key,
                    kind: NodeKind::Effect,
                    parents: vec![],
                    canon: "effect".to_owned(),
                    line: 3,
                },
                Node {
                    key: lease.head,
                    kind: NodeKind::World,
                    parents: vec![seed_key, service_key, effect_key],
                    canon: "world".to_owned(),
                    line: 4,
                },
            ],
        )
        .expect("the world head is a valid graph head");
        assert_eq!(graph.head(), lease.head);

        let mut stale = lease.clone();
        stale.services.get_mut("api").expect("api").image = NodeKey::base("amd64", "x", "p");
        assert!(
            stale.is_consistent(),
            "only entry keys feed the derived head"
        );
        stale.services.get_mut("api").expect("api").key =
            NodeKey::service("amd64", &image(), "other", "s");
        assert!(!stale.is_consistent());
    }

    #[test]
    fn effect_current_and_gc_mark() {
        let lease = lease();
        let effect_key = lease.effects["backend:setup#1"].key;
        assert!(lease.effect_current("backend:setup#1", &effect_key));
        assert!(!lease.effect_current("backend:setup#1", &image()));
        let marked = lease.gc_mark();
        assert!(marked.contains(&lease.seeds[0]));
        assert!(marked.contains(&image()));
        assert_eq!(marked.len(), 2, "seeds plus every entry image");
    }
}
